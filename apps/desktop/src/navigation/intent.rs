//! Typed intents and effect descriptors.

use super::action::ActionId;
use super::route::{Route, SettingsPage};
use crate::core::LocalProjectId;
use crate::core::ids::{DocumentId, VersionedRoot};
use crate::model::snapshot::{DeltaId, ObjectId};
use std::path::PathBuf;
use std::sync::Arc;

/// Stable identity for a background effect.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId {
    epoch: u64,
    sequence: u64,
}

impl RequestId {
    /// Creates a request identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self {
            epoch: 0,
            sequence: value,
        }
    }

    /// Binds a request sequence to the producer authority that admitted its
    /// basis. Runtime-created requests use this constructor; [`Self::new`]
    /// remains for deterministic reducer fixtures.
    #[must_use]
    pub const fn from_authority(basis: VersionedRoot, sequence: u64) -> Self {
        Self {
            epoch: basis.producer_epoch(),
            sequence,
        }
    }

    /// Returns the producer epoch carried by this request.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Returns the monotonic sequence within the producer epoch.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the stable value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.sequence
    }
}

/// Typed result returned by GPUI's native path prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FolderPickerOutcome {
    /// One or more existing folders were selected.
    Selected(Arc<[PathBuf]>),
    /// The user dismissed the native prompt without a selection.
    Cancelled,
    /// The platform exposes no usable native folder prompt for this window.
    Unavailable(Arc<str>),
    /// The prompt opened but returned an explicit platform error.
    Failed(Arc<str>),
}

/// Typed navigation and shell intents.  Widgets dispatch this enum; they do
/// not concatenate route strings or call an engine client directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Replace the current typed route.
    Navigate(Route),
    /// Descend to source while retaining a selected object.
    OpenSource {
        /// Parent package coordinate.
        package: crate::core::PackageId,
        /// Source coordinate selected by the page.
        coordinate: super::route::Coordinate,
        /// Source line.
        line: u32,
        /// Selected object retained by the route.
        object: Option<ObjectId>,
    },
    /// Move one content level up.
    ZoomOut,
    /// Move backward in typed history.
    Back,
    /// Move forward in typed history.
    Forward,
    /// Select a stable object in the current route.
    Select(ObjectId),
    /// Select a document tab.
    SelectDocument(DocumentId),
    /// Open command palette.
    OpenCommandPalette,
    /// Close the top shell overlay.
    DismissOverlay,
    /// Open one typed settings page.
    OpenSettings(SettingsPage),
    /// Toggle reduced motion.
    ToggleReducedMotion,
    /// Toggle the persistent shelf visibility.
    ToggleShelf,
    /// Toggle the context panel visibility.
    ToggleContext,
    /// Cycle surface appearance.
    ToggleAppearance,
    /// Increase or decrease the global interface text scale.
    SetTextScale {
        /// Move one step up or down the closed scale ladder.
        up: bool,
    },
    /// Toggle local-only versus registry metadata policy.
    TogglePrivacy,
    /// Toggle registry advisory fetching.
    ToggleAdvisories,
    /// Toggle immutable registry cache usage.
    ToggleCache,
    /// Move the immutable registry cache retention through its supported days.
    SetCacheDays {
        /// Move toward a longer or shorter retention window.
        up: bool,
    },
    /// Open the keyboard-first local project admission dialog.
    OpenAddProject,
    /// Open the native folder picker.
    OpenFolderPicker,
    /// Deliver one typed native picker result back to the root reducer.
    FolderPickerResult {
        /// Native selection, cancellation, or platform failure.
        outcome: FolderPickerOutcome,
    },
    /// Request one canonical project index through the root-owned actor.
    IndexProject {
        /// Local project identity admitted by the path boundary.
        project: LocalProjectId,
        /// Root authority captured before the request was queued.
        basis: VersionedRoot,
        /// Compatibility request retained until the service owner replies.
        request: RequestId,
    },
    /// Admit one validated native local folder to the durable shelf.
    AddProject {
        /// Exact native identity admitted by the dialog or platform picker.
        project: LocalProjectId,
    },
    /// Retain an admission failure in the open project dialog.
    RejectProjectPath {
        /// Bounded product-facing validation message.
        message: Arc<str>,
    },
    /// Mark a persisted project as the active workspace.
    ActivateProject(LocalProjectId),
    /// Remove one project from the shelf and stop any index work.
    RemoveProject(LocalProjectId),
    /// Reveal a project folder in the platform file browser.
    RevealProject(LocalProjectId),
    /// Retry a stopped or failed index job.
    RetryIndex(LocalProjectId),
    /// Cancel an active index job while retaining the shelf row.
    CancelIndex(LocalProjectId),
    /// Run a local connection probe.
    TestConnection,
    /// Complete a local connection probe.
    ConnectionResult {
        /// Whether the service answered.
        connected: bool,
    },
    /// Open the help page in Settings.
    OpenHelp,
    /// Stop all currently running engine work without changing the route.
    Stop,
    /// Begin a version-pinned root refresh.
    RefreshRoot {
        /// Root the response must match.
        basis: VersionedRoot,
        /// Request identity allocated by the UI coordinator.
        request: RequestId,
    },
    /// Begin a version-pinned object/delta refresh.
    RefreshObject {
        /// Object identity admitted by the current view root.
        object: ObjectId,
        /// Exact transition identity for cache invalidation.
        delta: DeltaId,
        /// Root the response must match.
        basis: VersionedRoot,
        /// Request identity allocated by the UI coordinator.
        request: RequestId,
    },
    /// Read one typed product surface at the exact snapshot root.
    RefreshSurface {
        /// Typed daemon-owned surface command.
        command: backend_library::SurfaceCommand,
        /// Root the response must match.
        basis: VersionedRoot,
        /// Request identity.
        request: RequestId,
    },
    /// Read one local project's own package facts (manifest, README) off the
    /// UI thread. The facts are local files, not producer state.
    RefreshLocalPackage {
        /// Local project whose manifests are read.
        project: LocalProjectId,
        /// Root current when the read was requested.
        basis: VersionedRoot,
        /// Request identity.
        request: RequestId,
    },
    /// Apply a stable action from a keymap or command palette.
    Action(ActionId),
    /// No-op used by deterministic replay and reducer laws.
    Noop,
}

/// Typed engine work emitted by the reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineCommand {
    /// Ask the engine for the latest projection at a basis.
    ReadRoot {
        /// Root basis to match.
        basis: VersionedRoot,
        /// Request identity used for stale result rejection.
        request: RequestId,
    },
    /// Submit one local project through the canonical service index command.
    IndexProject {
        /// Local identity admitted by the native path boundary.
        project: LocalProjectId,
        /// Root basis captured before submission.
        basis: VersionedRoot,
        /// Request identity used for terminal projection matching.
        request: RequestId,
    },
    /// Ask for one object/delta projection.
    ReadObject {
        /// Stable object identity.
        object: ObjectId,
        /// Delta basis.
        delta: DeltaId,
        /// Root basis to match.
        basis: VersionedRoot,
        /// Request identity.
        request: RequestId,
    },
    /// Read one typed product surface.
    RefreshSurface {
        /// Typed daemon-owned surface command.
        command: backend_library::SurfaceCommand,
        /// Root basis to match.
        basis: VersionedRoot,
        /// Request identity.
        request: RequestId,
    },
    /// Read one local project's package facts on the actor's local lane.
    ReadLocalPackage {
        /// Local project whose manifests are read.
        project: LocalProjectId,
        /// Root current when the read was requested.
        basis: VersionedRoot,
        /// Request identity.
        request: RequestId,
    },
}

/// Pure reducer output. Effects are interpreted only by runtime code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    /// New immutable snapshot.
    pub snapshot: crate::model::AppSnapshot,
    /// Typed work that may run in the background.
    pub effects: Vec<Effect>,
}

/// Background work emitted by a pure reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Execute one typed engine command.
    Engine(EngineCommand),
    /// Persist shelf/settings/session state.
    Persist,
    /// Cancel an earlier request.
    Cancel(RequestId),
    /// Cancel every request still owned by the UI runtime.
    CancelAll,
}
