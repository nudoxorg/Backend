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

// --- FLIP continuity (flow-list, flow-reflow) --------------------------------

/// The probe's continuity rule as `gallery::align` applies it, restated so
/// these tests run without the gallery feature: between consecutive samples
/// of one key the value may move at most `1.5 × speed × dt + 2 % of the
/// distance to target + 1e-3`, where `speed` is the larger velocity of the
/// two samples inside one segment and the earlier sample's across a
/// retarget. Two samples at one instant in one segment (or both at rest)
/// count once, the later. Returns every violation of keys under `prefix`.
pub(super) fn continuity(ledgers: &[crate::probe::Ledger], prefix: &str) -> Vec<String> {
    use crate::probe::{TrackKind, TrackSample};
    use std::collections::BTreeMap;
    let at_rest = |s: &TrackSample| !s.live && s.budget_ms <= 0.0;
    let same = |a: &TrackSample, b: &TrackSample| {
        !at_rest(a) && !at_rest(b) && (a.started_ms - b.started_ms).abs() < 1e-6
    };
    let mut keys: BTreeMap<&str, Vec<&TrackSample>> = BTreeMap::new();
    for ledger in ledgers {
        for sample in &ledger.tracks {
            if sample.kind == TrackKind::Pulse || !sample.key.starts_with(prefix) {
                continue;
            }
            let sequence = keys.entry(sample.key.as_str()).or_default();
            match sequence.last_mut() {
                Some(last)
                    if (last.at_ms - sample.at_ms).abs() < 1e-6
                        && (same(last, sample) || (at_rest(last) && at_rest(sample))) =>
                {
                    *last = sample;
                }
                _ => sequence.push(sample),
            }
        }
    }
    let mut out = Vec::new();
    for (key, sequence) in keys {
        for pair in sequence.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            #[allow(clippy::cast_possible_truncation)]
            let dt = ((b.at_ms - a.at_ms) / 1000.0).max(0.0) as f32;
            let speed = if same(a, b) {
                a.velocity.abs().max(b.velocity.abs())
            } else {
                a.velocity.abs()
            };
            let scale = (a.target - a.value).abs().max((b.target - a.value).abs());
            let allowed = 1.5 * speed * dt + 0.02 * scale + 1e-3;
            let step = (b.value - a.value).abs();
            if step > allowed {
                out.push(format!(
                    "{key} @{:.0} ms: jumped {step:.3} ({:.3} -> {:.3}) in {:.0} ms; velocity {speed:.2}/s allows {allowed:.3}",
                    b.at_ms,
                    a.value,
                    b.value,
                    dt * 1000.0
                ));
            }
        }
    }
    out
}

/// One frame with the probe on: its ledger.
fn probed(cx: &mut VisualTestContext) -> crate::probe::Ledger {
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear(cx);
        crate::probe::take(cx)
    })
}

/// Rows in presence slots, each a flow item: a neighbour arriving or leaving
/// opens or closes room while no flow epoch changes (flow-list at 700 ms).
struct Pushed {
    presence: super::Presence,
    flow: super::Flow,
    rows: Vec<u64>,
    version: u64,
}

impl Render for Pushed {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{ElementId, px};
        self.flow.epoch(self.version);
        let items = self.presence.sync(
            self.rows.iter().map(|&row| ElementId::Integer(row)),
            window,
            cx,
        );
        div().flex().flex_col().children(items.into_iter().map(|item| {
            let key = item.key.clone();
            item.slot(div().pb(px(10.0)).child(self.flow.item(
                key.clone(),
                crate::probe::measure(
                    ElementId::Name(format!("pushed.row.{key}").into()),
                    div().w(px(120.0)).h(px(40.0)),
                ),
            )))
        }))
    }
}

/// Frame intervals like a harness run: the 16 ms loop with captures between.
const CADENCE: [u64; 5] = [16, 4, 12, 16, 8];

#[gpui::test]
fn rows_carried_by_a_neighbours_room_move_continuously(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Pushed {
        presence: super::Presence::new("pushed.presence").enter(super::act::RISE),
        flow: super::Flow::new("pushed.flip"),
        rows: vec![1, 2, 3],
        version: 0,
    });
    cx.update(|_, cx| {
        crate::probe::enable(cx);
        super::reset_epoch(cx);
    });
    let mut ledgers = vec![probed(cx)];
    let run = |cx: &mut VisualTestContext, ledgers: &mut Vec<crate::probe::Ledger>, frames: usize| {
        for step in 0..frames {
            advance(cx, CADENCE[step % CADENCE.len()]);
            ledgers.push(probed(cx));
        }
    };
    // A reorder (an epoch) first, as in flow-list: the rows flow, settle and
    // report their rest, so what follows is judged against that rest.
    view.update(cx, |pushed, cx| {
        pushed.rows = vec![3, 1, 2];
        pushed.version += 1;
        cx.notify();
    });
    ledgers.push(probed(cx));
    run(cx, &mut ledgers, 70);
    let clock = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            super::now(cx)
                .saturating_duration_since(super::epoch(cx))
                .as_secs_f64()
                * 1000.0
        })
    };
    // A row arrives at the top: its room opens and pushes the others down.
    let inserted = clock(cx);
    view.update(cx, |pushed, cx| {
        pushed.rows = vec![0, 3, 1, 2];
        cx.notify();
    });
    ledgers.push(probed(cx));
    run(cx, &mut ledgers, 60);
    // One leaves from the middle: the room closes and pulls the rows below
    // it up.
    let removed = clock(cx);
    view.update(cx, |pushed, cx| {
        pushed.rows = vec![0, 1, 2];
        cx.notify();
    });
    ledgers.push(probed(cx));
    run(cx, &mut ledgers, 80);
    let jumps = continuity(&ledgers, "pushed.flip.");
    assert!(jumps.is_empty(), "{} jumps:\n{}", jumps.len(), jumps.join("\n"));
    // Carried, not sprung. The arrival's room announces its speed on the
    // frame it starts, so the rows below ride it exactly (a pixel-snap step
    // is smoothed, never more than one device pixel behind).
    let lag = |from: f64, to: f64| {
        ledgers
            .iter()
            .flat_map(|ledger| &ledger.tracks)
            .filter(|sample| sample.key.starts_with("pushed.flip.") && sample.key.ends_with(".y"))
            .filter(|sample| sample.at_ms >= from && sample.at_ms < to)
            .map(|sample| (sample.value - sample.target).abs())
            .fold(0.0_f32, f32::max)
    };
    assert!(
        ledgers.iter().flat_map(|ledger| &ledger.tracks).any(|sample| sample.key.starts_with("pushed.flip.2.")),
        "carried rows publish their motion"
    );
    assert!(lag(inserted, removed) <= 0.5, "rows lagged the opening room by {}", lag(inserted, removed));
    // The leaver's room holds, then starts closing at full speed between two
    // frames (a keyframe corner nothing announced ahead): the rows below
    // absorb that one frame's step and catch up, never more than the room
    // closed in one frame behind (a slot is 50 px: 40 + 10 of gap).
    let room = ledgers
        .iter()
        .flat_map(|ledger| &ledger.tracks)
        .filter(|sample| sample.key == "pushed.presence.3.room")
        .map(|sample| sample.value * 50.0)
        .collect::<Vec<_>>();
    let step = room.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0_f32, f32::max);
    assert!(
        lag(removed, f64::MAX) <= step + 0.5,
        "rows lagged the closing room by {} (its largest frame step {step})",
        lag(removed, f64::MAX)
    );
    assert!(view.read_with(cx, |pushed, cx| pushed.flow.is_settled(cx) && pushed.presence.is_settled(cx)));
}

/// Cards laid out for a width that is dragged: continuous in the width,
/// except where the column count changes (undeclared) and where the class
/// changes (declared: the epoch token), which also opens a margin.
struct Dragged {
    flow: super::Flow,
    width: f32,
}

const CLASS_AT: f32 = 520.0;

fn card_at(width: f32, index: usize) -> (f32, f32, f32) {
    let margin = if width >= CLASS_AT { 100.0 } else { 0.0 };
    let room = width - margin;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let columns = (((room + 10.0) / 100.0).floor() as usize).clamp(1, 4);
    #[allow(clippy::cast_precision_loss)]
    let card = (room - 10.0 * (columns - 1) as f32) / columns as f32;
    #[allow(clippy::cast_precision_loss)]
    let (column, line) = ((index % columns) as f32, (index / columns) as f32);
    (margin + column * (card + 10.0), line * 50.0, card)
}

impl Render for Dragged {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{ElementId, px};
        self.flow.epoch(self.width >= CLASS_AT);
        let width = self.width;
        div().relative().w(px(width)).h(px(300.0)).children((0..6).map(|index| {
            let (x, y, card) = card_at(width, index);
            div().absolute().left(px(x)).top(px(y)).child(self.flow.item(
                ElementId::Integer(index as u64),
                crate::probe::measure(
                    ElementId::Name(format!("dragged.card.{index}").into()),
                    div().w(px(card)).h(px(40.0)),
                ),
            ))
        }))
    }
}

#[gpui::test]
fn a_class_change_mid_drag_springs_from_the_painted_position_and_keeps_following(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_, _| Dragged {
        flow: super::Flow::new("dragged"),
        width: 300.0,
    });
    cx.update(|_, cx| {
        crate::probe::enable(cx);
        super::reset_epoch(cx);
    });
    let x = |ledger: &crate::probe::Ledger, index: usize| {
        ledger
            .bounds(&format!("dragged.card.{index}"))
            .expect("card")
            .x
    };
    let mut ledgers = vec![probed(cx)];
    for _ in 0..4 {
        advance(cx, 16);
        ledgers.push(probed(cx));
    }
    // Dragged out at 375 px/s, one frame at a time: across three column
    // changes (320, 430, 540; undeclared) and the class change (520).
    let mut width = 300.0;
    let mut class_frame = None;
    while width < 660.0 {
        width += 6.0;
        advance(cx, 16);
        view.update(cx, |dragged, cx| {
            dragged.width = width;
            cx.notify();
        });
        if class_frame.is_none() && width >= CLASS_AT {
            class_frame = Some(ledgers.len());
        }
        ledgers.push(probed(cx));
    }
    let class_frame = class_frame.expect("crossed the class");
    // Card 0 stays in column 0: before the class change it is carried by
    // nothing (x = 0); card 1 rides the drag. On the class frame card 1's
    // layout jumps 100 px right with the margin; painted, it keeps the drag's
    // pace instead of stalling or jumping.
    let before = x(&ledgers[class_frame - 1], 1) - x(&ledgers[class_frame - 2], 1);
    let during = x(&ledgers[class_frame], 1) - x(&ledgers[class_frame - 1], 1);
    assert!(before > 1.0, "card 1 was following the drag: {before}");
    assert!(
        (during - before).abs() <= 1.0,
        "the class change kept the drag-follow: {before} then {during}"
    );
    let jumps = continuity(&ledgers, "dragged.");
    assert!(jumps.is_empty(), "{} jumps:\n{}", jumps.len(), jumps.join("\n"));
    // Released: it lands exactly on layout and stops asking for frames.
    for _ in 0..80 {
        advance(cx, 16);
        ledgers.push(probed(cx));
    }
    let last = ledgers.last().expect("frames");
    for index in 0..6 {
        let (want, _, _) = card_at(width, index);
        // Layout lands on the device-pixel grid (0.5 px here).
        assert!((x(last, index) - want).abs() <= 0.5, "card {index} settled on layout: {} vs {want}", x(last, index));
    }
    assert!(view.read_with(cx, |dragged, cx| dragged.flow.is_settled(cx)));
    assert_eq!(frame(cx), 0, "a settled flow requests no frames");
}
