//! The one deadline timer of the overlay layers: a layer that owns a model
//! driven by an explicit clock (the float layer's cards, the toast stack)
//! asks its model for the next instant something changes, and arms a single
//! executor timer for it. A newer deadline replaces the timer (dropping a
//! task cancels it); no deadline disarms it; when it passes, the layer
//! advances its model and arms the next one.
//!
//! Both layers used to carry their own copy of this; they now share it.

use crate::motion;
use gpui::{App, Task, Window};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// The timer armed for one deadline: the instant and the task that waits
/// for it.
#[derive(Default)]
pub(crate) struct Deadline(Option<(Instant, Task<()>)>);

impl Deadline {
    /// The instant it is armed for.
    pub(crate) fn at(&self) -> Option<Instant> {
        self.0.as_ref().map(|(at, _)| *at)
    }

    fn disarm(&mut self) {
        self.0 = None;
    }

    fn arm(&mut self, at: Instant, task: Task<()>) {
        self.0 = Some((at, task));
    }
}

/// Makes `state`'s [`Deadline`] (found by `slot`) wait for `deadline`. When
/// the timer passes, the deadline is disarmed and `fire` runs with the
/// motion clock's `now` (it advances the model and arms the next deadline).
/// Arming the instant it is already armed for does nothing.
pub(crate) fn arm<S: 'static>(
    state: &Rc<RefCell<S>>,
    slot: fn(&mut S) -> &mut Deadline,
    deadline: Option<Instant>,
    now: Instant,
    window: &mut Window,
    cx: &mut App,
    fire: fn(&Rc<RefCell<S>>, Instant, &mut Window, &mut App),
) {
    let current = slot(&mut state.borrow_mut()).at();
    if current == deadline {
        return;
    }
    let Some(at) = deadline else {
        slot(&mut state.borrow_mut()).disarm();
        return;
    };
    let delay = at.saturating_duration_since(now);
    let weak = Rc::downgrade(state);
    let task = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(delay).await;
        let _ = cx.update(|window, cx| {
            if let Some(state) = weak.upgrade() {
                slot(&mut state.borrow_mut()).disarm();
                let now = motion::now(cx);
                fire(&state, now, window, cx);
            }
        });
    });
    slot(&mut state.borrow_mut()).arm(at, task);
}
