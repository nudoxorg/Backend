//! Typed intents and effect descriptors.

use super::action::ActionId;
use super::route::{Route, SettingsPage};
use crate::core::ids::{DocumentId, VersionedRoot};
use crate::core::LocalProjectId;
use crate::model::snapshot::{DeltaId, ObjectId};
use std::sync::Arc;

/// Stable identity for a background effect.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    /// Creates a request identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the stable value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
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
    /// Open the native folder picker.
    OpenFolderPicker,
    /// Request one canonical project index through the root-owned actor.
    IndexProject {
        /// Local project identity admitted by the path boundary.
        project: LocalProjectId,
        /// Root authority captured before the request was queued.
        basis: VersionedRoot,
        /// Request identity owned by the UI coordinator.
        request: RequestId,
    },
    /// Admit one canonical local folder to the durable shelf.
    AddProject {
        /// Canonical folder spelling selected by the native picker.
        path: Arc<str>,
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
    /// Ask the canonical local service seam to ingest or refresh one project.
    IndexProject {
        /// Canonical local identity sent to the service.
        project: LocalProjectId,
        /// Root authority used to reject obsolete completion events.
        basis: VersionedRoot,
        /// Request identity used by the actor.
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
