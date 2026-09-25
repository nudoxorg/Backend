//! Engine tests against GPUI's test platform and its virtual clock.

use super::keys::{self, Pose};
use super::{Motion, Spec, frames_requested, pulse};
use crate::theme::{Facet, set_facet};
use crate::tokens::motion::GLIDE;
use gpui::{
    Animation, AnimationExt, Context, IntoElement, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, div,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// One platform frame: run the next-frame callbacks (the gate's), then draw.
fn frame(cx: &mut VisualTestContext) -> usize {
    cx.update(|window, cx| {
        let callbacks = window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear(cx);
        callbacks
    })
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}

struct Tweened {
    seen: Rc<Cell<f32>>,
}

impl Render for Tweened {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let seen = Rc::clone(&self.seen);
        div().size_full().child(div().size_4().with_animation(
            "tweened",
            Animation::new(Duration::from_millis(400)),
            move |element, delta| {
                seen.set(delta);
                element
            },
        ))
    }
}

#[gpui::test]
fn with_animation_advances_only_with_the_virtual_clock(cx: &mut TestAppContext) {
    let seen = Rc::new(Cell::new(-1.0));
    let (_view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Tweened { seen }
    });
    frame(cx);
    assert!(seen.get().abs() < 1e-6, "first frame at {}", seen.get());
    // Real time passing moves nothing: the patched element reads the executor clock.
    std::thread::sleep(Duration::from_millis(40));
    frame(cx);
    assert!(
        seen.get().abs() < 1e-6,
        "wall clock leaked in: {}",
        seen.get()
    );
    advance(cx, 100);
    frame(cx);
    assert!(
        (seen.get() - 0.25).abs() < 1e-4,
        "at 100 of 400 ms: {}",
        seen.get()
    );
    advance(cx, 400);
    frame(cx);
    assert!((seen.get() - 1.0).abs() < 1e-6);
}

struct Mover {
    motion: Motion,
    target: f32,
    seen: Rc<Cell<f32>>,
}

impl Render for Mover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.motion.animate(
            "x",
            self.target,
            Spec::tween(Duration::from_millis(240), GLIDE),
            window,
            cx,
        );
        self.seen.set(value);
        div().size_full()
    }
}

#[gpui::test]
fn a_settled_store_requests_no_further_frames(cx: &mut TestAppContext) {
    let seen = Rc::new(Cell::new(f32::NAN));
    let (view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Mover {
            motion: Motion::new(),
            target: 0.0,
            seen,
        }
    });
    frame(cx);
    // A first sighting is not animated and schedules nothing.
    assert_eq!(cx.update(|_, cx| frames_requested(cx)), 0);
    assert_eq!(frame(cx), 0);

    view.update(cx, |mover, cx| {
        mover.target = 100.0;
        cx.notify();
    });
    frame(cx);
    // The test platform also draws a notified window on `update`, so the
    // retarget has been rendered once or twice by now; each asked for a frame.
    let mut requested = cx.update(|_, cx| frames_requested(cx));
    assert!((1..=2).contains(&requested), "requested {requested}");
    let mut previous = seen.get();
    for _ in 0..6 {
        advance(cx, 40);
        assert_eq!(
            frame(cx),
            1,
            "the gate schedules exactly one callback per frame"
        );
        assert!(
            seen.get() >= previous,
            "monotone under glide: {previous} -> {}",
            seen.get()
        );
        previous = seen.get();
    }
    advance(cx, 40);
    frame(cx);
    assert_eq!(
        seen.get().to_bits(),
        100.0_f32.to_bits(),
        "settles exactly on target"
    );
    requested = cx.update(|_, cx| frames_requested(cx)).max(requested);
    for _ in 0..5 {
        advance(cx, 40);
        assert_eq!(
            frame(cx),
            0,
            "a settled store leaves no next-frame callback"
        );
    }
    assert_eq!(cx.update(|_, cx| frames_requested(cx)), requested);
}

#[gpui::test]
fn reduced_motion_snaps_and_schedules_nothing(cx: &mut TestAppContext) {
    cx.update(|cx| {
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
    });
    let seen = Rc::new(Cell::new(f32::NAN));
    let (view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Mover {
            motion: Motion::new(),
            target: 0.0,
            seen,
        }
    });
    frame(cx);
    view.update(cx, |mover, cx| {
        mover.target = 50.0;
        cx.notify();
    });
    frame(cx);
    assert_eq!(seen.get().to_bits(), 50.0_f32.to_bits());
    assert_eq!(cx.update(|_, cx| frames_requested(cx)), 0);
}

struct Ambient {
    on: bool,
    seen: Rc<Cell<f32>>,
}

impl Render for Ambient {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.on {
            self.seen.set(pulse::lease(window, cx).seconds);
        }
        div().size_full()
    }
}

#[gpui::test]
fn the_pulse_ticks_while_leased_and_lets_go_when_unleased(cx: &mut TestAppContext) {
    let seen = Rc::new(Cell::new(f32::NAN));
    let (view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Ambient { on: true, seen }
    });
    frame(cx);
    assert!(cx.update(|_, cx| pulse::running(cx)));
    for _ in 0..4 {
        advance(cx, 84);
        frame(cx);
    }
    let ticks = cx.update(|_, cx| pulse::ticks(cx));
    assert!(ticks >= 4, "ticked {ticks} times");
    // Quantised to whole ticks of 1/12 s.
    let seconds = seen.get();
    let tick = pulse::TICK.as_secs_f32();
    assert!(
        ((seconds / tick) - (seconds / tick).round()).abs() < 1e-3,
        "{seconds}"
    );
    assert!(seconds >= 3.0 * tick, "{seconds}");

    view.update(cx, |ambient, cx| {
        ambient.on = false;
        cx.notify();
    });
    frame(cx);
    advance(cx, 500);
    assert!(
        !cx.update(|_, cx| pulse::running(cx)),
        "the lease lapsed but the timer runs"
    );
    let stopped = cx.update(|_, cx| pulse::ticks(cx));
    advance(cx, 2_000);
    assert_eq!(
        cx.update(|_, cx| pulse::ticks(cx)),
        stopped,
        "ticks after release"
    );
    assert_eq!(cx.update(|_, cx| pulse::leases(cx)), 0);
}

struct Exiting {
    motion: Motion,
    seen: Rc<Cell<Pose>>,
}

impl Render for Exiting {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pose = self.motion.play("row-7.exit", &keys::DROP_IN, window, cx);
        self.seen.set(pose);
        div().size_full()
    }
}

#[gpui::test]
fn an_exit_does_not_replay_when_its_view_is_rebuilt(cx: &mut TestAppContext) {
    let seen = Rc::new(Cell::new(Pose::REST));
    let (_first, vcx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, cx| Exiting {
            motion: Motion::scoped("list", cx),
            seen,
        }
    });
    frame(vcx);
    let start = seen.get();
    advance(vcx, 200);
    frame(vcx);
    let mid = seen.get();
    assert_ne!(start, mid);
    // Rebuild the view from scratch: same scope, fresh entity and element tree.
    let (_second, vcx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, cx| Exiting {
            motion: Motion::scoped("list", cx),
            seen,
        }
    });
    frame(vcx);
    assert_eq!(seen.get(), mid, "the rebuilt view continued, not replayed");
    advance(vcx, 1_000);
    frame(vcx);
    assert!(seen.get().is_rest());
}
