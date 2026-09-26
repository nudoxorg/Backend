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
        let label: Arc<str> = std::path::Path::new(path.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(Arc::from)
            .unwrap_or_else(|| Arc::clone(&path));
        Self {
            id,
            path,
            label,
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
    /// Follow the operating system: Abyss when it is dark, Glacier when light.
    System,
}

/// One step of the ⌘+ / ⌘− / ⌘0 zoom.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ZoomStep {
    /// ⌘+: one step larger.
    In,
    /// ⌘−: one step smaller.
    Out,
    /// ⌘0: back to the system's own size.
    Reset,
}

/// The window's text zoom, one offset per display, on top of the text size
/// the system asks for. There is no text-size setting: the system decides
/// the baseline, ⌘+ / ⌘− / ⌘0 move away from it, and a display remembers
/// where you left it (a laptop panel and a wall screen want different
/// sizes).
///
/// Every width decision in the shell is made on `width ÷ scale`, so a larger
/// zoom behaves exactly like a smaller window: margins fold and the shelf
/// becomes a spine instead of text overflowing its box.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ZoomPreference {
    /// Steps away from the system size, per display key.
    steps: Arc<std::collections::BTreeMap<Arc<str>, i8>>,
}

impl ZoomPreference {
    /// The zoom ladder, in percent of the system size.
    pub const LADDER: [u16; 8] = [85, 90, 100, 110, 125, 150, 175, 200];
    /// Where 100 % sits on the ladder.
    const HOME: i8 = 2;

    /// Rebuilds the preference from persisted per-display steps.
    #[must_use]
    pub fn from_steps(steps: impl IntoIterator<Item = (Arc<str>, i8)>) -> Self {
        Self {
            steps: Arc::new(
                steps
                    .into_iter()
                    .map(|(display, step)| (display, Self::clamp(step)))
                    .filter(|(_, step)| *step != 0)
                    .collect(),
            ),
        }
    }

    /// Every display's steps (for persistence).
    pub fn steps(&self) -> impl Iterator<Item = (&Arc<str>, i8)> {
        self.steps.iter().map(|(display, step)| (display, *step))
    }

    fn clamp(step: i8) -> i8 {
        let top = i8::try_from(Self::LADDER.len()).unwrap_or(i8::MAX) - 1 - Self::HOME;
        step.clamp(-Self::HOME, top)
    }

    /// The zoom on `display`, in percent of the system size.
    #[must_use]
    pub fn percent(&self, display: &str) -> u16 {
        let step = self.steps.get(display).copied().unwrap_or(0);
        let index = usize::try_from(Self::HOME + Self::clamp(step)).unwrap_or(0);
        Self::LADDER[index.min(Self::LADDER.len() - 1)]
    }

    /// The zoom on `display` as a multiplier.
    #[must_use]
    pub fn factor(&self, display: &str) -> f32 {
        f32::from(self.percent(display)) / 100.0
    }

    /// One step on `display`.
    #[must_use]
    pub fn step(&self, display: &str, step: ZoomStep) -> Self {
        let current = self.steps.get(display).copied().unwrap_or(0);
        let next = match step {
            ZoomStep::In => Self::clamp(current.saturating_add(1)),
            ZoomStep::Out => Self::clamp(current.saturating_sub(1)),
            ZoomStep::Reset => 0,
        };
        self.with(display, next)
    }

    /// The ladder step nearest `percent` on `display` (tests and captures).
    #[must_use]
    pub fn to_percent(&self, display: &str, percent: u16) -> Self {
        let index = Self::LADDER
            .iter()
            .enumerate()
            .min_by_key(|(_, step)| step.abs_diff(percent))
            .map_or(Self::HOME, |(index, _)| i8::try_from(index).unwrap_or(Self::HOME));
        self.with(display, index - Self::HOME)
    }

    fn with(&self, display: &str, step: i8) -> Self {
        let mut steps = (*self.steps).clone();
        if step == 0 {
            steps.remove(display);
        } else {
            steps.insert(Arc::from(display), step);
        }
        Self {
            steps: Arc::new(steps),
        }
    }
}

/// How much the interface packs into a pixel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum DensityPreference {
    /// The boards' spacing.
    #[default]
    Comfortable,
    /// Tighter rows and gaps.
    Compact,
    /// The tightest rows that stay readable; summaries fold away.
    Dense,
}

/// How much the interface moves.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum MotionPreference {
    /// Follow the operating system's reduce-motion setting.
    #[default]
    System,
    /// Full motion.
    Full,
    /// Reduced: springs snap, ambient motion stops.
    Reduced,
}

/// Contrast treatment layered over the appearance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum ContrastPreference {
    /// The boards' palette.
    #[default]
    Normal,
    /// Every ink one step stronger, firmer lines and bevels.
    High,
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
    /// ⌘± zoom per display, on top of the system text size.
    pub zoom: ZoomPreference,
    /// Spacing and row density.
    pub density: DensityPreference,
    /// Contrast treatment.
    pub contrast: ContrastPreference,
    /// Motion preference; `reduced_motion` mirrors `Reduced`.
    pub motion: MotionPreference,
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
            zoom: ZoomPreference::default(),
            density: DensityPreference::Comfortable,
            contrast: ContrastPreference::Normal,
            motion: MotionPreference::System,
            privacy: PrivacyPreference::LocalOnly,
            service_mode: ServiceMode::Embedded,
            advisories: true,
            cache_enabled: true,
            cache_days: 14,
            connection: ConnectionStatus::Unknown,
        }
    }
}
