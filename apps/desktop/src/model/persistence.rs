//! Durable shelf/settings/session schema with crash-safe publication.

use super::snapshot::{AppSnapshot, SessionState, SettingsState};
use crate::core::ids::LocalProjectId;
use crate::navigation::{Coordinate, Overlay, PackageLane, Route, SettingsPage};
use backend_platform::durable;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Persistent shelf entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedShelfItem {
    /// Local project path retained for cold reload.
    pub local_path: String,
    /// Stable user-facing label.
    pub label: String,
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
    },
    /// A declaration page route.
    Page {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        coordinate: String,
    },
    /// A source route.
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
                crate::core::ResourceIdentity::Local(id) => Some(PersistedShelfItem {
                    local_path: id.as_str().to_owned(),
                    label: item.label.to_string(),
                }),
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
            route: match snapshot.overlay() {
                Some(Overlay::Settings(_)) => PersistedRoute::Settings,
                Some(Overlay::CommandPalette) | None => match snapshot.route() {
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
                    },
                    Route::Page(route) => PersistedRoute::Page {
                        project: route.project.as_ref().map(|project| project.get().get()),
                        package: route.package.as_str().to_owned(),
                        coordinate: route.coordinate.as_str().to_owned(),
                    },
                    Route::Source(route) => PersistedRoute::Source {
                        project: route.project.as_ref().map(|project| project.get().get()),
                        package: route.package.as_str().to_owned(),
                        page: route.page.as_str().to_owned(),
                        line: route.line,
                    },
                },
            },
            settings_page: match snapshot.overlay() {
                Some(Overlay::Settings(page)) => Some(page.as_str().to_owned()),
                Some(Overlay::CommandPalette) | None => None,
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
            | PersistedRoute::Source { .. } => None,
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
            PersistedRoute::Package {
                project,
                package,
                lane,
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
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::Page {
                project,
                package,
                coordinate,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .zip(Coordinate::new(coordinate).ok())
                    .map(|(package, coordinate)| {
                        Route::Page(crate::navigation::PageRoute {
                            project,
                            package,
                            coordinate,
                            selected: None,
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::Source {
                project,
                package,
                page,
                line,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .zip(Coordinate::new(page).ok())
                    .map(|(package, page)| {
                        Route::Source(crate::navigation::SourceRoute {
                            project,
                            package,
                            page,
                            line: *line,
                            selected: None,
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
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
        }
    }

    /// Parses one local shelf identity at the persistence boundary.
    pub fn local_identity(item: &PersistedShelfItem) -> Option<LocalProjectId> {
        LocalProjectId::new(&item.local_path).ok()
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
        let candidate = candidate.to_string_lossy();
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
                label: "project".to_owned(),
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
    fn command_palette_persists_the_content_route_without_overlay_history() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::new(
            backend_library::view_state_root(&[("persistence".to_owned(), "route".to_owned())]),
            1,
        ));
        let package = crate::core::PackageId::new("pkg").expect("package");
        let session = SessionState {
            route: Route::Page(crate::navigation::PageRoute {
                project: None,
                package,
                coordinate: Coordinate::new("pkg::Item").expect("coordinate"),
                selected: None,
            }),
            overlay: Some(Overlay::CommandPalette),
            ..SessionState::default()
        };
        let snapshot = snapshot.with_session(session);
        let value = PersistentState::project(&snapshot);
        assert!(matches!(value.route, PersistedRoute::Page { .. }));
        assert_eq!(value.settings_page, None);
        let restored = PersistentState::at("unused").cold_reload(&value);
        assert!(matches!(restored.route, Route::Page(_)));
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
