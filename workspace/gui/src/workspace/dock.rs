//! Dock geometry and banner state — the shell chrome that survives a rebuild.

use gpui::{App, SharedString};

// ─────────────────────────────────────────────────────────────────────────────
// Dock geometry
// ─────────────────────────────────────────────────────────────────────────────

/// The left dock's width at first launch, in pixels.
pub(crate) const LEFT_DOCK_DEFAULT_W: f32 = 240.0;
/// The right dock's width at first launch, in pixels.
pub(crate) const RIGHT_DOCK_DEFAULT_W: f32 = 220.0;
/// The bottom dock's height at first launch, in pixels.
pub(crate) const BOTTOM_DOCK_DEFAULT_H: f32 = 200.0;

/// One dock's persisted state: whether it is showing, and how big it is when it
/// is.
///
/// Two fields rather than one signed number because they are independent facts
/// — a closed dock still remembers the width it will re-open to, and collapsing
/// that into "size 0 means closed" is how the re-open size gets lost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DockState {
    /// Whether the dock is currently showing.
    pub open: bool,
    /// The size it occupies when open — width for left/right, height for bottom.
    pub size: f32,
}

impl DockState {
    /// The size the dock's spring should be sitting at right now: its own size
    /// when open, zero when closed.
    ///
    /// A method rather than a `match` repeated at three call sites, because
    /// getting it wrong at one of them produces a window that renders a dock
    /// the layout says is closed.
    pub fn rendered_size(self) -> f32 {
        if self.open { self.size } else { 0.0 }
    }
}

/// Dock geometry that has to outlive any particular window.
///
/// # Why this is a `Global` and not just `Shell` state
///
/// It *was* just `Shell` state, and dismissing the window threw it away.
/// `28-restored.png` caught it on the first run: every document came back, the
/// active tab came back, the endpoint came back — and the bottom dock came back
/// closed, because `Shell::new` hardcoded `false`.
///
/// The general rule that follows, and the reason this type is worth its weight:
/// **once the window can be rebuilt, anything the reader would notice missing
/// has to live on the `App`.** A store is the usual home for that; dock
/// geometry is not corpus data and has no store, so it lives here. The
/// screenshot suite's `28-restored` frame is the guard — it asserts the rebuilt
/// frame *matches* the dismissed one, so the next piece of `Shell` state that
/// silently fails to survive shows up as a pixel diff rather than as a
/// complaint six months later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DockLayout {
    /// Project panel. Open at first launch — it is the corpus, and LR-10 says
    /// local content is not something you go and find.
    pub left: DockState,
    /// Jobs + Logs.
    pub bottom: DockState,
    /// Outline.
    pub right: DockState,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            left: DockState {
                open: true,
                size: LEFT_DOCK_DEFAULT_W,
            },
            bottom: DockState {
                open: false,
                size: BOTTOM_DOCK_DEFAULT_H,
            },
            right: DockState {
                open: false,
                size: RIGHT_DOCK_DEFAULT_W,
            },
        }
    }
}

impl gpui::Global for DockLayout {}

impl DockLayout {
    /// What the next window should be built with.
    ///
    /// [`Default`] on a cold launch, and whatever the last window published on
    /// a rebuild — so a first launch and a restore go through the same code
    /// path with different data, rather than through two code paths.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Banner
// ─────────────────────────────────────────────────────────────────────────────

/// The kind of banner currently shown, if any (§13.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerKind {
    /// Offline: serving local Ready subset, some packages unavailable.
    Offline { unavailable: u32 },
    /// Index server unreachable; shows a retry countdown.
    IndexUnreachable { retry_in_secs: u32 },
}

/// The current banner, including its pre-built display label.
///
/// Labels are pre-built at update time (§1.1.4 — no `format!` in render).
#[derive(Debug, Clone)]
pub struct Banner {
    pub kind: BannerKind,
    /// Ready-to-render label; built when the banner is pushed to `Shell`.
    pub label: SharedString,
}
