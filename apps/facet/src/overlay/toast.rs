//! Toasts: a short, calm stack at the window's bottom right, drawn by the
//! float layer.
//!
//! - **Says it once.** Showing a message that is already up refreshes that
//!   toast's time instead of stacking a copy.
//! - **At most three.** A fourth pushes the oldest out. The newest arrives on
//!   top of the stack, so nothing moves to make room; after one leaves, the
//!   ones above drop into its place on a spring.
//! - **Undo.** A toast may carry one action; pressing it runs the action and
//!   the toast leaves.
//! - **Pause on hover.** While the pointer is on the stack, nothing expires.
//! - The bevel is the only state channel: a toast that reports a fault gets
//!   the coral bevel, a waiting one amber, a done one mint. No dots, no pills.

use super::float::{self, Presence};
use crate::controls::{KbdVoice, keys};
use crate::measure::{Measure, Set};
use crate::motion::{self, Motion, spec};
use crate::paint::{Bevel, Chamfer, cut};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, Voice};
use gpui::{
    AnyElement, App, Global, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, WindowId, div, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How many toasts show at once.
pub const MAX: usize = 3;
/// How long a toast stays by default.
pub const TTL: Duration = Duration::from_millis(5_000);
const ENTER: Duration = Duration::from_millis(380);
const EXIT: Duration = Duration::from_millis(200);

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

/// One toast.
#[derive(Clone)]
pub struct Toast {
    /// What happened, in plain words.
    pub message: SharedString,
    /// The bevel's voice (none: a plain note).
    pub voice: Option<Voice>,
    /// One action (usually "Undo"), and what it does.
    pub action: Option<(SharedString, Action)>,
    /// How long it stays.
    pub ttl: Duration,
}

impl Toast {
    /// A plain note.
    #[must_use]
    pub fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            voice: None,
            action: None,
            ttl: TTL,
        }
    }

    /// Speaks in `voice` (its bevel).
    #[must_use]
    pub const fn voice(mut self, voice: Voice) -> Self {
        self.voice = Some(voice);
        self
    }

    /// With an undo.
    #[must_use]
    pub fn undo(mut self, undo: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.action = Some(("Undo".into(), Rc::new(undo)));
        self
    }
}

/// A toast in the stack.
#[derive(Clone)]
pub struct Entry {
    /// Stable id.
    pub id: u64,
    /// The toast.
    pub toast: Toast,
    /// Entrance and exit.
    pub presence: Presence,
    /// Leaving.
    pub leaving: bool,
    /// Time left while not paused.
    remaining: Duration,
    /// When `remaining` was last measured from (None while paused).
    running_since: Option<Instant>,
}

impl Entry {
    fn expires(&self) -> Option<Instant> {
        self.running_since.map(|since| since + self.remaining)
    }
}

/// The stack: plain data, driven by an explicit clock.
#[derive(Default)]
pub struct Stack {
    entries: Vec<Entry>,
    next: u64,
    paused: bool,
}

impl Stack {
    /// Shows `toast`; a message already up is refreshed instead. Returns
    /// the toast's id.
    pub fn show(&mut self, toast: Toast, now: Instant) -> u64 {
        self.tick(now);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| !entry.leaving && entry.toast.message == toast.message)
        {
            entry.remaining = toast.ttl;
            entry.running_since = (!self.paused).then_some(now);
            entry.toast = toast;
            return entry.id;
        }
        self.next += 1;
        let id = self.next;
        self.entries.push(Entry {
            id,
            remaining: toast.ttl,
            running_since: (!self.paused).then_some(now),
            toast,
            presence: Presence::entering(now, ENTER),
            leaving: false,
        });
        let staying: Vec<u64> = self
            .entries
            .iter()
            .filter(|entry| !entry.leaving)
            .map(|entry| entry.id)
            .collect();
        for id in staying.iter().take(staying.len().saturating_sub(MAX)) {
            self.dismiss(*id, now);
        }
        id
    }

    /// Sends a toast away.
    pub fn dismiss(&mut self, id: u64, now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id && !entry.leaving) {
            entry.leaving = true;
            entry.presence.retarget(0.0, EXIT, now);
        }
    }

    /// The pointer is on the stack (true) or left it: timers pause and resume.
    pub fn hover(&mut self, hovered: bool, now: Instant) {
        if hovered == self.paused {
            return;
        }
        self.paused = hovered;
        for entry in &mut self.entries {
            if hovered {
                if let Some(since) = entry.running_since.take() {
                    entry.remaining = entry.remaining.saturating_sub(now.saturating_duration_since(since));
                }
            } else if !entry.leaving {
                entry.running_since = Some(now);
            }
        }
    }

    /// Expires what ran out and forgets settled exits.
    pub fn tick(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter(|entry| !entry.leaving && entry.expires().is_some_and(|at| at <= now))
            .map(|entry| entry.id)
            .collect();
        for id in expired {
            self.dismiss(id, now);
        }
        self.entries
            .retain(|entry| !entry.leaving || entry.presence.live(now) || entry.presence.value(now) > 0.0);
    }

    /// The next moment something changes on its own.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        self.entries
            .iter()
            .filter_map(|entry| {
                if entry.leaving {
                    Some(entry.presence.ends().max(now))
                } else {
                    entry.expires()
                }
            })
            .min()
    }

    /// The toasts, oldest first (including leaving ones).
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

// ------------------------------------------------------------------ window

struct Toasts {
    stack: Stack,
    timer: Option<(Instant, Task<()>)>,
    motion: Motion,
}

#[derive(Default)]
struct PerWindow(HashMap<WindowId, Rc<RefCell<Toasts>>>);

impl Global for PerWindow {}

fn toasts(window: &Window, cx: &mut App) -> Rc<RefCell<Toasts>> {
    let id = window.window_handle().window_id();
    cx.default_global::<PerWindow>()
        .0
        .entry(id)
        .or_insert_with(|| {
            Rc::new(RefCell::new(Toasts {
                stack: Stack::default(),
                timer: None,
                motion: Motion::new(),
            }))
        })
        .clone()
}

fn changed(state: &Rc<RefCell<Toasts>>, window: &mut Window, cx: &mut App) {
    let now = motion::now(cx);
    let deadline = state.borrow().stack.deadline(now);
    let current = state.borrow().timer.as_ref().map(|(at, _)| *at);
    if deadline != current {
        match deadline {
            None => state.borrow_mut().timer = None,
            Some(at) => {
                let weak = Rc::downgrade(state);
                let task = window.spawn(cx, async move |cx| {
                    cx.background_executor()
                        .timer(at.saturating_duration_since(now))
                        .await;
                    let _ = cx.update(|window, cx| {
                        if let Some(state) = weak.upgrade() {
                            state.borrow_mut().timer = None;
                            let now = motion::now(cx);
                            state.borrow_mut().stack.tick(now);
                            changed(&state, window, cx);
                        }
                    });
                });
                state.borrow_mut().timer = Some((at, task));
            }
        }
    }
    float::refresh(window, cx);
}

/// Shows `toast` in this window. Returns its id.
pub fn show(toast: Toast, window: &mut Window, cx: &mut App) -> u64 {
    let state = toasts(window, cx);
    let now = motion::now(cx);
    let id = state.borrow_mut().stack.show(toast, now);
    changed(&state, window, cx);
    id
}

/// Sends toast `id` away.
pub fn dismiss(id: u64, window: &mut Window, cx: &mut App) {
    let state = toasts(window, cx);
    let now = motion::now(cx);
    state.borrow_mut().stack.dismiss(id, now);
    changed(&state, window, cx);
}

const MESSAGE: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 13.0,
    line: 18.0,
    tracking: 0.0,
    italic: false,
};

const ACTION: TypeRole = TypeRole {
    weight: 500.0,
    ..MESSAGE
};

fn height(measure: &Measure) -> f32 {
    42.0 * measure.scale()
}

/// The stack as the layer draws it: absolutely placed at the bottom right of
/// the layer's viewport-sized box. `None` when there is nothing to show.
pub fn element(measure: &Measure, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let state = toasts(window, cx);
    let now = motion::now(cx);
    state.borrow_mut().stack.tick(now);
    let entries: Vec<Entry> = state.borrow().stack.entries().to_vec();
    if entries.is_empty() {
        return None;
    }
    let palette = cx.facet().palette();
    let scale = measure.scale();
    let row = height(measure);
    let gap = 8.0 * scale;
    let width = px(360.0 * scale).min(measure.width() - px(32.0));
    let motion_store = state.borrow().motion.clone();
    let count = entries.len();
    let mut stack = div()
        .id("toasts")
        .absolute()
        .right(px(16.0 * scale))
        .bottom(px(16.0 * scale))
        .w(width)
        .h(px(count as f32 * (row + gap)))
        .on_hover({
            let state = state.clone();
            move |hovered, window, cx| {
                let now = motion::now(cx);
                state.borrow_mut().stack.hover(*hovered, now);
                changed(&state, window, cx);
            }
        });
    let mut live = false;
    for (slot, entry) in entries.iter().enumerate() {
        // Oldest at the bottom, the newest arrives on top: nothing has to
        // move to make room, and toasts only drop to fill a gap after one
        // has finished leaving.
        let from_bottom = slot as f32;
        let target = from_bottom * (row + gap);
        let y = motion_store.animate(("toast-y", entry.id), target, spec::FOLLOW, window, cx);
        let t = entry.presence.value(now);
        live |= entry.presence.live(now);
        let bevel = match entry.toast.voice {
            Some(Voice::Mint) => Bevel::Hot,
            Some(Voice::Amber) => Bevel::Amber,
            Some(Voice::Coral) => Bevel::Coral,
            Some(Voice::Peri) => Bevel::Peri,
            None => Bevel::Rest,
        };
        let id = entry.id;
        let mut plate = cut()
            .chamfer(Chamfer::Sm)
            .fill(palette.plate3)
            .bevel(bevel)
            .floating()
            .h(px(row))
            .w_full()
            .px(px(14.0 * scale))
            .flex()
            .items_center()
            .gap(px(12.0 * scale))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .set(MESSAGE, measure)
                    .text_color(palette.ink1.hsla())
                    .child(entry.toast.message.clone()),
            );
        if let Some((label, action)) = entry.toast.action.clone() {
            let mut button = div()
                .id(("toast-action", id))
                .flex()
                .items_center()
                .gap(px(6.0 * scale))
                .cursor_pointer()
                .set(ACTION, measure)
                .text_color(palette.peri.base.hsla())
                .child(label)
                .on_click(move |_, window, cx| {
                    action(window, cx);
                    dismiss(id, window, cx);
                });
            if measure.reveal().keys {
                button = button.child(keys(&["⌘", "Z"], KbdVoice::Plain, measure));
            }
            plate = plate.child(button);
        }
        let plate = div()
            .id(("toast", id))
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(y))
            .child(plate)
            .on_click(move |_, window, cx| dismiss(id, window, cx));
        // One composited surface per toast: its bevel never shows through
        // its own plate while it fades.
        stack = stack.child(
            gpui::layer(plate)
                .translate(gpui::point(px(0.0), px((1.0 - t) * 14.0 * scale)))
                .opacity(t),
        );
    }
    if live {
        motion::request_frame(window, cx);
    }
    Some(stack.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{MAX, Stack, Toast};
    use std::time::{Duration, Instant};

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn open(stack: &Stack) -> Vec<String> {
        stack
            .entries()
            .iter()
            .filter(|entry| !entry.leaving)
            .map(|entry| entry.toast.message.to_string())
            .collect()
    }

    #[test]
    fn a_message_is_said_once() {
        let t0 = Instant::now();
        let mut stack = Stack::default();
        let a = stack.show(Toast::new("Pinned"), t0);
        let again = stack.show(Toast::new("Pinned"), t0 + ms(4_000));
        assert_eq!(a, again);
        assert_eq!(open(&stack), ["Pinned"]);
        // The repeat refreshed its time: still up 4.9 s after the repeat.
        stack.tick(t0 + ms(8_900));
        assert_eq!(open(&stack), ["Pinned"]);
        stack.tick(t0 + ms(9_000));
        assert!(open(&stack).is_empty());
    }

    #[test]
    fn a_fourth_pushes_the_oldest_out() {
        let t0 = Instant::now();
        let mut stack = Stack::default();
        for (index, message) in ["a", "b", "c", "d"].into_iter().enumerate() {
            stack.show(Toast::new(message), t0 + ms(index as u64));
        }
        assert_eq!(open(&stack), ["b", "c", "d"]);
        assert_eq!(stack.entries().len(), MAX + 1, "the oldest is still leaving");
        stack.tick(t0 + ms(1_000));
        assert_eq!(stack.entries().len(), MAX);
    }

    #[test]
    fn hovering_pauses_the_clock() {
        let t0 = Instant::now();
        let mut stack = Stack::default();
        stack.show(Toast::new("Saved"), t0);
        stack.hover(true, t0 + ms(1_000));
        stack.tick(t0 + ms(60_000));
        assert_eq!(open(&stack), ["Saved"], "paused while hovered");
        stack.hover(false, t0 + ms(60_000));
        stack.tick(t0 + ms(60_000) + ms(3_999));
        assert_eq!(open(&stack), ["Saved"], "4 s were left when it paused");
        stack.tick(t0 + ms(60_000) + ms(4_000));
        assert!(open(&stack).is_empty());
    }

    #[test]
    fn nothing_is_due_once_every_toast_has_gone() {
        let t0 = Instant::now();
        let mut stack = Stack::default();
        stack.show(Toast::new("x"), t0);
        let mut now = t0;
        while let Some(next) = stack.deadline(now) {
            assert!(next >= now);
            now = next.max(now + ms(1));
            stack.tick(now);
        }
        assert!(stack.entries().is_empty());
    }
}
