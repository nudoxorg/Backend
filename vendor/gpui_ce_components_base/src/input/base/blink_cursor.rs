use gpui::{Context, Pixels, Task, px};
use instant::Duration;

static INTERVAL: Duration = Duration::from_millis(500);
static PAUSE_DELAY: Duration = Duration::from_millis(300);

// On Windows, Linux, we should use integer to avoid blurry cursor.
#[cfg(not(target_os = "macos"))]
pub(super) const CURSOR_WIDTH: Pixels = px(2.);
#[cfg(target_os = "macos")]
pub(super) const CURSOR_WIDTH: Pixels = px(1.5);

/// To manage the Input cursor blinking.
///
/// It will start blinking with a interval of 500ms.
/// Every loop will notify the view to update the `visible`, and Input will observe this update to touch repaint.
///
/// The input painter will check if this in visible state, then it will draw the cursor.
pub(crate) struct BlinkCursor {
    // NUDOX: one focus-owned, cancelable clock; stale deadlines cannot revive it.
    active: bool,
    visible: bool,
    paused: bool,
    generation: u64,

    task: Option<Task<()>>,
}

impl BlinkCursor {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            visible: false,
            paused: false,
            generation: 0,
            task: None,
        }
    }

    /// Acquires one blink clock. Repeated focus notifications keep its phase.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        if self.active {
            return;
        }
        self.active = true;
        self.paused = false;
        self.visible = true;
        self.invalidate_timer();
        cx.notify();
        self.schedule(INTERVAL, false, cx);
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let changed = self.visible();
        self.active = false;
        self.paused = false;
        self.visible = false;
        self.invalidate_timer();
        if changed {
            cx.notify();
        }
    }

    fn invalidate_timer(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.task = None;
    }

    fn blink(&mut self, generation: u64, cx: &mut Context<Self>) {
        if !self.active || self.paused || generation != self.generation {
            return;
        }

        self.visible = !self.visible;
        cx.notify();

        self.schedule(INTERVAL, false, cx);
    }

    pub(crate) fn visible(&self) -> bool {
        // Keep showing the cursor if paused
        self.active && (self.paused || self.visible)
    }

    /// Holds a focused cursor visible for 300ms. Editing an inactive input
    /// cannot acquire a blink clock or restart one that blur already stopped.
    pub(crate) fn pause(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let changed = !self.visible();
        self.paused = true;
        self.visible = true;
        self.invalidate_timer();
        if changed {
            cx.notify();
        }
        self.schedule(PAUSE_DELAY, true, cx);
    }

    fn resume(&mut self, generation: u64, cx: &mut Context<Self>) {
        if !self.active || generation != self.generation {
            return;
        }
        self.paused = false;
        self.blink(generation, cx);
    }

    fn schedule(&mut self, delay: Duration, resume: bool, cx: &mut Context<Self>) {
        let generation = self.generation;
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if resume {
                        this.resume(generation, cx);
                    } else {
                        this.blink(generation, cx);
                    }
                });
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{BlinkCursor, Duration};
    use gpui::{AppContext as _, Entity, Subscription, TestAppContext};
    use std::{cell::Cell, rc::Rc};

    fn watched(cx: &mut TestAppContext) -> (Entity<BlinkCursor>, Rc<Cell<usize>>, Subscription) {
        let cursor = cx.new(|_| BlinkCursor::new());
        let notifications = Rc::new(Cell::new(0));
        let count = notifications.clone();
        let subscription =
            cx.update(|cx| cx.observe(&cursor, move |_, _| count.set(count.get() + 1)));
        (cursor, notifications, subscription)
    }

    fn advance(cx: &mut TestAppContext, millis: u64) {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(millis));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn stopped_cursor_is_quiet_and_editing_cannot_restart_it(cx: &mut TestAppContext) {
        let (cursor, notifications, _subscription) = watched(cx);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        advance(cx, 100);
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        let stopped = notifications.get();
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        advance(cx, 2000);
        assert_eq!(notifications.get(), stopped);
        cursor.update(cx, |cursor, _| {
            assert!(!cursor.active && !cursor.paused && !cursor.visible());
            assert!(cursor.task.is_none());
        });
    }

    #[gpui::test]
    fn repeated_start_preserves_one_clock_and_its_phase(cx: &mut TestAppContext) {
        let (cursor, notifications, _subscription) = watched(cx);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        let started = notifications.get();
        let generation = cursor.read_with(cx, |cursor, _| cursor.generation);
        advance(cx, 100);
        for _ in 0..16 {
            cursor.update(cx, |cursor, cx| cursor.start(cx));
        }
        assert_eq!(
            cursor.read_with(cx, |cursor, _| cursor.generation),
            generation
        );
        assert_eq!(notifications.get(), started);
        advance(cx, 400);
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), started + 1);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        advance(cx, 500);
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), started + 2);
    }

    #[gpui::test]
    fn restart_rejects_old_blink_and_pause_deadlines(cx: &mut TestAppContext) {
        let (cursor, notifications, _subscription) = watched(cx);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        let old_blink = cursor.read_with(cx, |cursor, _| cursor.generation);
        advance(cx, 100);
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        let old_pause = cursor.read_with(cx, |cursor, _| cursor.generation);
        advance(cx, 50);
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        advance(cx, 50);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        let restarted = notifications.get();
        cursor.update(cx, |cursor, cx| {
            cursor.blink(old_blink, cx);
            cursor.resume(old_pause, cx);
        });
        advance(cx, 300); // Both old deadlines have passed at virtual 500ms.
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), restarted);
        advance(cx, 200); // The new clock's first deadline is 700ms.
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), restarted + 1);
    }

    #[gpui::test]
    fn a_pause_owns_one_resume_and_stop_cancels_it(cx: &mut TestAppContext) {
        let (cursor, notifications, _subscription) = watched(cx);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        advance(cx, 450);
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        let paused = cursor.read_with(cx, |cursor, _| cursor.generation);
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        assert_eq!(cursor.read_with(cx, |cursor, _| cursor.generation), paused);
        advance(cx, 50);
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        advance(cx, 250);
        assert!(
            !cursor.read_with(cx, |cursor, _| cursor.paused)
                && !cursor.read_with(cx, |cursor, _| cursor.visible())
        );
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        let stale = cursor.read_with(cx, |cursor, _| cursor.generation);
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        let stopped = notifications.get();
        cursor.update(cx, |cursor, cx| cursor.resume(stale, cx));
        advance(cx, 2000);
        assert_eq!(notifications.get(), stopped);
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn dropping_the_cursor_releases_its_timer_without_notifications(cx: &mut TestAppContext) {
        let (cursor, notifications, _subscription) = watched(cx);
        cursor.update(cx, |cursor, cx| {
            cursor.start(cx);
            cursor.pause(cx);
        });
        cx.run_until_parked();
        let weak = cursor.downgrade();
        let before = notifications.get();
        cx.update(|_| drop(cursor)); // Flush GPUI's deferred entity destruction.
        advance(cx, 2000);
        assert!(weak.upgrade().is_none());
        assert_eq!(notifications.get(), before);
    }
}
