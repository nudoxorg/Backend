//! Typed local workspace lifecycle shared by onboarding, the shelf, and settings.
//!
//! These values intentionally live beside the immutable snapshot rather than
//! inside GPUI views. A folder can remain visible while its service index is
//! building, paused, failed, or missing, and the same state must survive a
//! cold restart without inventing progress.

use std::sync::Arc;

use crate::core::{IdentityError, LocalProjectId};
use crate::navigation::RequestId;

/// The lifecycle of a local project admitted to the shelf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectPhase {
    /// The project has been admitted and its index is current.
    Ready,
    /// The local service is indexing the project.
    Indexing,
    /// Cancellation has been requested but the live producer has not
    /// returned yet. The row must remain owned until that boundary answers.
    Cancelling,
    /// Indexing was cancelled by the user.
    Cancelled,
    /// The last index attempt failed and can be retried.
    Failed,
    /// The persisted folder no longer exists or is not a directory.
    Missing,
}

impl ProjectPhase {
    /// Returns the short product label used by the shelf and diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Indexing => "Indexing",
            Self::Cancelling => "Cancelling…",
            Self::Cancelled => "Paused",
            Self::Failed => "Needs attention",
            Self::Missing => "Folder missing",
        }
    }
}

/// One durable local project lifecycle row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceProject {
    /// Lossless native identity used for reducer/runtime matching.
    pub id: LocalProjectId,
    /// Presentation spelling for the local folder. Matching and persistence
    /// use [`Self::id`] so this field may be lossy on platforms with wide or
    /// non-UTF-8 native path units.
    pub path: Arc<str>,
    /// Short display name shown in the shelf.
    pub label: Arc<str>,
    /// Current service index lifecycle.
    pub phase: ProjectPhase,
    /// Backend-reported progress, when the service exposes it.
    pub progress: Option<u8>,
    /// Backend-reported file count, when the service exposes it.
    pub files_indexed: Option<u64>,
    /// Ephemeral compatibility request while the shared ProjectIngest receipt
    /// surface is unavailable. This is cleared on terminal owner response and
    /// is never used as durable identity.
    pub request: Option<RequestId>,
    /// Bounded error text suitable for a diagnostic row.
    pub error: Option<Arc<str>>,
    /// Whether this row was opened recently.
    pub recent: bool,
}

impl WorkspaceProject {
    /// Creates an indeterminate indexing row from an admitted folder.
    #[must_use]
    pub fn indexing(path: impl Into<Arc<str>>) -> Result<Self, IdentityError> {
        let path = path.into();
        let id = LocalProjectId::new(path.as_ref())?;
        Ok(Self::indexing_with_display(id, path))
    }

    /// Creates an indeterminate row from a lossless admitted identity.
    #[must_use]
    pub fn indexing_with_id(id: LocalProjectId) -> Self {
        let path: Arc<str> = id.as_str().into();
        Self::indexing_with_display(id, path)
    }

    /// Creates an indeterminate row while retaining the user's selected
    /// presentation spelling separately from the canonical native identity.
    #[must_use]
    pub fn indexing_with_display(id: LocalProjectId, path: impl Into<Arc<str>>) -> Self {
        let path = path.into();
        let label = std::path::Path::new(path.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(path.as_ref());
        Self {
            id,
            path,
            label: Arc::from(label),
            phase: ProjectPhase::Indexing,
            progress: None,
            files_indexed: None,
            request: None,
            error: None,
            recent: true,
        }
    }

    /// Returns a bounded display-safe failure row.
    #[must_use]
    pub fn with_error(mut self, message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        let message = message
            .chars()
            .filter(|character| !character.is_control())
            .take(240)
            .collect::<String>();
        self.phase = ProjectPhase::Failed;
        self.error = Some(Arc::from(message));
        self
    }
}

/// Project lifecycle evidence shared by onboarding, the shelf, and settings.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct WorkspaceState {
    /// One row for every local folder in the shelf.
    pub projects: Arc<[WorkspaceProject]>,
    /// Lossless identity of the active shelf project.
    pub active: Option<LocalProjectId>,
    /// Lossless identity of the project the local service currently serves.
    pub host: Option<LocalProjectId>,
    /// Last path rejection shown by the add flow.
    pub path_error: Option<Arc<str>>,
}

impl WorkspaceState {
    /// Returns whether the active shelf row must be rebound to the live
    /// service before deep queries are admitted after startup.
    #[must_use]
    pub fn requires_rebind(&self) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        if self.host.as_ref() != Some(active) {
            return true;
        }
        self.projects
            .iter()
            .find(|project| project.id == *active)
            .map_or(true, |project| project.phase != ProjectPhase::Ready)
    }
}

/// Surface appearance preference exposed by Settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppearancePreference {
    /// Dark abyss palette from the Nudox design system.
    #[default]
    Abyss,
    /// Light glacier palette for bright environments.
    Glacier,
}

/// Interface scale preference, expressed as a percentage of the baseline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextScalePreference {
    /// The compact design baseline.
    #[default]
    Percent100,
    /// A comfortable reading size.
    Percent115,
    /// A large reading size.
    Percent135,
    /// A very large reading size.
    Percent160,
    /// A two-times text stress size for accessibility validation.
    Percent200,
}

impl TextScalePreference {
    /// Returns the percentage represented by this closed setting.
    #[must_use]
    pub const fn percent(self) -> u16 {
        match self {
            Self::Percent100 => 100,
            Self::Percent115 => 115,
            Self::Percent135 => 135,
            Self::Percent160 => 160,
            Self::Percent200 => 200,
        }
    }

    /// Advances the scale while clamping at the supported bounds.
    #[must_use]
    pub const fn step(self, up: bool) -> Self {
        match (self, up) {
            (Self::Percent100, true) => Self::Percent115,
            (Self::Percent115, true) => Self::Percent135,
            (Self::Percent135, true) => Self::Percent160,
            (Self::Percent160, true) => Self::Percent200,
            (Self::Percent200, true) => Self::Percent200,
            (Self::Percent200, false) => Self::Percent160,
            (Self::Percent160, false) => Self::Percent135,
            (Self::Percent135, false) => Self::Percent115,
            (Self::Percent115, false) => Self::Percent100,
            (Self::Percent100, false) => Self::Percent100,
        }
    }
}

/// Remote data policy. Local source and indexes remain the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PrivacyPreference {
    /// Keep source, index, and registry requests on this machine.
    #[default]
    LocalOnly,
    /// Permit remote registry metadata while keeping source local.
    RegistryMetadata,
}

/// Local service hosting mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ServiceMode {
    /// Start the embedded daemon for this desktop process.
    #[default]
    Embedded,
    /// Attach to a daemon already serving the workspace.
    Attached,
}

/// Current local-service connection status.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionStatus {
    /// No probe has been run yet.
    #[default]
    Unknown,
    /// A probe is in flight.
    Testing,
    /// The local service answered successfully.
    Connected,
    /// The service could not answer the last probe.
    Disconnected,
}

/// Persistent shell and local-first settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsState {
    /// Whether reduced motion is requested.
    pub reduced_motion: bool,
    /// Whether the shelf is open.
    pub shelf_open: bool,
    /// Whether the context panel is open.
    pub context_open: bool,
    /// Light/dark surface preference.
    pub appearance: AppearancePreference,
    /// Global interface text scale.
    pub text_scale: TextScalePreference,
    /// Whether remote registry metadata may be requested.
    pub privacy: PrivacyPreference,
    /// How the local daemon is hosted.
    pub service_mode: ServiceMode,
    /// Whether advisory data is included in registry refreshes.
    pub advisories: bool,
    /// Whether immutable registry responses may be reused locally.
    pub cache_enabled: bool,
    /// Maximum age for cached registry responses.
    pub cache_days: u16,
    /// Current connection probe status.
    pub connection: ConnectionStatus,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            reduced_motion: false,
            shelf_open: true,
            context_open: true,
            appearance: AppearancePreference::Abyss,
            text_scale: TextScalePreference::Percent100,
            privacy: PrivacyPreference::LocalOnly,
            service_mode: ServiceMode::Embedded,
            advisories: true,
            cache_enabled: true,
            cache_days: 14,
            connection: ConnectionStatus::Unknown,
        }
    }
}
