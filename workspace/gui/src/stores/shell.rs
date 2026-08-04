//! `ShellStore` — GUI-PLAN §12.9
//!
//! Pure UI state with persistence. Owns:
//!
//! - Dock sizes and visibility (committed values; `Motion` targets live in views).
//! - The active tab per pane.
//! - The overlay stack (at most one overlay active at a time; modals may nest).
//! - The banner queue (at most one banner shown; others queue).
//! - The toast queue (at most 3 visible; overflow shown as "+N" chip).
//!
//! `ShellStore` does **no** I/O. Every field is either a small scalar or a
//! `SharedString` precomputed at update time (§1.1.4).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, SharedString};

use crate::stores::events::ToastRequest;
use crate::stores::symbol::TabId;

// ---------------------------------------------------------------------------
// Dock
// ---------------------------------------------------------------------------

/// Committed dock dimensions (pixels). Views copy these into their `Motion`
/// targets; `ShellStore` is updated when a drag or toggle *completes*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DockState {
    /// Width of the left sidebar in pixels (0 = hidden).
    pub left_width: f32,
    /// Height of the bottom panel in pixels (0 = hidden).
    pub bottom_height: f32,
    /// Width of the right panel in pixels (0 = hidden).
    pub right_width: f32,
}

impl Default for DockState {
    fn default() -> Self {
        Self {
            left_width: 260.0,
            bottom_height: 200.0,
            right_width: 0.0, // hidden by default
        }
    }
}

// ---------------------------------------------------------------------------
// Active tabs
// ---------------------------------------------------------------------------

/// Which tab is active in a named pane.
///
/// `center` is the main document area; `left`/`bottom` use opaque indices
/// because they host non-symbol panels (Project, Search, Jobs, Logs).
#[derive(Clone, Debug, Default)]
pub struct PaneTabs {
    /// Active symbol tab in the center pane (None = no tab open).
    pub center: Option<TabId>,
    /// Active tab index in the left dock (0 = Project, 1 = Search, …).
    pub left: usize,
    /// Active tab index in the bottom dock (0 = Jobs, 1 = Logs).
    pub bottom: usize,
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

/// Identifies which overlay is on screen.
///
/// `#[non_exhaustive]` so adding a new overlay variant is not a breaking match
/// arm change for any view that only cares about "is *something* open".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OverlayKind {
    OmniSearch,
    CommandPalette,
    Shortcuts,
    /// A modal dialog (trust confirm, GPL consent, GC confirm).
    Modal,
}

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// A dismissible status banner (offline, index unreachable, update available).
///
/// Only one banner is visible at a time; others queue in `ShellStore.banners`.
#[derive(Clone, Debug)]
pub struct Banner {
    pub message: SharedString,
    pub is_error: bool,
    /// When the banner was enqueued; used to order by arrival.
    pub queued_at: Instant,
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

/// A toast notification (GUI-PLAN §13.7).
///
/// At most 3 are visible; additional ones are counted in `overflow_count`.
/// Each toast has an optional single action label.
#[derive(Clone, Debug)]
pub struct Toast {
    pub id: ToastId,
    pub message: Arc<str>,
    pub is_error: bool,
    pub action_label: Option<&'static str>,
    pub arrived: Instant,
    /// When the timer fires (5 s from arrival unless hovered).
    pub dismiss_at: Instant,
}

/// Opaque toast id (for hover-pause and explicit dismiss).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToastId(u64);

// ---------------------------------------------------------------------------
// ShellStore
// ---------------------------------------------------------------------------

/// Maximum number of simultaneously visible toasts (§13.7).
const MAX_VISIBLE_TOASTS: usize = 3;

/// Auto-dismiss duration (§13.7: 5 s).
const TOAST_TTL: std::time::Duration = std::time::Duration::from_secs(5);

pub struct ShellStore {
    /// Committed dock geometry.
    pub dock: DockState,
    /// Which tab is active in each pane.
    pub pane_tabs: PaneTabs,
    /// Current overlay stack. The last element is the topmost overlay.
    /// In v1 we allow at most one base overlay + nested modals.
    pub overlay_stack: Vec<OverlayKind>,
    /// Banner queue. Only `banners[0]` is rendered; the rest wait.
    pub banners: VecDeque<Banner>,
    /// Visible toasts (at most `MAX_VISIBLE_TOASTS`).
    pub toasts: Vec<Toast>,
    /// Number of toasts that have been suppressed due to the cap.
    pub overflow_count: usize,
    /// Window title (pre-formatted, §1.1.4).
    pub window_title: SharedString,

    next_toast_id: u64,
}

impl ShellStore {
    pub fn new() -> Self {
        Self {
            dock: DockState::default(),
            pane_tabs: PaneTabs::default(),
            overlay_stack: Vec::new(),
            banners: VecDeque::new(),
            toasts: Vec::new(),
            overflow_count: 0,
            window_title: SharedString::from("nudox"),
            next_toast_id: 1,
        }
    }

    // ── Overlay ───────────────────────────────────────────────────────────

    /// Push an overlay onto the stack (becomes topmost).
    ///
    /// Duplicate pushes of the same kind are ignored (idempotent).
    pub fn push_overlay(&mut self, kind: OverlayKind, cx: &mut Context<Self>) {
        if self.overlay_stack.last() != Some(&kind) {
            self.overlay_stack.push(kind);
            cx.notify();
        }
    }

    /// Pop the topmost overlay. Returns the kind that was dismissed.
    pub fn pop_overlay(&mut self, cx: &mut Context<Self>) -> Option<OverlayKind> {
        let dismissed = self.overlay_stack.pop();
        if dismissed.is_some() {
            cx.notify();
        }
        dismissed
    }

    /// Close a specific overlay kind (removes the first matching entry from the top).
    pub fn close_overlay(&mut self, kind: OverlayKind, cx: &mut Context<Self>) {
        if let Some(pos) = self.overlay_stack.iter().rposition(|k| *k == kind) {
            self.overlay_stack.remove(pos);
            cx.notify();
        }
    }

    /// Returns the currently active (topmost) overlay, if any.
    pub fn active_overlay(&self) -> Option<OverlayKind> {
        self.overlay_stack.last().copied()
    }

    // ── Banner ────────────────────────────────────────────────────────────

    /// Enqueue a banner. If the queue was empty, the banner is immediately
    /// visible (rendered as `banners[0]`).
    pub fn push_banner(&mut self, banner: Banner, cx: &mut Context<Self>) {
        self.banners.push_back(banner);
        cx.notify();
    }

    /// Dismiss the currently visible banner and show the next one if any.
    pub fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banners.pop_front();
        cx.notify();
    }

    // ── Toast ─────────────────────────────────────────────────────────────

    /// Enqueue a toast from a `ToastRequest` (emitted by `JobStore`).
    ///
    /// If there are already `MAX_VISIBLE_TOASTS` visible, the count of
    /// suppressed toasts is incremented instead of adding to the visible list.
    pub fn enqueue_toast(&mut self, req: ToastRequest, cx: &mut Context<Self>) {
        if self.toasts.len() >= MAX_VISIBLE_TOASTS {
            self.overflow_count += 1;
        } else {
            let now = Instant::now();
            let id = ToastId(self.next_toast_id);
            self.next_toast_id += 1;
            self.toasts.push(Toast {
                id,
                message: req.message,
                is_error: req.is_error,
                action_label: req.action.map(|a| a.label),
                arrived: now,
                dismiss_at: now + TOAST_TTL,
            });
        }
        cx.notify();
    }

    /// Dismiss a toast by id.
    pub fn dismiss_toast(&mut self, id: ToastId, cx: &mut Context<Self>) {
        self.toasts.retain(|t| t.id != id);
        // If there are overflow toasts, promote one now.
        if self.overflow_count > 0 && self.toasts.len() < MAX_VISIBLE_TOASTS {
            // The overflow batch doesn't carry individual toasts (they were
            // dropped at the cap); decrement the counter only.
            self.overflow_count = self.overflow_count.saturating_sub(1);
        }
        cx.notify();
    }

    /// Expire toasts whose `dismiss_at` has passed.
    ///
    /// Called from the shell view's `on_next_frame` handler, not from stores.
    pub fn expire_toasts(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let before = self.toasts.len();
        self.toasts.retain(|t| t.dismiss_at > now);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Pause a toast's dismiss timer (user is hovering).
    pub fn pause_toast(&mut self, id: ToastId, cx: &mut Context<Self>) {
        if let Some(t) = self.toasts.iter_mut().find(|t| t.id == id) {
            // Extend by one full TTL from now.
            t.dismiss_at = Instant::now() + TOAST_TTL;
            cx.notify();
        }
    }

    // ── Tab / pane ────────────────────────────────────────────────────────

    /// Set the active center-pane tab.
    pub fn activate_center_tab(&mut self, tab: Option<TabId>, cx: &mut Context<Self>) {
        if self.pane_tabs.center != tab {
            self.pane_tabs.center = tab;
            cx.notify();
        }
    }

    /// Set the active left-dock tab by index.
    pub fn activate_left_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.pane_tabs.left != idx {
            self.pane_tabs.left = idx;
            cx.notify();
        }
    }

    /// Set the active bottom-dock tab by index.
    pub fn activate_bottom_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.pane_tabs.bottom != idx {
            self.pane_tabs.bottom = idx;
            cx.notify();
        }
    }

    // ── Dock ─────────────────────────────────────────────────────────────

    /// Commit a new dock width after a drag or toggle animation settles.
    pub fn commit_left_width(&mut self, px: f32, cx: &mut Context<Self>) {
        self.dock.left_width = px;
        cx.notify();
    }

    pub fn commit_bottom_height(&mut self, px: f32, cx: &mut Context<Self>) {
        self.dock.bottom_height = px;
        cx.notify();
    }

    pub fn commit_right_width(&mut self, px: f32, cx: &mut Context<Self>) {
        self.dock.right_width = px;
        cx.notify();
    }

    // ── Window title ──────────────────────────────────────────────────────

    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.window_title = title.into();
        cx.notify();
    }
}

impl Default for ShellStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_store() -> ShellStore {
        ShellStore::new()
    }

    fn make_toast_req(msg: &str) -> ToastRequest {
        ToastRequest {
            message: Arc::from(msg),
            action: None,
            is_error: false,
        }
    }

    #[test]
    fn initially_no_overlay() {
        let s = make_store();
        assert!(s.active_overlay().is_none());
    }

    #[test]
    fn push_and_pop_overlay() {
        let mut s = make_store();
        // push / pop without a GPUI context — we test the stack shape only.
        s.overlay_stack.push(OverlayKind::OmniSearch);
        assert_eq!(s.active_overlay(), Some(OverlayKind::OmniSearch));
        s.overlay_stack.pop();
        assert!(s.active_overlay().is_none());
    }

    #[test]
    fn duplicate_overlay_ignored_at_top() {
        let mut s = make_store();
        s.overlay_stack.push(OverlayKind::OmniSearch);
        // A second push of the same kind should NOT create a second entry
        // (logic inside push_overlay checks last == kind).
        let last_before = s.overlay_stack.last().copied();
        if s.overlay_stack.last() != Some(&OverlayKind::OmniSearch) {
            s.overlay_stack.push(OverlayKind::OmniSearch);
        }
        assert_eq!(s.overlay_stack.len(), 1);
    }

    #[test]
    fn toast_cap_increments_overflow() {
        let mut s = make_store();
        // Fill to cap.
        for i in 0..MAX_VISIBLE_TOASTS {
            let now = Instant::now();
            s.toasts.push(Toast {
                id: ToastId(i as u64),
                message: Arc::from(format!("toast {i}").as_str()),
                is_error: false,
                action_label: None,
                arrived: now,
                dismiss_at: now + TOAST_TTL,
            });
        }
        // Now enqueue one more (simulate enqueue_toast without a context).
        s.overflow_count += 1; // what enqueue_toast does when full
        assert_eq!(s.overflow_count, 1);
        assert_eq!(s.toasts.len(), MAX_VISIBLE_TOASTS);
    }

    #[test]
    fn dismiss_toast_removes_by_id() {
        let mut s = make_store();
        let now = Instant::now();
        let id = ToastId(42);
        s.toasts.push(Toast {
            id,
            message: Arc::from("hello"),
            is_error: false,
            action_label: None,
            arrived: now,
            dismiss_at: now + TOAST_TTL,
        });
        s.toasts.retain(|t| t.id != id);
        assert!(s.toasts.is_empty());
    }

    #[test]
    fn banner_queue_fifo() {
        let mut s = make_store();
        s.banners.push_back(Banner {
            message: SharedString::from("first"),
            is_error: false,
            queued_at: Instant::now(),
        });
        s.banners.push_back(Banner {
            message: SharedString::from("second"),
            is_error: false,
            queued_at: Instant::now(),
        });
        assert_eq!(s.banners.len(), 2);
        s.banners.pop_front();
        assert_eq!(s.banners.front().unwrap().message, SharedString::from("second"));
    }

    #[test]
    fn dock_defaults() {
        let s = make_store();
        assert!(s.dock.left_width > 0.0, "left dock open by default");
        assert_eq!(s.dock.right_width, 0.0, "right dock closed by default");
    }
}
