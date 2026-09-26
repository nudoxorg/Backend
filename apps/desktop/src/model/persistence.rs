//! Durable shelf/settings/session schema with crash-safe publication.

use super::snapshot::{AppSnapshot, SessionState, ShelfItem, ShelfState};
use super::workspace::{
    AppearancePreference, ConnectionStatus, ContrastPreference, DensityPreference,
    MotionPreference, PrivacyPreference, ProjectPhase, ServiceMode, SettingsState,
    ZoomPreference, WorkspaceProject, WorkspaceState,
};
use crate::core::ids::LocalProjectId;
use crate::navigation::{Coordinate, Overlay, PackageLane, ReleaseId, Route, SettingsPage, View};
use backend_platform::durable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Persistent shelf entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedShelfItem {
    /// Local project path retained for cold reload.
    pub local_path: String,
    /// User-selected presentation spelling, kept separate from the canonical
    /// native identity so lexical normalization never changes what the user
    /// sees in the shelf.
    #[serde(default)]
    pub display_path: Option<String>,
    /// Reversible native path identity. Older UTF-8 state files may omit it.
    #[serde(default)]
    pub native_path: Option<backend_platform::NativePathWire>,
    /// Stable user-facing label.
    pub label: String,
    /// Last known index lifecycle.
    #[serde(default)]
    pub phase: PersistedProjectPhase,
    /// Last backend-reported scan progress, when available.
    #[serde(default)]
    pub progress: Option<u8>,
    /// Backend-reported file count, when available.
    #[serde(default)]
    pub files_indexed: Option<u64>,
    /// Bounded error from the last index attempt.
    #[serde(default)]
    pub error: Option<String>,
}

/// Serializable project lifecycle vocabulary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedProjectPhase {
    /// Index is current.
    #[default]
    Ready,
    /// Indexing is in progress.
    Indexing,
    /// Cancellation was requested and is waiting for the producer boundary.
    Cancelling,
    /// Indexing was cancelled.
    Cancelled,
    /// Indexing failed.
    Failed,
    /// Folder is no longer available.
    Missing,
}

impl From<ProjectPhase> for PersistedProjectPhase {
    fn from(value: ProjectPhase) -> Self {
        match value {
            ProjectPhase::Ready => Self::Ready,
            ProjectPhase::Indexing => Self::Indexing,
            ProjectPhase::Cancelling => Self::Cancelling,
            ProjectPhase::Cancelled => Self::Cancelled,
            ProjectPhase::Failed => Self::Failed,
            ProjectPhase::Missing => Self::Missing,
        }
    }
}

impl From<PersistedProjectPhase> for ProjectPhase {
    fn from(value: PersistedProjectPhase) -> Self {
        match value {
            PersistedProjectPhase::Ready => Self::Ready,
            PersistedProjectPhase::Indexing => Self::Indexing,
            PersistedProjectPhase::Cancelling => Self::Cancelling,
            PersistedProjectPhase::Cancelled => Self::Cancelled,
            PersistedProjectPhase::Failed => Self::Failed,
            PersistedProjectPhase::Missing => Self::Missing,
        }
    }
}

/// Serializable appearance preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedAppearance {
    /// Dark abyss palette.
    #[default]
    Abyss,
    /// Light glacier palette.
    Glacier,
    /// Follow the operating system.
    System,
}

/// Serializable density preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedDensity {
    /// The boards' spacing.
    #[default]
    Comfortable,
    /// Tighter rows.
    Compact,
    /// The tightest readable rows.
    Dense,
}

/// Serializable motion preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedMotion {
    /// Follow the operating system.
    #[default]
    System,
    /// Full motion.
    Full,
    /// Reduced motion.
    Reduced,
}

/// Serializable contrast preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedContrast {
    /// The boards' palette.
    #[default]
    Normal,
    /// Stronger inks, firmer lines.
    High,
}

/// Serializable privacy preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedPrivacy {
    /// Keep all source/index traffic local.
    #[default]
    LocalOnly,
    /// Allow registry metadata requests.
    RegistryMetadata,
}

/// Serializable service-hosting preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedServiceMode {
    /// Start a local embedded daemon.
    #[default]
    Embedded,
    /// Attach to an existing daemon.
    Attached,
}

/// Rebuilds a declaration route from its persisted spelling, or Orbit when
/// the spelling no longer admits.
fn symbol_route(
    project: Option<u64>,
    package: &str,
    id: &str,
    at: Option<&str>,
    view: View,
    line: Option<u32>,
) -> Route {
    let project = project
        .and_then(std::num::NonZeroU64::new)
        .map(|project| crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project)));
    crate::core::PackageId::new(package)
        .ok()
        .zip(Coordinate::new(id).ok())
        .map(|(package, id)| {
            Route::Symbol(crate::navigation::SymbolRoute {
                project,
                package,
                id,
                at: at.and_then(|at| ReleaseId::new(at).ok()),
                view,
                line,
                selected: None,
            })
        })
        .unwrap_or(Route::Orbit(crate::navigation::OrbitRoute::Home))
}

/// Versioned, forward-compatible desktop state file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedDesktopState {
    /// Schema version for migrations.
    pub schema: u32,
    /// Durable shelf rows.
    pub shelf: Vec<PersistedShelfItem>,
    /// Reduced-motion preference.
    pub reduced_motion: bool,
    /// Shelf visibility preference.
    pub shelf_open: bool,
    /// Context visibility preference.
    pub context_open: bool,
    /// Last route, encoded by a small typed tag.
    pub route: PersistedRoute,
    /// Last settings page.
    pub settings_page: Option<String>,
    /// Active shelf project retained across a cold restart.
    #[serde(default)]
    pub active_project: Option<String>,
    /// Reversible native identity for the active shelf project.
    #[serde(default)]
    pub active_native_path: Option<backend_platform::NativePathWire>,
    /// Surface appearance.
    #[serde(default)]
    pub appearance: PersistedAppearance,
    /// ⌘± zoom steps per display key (older files carried a `text_scale`
    /// setting instead; it is ignored — the system now sets the baseline).
    #[serde(default)]
    pub zoom: std::collections::BTreeMap<String, i8>,
    /// Spacing density.
    #[serde(default)]
    pub density: PersistedDensity,
    /// Contrast treatment.
    #[serde(default)]
    pub contrast: PersistedContrast,
    /// Motion preference; absent in older files, where `reduced_motion`
    /// alone decides.
    #[serde(default)]
    pub motion: Option<PersistedMotion>,
    /// Local/remote registry policy.
    #[serde(default)]
    pub privacy: PersistedPrivacy,
    /// Embedded or attached daemon.
    #[serde(default)]
    pub service_mode: PersistedServiceMode,
    /// Whether advisory data is enabled.
    #[serde(default = "default_true")]
    pub advisories: bool,
    /// Whether immutable responses may be cached.
    #[serde(default = "default_true")]
    pub cache_enabled: bool,
    /// Cache retention in days.
    #[serde(default = "default_cache_days")]
    pub cache_days: u16,
}

fn default_true() -> bool {
    true
}

fn default_cache_days() -> u16 {
    14
}

impl Default for PersistedDesktopState {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            shelf: Vec::new(),
            reduced_motion: false,
            shelf_open: true,
            context_open: true,
            route: PersistedRoute::Home,
            settings_page: None,
            active_project: None,
            active_native_path: None,
            appearance: PersistedAppearance::default(),
            zoom: std::collections::BTreeMap::new(),
            density: PersistedDensity::default(),
            contrast: PersistedContrast::default(),
            motion: None,
            privacy: PersistedPrivacy::default(),
            service_mode: PersistedServiceMode::default(),
            advisories: true,
            cache_enabled: true,
            cache_days: 14,
        }
    }
}

/// Closed route schema used only at the persistence edge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedRoute {
    /// Home route.
    Home,
    /// A producer project selected at the orbit level.
    Project {
        /// Producer project identity.
        project: u64,
    },
    /// A package route with its closed lane.
    Package {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Package lane.
        lane: PersistedPackageLane,
        /// The release viewed instead of the pinned one.
        #[serde(default)]
        at: Option<String>,
    },
    /// A declaration shown as a page, its code, or its graph.
    Symbol {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        id: String,
        /// The release viewed instead of the pinned one.
        #[serde(default)]
        at: Option<String>,
        /// `page`, `code` or `graph`.
        view: String,
        /// The source line the code view opens at.
        #[serde(default)]
        line: Option<u32>,
    },
    /// The whole dependency graph.
    World,
    /// A declaration page route (older files; read as a page view).
    Page {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        coordinate: String,
    },
    /// A source route (older files; read as a code view).
    Source {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        page: String,
        /// Selected source line.
        line: u32,
    },
    /// Settings route; the page is validated on load.
    Settings,
}

/// Serializable package lane vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedPackageLane {
    /// Package summary.
    Overview,
    /// Dependency graph.
    Dependencies,
    /// Reverse dependency graph.
    Dependents,
    /// Release history.
    Releases,
    /// Advisory surface.
    Security,
}

impl From<PackageLane> for PersistedPackageLane {
    fn from(value: PackageLane) -> Self {
        match value {
            PackageLane::Overview => Self::Overview,
            PackageLane::Dependencies => Self::Dependencies,
            PackageLane::Dependents => Self::Dependents,
            PackageLane::Releases => Self::Releases,
            PackageLane::Security => Self::Security,
        }
    }
}

impl From<PersistedPackageLane> for PackageLane {
    fn from(value: PersistedPackageLane) -> Self {
        match value {
            PersistedPackageLane::Overview => Self::Overview,
            PersistedPackageLane::Dependencies => Self::Dependencies,
            PersistedPackageLane::Dependents => Self::Dependents,
            PersistedPackageLane::Releases => Self::Releases,
            PersistedPackageLane::Security => Self::Security,
        }
    }
}

/// A persistence error that keeps the target path visible to diagnostics.
#[derive(Debug)]
pub struct PersistenceError {
    /// Path involved in the operation.
    pub path: PathBuf,
    /// Underlying filesystem or codec error.
    pub source: io::Error,
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for PersistenceError {}

/// Result of admitting the desktop state at cold start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistenceLoad {
    /// Admitted current-schema state, or the safe default after recovery.
    pub state: PersistedDesktopState,
    /// Exact recovery action taken before the state was returned.
    pub recovery: PersistenceRecovery,
}

/// Durable-state admission outcome visible to startup diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceRecovery {
    /// A current-schema state file was admitted unchanged.
    Current,
    /// No state file existed, so the first-run default was admitted.
    Missing,
    /// An unreadable or incompatible file was preserved under `backup`.
    Preserved {
        /// Preserved sibling path containing the original bytes.
        backup: PathBuf,
        /// Why the canonical path could not be admitted.
        reason: PersistenceRecoveryReason,
    },
}

/// Closed reason vocabulary for persistence recovery and support diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceRecoveryReason {
    /// The payload was not valid current-schema JSON.
    Corrupt,
    /// The payload exceeded the bounded desktop-state envelope.
    Oversized {
        /// Observed byte length.
        bytes: u64,
        /// Maximum accepted byte length.
        limit: u64,
    },
    /// The payload uses a schema this binary must not reinterpret.
    IncompatibleSchema {
        /// Schema encoded by the preserved file.
        found: u32,
        /// Schema understood by this binary.
        supported: u32,
    },
}

impl std::fmt::Display for PersistenceRecoveryReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt => f.write_str("invalid JSON or state shape"),
            Self::Oversized { bytes, limit } => {
                write!(f, "state is {bytes} bytes (limit {limit})")
            }
            Self::IncompatibleSchema { found, supported } => {
                write!(f, "schema {found} is incompatible with schema {supported}")
            }
        }
    }
}

/// Atomic state-file owner.
#[derive(Clone, Debug)]
pub struct PersistentState {
    path: PathBuf,
}

impl PersistentState {
    /// Opens a state file path without touching disk.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the exact path used for persistence.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a complete state or defaults when no file exists.
    pub fn load(&self) -> Result<PersistedDesktopState, PersistenceError> {
        let bytes = match read_bounded(&self.path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(PersistedDesktopState::default());
            }
            Err(source) => {
                return Err(PersistenceError {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let value: PersistedDesktopState =
            serde_json::from_slice(&bytes).map_err(|error| PersistenceError {
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, error),
            })?;
        if value.schema != SCHEMA {
            return Err(PersistenceError {
                path: self.path.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "desktop state schema {} is incompatible with schema {SCHEMA}",
                        value.schema
                    ),
                ),
            });
        }
        Ok(value)
    }

    /// Admits current state while preserving corrupt, oversized, or
    /// incompatible bytes for diagnostics. Interrupted sibling temporaries are
    /// discarded because publication only occurs at the canonical rename.
    pub fn load_recovering(&self) -> Result<PersistenceLoad, PersistenceError> {
        let bytes = match read_bounded(&self.path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                cleanup_interrupted_temporaries(&self.path).map_err(|source| PersistenceError {
                    path: self.path.clone(),
                    source,
                })?;
                return Ok(PersistenceLoad {
                    state: PersistedDesktopState::default(),
                    recovery: PersistenceRecovery::Missing,
                });
            }
            Err(source) if source.kind() == io::ErrorKind::InvalidData => {
                let bytes = fs::metadata(&self.path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let reason = PersistenceRecoveryReason::Oversized {
                    bytes,
                    limit: MAX_STATE_BYTES,
                };
                return self.preserve_and_default(reason);
            }
            Err(source) => {
                return Err(PersistenceError {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let value: PersistedDesktopState = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return self.preserve_and_default(PersistenceRecoveryReason::Corrupt),
        };
        if value.schema != SCHEMA {
            return self.preserve_and_default(PersistenceRecoveryReason::IncompatibleSchema {
                found: value.schema,
                supported: SCHEMA,
            });
        }
        cleanup_interrupted_temporaries(&self.path).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })?;
        Ok(PersistenceLoad {
            state: value,
            recovery: PersistenceRecovery::Current,
        })
    }

    fn preserve_and_default(
        &self,
        reason: PersistenceRecoveryReason,
    ) -> Result<PersistenceLoad, PersistenceError> {
        let backup = recovery_path(&self.path, &reason);
        durable::replace_file(&self.path, &backup).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })?;
        durable::sync_parent(&self.path).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })?;
        cleanup_interrupted_temporaries(&self.path).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })?;
        Ok(PersistenceLoad {
            state: PersistedDesktopState::default(),
            recovery: PersistenceRecovery::Preserved { backup, reason },
        })
    }

    /// Publishes one complete state via a sibling temporary and atomic rename.
    pub fn save(&self, value: &PersistedDesktopState) -> Result<(), PersistenceError> {
        if value.schema != SCHEMA {
            return Err(PersistenceError {
                path: self.path.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "refusing to write desktop state schema {}; expected {SCHEMA}",
                        value.schema
                    ),
                ),
            });
        }
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| PersistenceError {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })?;
        durable::write_atomic(&self.path, &bytes).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })
    }

    /// Encodes the snapshot's durable shelf/settings/session branches.
    #[must_use]
    pub fn project(snapshot: &AppSnapshot) -> PersistedDesktopState {
        let shelf = snapshot
            .shelf()
            .items
            .iter()
            .filter_map(|item| match &item.identity {
                crate::core::ResourceIdentity::Local(id) => {
                    let project = snapshot
                        .workspace()
                        .projects
                        .iter()
                        .find(|project| project.id == *id);
                    Some(PersistedShelfItem {
                        local_path: id.as_str().to_owned(),
                        display_path: project.map(|project| project.path.to_string()),
                        native_path: id.native_wire().ok(),
                        label: item.label.to_string(),
                        phase: project
                            .map_or(PersistedProjectPhase::Ready, |project| project.phase.into()),
                        progress: project.and_then(|project| project.progress),
                        files_indexed: project.and_then(|project| project.files_indexed),
                        error: project
                            .and_then(|project| project.error.as_ref().map(ToString::to_string)),
                    })
                }
                crate::core::ResourceIdentity::Package(_)
                | crate::core::ResourceIdentity::Project(_) => None,
            })
            .collect();
        Self::project_session(snapshot, shelf)
    }

    fn project_session(
        snapshot: &AppSnapshot,
        shelf: Vec<PersistedShelfItem>,
    ) -> PersistedDesktopState {
        PersistedDesktopState {
            schema: SCHEMA,
            shelf,
            reduced_motion: snapshot.settings().reduced_motion,
            shelf_open: snapshot.settings().shelf_open,
            context_open: snapshot.settings().context_open,
            active_project: snapshot
                .workspace()
                .active
                .as_ref()
                .map(|project| project.as_str().to_owned()),
            active_native_path: snapshot
                .workspace()
                .active
                .as_ref()
                .and_then(|project| project.native_wire().ok()),
            appearance: match snapshot.settings().appearance {
                AppearancePreference::Abyss => PersistedAppearance::Abyss,
                AppearancePreference::Glacier => PersistedAppearance::Glacier,
                AppearancePreference::System => PersistedAppearance::System,
            },
            zoom: snapshot
                .settings()
                .zoom
                .steps()
                .map(|(display, step)| (display.to_string(), step))
                .collect(),
            density: match snapshot.settings().density {
                DensityPreference::Comfortable => PersistedDensity::Comfortable,
                DensityPreference::Compact => PersistedDensity::Compact,
                DensityPreference::Dense => PersistedDensity::Dense,
            },
            contrast: match snapshot.settings().contrast {
                ContrastPreference::Normal => PersistedContrast::Normal,
                ContrastPreference::High => PersistedContrast::High,
            },
            motion: Some(match snapshot.settings().motion {
                MotionPreference::System => PersistedMotion::System,
                MotionPreference::Full => PersistedMotion::Full,
                MotionPreference::Reduced => PersistedMotion::Reduced,
            }),
            privacy: match snapshot.settings().privacy {
                PrivacyPreference::LocalOnly => PersistedPrivacy::LocalOnly,
                PrivacyPreference::RegistryMetadata => PersistedPrivacy::RegistryMetadata,
            },
            service_mode: match snapshot.settings().service_mode {
                ServiceMode::Embedded => PersistedServiceMode::Embedded,
                ServiceMode::Attached => PersistedServiceMode::Attached,
            },
            advisories: snapshot.settings().advisories,
            cache_enabled: snapshot.settings().cache_enabled,
            cache_days: snapshot.settings().cache_days,
            route: match snapshot.overlay() {
                Some(Overlay::Settings(_)) => PersistedRoute::Settings,
                Some(Overlay::AddProject | Overlay::CommandPalette | Overlay::Inbox) | None => {
                    match snapshot.route() {
                        Route::Orbit(crate::navigation::OrbitRoute::Home) => PersistedRoute::Home,
                        Route::Orbit(crate::navigation::OrbitRoute::Project(project)) => {
                            PersistedRoute::Project {
                                project: project.get().get(),
                            }
                        }
                        Route::Package(route) => PersistedRoute::Package {
                            project: route.project.as_ref().map(|project| project.get().get()),
                            package: route.package.as_str().to_owned(),
                            lane: route.lane.into(),
                            at: route.at.as_ref().map(|at| at.as_str().to_owned()),
                        },
                        Route::Symbol(route) => PersistedRoute::Symbol {
                            project: route.project.as_ref().map(|project| project.get().get()),
                            package: route.package.as_str().to_owned(),
                            id: route.id.as_str().to_owned(),
                            at: route.at.as_ref().map(|at| at.as_str().to_owned()),
                            view: route.view.as_str().to_owned(),
                            line: route.line,
                        },
                        Route::World => PersistedRoute::World,
                    }
                }
            },
            settings_page: match snapshot.overlay() {
                Some(Overlay::Settings(page)) => Some(page.as_str().to_owned()),
                Some(Overlay::AddProject | Overlay::CommandPalette | Overlay::Inbox) | None => None,
            },
        }
    }

    /// Restores only durable session information.  Engine-owned content is
    /// still fetched after the root is admitted.
    pub fn cold_reload(&self, state: &PersistedDesktopState) -> SessionState {
        let settings_page = state
            .settings_page
            .as_deref()
            .and_then(SettingsPage::parse)
            .unwrap_or_default();
        let overlay = match &state.route {
            PersistedRoute::Settings => Some(Overlay::Settings(settings_page)),
            PersistedRoute::Home
            | PersistedRoute::Project { .. }
            | PersistedRoute::Package { .. }
            | PersistedRoute::Page { .. }
            | PersistedRoute::Source { .. }
            | PersistedRoute::Symbol { .. }
            | PersistedRoute::World => None,
        };
        let route = match &state.route {
            PersistedRoute::Settings | PersistedRoute::Home => {
                Route::Orbit(crate::navigation::OrbitRoute::Home)
            }
            PersistedRoute::Project { project } => std::num::NonZeroU64::new(*project)
                .map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                })
                .map(crate::navigation::OrbitRoute::Project)
                .map(Route::Orbit)
                .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::World => Route::World,
            PersistedRoute::Package {
                project,
                package,
                lane,
                at,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .map(|package| {
                        Route::Package(crate::navigation::PackageRoute {
                            project,
                            package,
                            lane: (*lane).into(),
                            selected: None,
                            at: at.as_deref().and_then(|at| ReleaseId::new(at).ok()),
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::Page {
                project,
                package,
                coordinate,
            } => symbol_route(*project, package, coordinate, None, View::Page, None),
            PersistedRoute::Source {
                project,
                package,
                page,
                line,
            } => symbol_route(*project, package, page, None, View::Code, Some(*line)),
            PersistedRoute::Symbol {
                project,
                package,
                id,
                at,
                view,
                line,
            } => symbol_route(
                *project,
                package,
                id,
                at.as_deref(),
                View::parse(view).unwrap_or_default(),
                *line,
            ),
        };
        SessionState {
            route,
            overlay,
            ..SessionState::default()
        }
    }

    /// Restores persistent settings into the immutable snapshot builder.
    #[must_use]
    pub fn cold_settings(&self, state: &PersistedDesktopState) -> SettingsState {
        SettingsState {
            reduced_motion: state.reduced_motion,
            shelf_open: state.shelf_open,
            context_open: state.context_open,
            appearance: match state.appearance {
                PersistedAppearance::Abyss => AppearancePreference::Abyss,
                PersistedAppearance::Glacier => AppearancePreference::Glacier,
                PersistedAppearance::System => AppearancePreference::System,
            },
            zoom: ZoomPreference::from_steps(
                state
                    .zoom
                    .iter()
                    .map(|(display, step)| (std::sync::Arc::from(display.as_str()), *step)),
            ),
            density: match state.density {
                PersistedDensity::Comfortable => DensityPreference::Comfortable,
                PersistedDensity::Compact => DensityPreference::Compact,
                PersistedDensity::Dense => DensityPreference::Dense,
            },
            contrast: match state.contrast {
                PersistedContrast::Normal => ContrastPreference::Normal,
                PersistedContrast::High => ContrastPreference::High,
            },
            motion: match state.motion {
                Some(PersistedMotion::System) => MotionPreference::System,
                Some(PersistedMotion::Full) => MotionPreference::Full,
                Some(PersistedMotion::Reduced) => MotionPreference::Reduced,
                None if state.reduced_motion => MotionPreference::Reduced,
                None => MotionPreference::System,
            },
            privacy: match state.privacy {
                PersistedPrivacy::LocalOnly => PrivacyPreference::LocalOnly,
                PersistedPrivacy::RegistryMetadata => PrivacyPreference::RegistryMetadata,
            },
            service_mode: match state.service_mode {
                PersistedServiceMode::Embedded => ServiceMode::Embedded,
                PersistedServiceMode::Attached => ServiceMode::Attached,
            },
            advisories: state.advisories,
            cache_enabled: state.cache_enabled,
            cache_days: state.cache_days,
            connection: ConnectionStatus::Unknown,
        }
    }

    /// Restores durable project rows and marks folders that disappeared while
    /// Nudox was closed. The path is retained so the user can repair it.
    #[must_use]
    pub fn cold_workspace(&self, state: &PersistedDesktopState) -> WorkspaceState {
        let mut seen = BTreeSet::new();
        let projects = state
            .shelf
            .iter()
            .filter_map(|item| {
                let id = Self::local_identity(item)?;
                // Older state files could contain the same local path more
                // than once. Keep the first durable row so the shelf and
                // active workspace cannot diverge after a restart. The
                // identity is native-unit based when the newer wire field is
                // present, so non-UTF-8 folders remain distinct.
                if !seen.insert(id.clone()) {
                    return None;
                }
                let path = id.path();
                let phase = if path.is_dir() {
                    match item.phase {
                        // There is no live request to observe after restart;
                        // re-admit the folder as a fresh authoritative index.
                        PersistedProjectPhase::Cancelling => ProjectPhase::Indexing,
                        phase => phase.into(),
                    }
                } else {
                    ProjectPhase::Missing
                };
                let display_path: Arc<str> = item
                    .display_path
                    .as_deref()
                    .unwrap_or_else(|| id.as_str())
                    .into();
                Some(WorkspaceProject {
                    id,
                    path: display_path,
                    label: item.label.clone().into(),
                    phase,
                    progress: item.progress.map(|progress| progress.min(100)),
                    files_indexed: item.files_indexed,
                    request: None,
                    error: item.error.clone().map(Into::into),
                    recent: true,
                })
            })
            .collect::<Vec<_>>();
        let active = state
            .active_native_path
            .as_ref()
            .and_then(|wire| LocalProjectId::from_native_wire(wire).ok())
            .and_then(|active| projects.iter().find(|project| project.id == active))
            .or_else(|| {
                state
                    .active_project
                    .as_deref()
                    .and_then(|active| LocalProjectId::new(active).ok())
                    .and_then(|active| projects.iter().find(|project| project.id == active))
            })
            .map(|project| project.id.clone())
            .or_else(|| projects.first().map(|project| project.id.clone()));
        WorkspaceState {
            active,
            host: None,
            projects: projects.into(),
            path_error: None,
        }
    }

    /// Restores one durable shelf and merges the service's selected workspace.
    ///
    /// The host path is optional because ambient startup can point at a
    /// directory that is not a project boundary. A real project path is
    /// admitted exactly once, while the persisted active path remains visible
    /// when it differs so the UI can explain the workspace mismatch.
    #[must_use]
    pub fn cold_shelf(
        &self,
        state: &PersistedDesktopState,
        host_project: Option<&Path>,
    ) -> (ShelfState, WorkspaceState) {
        let mut workspace = self.cold_workspace(state);
        let mut shelf = Vec::with_capacity(state.shelf.len().saturating_add(1));
        for item in &state.shelf {
            let Some(project) = Self::local_identity(item) else {
                continue;
            };
            if shelf.iter().any(|existing: &ShelfItem| {
                existing.identity == crate::core::ResourceIdentity::Local(project.clone())
            }) {
                continue;
            }
            shelf.push(ShelfItem {
                object: crate::model::ObjectId::from_backend(project.key()),
                identity: crate::core::ResourceIdentity::Local(project),
                label: item.label.clone().into(),
            });
        }
        if let Some(host) = host_project {
            if let Ok(project) = LocalProjectId::from_path(host) {
                if !shelf.iter().any(|item| {
                    item.identity == crate::core::ResourceIdentity::Local(project.clone())
                }) {
                    shelf.push(ShelfItem {
                        object: crate::model::ObjectId::from_backend(project.key()),
                        identity: crate::core::ResourceIdentity::Local(project.clone()),
                        label: host
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Workspace")
                            .to_owned()
                            .into(),
                    });
                }
                let mut projects = workspace.projects.to_vec();
                if let Some(existing) = projects.iter_mut().find(|item| item.id == project) {
                    if existing.phase == ProjectPhase::Missing && host.is_dir() {
                        existing.phase = ProjectPhase::Indexing;
                        existing.progress = None;
                        existing.files_indexed = None;
                        existing.error = None;
                    }
                } else {
                    projects.push(WorkspaceProject {
                        id: project.clone(),
                        // The host path is shown as a label only; persisted
                        // identity is carried by `NativePathWire` above.
                        path: host.display().to_string().into(),
                        label: host
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Workspace")
                            .to_owned()
                            .into(),
                        phase: if host.is_dir() {
                            ProjectPhase::Indexing
                        } else {
                            ProjectPhase::Missing
                        },
                        progress: None,
                        files_indexed: None,
                        request: None,
                        error: None,
                        recent: true,
                    });
                }
                workspace.projects = projects.into();
                if workspace.active.is_none() {
                    workspace.active = Some(project.clone());
                }
            }
        }
        let selected = workspace
            .active
            .as_ref()
            .and_then(|active| {
                shelf.iter().find_map(|item| match &item.identity {
                    crate::core::ResourceIdentity::Local(project) if project == active => {
                        Some(item.identity.clone())
                    }
                    _ => None,
                })
            })
            .or_else(|| shelf.first().map(|item| item.identity.clone()));
        (
            ShelfState {
                selected,
                items: shelf.into(),
            },
            workspace,
        )
    }

    /// Parses one local shelf identity at the persistence boundary.
    pub fn local_identity(item: &PersistedShelfItem) -> Option<LocalProjectId> {
        item.native_path
            .as_ref()
            .and_then(|wire| LocalProjectId::from_native_wire(wire).ok())
            .or_else(|| LocalProjectId::new(&item.local_path).ok())
    }
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let bytes = file.metadata()?.len();
    if bytes > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("desktop state is {bytes} bytes; limit is {MAX_STATE_BYTES}"),
        ));
    }
    let mut payload = Vec::with_capacity(bytes as usize);
    file.take(MAX_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut payload)?;
    if payload.len() as u64 > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("desktop state exceeded {MAX_STATE_BYTES} bytes while it was being read"),
        ));
    }
    Ok(payload)
}

fn recovery_path(path: &Path, reason: &PersistenceRecoveryReason) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("desktop-state.json");
    let label = match reason {
        PersistenceRecoveryReason::Corrupt => "corrupt".to_owned(),
        PersistenceRecoveryReason::Oversized { .. } => "oversized".to_owned(),
        PersistenceRecoveryReason::IncompatibleSchema { found, .. } => {
            format!("schema-{found}")
        }
    };
    loop {
        let sequence = RECOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.{label}.{}.{}.preserved",
            std::process::id(),
            sequence
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
}

fn cleanup_interrupted_temporaries(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!(".{name}.");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let candidate = entry.file_name();
        let Some(candidate) = candidate.to_str() else {
            continue;
        };
        if candidate.starts_with(&prefix) && candidate.ends_with(".tmp") {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("nudox-v3-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    #[test]
    fn cold_reload_reads_a_complete_atomic_state() {
        let root = fixture("persistence");
        let store = PersistentState::at(root.join("desktop.json"));
        let value = PersistedDesktopState {
            shelf: vec![PersistedShelfItem {
                local_path: "/tmp/project".to_owned(),
                display_path: None,
                native_path: None,
                label: "project".to_owned(),
                phase: PersistedProjectPhase::Ready,
                progress: None,
                files_indexed: None,
                error: None,
            }],
            route: PersistedRoute::Settings,
            settings_page: Some("appearance".to_owned()),
            ..PersistedDesktopState::default()
        };
        store.save(&value).expect("save");
        assert_eq!(store.load().expect("load"), value);
        let session = store.cold_reload(&value);
        assert_eq!(
            session.route,
            Route::Orbit(crate::navigation::OrbitRoute::Home)
        );
        assert_eq!(
            session.overlay,
            Some(Overlay::Settings(SettingsPage::Appearance))
        );
        let settings = store.cold_settings(&value);
        assert!(settings.shelf_open);
        assert!(settings.context_open);
        assert!(PersistentState::local_identity(&value.shelf[0]).is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cold_workspace_deduplicates_legacy_shelf_paths() {
        let state = PersistedDesktopState {
            shelf: vec![
                PersistedShelfItem {
                    local_path: "/tmp/nudox-duplicate-project".to_owned(),
                    display_path: None,
                    native_path: None,
                    label: "first".to_owned(),
                    phase: PersistedProjectPhase::Ready,
                    progress: None,
                    files_indexed: None,
                    error: None,
                },
                PersistedShelfItem {
                    local_path: "/tmp/nudox-duplicate-project".to_owned(),
                    display_path: None,
                    native_path: None,
                    label: "second".to_owned(),
                    phase: PersistedProjectPhase::Ready,
                    progress: None,
                    files_indexed: None,
                    error: None,
                },
            ],
            active_project: Some("/tmp/nudox-duplicate-project".to_owned()),
            ..PersistedDesktopState::default()
        };
        let workspace = PersistentState::at("unused").cold_workspace(&state);

        assert_eq!(workspace.projects.len(), 1);
        assert_eq!(workspace.projects[0].label.as_ref(), "first");
        assert_eq!(
            workspace.active.as_ref().map(LocalProjectId::as_str),
            Some("/tmp/nudox-duplicate-project")
        );
    }

    #[test]
    fn cold_shelf_keeps_active_selection_visible_when_service_host_differs() {
        let active = "/tmp/nudox-selected-project";
        let host = "/tmp/nudox-served-project";
        let state = PersistedDesktopState {
            shelf: vec![PersistedShelfItem {
                local_path: active.to_owned(),
                display_path: None,
                native_path: None,
                label: "selected".to_owned(),
                phase: PersistedProjectPhase::Ready,
                progress: None,
                files_indexed: None,
                error: None,
            }],
            active_project: Some(active.to_owned()),
            ..PersistedDesktopState::default()
        };

        let (shelf, workspace) =
            PersistentState::at("unused").cold_shelf(&state, Some(Path::new(host)));

        assert_eq!(
            workspace.active.as_ref().map(LocalProjectId::as_str),
            Some(active)
        );
        assert_eq!(
            shelf.selected.as_ref().and_then(|identity| match identity {
                crate::core::ResourceIdentity::Local(project) => Some(project.as_str()),
                _ => None,
            }),
            Some(active)
        );
        assert!(
            workspace
                .projects
                .iter()
                .any(|project| project.path.as_ref() == host)
        );
        assert!(shelf.items.iter().any(|item| match &item.identity {
            crate::core::ResourceIdentity::Local(project) => project.as_str() == host,
            _ => false,
        }));
    }

    #[test]
    fn command_palette_persists_the_content_route_without_overlay_history() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("persistence".to_owned(), "route".to_owned())]),
            1,
        ));
        let package = crate::core::PackageId::new("pkg").expect("package");
        let session = SessionState {
            route: Route::Symbol(crate::navigation::SymbolRoute {
                project: None,
                package,
                id: Coordinate::new("pkg::Item").expect("coordinate"),
                at: Some(ReleaseId::new("1.0.190").expect("release")),
                view: View::Code,
                line: Some(12),
                selected: None,
            }),
            overlay: Some(Overlay::CommandPalette),
            ..SessionState::default()
        };
        let snapshot = snapshot.with_session(session);
        let value = PersistentState::project(&snapshot);
        assert!(matches!(value.route, PersistedRoute::Symbol { .. }));
        assert_eq!(value.settings_page, None);
        let restored = PersistentState::at("unused").cold_reload(&value);
        assert_eq!(restored.route, snapshot.route().clone(), "view, release and line survive");
        assert_eq!(restored.overlay, None);
    }

    #[test]
    fn corrupt_state_is_preserved_and_cold_start_recovers() {
        let root = fixture("persistence-corrupt");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let corrupt = br#"{"schema":1,"shelf":["#;
        fs::write(&path, corrupt).expect("write corrupt fixture");

        let admitted = store.load_recovering().expect("recover corrupt state");
        assert_eq!(admitted.state, PersistedDesktopState::default());
        let PersistenceRecovery::Preserved { backup, reason } = admitted.recovery else {
            panic!("corrupt state must be preserved");
        };
        assert_eq!(reason, PersistenceRecoveryReason::Corrupt);
        assert_eq!(fs::read(backup).expect("preserved bytes"), corrupt);
        assert!(!path.exists());

        store
            .save(&admitted.state)
            .expect("publish recovered default");
        assert_eq!(
            store.load().expect("read recovered default"),
            admitted.state
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn future_schema_is_never_reinterpreted_or_overwritten() {
        let root = fixture("persistence-future");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let mut future = PersistedDesktopState::default();
        future.schema = SCHEMA.saturating_add(1);
        let bytes = serde_json::to_vec(&future).expect("encode future fixture");
        fs::write(&path, &bytes).expect("write future fixture");

        assert!(store.load().is_err());
        let admitted = store.load_recovering().expect("preserve future state");
        let PersistenceRecovery::Preserved { backup, reason } = admitted.recovery else {
            panic!("future state must be preserved");
        };
        assert_eq!(
            reason,
            PersistenceRecoveryReason::IncompatibleSchema {
                found: SCHEMA.saturating_add(1),
                supported: SCHEMA,
            }
        );
        assert_eq!(fs::read(backup).expect("preserved bytes"), bytes);
        assert!(!path.exists());
        assert!(store.save(&future).is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn interrupted_temporary_never_competes_with_the_canonical_state() {
        let root = fixture("persistence-interrupted");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let current = PersistedDesktopState {
            reduced_motion: true,
            ..PersistedDesktopState::default()
        };
        store.save(&current).expect("publish current state");
        let interrupted = root.join(format!(".desktop.json.{}.999999.tmp", std::process::id()));
        fs::write(&interrupted, br#"{"schema":1"#).expect("write interrupted temporary");

        let admitted = store.load_recovering().expect("admit canonical state");
        assert_eq!(admitted.state, current);
        assert_eq!(admitted.recovery, PersistenceRecovery::Current);
        assert!(!interrupted.exists());
        assert_eq!(store.load().expect("canonical survives"), current);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn oversized_state_is_bounded_before_json_allocation() {
        let root = fixture("persistence-oversized");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let oversized = vec![b' '; MAX_STATE_BYTES as usize + 1];
        fs::write(&path, &oversized).expect("write oversized fixture");

        let admitted = store.load_recovering().expect("preserve oversized state");
        let PersistenceRecovery::Preserved { backup, reason } = admitted.recovery else {
            panic!("oversized state must be preserved");
        };
        assert_eq!(
            reason,
            PersistenceRecoveryReason::Oversized {
                bytes: MAX_STATE_BYTES + 1,
                limit: MAX_STATE_BYTES,
            }
        );
        assert_eq!(
            fs::metadata(backup).expect("preserved metadata").len(),
            MAX_STATE_BYTES + 1
        );
        assert_eq!(admitted.state, PersistedDesktopState::default());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
