//! The overlay stack (GUI-PLAN §13.5) — omni-search, palette, modals, peeks.
//!
//! # Why a stack rather than an `Option`
//!
//! Most of the time there is one overlay or none. But §23.4's trust-confirm
//! modal can legitimately open *over* the omni-search that triggered it, and
//! when it closes the search must still be there, still focused, with the query
//! intact. An `Option<Overlay>` forces the search to be destroyed and rebuilt,
//! which loses both.
//!
//! So the invariant is: **one overlay is interactive, the rest are suspended
//! beneath it**, and Escape pops exactly one level. That is a stack.
//!
//! # Focus is borrowed, not taken
//!
//! Opening an overlay records where focus was and traps focus inside; closing
//! restores it. Without that, dismissing a palette leaves focus nowhere and the
//! next keystroke goes to the void — the single most common way keyboard flow
//! breaks in apps that grow overlays incrementally (LD-13: every mouse path has
//! a keyboard path, and that includes the path *back*).

use gpui::SharedString;
use gpui::prelude::*;

/// Which overlay a stack entry is.
///
/// A closed enum rather than a boxed view so the shell can reason about
/// precedence and dismissal without downcasting, and so exhaustiveness checking
/// catches a new overlay that forgot to declare its behaviour.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OverlayKind {
    /// `cmd-K` fused search (§15).
    OmniSearch,
    /// `cmd-shift-P` command palette (§23.1).
    CommandPalette,
    /// `?` shortcuts cheat sheet (§23.3).
    Shortcuts,
    /// A hover/click documentation peek (§23.2).
    QuickPeek,
    /// A blocking confirmation (§23.4), identified so the shell can route its
    /// result.
    Modal(SharedString),
}

impl OverlayKind {
    /// Whether clicking the scrim dismisses this overlay.
    ///
    /// Modals say no: a destructive confirmation that vanishes on a stray click
    /// is worse than no confirmation, because it trains people to click through.
    pub fn dismiss_on_scrim_click(&self) -> bool {
        !matches!(self, Self::Modal(_))
    }

    /// Whether this overlay dims the content behind it.
    ///
    /// Quick peek is a reading aid anchored next to its source; dimming the
    /// page would hide the very context it exists to supplement.
    pub fn has_scrim(&self) -> bool {
        !matches!(self, Self::QuickPeek)
    }
}

/// One entry in the stack.
#[derive(Clone, Debug)]
pub struct OverlayEntry {
    /// What this overlay is.
    pub kind: OverlayKind,
    /// An opaque token identifying the focus handle to restore on close.
    ///
    /// The shell owns the actual `FocusHandle`; the stack stores only an
    /// identifier so this module stays pure and testable.
    pub restore_focus_to: Option<SharedString>,
}

/// The overlay stack.
///
/// Pure state, no GPUI types — push/pop/Escape ordering is testable without a
/// window, which matters because focus bugs are otherwise only reproducible by
/// hand.
#[derive(Debug, Default)]
pub struct OverlayStack {
    entries: Vec<OverlayEntry>,
}

impl OverlayStack {
    /// An empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open an overlay above any already open.
    ///
    /// Re-opening a kind that is already the top entry is a no-op: pressing
    /// `cmd-K` twice should not stack two searches.
    pub fn push(&mut self, kind: OverlayKind, restore_focus_to: Option<SharedString>) {
        if self.top().map(|e| &e.kind) == Some(&kind) {
            return;
        }
        self.entries.push(OverlayEntry {
            kind,
            restore_focus_to,
        });
    }

    /// Close the topmost overlay, returning it so the shell can restore focus.
    pub fn pop(&mut self) -> Option<OverlayEntry> {
        self.entries.pop()
    }

    /// Close every overlay, innermost first, returning them in dismissal order.
    ///
    /// Used when navigating away: leaving a stack of overlays open behind a
    /// screen transition is how "escape does nothing" bugs are born.
    pub fn clear(&mut self) -> Vec<OverlayEntry> {
        let mut drained = Vec::new();
        while let Some(entry) = self.entries.pop() {
            drained.push(entry);
        }
        drained
    }

    /// The interactive overlay, if any.
    pub fn top(&self) -> Option<&OverlayEntry> {
        self.entries.last()
    }

    /// Whether an overlay of this kind is anywhere in the stack.
    pub fn contains(&self, kind: &OverlayKind) -> bool {
        self.entries.iter().any(|e| &e.kind == kind)
    }

    /// Whether anything is open — the shell uses this to decide whether the
    /// scrim and focus trap are active at all.
    pub fn is_open(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Stack depth.
    pub fn depth(&self) -> usize {
        self.entries.len()
    }

    /// Whether the topmost overlay wants a scrim behind it.
    pub fn wants_scrim(&self) -> bool {
        self.top().is_some_and(|e| e.kind.has_scrim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_pops_exactly_one_level() {
        let mut stack = OverlayStack::new();
        stack.push(OverlayKind::OmniSearch, Some("editor".into()));
        stack.push(OverlayKind::Modal("trust".into()), Some("omni".into()));
        assert_eq!(stack.depth(), 2);

        let popped = stack.pop().expect("modal should pop");
        assert_eq!(popped.kind, OverlayKind::Modal("trust".into()));
        // The search must survive the modal that opened over it.
        assert_eq!(stack.top().map(|e| &e.kind), Some(&OverlayKind::OmniSearch));
    }

    #[test]
    fn reopening_the_top_overlay_is_a_noop() {
        let mut stack = OverlayStack::new();
        stack.push(OverlayKind::OmniSearch, None);
        stack.push(OverlayKind::OmniSearch, None);
        assert_eq!(stack.depth(), 1, "cmd-K twice must not stack two searches");
    }

    #[test]
    fn focus_restoration_target_travels_with_the_entry() {
        let mut stack = OverlayStack::new();
        stack.push(OverlayKind::CommandPalette, Some("symbol-page".into()));
        let popped = stack.pop().expect("palette should pop");
        assert_eq!(
            popped.restore_focus_to.as_deref(),
            Some("symbol-page"),
            "closing an overlay must know where focus came from"
        );
    }

    #[test]
    fn clear_dismisses_innermost_first() {
        let mut stack = OverlayStack::new();
        stack.push(OverlayKind::OmniSearch, None);
        stack.push(OverlayKind::Modal("gpl".into()), None);
        let order: Vec<OverlayKind> = stack.clear().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            order,
            vec![OverlayKind::Modal("gpl".into()), OverlayKind::OmniSearch]
        );
        assert!(!stack.is_open());
    }

    /// A destructive confirmation must not be dismissable by a stray click.
    #[test]
    fn modals_do_not_dismiss_on_scrim_click() {
        assert!(!OverlayKind::Modal("gc".into()).dismiss_on_scrim_click());
        assert!(OverlayKind::OmniSearch.dismiss_on_scrim_click());
    }

    /// Quick peek supplements the page; dimming it would hide the context.
    #[test]
    fn quick_peek_has_no_scrim() {
        let mut stack = OverlayStack::new();
        stack.push(OverlayKind::QuickPeek, None);
        assert!(!stack.wants_scrim());
        stack.push(OverlayKind::CommandPalette, None);
        assert!(stack.wants_scrim());
    }
}
