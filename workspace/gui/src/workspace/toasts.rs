//! Toasts (GUI-PLAN §13.7) — completion notices for background work.
//!
//! # What a toast is allowed to be
//!
//! LD-16 draws a hard line: **toasts announce the completion of background
//! work, never an error in the input loop.** If the user typed something we
//! could not do, that belongs in the surface they typed into, where they can
//! see it and act on it. A toast for an input error is a message that appears
//! away from the thing it is about and then leaves before it can be read.
//!
//! So: "index finished", "generation ready", "compile failed" — yes. "invalid
//! query" — no, that is an `ErrorState` on the query surface.
//!
//! # The paused countdown
//!
//! Hovering a toast pauses its dismissal, *and the hairline countdown pauses
//! with it*. That detail is the difference between a notification you can
//! actually read and one that taunts you: reaching for a toast that is about to
//! vanish, and having it wait, is the interaction working the way people
//! already expect it to.
//!
//! The queue holds at most [`MAX_VISIBLE`] toasts; the rest are counted in an
//! overflow chip rather than stacking off-screen.

use std::time::{Duration, Instant};

use gpui::SharedString;

/// How many toasts are shown at once before the overflow chip appears (§13.7).
pub const MAX_VISIBLE: usize = 3;

/// How long a toast lives when not hovered.
pub const DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Severity, which selects the accent colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToastLevel {
    /// Work finished as intended.
    Success,
    /// Work finished with something worth knowing.
    Warning,
    /// Work failed. Still a *completion*, which is why it is allowed here.
    Failure,
}

/// The single action a toast may offer (§13.7: one action maximum).
#[derive(Clone, Debug)]
pub struct ToastAction {
    /// Button label — "View", "Retry".
    pub label: SharedString,
    /// Identifier the shell dispatches when the action is taken.
    pub id: SharedString,
}

/// One queued toast.
#[derive(Clone, Debug)]
pub struct Toast {
    /// Stable identity, so re-notifying the same job replaces rather than stacks.
    pub id: SharedString,
    /// Pre-formatted message (§1.1.4 — never built in `render`).
    pub message: SharedString,
    pub level: ToastLevel,
    pub action: Option<ToastAction>,
    /// When the toast was shown, or `None` while hovered.
    ///
    /// Modelling the pause as "no deadline" rather than a `paused: bool` plus a
    /// separate instant means a paused toast cannot accidentally expire: there
    /// is no deadline to compare against (LR-11 in spirit — states that permit
    /// different operations are different shapes).
    shown_at: Option<Instant>,
    /// Time already elapsed before the current pause.
    elapsed_before_pause: Duration,
}

impl Toast {
    /// Create a toast that starts counting down at `now`.
    pub fn new(
        id: impl Into<SharedString>,
        message: impl Into<SharedString>,
        level: ToastLevel,
        now: Instant,
    ) -> Self {
        Self {
            id: id.into(),
            message: message.into(),
            level,
            action: None,
            shown_at: Some(now),
            elapsed_before_pause: Duration::ZERO,
        }
    }

    /// Attach the single permitted action.
    pub fn with_action(mut self, action: ToastAction) -> Self {
        self.action = Some(action);
        self
    }

    /// Fraction of the dismissal timer consumed, in `0.0..=1.0`.
    ///
    /// This drives the hairline countdown; it freezes while hovered because
    /// `shown_at` is `None` and only `elapsed_before_pause` contributes.
    pub fn progress(&self, now: Instant) -> f32 {
        let elapsed = match self.shown_at {
            Some(started) => self.elapsed_before_pause + now.saturating_duration_since(started),
            None => self.elapsed_before_pause,
        };
        (elapsed.as_secs_f32() / DISMISS_AFTER.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Whether this toast has outlived its welcome.
    pub fn is_expired(&self, now: Instant) -> bool {
        self.shown_at.is_some() && self.progress(now) >= 1.0
    }

    /// Pause the countdown (pointer entered).
    pub fn pause(&mut self, now: Instant) {
        if let Some(started) = self.shown_at.take() {
            self.elapsed_before_pause += now.saturating_duration_since(started);
        }
    }

    /// Resume the countdown (pointer left).
    pub fn resume(&mut self, now: Instant) {
        if self.shown_at.is_none() {
            self.shown_at = Some(now);
        }
    }

    /// Whether the countdown is currently paused.
    pub fn is_paused(&self) -> bool {
        self.shown_at.is_none()
    }
}

/// The bounded toast queue.
///
/// Pure state — no GPUI types — so the cap, replacement and expiry rules are
/// testable without a window.
#[derive(Debug, Default)]
pub struct ToastQueue {
    toasts: Vec<Toast>,
}

impl ToastQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a toast, replacing any existing one with the same id.
    ///
    /// Replacement rather than stacking is what keeps a chatty job from
    /// flooding the corner: ten progress notices for one compile collapse to
    /// the latest.
    pub fn push(&mut self, toast: Toast) {
        if let Some(slot) = self.toasts.iter_mut().find(|t| t.id == toast.id) {
            *slot = toast;
        } else {
            self.toasts.push(toast);
        }
    }

    /// Drop expired toasts. Call once per frame while any toast is visible.
    pub fn expire(&mut self, now: Instant) {
        self.toasts.retain(|t| !t.is_expired(now));
    }

    /// Dismiss one toast by id (user clicked the close affordance).
    pub fn dismiss(&mut self, id: &str) {
        self.toasts.retain(|t| t.id.as_ref() != id);
    }

    /// The toasts that should be rendered, newest last, capped at [`MAX_VISIBLE`].
    pub fn visible(&self) -> &[Toast] {
        let start = self.toasts.len().saturating_sub(MAX_VISIBLE);
        &self.toasts[start..]
    }

    /// How many toasts are queued beyond the visible ones — the "+n" chip.
    pub fn overflow(&self) -> usize {
        self.toasts.len().saturating_sub(MAX_VISIBLE)
    }

    /// Mutable access for hover pause/resume.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Toast> {
        self.toasts.iter_mut().find(|t| t.id.as_ref() == id)
    }

    /// Total queued, visible or not.
    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// Whether anything is queued.
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(id: &str, now: Instant) -> Toast {
        Toast::new(id, "done", ToastLevel::Success, now)
    }

    #[test]
    fn queue_caps_visible_and_reports_overflow() {
        let now = Instant::now();
        let mut q = ToastQueue::new();
        for i in 0..5 {
            q.push(toast(&format!("job-{i}"), now));
        }
        assert_eq!(q.visible().len(), MAX_VISIBLE);
        assert_eq!(q.overflow(), 2);
        // The newest survive — a completion you just triggered must be the one
        // you see.
        assert_eq!(q.visible()[MAX_VISIBLE - 1].id.as_ref(), "job-4");
    }

    #[test]
    fn same_id_replaces_rather_than_stacks() {
        let now = Instant::now();
        let mut q = ToastQueue::new();
        q.push(toast("compile", now));
        q.push(Toast::new("compile", "failed", ToastLevel::Failure, now));
        assert_eq!(q.len(), 1);
        assert_eq!(q.visible()[0].level, ToastLevel::Failure);
    }

    /// The detail from §13.7: hovering must stop the clock, and the countdown
    /// hairline must stop with it.
    #[test]
    fn hover_pauses_the_countdown_and_its_progress() {
        let start = Instant::now();
        let mut t = toast("job", start);

        let two_secs = start + Duration::from_secs(2);
        let before = t.progress(two_secs);
        assert!(before > 0.0, "countdown should have advanced");

        t.pause(two_secs);
        assert!(t.is_paused());

        // Time passes while hovered — progress must not move.
        let much_later = start + Duration::from_secs(30);
        assert_eq!(
            t.progress(much_later),
            before,
            "the hairline countdown kept running while hovered"
        );
        assert!(
            !t.is_expired(much_later),
            "a hovered toast must never expire out from under the pointer"
        );

        // Resuming continues from where it stopped, not from zero.
        t.resume(much_later);
        let after_resume = t.progress(much_later + Duration::from_millis(1));
        assert!(after_resume >= before);
    }

    #[test]
    fn expired_toasts_are_dropped() {
        let start = Instant::now();
        let mut q = ToastQueue::new();
        q.push(toast("job", start));
        q.expire(start + DISMISS_AFTER + Duration::from_millis(1));
        assert!(q.is_empty());
    }
}
