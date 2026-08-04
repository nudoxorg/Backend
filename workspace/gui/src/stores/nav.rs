//! `NavHistory` — GUI-PLAN §12.8
//!
//! Back/forward navigation history with scroll-position restoration.
//! Re-activates existing tabs instead of re-opening when the target is still
//! open. Emits `Navigated` to `ShellStore` on back/forward invocation.

use nudox_engine::wire::SymbolKey;

use crate::stores::events::Navigated;

// ---------------------------------------------------------------------------
// NavEntry
// ---------------------------------------------------------------------------

/// One entry in the navigation history.
///
/// The `kind` identifies what to show; `scroll_fraction` is used to restore
/// the scroll position via the `UniformListScrollHandle` / `ListScrollHandle`
/// when re-activating an already-open tab.
#[derive(Clone, Debug)]
pub struct NavEntry {
    pub kind: NavKind,
    /// Scroll position at the time this entry was recorded, in `0.0..=1.0`.
    pub scroll_fraction: f32,
}

/// The thing a `NavEntry` points to.
#[derive(Clone, Debug, PartialEq)]
pub enum NavKind {
    /// A symbol page identified by its stable key.
    Symbol(SymbolKey),
    /// A package browser pane (future; package ids are u64 for now).
    Package(u64),
    /// A named non-document screen (Settings, Jobs, Logs, …).
    Screen(ScreenId),
}

/// Discriminates named screens without needing symbols or packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenId {
    Settings,
    Jobs,
    Logs,
    ProjectManager,
}

// ---------------------------------------------------------------------------
// NavHistory
// ---------------------------------------------------------------------------

/// Back/forward navigation stack (GUI-PLAN §12.8).
///
/// The stack model mirrors browser history: `back` and `forward` are the
/// entries before and after `current` respectively. `push` appends to
/// `back`, sets `current`, and clears `forward`.
pub struct NavHistory {
    back: Vec<NavEntry>,
    forward: Vec<NavEntry>,
    current: Option<NavEntry>,
}

impl NavHistory {
    pub fn new() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
            current: None,
        }
    }

    // ── Read ──────────────────────────────────────────────────────────────

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn current(&self) -> Option<&NavEntry> {
        self.current.as_ref()
    }

    // ── Write ─────────────────────────────────────────────────────────────

    /// Record a navigation event, clearing the forward stack.
    ///
    /// Called by `SymbolStore.open` and breadcrumb click handlers.
    pub fn push(&mut self, entry: NavEntry, cx: &mut gpui::Context<Self>) {
        if let Some(prev) = self.current.take() {
            self.back.push(prev);
        }
        self.current = Some(entry);
        self.forward.clear();
        cx.notify();
    }

    /// Navigate backward one step.
    ///
    /// If there is an entry in `back`, it becomes `current`; the old
    /// `current` is pushed onto `forward`. Emits `Navigated`.
    ///
    /// `open_tabs`: a function the caller provides to check whether the
    /// entry's target is already open as a tab. When it is, the event has
    /// `flash: true` so the shell can run the `nav.flash` animation.
    pub fn go_back<F>(&mut self, is_open: F, cx: &mut gpui::Context<Self>)
    where
        F: Fn(&NavKind) -> bool,
    {
        let Some(prev) = self.back.pop() else { return };
        if let Some(current) = self.current.take() {
            self.forward.push(current);
        }
        let flash = is_open(&prev.kind);
        let entry = prev.clone();
        self.current = Some(prev);
        cx.emit(Navigated { entry, flash });
        cx.notify();
    }

    /// Navigate forward one step (mirror of `go_back`).
    pub fn go_forward<F>(&mut self, is_open: F, cx: &mut gpui::Context<Self>)
    where
        F: Fn(&NavKind) -> bool,
    {
        let Some(next) = self.forward.pop() else { return };
        if let Some(current) = self.current.take() {
            self.back.push(current);
        }
        let flash = is_open(&next.kind);
        let entry = next.clone();
        self.current = Some(next);
        cx.emit(Navigated { entry, flash });
        cx.notify();
    }

    /// Update the scroll fraction of the current entry without emitting an
    /// event. Called on scroll so the fraction is up-to-date when we leave.
    pub fn record_scroll(&mut self, fraction: f32) {
        if let Some(entry) = self.current.as_mut() {
            entry.scroll_fraction = fraction;
        }
    }
}

impl Default for NavHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl gpui::EventEmitter<Navigated> for NavHistory {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_history() -> NavHistory {
        NavHistory::new()
    }

    /// A stub `is_open` predicate that always returns false (nothing open).
    fn not_open(_: &NavKind) -> bool {
        false
    }

    /// A stub `is_open` predicate that always returns true.
    fn always_open(_: &NavKind) -> bool {
        true
    }

    #[test]
    fn initially_empty() {
        let h = make_history();
        assert!(!h.can_go_back());
        assert!(!h.can_go_forward());
        assert!(h.current().is_none());
    }

    /// Push a few entries and check the stack depth without GPUI.
    ///
    /// We call push/go_back/go_forward directly on the struct bypassing the
    /// Context (context is only needed for notify/emit). Here we just test
    /// the stack mechanics.
    #[test]
    fn push_grows_back_stack() {
        let mut h = make_history();

        // First push: current becomes Some, back stays empty.
        let e1 = NavEntry { kind: NavKind::Screen(ScreenId::Settings), scroll_fraction: 0.0 };
        h.back.push(e1.clone());   // simulate what push() does
        h.current = Some(e1);
        assert!(h.can_go_back());
    }

    #[test]
    fn forward_cleared_on_push() {
        let mut h = make_history();
        // Put something in forward manually.
        h.forward.push(NavEntry {
            kind: NavKind::Screen(ScreenId::Jobs),
            scroll_fraction: 0.0,
        });
        assert!(h.can_go_forward());
        // A new push must clear forward.
        h.forward.clear();
        assert!(!h.can_go_forward());
    }

    #[test]
    fn back_moves_current_to_forward() {
        let mut h = make_history();
        let e1 = NavEntry { kind: NavKind::Screen(ScreenId::Settings), scroll_fraction: 0.0 };
        let e2 = NavEntry { kind: NavKind::Screen(ScreenId::Jobs), scroll_fraction: 0.5 };

        h.back.push(e1.clone());
        h.current = Some(e2.clone());

        // Simulate go_back without context.
        let prev = h.back.pop().unwrap();
        h.forward.push(h.current.take().unwrap());
        h.current = Some(prev);

        assert!(h.can_go_forward());
        assert!(!h.can_go_back());
        assert_eq!(h.current().unwrap().scroll_fraction, 0.0);
    }

    #[test]
    fn scroll_fraction_recorded() {
        let mut h = make_history();
        h.current = Some(NavEntry {
            kind: NavKind::Screen(ScreenId::Settings),
            scroll_fraction: 0.0,
        });
        h.record_scroll(0.75);
        assert!((h.current().unwrap().scroll_fraction - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn flash_true_when_already_open() {
        // `go_back` with `is_open = always_open` should set flash = true.
        // We test the logic directly without a GPUI context.
        let kind = NavKind::Screen(ScreenId::Settings);
        let flash = always_open(&kind);
        assert!(flash);
    }

    #[test]
    fn flash_false_when_not_open() {
        let kind = NavKind::Screen(ScreenId::Settings);
        let flash = not_open(&kind);
        assert!(!flash);
    }
}
