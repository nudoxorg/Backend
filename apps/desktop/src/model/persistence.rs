//! Durable shelf/settings/session schema with crash-safe publication.

use super::snapshot::{AppSnapshot, PendingSelectionClaim, SessionState, ShelfItem, ShelfState};
use super::workspace::{
    AppearancePreference, ConnectionStatus, ContrastPreference, DensityPreference,
    MotionPreference, PrivacyPreference, ProjectPhase, ServiceMode, SettingsState,
    ZoomPreference, WindowSize, WorkspaceProject, WorkspaceState,
};
use crate::core::ids::LocalProjectId;
use crate::navigation::{BrowseRoute, CargoBrowseContext, CargoReadmeLinkAddress, CargoSourcePath, CargoSourceRoute, CargoSourceTarget, CompareSet, Coordinate, Overlay, PackageLane, ReleaseId, Route, SettingsPage, View};
use backend_platform::durable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One card of the hand. The order is not stored: it is recomputed from
/// the held set on load.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedHeld {
    /// The package's route spelling.
    pub package: String,
    /// The declaration's coordinate (none for a held package).
    #[serde(default)]
    pub coordinate: Option<String>,
    /// `pin`, `copy`, `compare` or `add`.
    pub why: String,
    /// When it was held (unix ms).
    pub held_at: u64,
    /// When it was last touched (unix ms).
    pub touched_at: u64,
}

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
    /// Exact caller key and payload saved before the first owner send.
    #[serde(default)]
    pub operation: Option<super::index_operation::IndexOperationClaim>,
}

/// Serializable project lifecycle vocabulary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedProjectPhase {
    /// Index is current.
    #[default]
    Ready,
    /// A local folder is admitted, but no owner request has been submitted.
    Queued,
    /// Indexing is in progress.
    Indexing,
    /// Cancellation was requested and is waiting for the producer boundary.
    Cancelling,
    /// Indexing was cancelled.
    Cancelled,
    /// Indexing failed.
    Failed,
    /// An index request may have committed; its receipt must be reconciled.
    Unconfirmed,
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
            ProjectPhase::Unconfirmed => Self::Unconfirmed,
            ProjectPhase::Missing => Self::Missing,
        }
    }
}

impl From<PersistedProjectPhase> for ProjectPhase {
    fn from(value: PersistedProjectPhase) -> Self {
        match value {
            PersistedProjectPhase::Ready => Self::Ready,
            PersistedProjectPhase::Queued | PersistedProjectPhase::Indexing => Self::Indexing,
            PersistedProjectPhase::Cancelling => Self::Cancelling,
            PersistedProjectPhase::Cancelled => Self::Cancelled,
            PersistedProjectPhase::Failed => Self::Failed,
            PersistedProjectPhase::Unconfirmed => Self::Unconfirmed,
            PersistedProjectPhase::Missing => Self::Missing,
        }
    }
}

#[cfg(test)]
mod unconfirmed_phase_tests {
    use super::{PersistedProjectPhase, ProjectPhase};

    #[test]
    fn uncertain_mutation_survives_the_persisted_phase_boundary() {
        let saved: PersistedProjectPhase = ProjectPhase::Unconfirmed.into();
        assert_eq!(saved, PersistedProjectPhase::Unconfirmed);
        let bytes = serde_json::to_vec(&saved).expect("phase wire");
        let restored: PersistedProjectPhase = serde_json::from_slice(&bytes).expect("phase wire");
        assert_eq!(ProjectPhase::from(restored), ProjectPhase::Unconfirmed);
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
                at: at.map(ReleaseId::from_persisted),
                view,
                line,
                selected: None,
            })
        })
        .unwrap_or(Route::Orbit(crate::navigation::OrbitRoute::Home))
}

/// The window's size when it was last resized, in logical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedWindow {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Untrusted cold-start UI focus claim. The route and owner root are repeated
/// so an edited or stale digest cannot silently attach to a different place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedSelectionClaim {
    /// Exact route that carried the selection when saved.
    pub route: PersistedRoute,
    /// Hex spelling of the owner view root; never decoded into authority.
    pub root: String,
    /// Hex spelling of the UI object key; never decoded into [`super::ObjectId`].
    pub object: String,
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
    /// Saved focus claim; a current owner catalog must resolve it after load.
    #[serde(default)]
    pub selected_claim: Option<PersistedSelectionClaim>,
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
    /// Registry network policy used by this desktop's next embedded service start.
    #[serde(default)]
    pub privacy: PersistedPrivacy,
    /// Embedded or attached daemon.
    #[serde(default)]
    pub service_mode: PersistedServiceMode,
    /// Whether explicit advisory-feed refreshes are enabled.
    #[serde(default = "default_true")]
    pub advisories: bool,
    /// Whether recently admitted registry results may be reused.
    #[serde(default = "default_true")]
    pub cache_enabled: bool,
    /// Maximum reusable registry-result age in days.
    #[serde(default = "default_cache_days")]
    pub cache_days: u16,
    /// What you hold (at most five).
    #[serde(default)]
    pub hand: Vec<PersistedHeld>,
    /// The first-card whisper has been shown (once per install).
    #[serde(default)]
    pub hand_whispered: bool,
    /// The window's size when it was last resized (absent in older files).
    #[serde(default)]
    pub window: Option<PersistedWindow>,
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
            selected_claim: None,
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
            hand: Vec::new(),
            hand_whispered: false,
            window: None,
        }
    }
}

/// An address-only copy of the complete Cargo Tree binding. Loading this
/// value never admits a resource or an owner attachment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedCargoBrowse {
    /// Exact submitted Tree directory, which may be a workspace member.
    pub project: String,
    /// Unchanged requested/effective commitments from the producer.
    pub request_binding: backend_library::browse::ProjectTreeRequestBindingV1,
}

impl PersistedCargoBrowse {
    fn project(context: &CargoBrowseContext) -> Option<Self> {
        Some(Self { project: context.requested_project().service_coordinate().ok()?.to_owned(), request_binding: context.request_binding() })
    }
    fn restore(&self) -> Option<CargoBrowseContext> {
        CargoBrowseContext::from_binding_address(LocalProjectId::from_path(Path::new(&self.project)).ok()?, self.request_binding)
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
    /// A local project's dependency tree.
    Tree {
        /// Native project path, admitted again on reload.
        project: String,
    },
    /// Find before a query is entered.
    FindHome,
    /// Find results at a stable query and page size.
    Find {
        /// The indexed query.
        text: String,
        /// Requested result page size.
        limit: u16,
    },
    /// Two to four packages in their chosen order.
    Compare {
        /// Exact package references.
        packages: Vec<String>,
    },
    /// A package route with its closed lane.
    Package {
        /// Independent Cargo browse address; legacy package routes omit it.
        #[serde(default)]
        cargo: Option<PersistedCargoBrowse>,
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
    /// A Cargo source file address; cold reload must ask the owner again.
    CargoSource {
        /// Complete owner binding. Legacy addresses must observe Tree first.
        #[serde(default)]
        request_binding: Option<backend_library::browse::ProjectTreeRequestBindingV1>,
        /// Exact project-tree address for owner observation rehydration.
        /// Older files had no project and cannot restore this capability.
        #[serde(default)]
        project: Option<String>,
        /// Full source-qualified package coordinate, including authority digest.
        package: String,
        /// Canonical package-relative file spelling.
        file: String,
        /// One-based line to reveal after revalidation.
        line: Option<u32>,
    },
    /// A README link address retaining the owner's exact package/workspace scope.
    CargoReadmeLink {
        /// Exact submitted project and complete root commitments.
        browse: PersistedCargoBrowse,
        /// Full source-qualified Cargo package address.
        package: String,
        /// Complete original README receipt, revalidated before reading bytes.
        origin: backend_library::CargoPackageReadmeOriginV1,
        /// Authored href resolved relative to the original README directory.
        href: String,
        /// Optional one-based source line address.
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

/// Keeps the content place even while Settings is layered over it. The
/// transient overlay is recorded separately in `settings_page`.
pub(crate) fn display_route_claim(route: &Route) -> PersistedRoute { persist_route(route) }

fn persist_route(route: &Route) -> PersistedRoute {
    match route {
        Route::Orbit(crate::navigation::OrbitRoute::Home) => PersistedRoute::Home,
        Route::Orbit(crate::navigation::OrbitRoute::Project(project)) => PersistedRoute::Project {
            project: project.get().get(),
        },
        Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => match browse {
            BrowseRoute::Tree(project) => PersistedRoute::Tree { project: project.as_str().to_owned() },
            BrowseRoute::FindHome => PersistedRoute::FindHome,
            BrowseRoute::Find(query) => PersistedRoute::Find { text: query.text.to_string(), limit: query.limit },
            BrowseRoute::Compare(selection) => PersistedRoute::Compare {
                packages: selection.packages().iter().map(|package| package.as_str().to_owned()).collect(),
            },
        },
        Route::Package(route) => PersistedRoute::Package {
            cargo: route.cargo.as_ref().and_then(PersistedCargoBrowse::project),
            project: route.project.as_ref().map(|project| project.get().get()),
            package: route.package.as_str().to_owned(),
            lane: route.lane.into(),
            at: route.at.as_ref().map(|at| at.persisted_wire().to_owned()),
        },
        Route::Symbol(route) => PersistedRoute::Symbol {
            project: route.project.as_ref().map(|project| project.get().get()),
            package: route.package.as_str().to_owned(),
            id: route.id.as_str().to_owned(),
            at: route.at.as_ref().map(|at| at.persisted_wire().to_owned()),
            view: route.view.as_str().to_owned(),
            line: route.line,
        },
        Route::CargoSource(route) => match &route.target {
            CargoSourceTarget::PackageFile(file) => PersistedRoute::CargoSource {
                request_binding: route.browse.context().map(CargoBrowseContext::request_binding),
                project: route.browse.requested_project().service_coordinate().ok().map(str::to_owned),
                package: route.package.as_str().to_owned(), file: file.as_str().to_owned(), line: route.line,
            },
            CargoSourceTarget::ReadmeLink(link) => route.browse.context().and_then(PersistedCargoBrowse::project)
                .map_or(PersistedRoute::Home, |browse| PersistedRoute::CargoReadmeLink {
                    browse, package: route.package.as_str().to_owned(), origin: link.origin().clone(), href: link.href().to_owned(), line: route.line,
                }),
        },
        Route::World => PersistedRoute::World,
    }
}

fn restore_cargo_readme_link(browse: &PersistedCargoBrowse, package: &str, origin: &backend_library::CargoPackageReadmeOriginV1, href: &str, line: Option<u32>) -> Option<CargoSourceRoute> {
    CargoSourceRoute::readme_link(browse.restore()?, crate::core::PackageId::new(package).ok()?, CargoReadmeLinkAddress::new(origin.clone(), href)?, line)
}

fn restore_cargo_source_address(
    project: Option<&str>,
    request_binding: Option<backend_library::browse::ProjectTreeRequestBindingV1>,
    package: &str, file: &str, line: Option<u32>,
) -> Option<CargoSourceRoute> {
    let project = LocalProjectId::from_path(Path::new(project?)).ok()?;
    let package = crate::core::PackageId::new(package).ok()?;
    let file = CargoSourcePath::new(file)?;
    if let Some(context) = request_binding.and_then(|binding| CargoBrowseContext::from_binding_address(project.clone(), binding)) {
        CargoSourceRoute::new(context, package, file, line)
    } else {
        CargoSourceRoute::from_unbound_saved(project, package, file, line)
    }
}

impl PersistedDesktopState {
    /// A legacy source with no usable requested project remains a recovery
    /// notice on Library. It cannot be rebound to the active workspace.
    pub(crate) fn cargo_source_recovery_note(&self) -> Option<super::workspace::Note> {
        match &self.route {
            PersistedRoute::CargoSource { project, request_binding, package, file, line }
                if restore_cargo_source_address(project.as_deref(), *request_binding, package, file, *line).is_none() => Some(crate::model::workspace::Note::CargoSourceAddressUnread),
            PersistedRoute::CargoReadmeLink { browse, package, origin, href, line }
                if restore_cargo_readme_link(browse, package, origin, href, *line).is_none() => Some(crate::model::workspace::Note::CargoSourceAddressUnread),
            _ => None,
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
    /// An oversized file could not be admitted into the byte budget; its
    /// original pathname was left untouched for the next launch or support.
    RetainedAtSource {
        /// The original state path, kept without another read.
        path: PathBuf,
        /// Why this launch used defaults.
        reason: PersistenceRecoveryReason,
    },
}

impl PersistenceRecovery {
    /// What the window says about this recovery, when there is anything to
    /// say: an unreadable session was kept, and this launch started fresh.
    #[must_use]
    pub fn note(&self) -> Option<super::workspace::Note> {
        match self {
            Self::Current | Self::Missing => None,
            Self::Preserved { backup, reason } | Self::RetainedAtSource { path: backup, reason } => Some(super::workspace::Note::StateKept {
                backup: Arc::from(backup.display().to_string()),
                why: Arc::from(reason.to_string()),
            }),
        }
    }
}

/// Closed reason vocabulary for persistence recovery and support diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceRecoveryReason {
    /// The payload was not valid current-schema JSON.
    Corrupt,
    /// The payload exceeded the bounded desktop-state envelope.
    Oversized {
        /// Lower bound established by the bounded read, without restating
        /// a pathname that another writer could replace.
        at_least: u64,
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
            Self::Oversized { at_least, limit } => {
                write!(f, "state is at least {at_least} bytes (limit {limit})")
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
                let reason = PersistenceRecoveryReason::Oversized {
                    at_least: MAX_STATE_BYTES + 1,
                    limit: MAX_STATE_BYTES,
                };
                return Ok(PersistenceLoad {
                    state: PersistedDesktopState::default(),
                    recovery: PersistenceRecovery::RetainedAtSource { path: self.path.clone(), reason },
                });
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
            Err(_) => return self.preserve_observed_and_default(&bytes, PersistenceRecoveryReason::Corrupt),
        };
        if value.schema != SCHEMA {
            return self.preserve_observed_and_default(&bytes, PersistenceRecoveryReason::IncompatibleSchema {
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

    fn preserve_observed_and_default(
        &self,
        observed: &[u8],
        reason: PersistenceRecoveryReason,
    ) -> Result<PersistenceLoad, PersistenceError> {
        let backup = preserve_observed(&self.path, observed, &reason).map_err(|source| PersistenceError {
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
        let mut bytes = Vec::new();
        let writer = durable::BoundedWriter::new(&mut bytes, MAX_STATE_BYTES as usize).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })?;
        serde_json::to_writer_pretty(writer, value).map_err(|error| PersistenceError {
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
                            .map_or(PersistedProjectPhase::Ready, |project| {
                                if project.phase == ProjectPhase::Indexing && project.request.is_none() {
                                    PersistedProjectPhase::Queued
                                } else { project.phase.into() }
                            }),
                        progress: project.and_then(|project| project.progress),
                        files_indexed: project.and_then(|project| project.files_indexed),
                        error: project
                            .and_then(|project| project.error.as_ref().map(ToString::to_string)),
                        operation: project.and_then(|project| project.operation.clone()),
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
        let route = persist_route(snapshot.committed_route());
        let selected_claim = if let Some(selected) = snapshot.committed_route().selected() {
            (!snapshot.key().is_unserved()).then(|| PersistedSelectionClaim {
                route: route.clone(),
                root: backend_library::encode_id(snapshot.key().root().as_bytes()),
                object: backend_library::encode_id(selected.get().as_bytes()),
            })
        } else {
            snapshot.session().pending_selection.as_ref()
                .filter(|claim| &claim.route == snapshot.committed_route())
                .map(|claim| PersistedSelectionClaim {
                    route: route.clone(),
                    root: backend_library::encode_id(&claim.root),
                    object: backend_library::encode_id(&claim.object),
                })
        };
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
            hand: snapshot
                .session()
                .hand
                .held()
                .iter()
                .map(|held| PersistedHeld {
                    package: held.package.as_str().to_owned(),
                    coordinate: held.id.as_ref().map(|id| id.as_str().to_owned()),
                    why: match held.why {
                        crate::model::hand::HeldWhy::Pin => "pin",
                        crate::model::hand::HeldWhy::Copy => "copy",
                        crate::model::hand::HeldWhy::Compare => "compare",
                        crate::model::hand::HeldWhy::Add => "add",
                    }
                    .to_owned(),
                    held_at: held.held_at,
                    touched_at: held.touched_at,
                })
                .collect(),
            hand_whispered: snapshot.session().whispered,
            window: snapshot
                .settings()
                .window
                .map(|window| PersistedWindow { width: window.width, height: window.height }),
            route,
            selected_claim,
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
        let overlay = if matches!(state.route, PersistedRoute::Settings) || state.settings_page.is_some() {
            Some(Overlay::Settings(settings_page))
        } else {
            None
        };
        let route = match &state.route {
            PersistedRoute::Settings | PersistedRoute::Home => {
                Route::Orbit(crate::navigation::OrbitRoute::Home)
            }
            PersistedRoute::Tree { project } => crate::core::LocalProjectId::new(project)
                .map(BrowseRoute::Tree)
                .map(crate::navigation::OrbitRoute::Browse)
                .map(Route::Orbit)
                .unwrap_or_else(|_| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::FindHome => Route::Orbit(crate::navigation::OrbitRoute::Browse(BrowseRoute::FindHome)),
            PersistedRoute::Find { text, limit } => crate::model::pages::SearchQuery::new(text, *limit)
                .map(BrowseRoute::Find)
                .map(crate::navigation::OrbitRoute::Browse)
                .map(Route::Orbit)
                .unwrap_or_else(|_| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::Compare { packages } => packages.iter()
                .map(|package| crate::model::pages::PackageRef::parse(package))
                .collect::<Result<Vec<_>, _>>()
                .ok()
                .and_then(|packages| CompareSet::new(packages).ok())
                .map(BrowseRoute::Compare)
                .map(crate::navigation::OrbitRoute::Browse)
                .map(Route::Orbit)
                .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::Project { project } => std::num::NonZeroU64::new(*project)
                .map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                })
                .map(crate::navigation::OrbitRoute::Project)
                .map(Route::Orbit)
                .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::World => Route::World,
            PersistedRoute::Package {
                cargo,
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
                            cargo: cargo.as_ref().and_then(PersistedCargoBrowse::restore),
                            project,
                            package,
                            lane: (*lane).into(),
                            selected: None,
                            at: at.as_deref().map(ReleaseId::from_persisted),
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
            PersistedRoute::CargoSource { project, request_binding, package, file, line } => {
                restore_cargo_source_address(project.as_deref(), *request_binding, package, file, *line)
                    .map(Route::CargoSource)
                    .unwrap_or(Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::CargoReadmeLink { browse, package, origin, href, line } => {
                restore_cargo_readme_link(browse, package, origin, href, *line)
                    .map(Route::CargoSource).unwrap_or(Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
        };
        let pending_selection = state.selected_claim.as_ref().and_then(|claim| {
            if claim.route != state.route || persist_route(&route) != state.route
                || route.at().is_some_and(|release| !release.is_valid())
            {
                return None;
            }
            match &route {
                Route::Package(_) | Route::Symbol(_) => Some(PendingSelectionClaim {
                    route: route.clone(),
                    root: backend_library::decode_id(&claim.root).ok()?,
                    object: backend_library::decode_id(&claim.object).ok()?,
                }),
                Route::CargoSource(_) | Route::Orbit(_) | Route::World => None,
            }
        });
        let hand = crate::model::hand::Hand::of(state.hand.iter().filter_map(|held| {
            Some(crate::model::hand::Held {
                package: crate::core::PackageId::new(&held.package).ok()?,
                id: match &held.coordinate {
                    Some(coordinate) => Some(Coordinate::new(coordinate).ok()?),
                    None => None,
                },
                why: match held.why.as_str() {
                    "copy" => crate::model::hand::HeldWhy::Copy,
                    "compare" => crate::model::hand::HeldWhy::Compare,
                    "add" => crate::model::hand::HeldWhy::Add,
                    _ => crate::model::hand::HeldWhy::Pin,
                },
                held_at: held.held_at,
                touched_at: held.touched_at,
            })
        }));
        SessionState {
            route,
            overlay,
            pending_selection,
            hand,
            whispered: state.hand_whispered,
            ..SessionState::default()
        }
    }

    /// Restores persistent settings into the immutable snapshot builder.
    #[must_use]
    pub fn cold_settings(&self, state: &PersistedDesktopState) -> SettingsState {
        SettingsState {
            window: state.window.map(|window| WindowSize { width: window.width, height: window.height }),
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
                    .map(|(display, step)| (Arc::from(display.as_str()), *step)),
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
                // Folder disappearance does not prove an in-flight owner
                // operation stopped. Keep its exact recovery path visible
                // even when the source is also temporarily unavailable.
                let valid_operation = item.operation.as_ref().filter(|operation| operation.belongs_to(&id)
                    && operation.observation.as_ref().is_none_or(|observation| operation.admits_observation(observation)));
                // Canonical operation evidence owns lifecycle when present.
                // A stale display phase cannot turn Unknown into Ready or
                // keep a checked publication permanently Unconfirmed.
                let operation_phase = valid_operation.map(|operation| {
                    use backend_library::{IndexOperationObservation, IndexOperationState, IndexOperationFailureReason};
                    match operation.observation.as_ref() {
                        Some(IndexOperationObservation::Known(status)) => match &status.state {
                            IndexOperationState::Published(_) => ProjectPhase::Ready,
                            IndexOperationState::Failed { reason, .. } => if *reason == IndexOperationFailureReason::Cancelled {
                                ProjectPhase::Cancelled
                            } else { ProjectPhase::Failed },
                            IndexOperationState::Accepted | IndexOperationState::Active { .. } | IndexOperationState::Unresolved { .. } => ProjectPhase::Unconfirmed,
                        },
                        None | Some(IndexOperationObservation::Unknown { .. }) => ProjectPhase::Unconfirmed,
                    }
                });
                let unresolved = operation_phase == Some(ProjectPhase::Unconfirmed)
                    || (item.operation.is_some() && valid_operation.is_none())
                    || (operation_phase.is_none() && matches!(item.phase, PersistedProjectPhase::Indexing | PersistedProjectPhase::Cancelling | PersistedProjectPhase::Unconfirmed));
                let phase = if unresolved { ProjectPhase::Unconfirmed }
                    else if !path.is_dir() { ProjectPhase::Missing }
                    else { operation_phase.unwrap_or_else(|| item.phase.into()) };
                let display_path: Arc<str> = item
                    .display_path
                    .as_deref()
                    .unwrap_or_else(|| id.as_str())
                    .into();
                Some(WorkspaceProject {
                    id: id.clone(),
                    path: display_path,
                    label: item.label.clone().into(),
                    phase,
                    progress: (!unresolved && valid_operation.is_none()).then_some(item.progress).flatten().map(|progress| progress.min(100)),
                    files_indexed: (!unresolved && valid_operation.is_none()).then_some(item.files_indexed).flatten(),
                    request: None,
                    operation: valid_operation.cloned(),
                    error: if unresolved && item.error.is_none() {
                        Some(Arc::from("This index request was interrupted. Check its exact owner operation before starting another."))
                    } else {
                        item.error.clone().map(Into::into)
                    },
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
            notes: Arc::from([]),
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
        if let Some(host) = host_project
            && let Ok(project) = LocalProjectId::from_path(host)
        {
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
                        operation: None,
                        recent: true,
                    });
                }
                workspace.projects = projects.into();
                if workspace.active.is_none() {
                    workspace.active = Some(project.clone());
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
    durable::read_regular_bounded(path, MAX_STATE_BYTES as usize)
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
    let sequence = RECOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{name}.{label}.{}.{}.preserved",
        std::process::id(),
        sequence
    ))
}

/// Copies exactly the bytes admitted from the held read into an exclusive
/// diagnostic. A concurrent atomic save may have replaced `path` by now, so
/// recovery must never rename or reread that pathname to preserve evidence.
fn preserve_observed(path: &Path, observed: &[u8], reason: &PersistenceRecoveryReason) -> io::Result<PathBuf> {
    if observed.len() > MAX_STATE_BYTES as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "observed state exceeds its byte budget"));
    }
    loop {
        let backup = recovery_path(path, reason);
        let mut file = match fs::OpenOptions::new().write(true).create_new(true).open(&backup) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(observed).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&backup);
            return Err(error);
        }
        drop(file);
        durable::sync_parent(&backup)?;
        return Ok(backup);
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
    fn prepared_operation_key_is_saved_and_cold_restart_retains_exact_reconciliation_claim() {
        let root = fixture("durable-index-operation");
        let project = LocalProjectId::from_path(&root).expect("project");
        let store = PersistentState::at(root.join("desktop.json"));
        let snapshot = crate::navigation::reduce(&AppSnapshot::empty(crate::core::VersionedRoot::unserved()),
            crate::navigation::Intent::AddProject { project: project.clone() }).snapshot;
        let operation = crate::model::index_operation::tests::claim(&project, 0x72);
        let mut workspace = snapshot.workspace().clone();
        let mut rows = workspace.projects.to_vec();
        rows[0].operation = Some(operation.clone());
        workspace.projects = rows.into();
        let snapshot = snapshot.with_workspace(workspace);
        let projected = PersistentState::project(&snapshot);
        store.save(&projected).expect("pre-send durable claim");
        let loaded = store.load().expect("cold state");
        assert_eq!(loaded.shelf[0].operation.as_ref(), Some(&operation));
        let restored = store.cold_workspace(&loaded);
        assert_eq!(restored.projects[0].phase, ProjectPhase::Unconfirmed,
            "a crash after preflight cannot prove whether transport started");
        assert_eq!(restored.projects[0].operation.as_ref(), Some(&operation));
        assert_eq!(restored.projects[0].request, None, "an ephemeral request is never durable identity");

        let mut mismatched = loaded;
        let other = LocalProjectId::new("/fixture/other-operation").expect("other project");
        mismatched.shelf[0].operation = Some(crate::model::index_operation::tests::claim(&other, 0x73));
        let restored = store.cold_workspace(&mismatched);
        assert_eq!(restored.projects[0].phase, ProjectPhase::Unconfirmed);
        assert_eq!(restored.projects[0].operation, None, "wrong package claim cannot reconcile this row");
        fs::remove_dir_all(root).expect("remove durable operation fixture");
    }

    #[test]
    fn cold_restart_keeps_terminal_receipt_and_never_promotes_accepted_or_unknown_work() {
        let directory = fixture("durable-terminal-state");
        let project = LocalProjectId::from_path(&directory).expect("project");
        let store = PersistentState::at(directory.join("desktop.json"));
        let added = crate::navigation::reduce(&AppSnapshot::empty(crate::core::VersionedRoot::unserved()),
            crate::navigation::Intent::AddProject { project: project.clone() }).snapshot;
        let operation = crate::model::index_operation::tests::claim(&project, 0x74);
        let mut value = PersistentState::project(&added);
        let mut published = operation.clone();
        published.observation = Some(crate::model::index_operation::tests::published(&operation));
        value.shelf[0].operation = Some(published.clone());
        value.shelf[0].phase = PersistedProjectPhase::Unconfirmed;
        store.save(&value).expect("terminal receipt persisted");
        let restored = store.cold_workspace(&store.load().expect("cold receipt"));
        assert_eq!(restored.projects[0].phase, ProjectPhase::Ready);
        assert_eq!(restored.projects[0].operation, Some(published));
        let mut accepted = operation.clone();
        accepted.observation = Some(crate::model::index_operation::tests::observation(&operation,
            backend_library::IndexOperationState::Accepted));
        value.shelf[0].operation = Some(accepted);
        assert_eq!(store.cold_workspace(&value).projects[0].phase, ProjectPhase::Unconfirmed,
            "a durable acceptance cannot prove terminal publication after restart");
        value.shelf[0].phase = PersistedProjectPhase::Ready; // stale paint state is not an owner receipt
        value.shelf[0].operation.as_mut().expect("claim").observation = Some(
            backend_library::IndexOperationObservation::Unknown { operation_key: operation.key });
        assert_eq!(store.cold_workspace(&value).projects[0].phase, ProjectPhase::Unconfirmed);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn queued_local_admission_survives_restart_without_becoming_an_uncertain_mutation() {
        let root = fixture("queued-admission");
        let project = LocalProjectId::from_path(&root).expect("local project");
        let store = PersistentState::at(root.join("desktop.json"));
        let snapshot = crate::navigation::reduce(
            &AppSnapshot::empty(crate::core::VersionedRoot::unserved()),
            crate::navigation::Intent::AddProject { project: project.clone() },
        ).snapshot;
        let queued = PersistentState::project(&snapshot);
        assert_eq!(queued.shelf[0].phase, PersistedProjectPhase::Queued);
        let restored = store.cold_workspace(&queued);
        assert_eq!(restored.projects[0].phase, ProjectPhase::Indexing);
        assert_eq!(restored.projects[0].request, None);
        assert_eq!(restored.projects[0].error, None);

        let submitted = crate::navigation::reduce(&snapshot, crate::navigation::Intent::IndexProject {
            operation: crate::model::index_operation::tests::claim(&project, 0x51),
            project, basis: snapshot.key(), request: crate::navigation::RequestId::new(7),
        }).snapshot;
        let submitted = PersistentState::project(&submitted);
        assert_eq!(submitted.shelf[0].phase, PersistedProjectPhase::Indexing);
        assert_eq!(store.cold_workspace(&submitted).projects[0].phase, ProjectPhase::Unconfirmed,
            "submitted and legacy index mutations still require exact recovery");
        fs::remove_dir_all(root).expect("remove queued admission fixture");
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
                operation: None,
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
                    operation: None,
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
                    operation: None,
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
    fn cold_active_index_and_cancellation_require_exact_reconciliation() {
        let root = fixture("cold-index-outcome");
        let state = PersistedDesktopState {
            shelf: [PersistedProjectPhase::Indexing, PersistedProjectPhase::Cancelling, PersistedProjectPhase::Indexing, PersistedProjectPhase::Unconfirmed]
                .into_iter()
                .enumerate()
                .map(|(index, phase)| {
                    let path = root.join(format!("project-{index}"));
                    if index < 2 {
                        fs::create_dir(&path).expect("project directory");
                    }
                    PersistedShelfItem {
                        local_path: path.to_str().expect("fixture UTF-8").to_owned(),
                        display_path: None,
                        native_path: None,
                        label: format!("project-{index}"),
                        phase,
                        progress: Some(41),
                        files_indexed: Some(100),
                        error: None,
                        operation: None,
                    }
                })
                .collect(),
            ..PersistedDesktopState::default()
        };
        let workspace = PersistentState::at(root.join("desktop.json")).cold_workspace(&state);
        assert_eq!(workspace.projects.len(), 4);
        for project in workspace.projects.iter() {
            assert_eq!(project.phase, ProjectPhase::Unconfirmed);
            assert_eq!(project.progress, None);
            assert_eq!(project.files_indexed, None);
            assert_eq!(project.request, None);
            assert!(project.error.as_deref().is_some_and(|message| message.contains("Check its exact owner operation")));
        }
        fs::remove_dir_all(root).expect("cleanup");
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
                operation: None,
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
    fn saved_focus_waits_for_the_exact_owner_catalog_row() {
        let key = crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("selection".to_owned(), "owner".to_owned())]),
            1,
        );
        let package = crate::core::PackageId::new("pkg:cargo/serde@1.0.0").expect("package");
        let object = crate::model::ObjectId::test(7);
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None,
            project: None,
            package: package.clone(),
            lane: PackageLane::Overview,
            selected: Some(object),
            at: None,
        });
        let snapshot = AppSnapshot::empty(key).with_session(SessionState { route, ..SessionState::default() });
        let wire = PersistentState::project(&snapshot);
        assert!(wire.selected_claim.is_some(), "the focus claim is saved with its route and root");
        let store = PersistentState::at("unused");
        let restored = store.cold_reload(&wire);
        assert_eq!(restored.route.selected(), None, "wire bytes cannot mint a typed ObjectId");
        assert!(restored.pending_selection.is_some());

        let row = crate::model::PackageSummary {
            coordinate: package,
            name: Arc::from("serde"),
            version: Arc::from("1.0.0"),
            ecosystem: Arc::from("Cargo"),
            bytes: 0,
            standing: Arc::from("current"),
            downloads: Arc::from("unknown"),
            advisory: Arc::from("unknown"),
            object,
        };
        let catalog = crate::model::CatalogState { packages: Arc::from([row.clone()]) };
        let opened = AppSnapshot::empty(key).with_session(restored).with_catalog(catalog.clone(), key);
        assert_eq!(opened.route().selected(), Some(object));
        assert_eq!(opened.session().selected, Some(crate::navigation::Selection::Object(object)));
        assert!(opened.session().pending_selection.is_none());

        let mut wrong_object = wire.clone();
        wrong_object.selected_claim.as_mut().expect("claim").object = backend_library::encode_id(crate::model::ObjectId::test(9).get().as_bytes());
        let rejected = AppSnapshot::empty(key).with_session(store.cold_reload(&wrong_object)).with_catalog(catalog.clone(), key);
        assert_eq!(rejected.route().selected(), None);
        assert!(rejected.session().pending_selection.is_none());

        let mut wrong_root = wire.clone();
        wrong_root.selected_claim.as_mut().expect("claim").root = backend_library::encode_id(&[3; 32]);
        let rejected = AppSnapshot::empty(key).with_session(store.cold_reload(&wrong_root)).with_catalog(catalog, key);
        assert_eq!(rejected.route().selected(), None);
        assert!(rejected.session().pending_selection.is_none());

        let mut moved_claim = wire.clone();
        moved_claim.route = PersistedRoute::Package {
            cargo: None,
            project: None,
            package: "pkg:cargo/other@1.0.0".to_owned(),
            lane: PersistedPackageLane::Overview,
            at: None,
        };
        assert!(store.cold_reload(&moved_claim).pending_selection.is_none(), "a claim cannot follow an edited route");

        let waiting = AppSnapshot::empty(key).with_session(store.cold_reload(&wire))
            .with_catalog(crate::model::CatalogState::default(), key);
        assert!(waiting.session().pending_selection.is_some(), "a paged catalog may not contain the row yet");
        assert!(PersistentState::project(&waiting).selected_claim.is_some(), "pending focus survives another save");
    }

    #[test]
    fn invalid_saved_release_remains_an_unread_address_after_cold_reload() {
        let mut state = PersistedDesktopState {
            route: PersistedRoute::Symbol {
                project: None,
                package: "pkg:cargo/serde@1.0.0".to_owned(),
                id: "pkg:cargo/serde@1.0.0::src/lib.rs:1::Item".to_owned(),
                at: Some("../other".to_owned()),
                view: "page".to_owned(),
                line: None,
            },
            ..PersistedDesktopState::default()
        };
        let store = PersistentState::at("unused");
        for route in [state.route.clone(), PersistedRoute::Package {
            cargo: None,
            project: None,
            package: "pkg:cargo/serde@1.0.0".to_owned(),
            lane: PersistedPackageLane::Overview,
            at: Some("../other".to_owned()),
        }] {
            state.route = route;
            let reopened = store.cold_reload(&state);
            let at = reopened.route.at().expect("saved alternate release remains present");
            assert!(!at.is_valid());
            assert!(crate::runtime::store::route_package(&reopened.route).is_none());
            if matches!(reopened.route, Route::Symbol(_)) {
                assert!(matches!(crate::runtime::store::route_declaration(&reopened.route), Err(crate::runtime::store::Unread::ReleaseNotHere(_))));
            }
            match persist_route(&reopened.route) {
                PersistedRoute::Symbol { at, .. } | PersistedRoute::Package { at, .. } => assert_eq!(at.as_deref(), Some("../other")),
                other => panic!("unexpected restored route: {other:?}"),
            }
        }
    }

    #[test]
    fn browsing_places_and_settings_overlay_survive_a_cold_restart() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("persistence".to_owned(), "browse".to_owned())]),
            1,
        ));
        let browse = [
            BrowseRoute::Tree(LocalProjectId::new("/tmp/nudox-tree").expect("tree")),
            BrowseRoute::FindHome,
            BrowseRoute::Find(crate::model::pages::SearchQuery::new("serde map", 73).expect("query")),
            BrowseRoute::Compare(CompareSet::new([
                crate::model::pages::PackageRef::parse("pkg:cargo/toml@0.8.23").expect("first"),
                crate::model::pages::PackageRef::parse("pkg:cargo/serde@1.0.0").expect("second"),
            ]).expect("compare")),
        ];
        for place in browse {
            let route = Route::Orbit(crate::navigation::OrbitRoute::Browse(place));
            let session = SessionState {
                route: route.clone(),
                overlay: Some(Overlay::Settings(SettingsPage::Help)),
                ..SessionState::default()
            };
            let persisted = PersistentState::project(&snapshot.with_session(session));
            let bytes = serde_json::to_vec(&persisted).expect("serialize");
            let decoded: PersistedDesktopState = serde_json::from_slice(&bytes).expect("deserialize");
            let reopened = PersistentState::at("unused").cold_reload(&decoded);
            assert_eq!(reopened.route, route);
            assert_eq!(reopened.overlay, Some(Overlay::Settings(SettingsPage::Help)));
        }
    }

    #[test]
    fn cargo_source_cold_address_keeps_authority_but_cannot_mint_file_proof() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("persistence".to_owned(), "cargo-source".to_owned())]),
            1,
        ));
        let package = crate::core::PackageId::new(
            "pkg:cargo/demo@1.2.3?cargo-authority=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ).expect("qualified package");
        let project = crate::core::LocalProjectId::new("/tmp/nudox-cargo-source-persistence").expect("tree address");
        let file = CargoSourcePath::new("src/lib.rs").expect("relative file");
        let route = Route::CargoSource(CargoSourceRoute::new(crate::navigation::cargo_browse::fixture_context(project.clone()), package, file, Some(43)).expect("source address"));
        let wire = PersistentState::project(&snapshot.with_session(SessionState { route: route.clone(), ..SessionState::default() }));
        let bytes = serde_json::to_vec(&wire).expect("serialize address");
        let decoded: PersistedDesktopState = serde_json::from_slice(&bytes).expect("decode address");
        let restored = PersistentState::at("unused").cold_reload(&decoded);
        assert_eq!(restored.route, route);
        assert_eq!(crate::runtime::store::route_keys(&restored.route).len(), 3,
            "cold address pairs file/path reads and watches its optional visible package header");

        let Route::CargoSource(bound) = &route else { panic!("source") };
        let parent = Route::Package(bound.package_route());
        let parent_wire = PersistentState::project(&snapshot.with_session(SessionState { route: parent.clone(), ..SessionState::default() }));
        assert_eq!(PersistentState::at("unused").cold_reload(&parent_wire).route, parent, "zoom-out persists its independent full binding without history");

        let mut legacy = decoded.clone();
        if let PersistedRoute::CargoSource { request_binding, .. } = &mut legacy.route { *request_binding = None; }
        let session = PersistentState::at("unused").cold_reload(&legacy);
        let Route::CargoSource(awaiting) = &session.route else { panic!("legacy source recovery") };
        let awaiting = awaiting.clone();
        assert!(matches!(awaiting.browse, crate::navigation::CargoBrowseAddress::AwaitingTree { .. }));
        assert_eq!(crate::runtime::store::route_keys(&session.route), vec![crate::model::pages::PageKey::Browse(crate::model::browse::BrowseKey::Tree(project.clone()))], "no binding means Tree only, never a source request with guessed roots");
        let resolved = crate::navigation::reduce(&snapshot.with_session(session.clone()), crate::navigation::Intent::ResolveCargoBrowse { expected: awaiting.clone(), context: bound.browse.context().expect("binding address").clone() }).snapshot;
        assert_eq!(resolved.route(), &route);
        assert_eq!(resolved.session().back, session.back, "resolving the current Tree binding is not a duplicate history stop");
        let other_context = crate::navigation::cargo_browse::fixture_context(crate::core::LocalProjectId::new("/tmp/unrelated-member").expect("other address"));
        assert_eq!(crate::navigation::reduce(&snapshot.with_session(session), crate::navigation::Intent::ResolveCargoBrowse { expected: awaiting.clone(), context: other_context }).snapshot.route(), &Route::CargoSource(awaiting.clone()));

        let mut forged = decoded;
        forged.route = PersistedRoute::CargoSource {
            request_binding: None,
            project: None,
            package: "pkg:cargo/demo@1.2.3?cargo-authority=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            file: "src/lib.rs".to_owned(),
            line: Some(43),
        };
        assert!(matches!(PersistentState::at("unused").cold_reload(&forged).route, Route::Orbit(_)),
            "an old source address without its tree cannot rehydrate owner authority");
        assert_eq!(forged.cargo_source_recovery_note(), Some(crate::model::workspace::Note::CargoSourceAddressUnread));
        assert!(legacy.cargo_source_recovery_note().is_none(), "a usable exact requested Tree has an explicit AwaitingTree route");
        forged.route = PersistedRoute::CargoSource {
            request_binding: None,
            project: Some(project.as_str().to_owned()),
            package: "pkg:cargo/demo@1.2.3".to_owned(),
            file: "../secret".to_owned(),
            line: Some(0),
        };
        assert!(matches!(PersistentState::at("unused").cold_reload(&forged).route, Route::Orbit(_)));
    }

    #[test]
    fn readme_link_cold_restore_keeps_full_scope_binding_href_and_fragment_as_addresses() {
        use backend_library::{CargoPackageReadmeRootScopeV1 as Scope, CargoPackageReadmeSelectionV1 as Selection};
        let (_, key, result) = crate::runtime::cargo_readme_reads::tests::fixture();
        let origin = backend_library::CargoPackageReadmeOriginV1::from_result(&result).expect("exact README origin");
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::unserved());
        let mut targets = Vec::new();
        for (scope, selection) in [(Scope::EffectiveWorkspace, Selection::WorkspaceInherited), (Scope::Package, Selection::ManifestPath)] {
            let mut origin = origin.clone(); origin.root_scope = scope; origin.selection = selection;
            let href = "../src/lib.rs#L7-L11";
            let link = CargoReadmeLinkAddress::new(origin.clone(), href).expect("scoped file address");
            assert_eq!(link.fragment(), Some("L7-L11"));
            let route = Route::CargoSource(CargoSourceRoute::readme_link(key.context.clone(), crate::core::PackageId::new(key.package.as_str()).expect("package"), link, Some(7)).expect("source address"));
            let wire = PersistentState::project(&snapshot.with_session(SessionState { route: route.clone(), ..SessionState::default() }));
            let decoded: PersistedDesktopState = serde_json::from_slice(&serde_json::to_vec(&wire).expect("serialize scoped address")).expect("decode scoped address");
            let restored = PersistentState::at("unused").cold_reload(&decoded);
            assert_eq!(restored.route, route);
            assert!(restored.back.is_empty(), "the binding is carried by the address, never reconstructed from navigation history");
            let PersistedRoute::CargoReadmeLink { browse, origin: saved, href: saved_href, line, .. } = &decoded.route else { panic!("scope-preserving wire route"); };
            assert_eq!(browse.project, key.context.requested_project().service_coordinate().expect("requested member"));
            assert_eq!(browse.request_binding, key.context.request_binding());
            assert_eq!(saved, &origin); assert_eq!(saved_href, href); assert_eq!(*line, Some(7));
            let plan = crate::runtime::store::RouteDependencies::new(&restored.route, None);
            let store = crate::runtime::store::DataStore::new(Arc::new(snapshot.with_session(restored)), None);
            let pair = plan.cargo().expect("exact file and independent package inventory");
            assert!(store.cargo_source(&pair.file).loaded_value().is_none());
            assert!(!plan.content_loaded(&store), "a restored source-qualified address creates no served bytes");
            assert!(plan.current_cargo_package(&store, &key.package).is_none());
            targets.push(pair.file.target.clone());
        }
        assert_ne!(targets[0], targets[1], "equal relative spelling never collapses workspace inheritance into package-file scope");
    }

    #[test]
    fn edited_readme_link_context_or_origin_cannot_restore_a_different_owner_receipt() {
        let (_, key, result) = crate::runtime::cargo_readme_reads::tests::fixture();
        let origin = backend_library::CargoPackageReadmeOriginV1::from_result(&result).expect("origin");
        let route = CargoSourceRoute::readme_link(key.context.clone(), crate::core::PackageId::new(key.package.as_str()).expect("package"), CargoReadmeLinkAddress::new(origin, "../src/lib.rs#L7").expect("file address"), Some(7)).expect("bound address");
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::unserved());
        let wire = PersistentState::project(&snapshot.with_session(SessionState { route: Route::CargoSource(route), ..SessionState::default() }));
        for change in 0..7 {
            let mut edited = wire.clone();
            let PersistedRoute::CargoReadmeLink { browse, package, origin, href, line } = &mut edited.route else { panic!("wire link"); };
            match change {
                0 => browse.project = "/fixture/workspace/another-member".into(),
                1 => browse.request_binding.effective_workspace_root_digest = [3; 32],
                2 => origin.request_binding.requested_root_digest = [4; 32],
                3 => *package = "pkg:cargo/serde@1.0.219".into(),
                4 => origin.root_scope = backend_library::CargoPackageReadmeRootScopeV1::Package,
                5 => *href = "../../outside.rs#L7".into(),
                _ => *line = Some(0),
            }
            assert!(matches!(PersistentState::at("unused").cold_reload(&edited).route, Route::Orbit(_)), "edited context/origin case {change} has no scoped route");
            assert_eq!(edited.cargo_source_recovery_note(), Some(crate::model::workspace::Note::CargoSourceAddressUnread));
        }
        let mut untrusted_digest = wire;
        if let PersistedRoute::CargoReadmeLink { origin, .. } = &mut untrusted_digest.route { origin.content_digest = [5; 32]; }
        let restored = PersistentState::at("unused").cold_reload(&untrusted_digest);
        assert!(matches!(restored.route, Route::CargoSource(_)), "a structurally valid edited digest remains an untrusted address for owner revalidation");
        let plan = crate::runtime::store::RouteDependencies::new(&restored.route, None);
        let store = crate::runtime::store::DataStore::new(Arc::new(snapshot.with_session(restored)), None);
        assert!(!plan.content_loaded(&store));
        assert!(plan.current_cargo_package(&store, &key.package).is_none(), "saved origin bytes cannot admit current file authority");
    }

    /// Quitting while the query previews a result reopens where you were,
    /// never on the provisional page.
    #[test]
    fn a_previewed_route_is_never_persisted_the_place_you_were_on_is() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("persistence".to_owned(), "preview".to_owned())]),
            1,
        ));
        let symbol = |name: &str| Route::Symbol(crate::navigation::SymbolRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            id: Coordinate::new(&format!("pkg::{name}")).expect("coordinate"),
            at: None,
            view: View::Page,
            line: None,
            selected: None,
        });
        let session = SessionState {
            route: symbol("Walked"),
            preview: Some(symbol("Origin")),
            overlay: Some(Overlay::CommandPalette),
            ..SessionState::default()
        };
        let value = PersistentState::project(&snapshot.with_session(session));
        let restored = PersistentState::at("unused").cold_reload(&value);
        assert_eq!(restored.route, symbol("Origin"), "the provisional page is not where you reopen");
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
        assert_eq!(fs::read(&path).expect("source untouched"), corrupt);

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
        assert_eq!(fs::read(&path).expect("source untouched"), bytes);
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

        let admitted = store.load_recovering().expect("retain oversized state");
        let PersistenceRecovery::RetainedAtSource { path: retained, reason } = admitted.recovery else {
            panic!("oversized state must be retained at its source");
        };
        assert_eq!(
            reason,
            PersistenceRecoveryReason::Oversized {
                at_least: MAX_STATE_BYTES + 1,
                limit: MAX_STATE_BYTES,
            }
        );
        assert_eq!(retained, path);
        assert_eq!(fs::read(&path).expect("oversized source untouched"), oversized);
        assert_eq!(admitted.state, PersistedDesktopState::default());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn oversized_save_never_replaces_the_last_admitted_state() {
        let root = fixture("persistence-write-budget");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let current = PersistedDesktopState { reduced_motion: true, ..PersistedDesktopState::default() };
        store.save(&current).expect("publish current state");
        let mut oversized = PersistedDesktopState::default();
        oversized.shelf.push(PersistedShelfItem {
            local_path: "/tmp/oversized".to_owned(),
            display_path: None,
            native_path: None,
            label: "x".repeat(MAX_STATE_BYTES as usize + 1),
            phase: PersistedProjectPhase::Ready,
            progress: None,
            files_indexed: None,
            error: None,
            operation: None,
        });
        assert!(store.save(&oversized).is_err(), "encoding must stop at the same byte budget as reads");
        assert_eq!(store.load().expect("last admitted state"), current);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn observed_corruption_is_preserved_without_moving_a_concurrent_valid_save() {
        let root = fixture("persistence-observed-race");
        let path = root.join("desktop.json");
        let store = PersistentState::at(&path);
        let corrupt = br#"{"schema":1,"shelf":["#;
        fs::write(&path, corrupt).expect("corrupt initial state");
        let observed = read_bounded(&path).expect("read exact corrupt bytes");
        let current = PersistedDesktopState { reduced_motion: true, ..PersistedDesktopState::default() };
        store.save(&current).expect("another writer publishes valid state");
        let recovered = store.preserve_observed_and_default(&observed, PersistenceRecoveryReason::Corrupt).expect("preserve observed bytes");
        let PersistenceRecovery::Preserved { backup, .. } = recovered.recovery else { panic!("preserved diagnostic"); };
        assert_eq!(fs::read(backup).expect("diagnostic bytes"), corrupt);
        assert_eq!(store.load().expect("concurrent valid canonical"), current);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
