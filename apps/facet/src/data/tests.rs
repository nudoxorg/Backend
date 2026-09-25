//! Marks against GPUI's test platform: real pointer and key events through
//! the element tree and the float layer, on the virtual clock.
//!
//! These are the lane's interaction proofs and its storm: every assertion
//! compares what the marks reported against geometry computed independently
//! here (tick slots, stone grid), so a wrong index, a stale hover, a leaked
//! rest or a card that never closes fails the test.

use super::comb::{tick_at as tick_x, tick_near};
use super::door::{self, Rested, part_key};
use super::{Door, Stone, StoneState, Tick, comb, mosaic};
use crate::motion::frames_requested;
use crate::overlay::float;
use crate::theme::ActiveFacet;
use crate::tokens::Family;
use gpui::{
    Context, ElementId, IntoElement, Modifiers, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, div, point, px, size,
};
use std::rc::Rc;
use std::time::Duration;

const COMB_AT: (f32, f32) = (20.0, 20.0);
const MOSAIC_AT: (f32, f32) = (20.0, 140.0);
const WIDTH: f32 = 640.0;
const TICKS: usize = 64;
const STONES: usize = 600;

struct Page {
    ticks: Rc<[Tick]>,
    stones: Rc<[Stone]>,
}

impl Page {
    fn new() -> Self {
        #[allow(clippy::cast_precision_loss)]
        let ticks: Vec<Tick> = (0..TICKS)
            .map(|i| Tick::new(8.0 + (i % 7) as f32 * 3.0).label(format!("1.0.{i}")))
            .collect();
        let stones: Vec<Stone> = (0..STONES)
            .map(|i| {
                Stone::new(
                    [Family::Type, Family::Callable, Family::Value][i % 3],
                    if i % 4 == 0 {
                        StoneState::Public
                    } else {
                        StoneState::Private
                    },
                )
            })
            .collect();
        Self {
            ticks: ticks.into(),
            stones: stones.into(),
        }
    }
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = cx.facet().measure(px(WIDTH));
        let lens = Door::lens(|i, _, _, _| div().child(format!("tick {i}")).into_any_element());
        let peek = Door::peek(|i, _, _, _| div().child(format!("stone {i}")).into_any_element());
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .absolute()
                    .left(px(COMB_AT.0))
                    .top(px(COMB_AT.1))
                    .child(comb("c", self.ticks.clone(), &m).thickness(40.0).door(lens)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(MOSAIC_AT.0))
                    .top(px(MOSAIC_AT.1))
                    .child(mosaic("m", self.stones.clone(), &m).door(peek)),
            )
            .child(float::layer(window, cx))
    }
}

/// One platform frame: next-frame callbacks (the motion gate's), then draw.
fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
    frame(cx);
}

fn move_to(cx: &mut VisualTestContext, x: f32, y: f32) {
    cx.simulate_mouse_move(point(px(x), px(y)), None, Modifiers::none());
}

fn rested(cx: &mut VisualTestContext) -> Vec<(ElementId, Rested)> {
    cx.update(|_, cx| door::rested(cx))
}

fn open(cx: &mut VisualTestContext, mark: &str, part: usize) -> bool {
    let key = part_key(&ElementId::Name(mark.to_owned().into()), part);
    cx.update(|window, cx| float::is_open(&key, window, cx))
}

fn open_count(cx: &mut VisualTestContext) -> usize {
    (0..TICKS).filter(|i| open(cx, "c", *i)).count()
        + (0..STONES).filter(|i| open(cx, "m", *i)).count()
}

/// The tick under window `(x, y)`: the comb's own placement rule (whose
/// inverse is tested in `comb::tests`), applied to the geometry laid out here.
fn tick_at(x: f32, y: f32, comb_h: f32) -> Option<usize> {
    let (cx0, cy0) = COMB_AT;
    if y < cy0 || y >= cy0 + comb_h {
        return None;
    }
    tick_near(x - cx0, TICKS, WIDTH, 2.0)
}

/// The stone under window `(x, y)`: 11 px stones on a 14 px pitch.
fn stone_at(x: f32, y: f32) -> Option<usize> {
    let (mx, my) = MOSAIC_AT;
    let cols = ((WIDTH + 3.0) / 14.0).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cols_n = cols as usize;
    let rows = STONES.div_ceil(cols_n);
    #[allow(clippy::cast_precision_loss)]
    let height = rows as f32 * 14.0 - 3.0;
    if x < mx || y < my || x >= mx + WIDTH || y >= my + height {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (c, r) = (((x - mx) / 14.0) as usize, ((y - my) / 14.0) as usize);
    if c >= cols_n {
        return None;
    }
    let i = r * cols_n + c;
    (i < STONES).then_some(i)
}

fn page(cx: &mut TestAppContext) -> &mut VisualTestContext {
    let (_view, cx) = cx.add_window_view(|_, _| Page::new());
    cx.simulate_resize(size(px(900.0), px(700.0)));
    frame(cx);
    cx
}

fn slot_centre(i: usize) -> f32 {
    COMB_AT.0 + tick_x(i, TICKS, WIDTH, 2.0)
}

#[gpui::test]
fn resting_on_a_tick_reports_it_and_its_lens_opens_after_intent(cx: &mut TestAppContext) {
    let cx = page(cx);
    move_to(cx, slot_centre(10), COMB_AT.1 + 30.0);
    frame(cx);
    let now = rested(cx);
    assert_eq!(now.len(), 1, "{now:?}");
    assert_eq!(now[0].0, ElementId::Name("c".into()));
    assert_eq!(now[0].1.part, 10);
    assert!(!now[0].1.keyboard);
    // Hover intent: nothing yet, then the lens.
    assert!(!open(cx, "c", 10), "a cold rest opened without intent");
    advance(cx, 400);
    assert!(open(cx, "c", 10), "the lens never opened");

    // Sweep to the next tick: the rest moves with it, one card at a time.
    move_to(cx, slot_centre(11), COMB_AT.1 + 30.0);
    advance(cx, 16);
    assert_eq!(rested(cx)[0].1.part, 11);
    advance(cx, 400);
    assert!(open(cx, "c", 11));
    assert_eq!(open_count(cx), 1, "more than one card open after a sweep");

    // Off the mark: the rest ends and the card leaves.
    move_to(cx, 880.0, 690.0);
    advance(cx, 16);
    assert!(rested(cx).is_empty(), "{:?}", rested(cx));
    advance(cx, 600);
    assert_eq!(open_count(cx), 0);
}

#[gpui::test]
fn the_keyboard_walks_parts_and_space_opens_at_once(cx: &mut TestAppContext) {
    let cx = page(cx);
    cx.update(|window, cx| window.focus_next(cx));
    frame(cx);
    cx.simulate_keystrokes("right right right");
    frame(cx);
    cx.simulate_keystrokes("space");
    frame(cx);
    let now = rested(cx);
    assert_eq!(now.len(), 1, "{now:?}");
    assert_eq!(now[0].1.part, 2, "walk landed on the wrong tick");
    assert!(now[0].1.keyboard);
    assert!(open(cx, "c", 2), "Space must open without hover intent");
    // The open card follows the walk.
    cx.simulate_keystrokes("right");
    frame(cx);
    assert_eq!(rested(cx)[0].1.part, 3);
    assert!(open(cx, "c", 3));
    advance(cx, 400);
    assert_eq!(open_count(cx), 1);
    // Esc closes it.
    cx.update(|window, cx| window.focus_next(cx));
    cx.update(|window, cx| window.focus_prev(cx));
    cx.simulate_keystrokes("escape");
    advance(cx, 600);
    assert!(rested(cx).is_empty(), "{:?}", rested(cx));
}

/// A tiny deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        #[allow(clippy::cast_precision_loss)]
        let v = (self.0 >> 40) as f32 / (1u64 << 24) as f32;
        v
    }
}

#[gpui::test]
fn storm_of_hover_sweeps_and_resizes_keeps_every_report_true(cx: &mut TestAppContext) {
    let cx = page(cx);
    let mut rng = Lcg(0x5eed);
    // The comb's hitbox height at the card rung (field 40 + axis).
    let comb_h = cx.update(|_, cx| {
        let m = cx.facet().measure(px(WIDTH));
        40.0 + 8.0 + m.role(crate::tokens::TypeRole { size: 12.5, line: 16.0, ..crate::tokens::ty::SMALL }).line
    });
    let (mut x, mut y) = (0.0_f32, 0.0_f32);
    for step in 0..600 {
        // Many events per frame: 1–6 moves between draws.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let moves = 1 + (rng.next() * 6.0) as usize;
        for _ in 0..moves {
            // Mostly over the marks, sometimes off them.
            x = rng.next() * 700.0;
            y = if rng.next() < 0.5 {
                COMB_AT.1 - 10.0 + rng.next() * (comb_h + 20.0)
            } else {
                MOSAIC_AT.1 - 10.0 + rng.next() * 460.0
            };
            move_to(cx, x, y);
        }
        if step % 37 == 0 {
            #[allow(clippy::cast_possible_truncation)]
            let w = 700.0 + (rng.next() * 900.0).round();
            cx.simulate_resize(size(px(w), px(640.0 + rng.next() * 300.0)));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        advance(cx, (rng.next() * 60.0) as u64);

        // Every report matches the geometry under the pointer.
        let expected: Vec<(String, usize)> = [
            tick_at(x, y, comb_h).map(|i| ("c".to_owned(), i)),
            stone_at(x, y).map(|i| ("m".to_owned(), i)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let got: Vec<(String, usize)> = rested(cx)
            .into_iter()
            .map(|(mark, r)| (mark.to_string(), r.part))
            .collect();
        assert_eq!(got, expected, "step {step} at ({x}, {y})");
        assert!(open_count(cx) <= 1, "step {step}: {} cards open", open_count(cx));
    }

    // Leave, settle: nothing rested, nothing open, and the window goes idle.
    move_to(cx, 5.0, 690.0);
    advance(cx, 2_000);
    assert!(rested(cx).is_empty());
    assert_eq!(open_count(cx), 0);
    let before = cx.update(|_, cx| frames_requested(cx));
    for _ in 0..5 {
        advance(cx, 100);
    }
    let after = cx.update(|_, cx| frames_requested(cx));
    assert_eq!(before, after, "the marks kept asking for frames after settling");
}

/// 20 000 stones, most of them outside the window.
struct Vast {
    stones: Rc<[Stone]>,
    regions: Rc<[super::Region]>,
}

impl Render for Vast {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .child(mosaic("vast-mosaic", self.stones.clone(), &facet.measure(px(640.0)))),
            )
            .child(
                div().absolute().left(px(0.0)).top(px(0.0)).child(
                    super::territory("vast-map", self.regions.clone(), &facet.measure(px(4400.0)))
                        .canvas(3000.0),
                ),
            )
    }
}

#[gpui::test]
fn paint_follows_what_is_visible_not_what_exists(cx: &mut TestAppContext) {
    let stones: Vec<Stone> = (0..20_000)
        .map(|i| Stone::new(Family::Type, if i % 3 == 0 { StoneState::Public } else { StoneState::Private }))
        .collect();
    let regions: Vec<super::Region> = (0..400).map(|i| super::Region::new(format!("m{i}"), 50)).collect();
    let (_view, cx) = cx.add_window_view(|_, _| Vast {
        stones: stones.into(),
        regions: regions.into(),
    });
    cx.simulate_resize(size(px(800.0), px(900.0)));
    frame(cx);
    cx.update(|_, cx| super::take_painted(cx));
    frame(cx);
    let painted = cx.update(|_, cx| super::take_painted(cx));
    println!("painted this frame: {painted:?} (of 40 000 stones, 400 regions)");
    // Mosaic: 45 columns of 14 px; the 900 px window shows 65 rows (+ one
    // row of slack each side), not the 445 that exist.
    let mosaic_visible = (900 / 14 + 3) * 45;
    // Territory: a 4400 × 3000 canvas of 20 000 stones; the window shows
    // 800 × 900 of it, about 5 %.
    assert!(
        painted.stones <= (mosaic_visible + 2_500) as u64,
        "painted {} stones of 40 000 (mosaic budget {mosaic_visible})",
        painted.stones
    );
    assert!(painted.stones >= 2_000, "painted too few: {}", painted.stones);
    assert!(painted.regions < 60, "drew {} of 400 regions", painted.regions);
    assert!(painted.regions >= 5);
}

/// The harness storm (seeded hover sweeps, clicks, keys, holds, resizes and
/// setting flips, many acts per frame) over the dense marks. Every finding
/// must be one of two classes owned elsewhere, and both are quoted when the
/// test runs: the float layer's own tracks, and a spring's last sub-pixel
/// step to rest (the motion store rests a spring at 1/1000 of its travel).
/// Anything else a data mark does wrong fails here.
#[cfg(feature = "gallery")]
#[test]
fn harness_storms_over_the_dense_marks_find_nothing_of_ours() {
    use crate::gallery::storm::{StormConfig, storm};
    use crate::gallery::{Shot, find};
    for id in ["data-dense", "data-20k-map"] {
        let scene = find(id).expect("scene registered");
        let mut shot = Shot::new(&scene);
        shot.scale = 1;
        for seed in 1..=3 {
            let report = storm(&scene, &shot, &StormConfig::new(seed)).expect("storm ran");
            let mut ours = Vec::new();
            let mut elsewhere = 0;
            for v in &report.run.violations {
                let snap = v.check == "continuity"
                    && v.detail
                        .split_whitespace()
                        .nth(1)
                        .and_then(|jump| jump.parse::<f32>().ok())
                        .is_some_and(|jump| jump < 0.5);
                if v.key.starts_with("float") || snap {
                    elsewhere += 1;
                } else {
                    ours.push(format!("{} {} ms {}: {}", v.check, v.at_ms, v.key, v.detail));
                }
            }
            println!(
                "{id} seed {seed}: {} frames, worst draw {:.1} ms, {} findings elsewhere, {} ours",
                report.run.frames,
                report.run.worst_draw.as_secs_f64() * 1000.0,
                elsewhere,
                ours.len()
            );
            assert!(ours.is_empty(), "{id} seed {seed}:\n{}", ours.join("\n"));
        }
    }
}
