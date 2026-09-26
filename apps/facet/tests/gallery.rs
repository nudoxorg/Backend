//! Headless gallery captures through the real macOS text system and renderer.

#![cfg(target_os = "macos")]
#![allow(
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use facet::gallery::{self, Frame, Scene, Shot};
use gpui::{AnyView, App, AppContext as _, Context, IntoElement, ParentElement, Render, Styled, Window, div};
use image::RgbaImage;
use std::sync::{Mutex, MutexGuard, PoisonError};

// The headless macOS platform owns process-global native state; captures in
// one test binary run one at a time.
static PLATFORM: Mutex<()> = Mutex::new(());

fn platform() -> MutexGuard<'static, ()> {
    PLATFORM.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scene(id: &str) -> Scene {
    gallery::find(id).expect("registered scene")
}

fn capture(id: &str, times: &[u64], edit: impl FnOnce(&mut Shot)) -> Vec<Frame> {
    let scene = scene(id);
    let mut shot = Shot::new(&scene);
    shot.times = times.to_vec();
    edit(&mut shot);
    gallery::capture(&scene, &shot).expect("headless capture")
}

fn same(a: &RgbaImage, b: &RgbaImage) -> bool {
    a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw()
}

#[test]
fn repeated_captures_are_byte_identical() {
    let _platform = platform();
    let times = [0, 120, 360];
    let first = capture("motion-drop-in", &times, |_| {});
    let second = capture("motion-drop-in", &times, |_| {});
    assert_eq!(first.len(), 3);
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(a.time_ms, b.time_ms);
        assert!(
            same(&a.image, &b.image),
            "t={} differs between runs",
            a.time_ms
        );
    }
    // And the frames are not trivially equal: the scene really moves.
    assert!(!same(&first[0].image, &first[1].image));
    assert!(!same(&first[1].image, &first[2].image));
}

/// Same scene, same times, played twice, well past the "several hundred ms"
/// virtual span where the coordinator's report (`wave2/float/CHECKPOINT-2.md`
/// `float-edges`) found a run-to-run divergence in a float scene before it
/// was worked around by splitting the scene. `flow-graph` is named in that
/// report as a candidate; this pins both its rasterised frames (all captures
/// use the same fixed 2x test-platform scale, so a hash difference cannot be
/// a scale artefact) and, at the deepest time, motion's own probe clock
/// (`facet::motion::epoch`) read back through a real view — if a wall clock
/// ever leaked into the virtual frame loop, the epoch would drift between
/// the two runs even on ticks where the rasterised pixels still happened to
/// match.
///
/// Reverting `facet::motion::now` from `cx.background_executor().now()`
/// (the `TestClock`, driven only by the harness's own virtual advances) to
/// `std::time::Instant::now()` (real wall time) fails this test:
///
/// ```text
/// thread '...' panicked at apps/facet/tests/gallery.rs:...:
/// t=1830 differs between runs
/// ```
#[test]
fn repeated_captures_stay_byte_identical_past_a_long_virtual_span() {
    let _platform = platform();
    let times = [0, 220, 640, 1140, 1830];
    let first = capture("flow-graph", &times, |_| {});
    let second = capture("flow-graph", &times, |_| {});
    assert_eq!(first.len(), times.len());
    for (a, b) in first.iter().zip(&second) {
        assert_eq!(a.time_ms, b.time_ms);
        assert!(
            same(&a.image, &b.image),
            "t={} differs between runs",
            a.time_ms
        );
    }
    // The scene really is still moving somewhere in this span (otherwise a
    // frozen scene would trivially "pass" this test with no clock driving
    // it at all).
    assert!(
        !same(&first[0].image, &first[4].image),
        "flow-graph render did not change between t=0 and t=1830; this test would not \
         catch a frozen virtual clock"
    );
}

#[test]
fn a_settled_scene_equals_a_fresh_reduced_motion_boot() {
    let _platform = platform();
    let settled = capture("motion-drop-in", &[1_000], |_| {});
    let fresh = capture("motion-drop-in", &[0], |shot| shot.reduced_motion = true);
    assert!(
        same(&settled[0].image, &fresh[0].image),
        "the settled drop-in differs from a fresh boot into the same state"
    );
}

#[test]
fn tracks_settle_on_the_layout_and_then_request_no_frames() {
    let _platform = platform();
    let frames = capture("motion-drop-in", &[0, 120, 700, 900, 1_100], |shot| {
        shot.probe = true;
        shot.scale = 1;
    });
    let at = |time: u64| {
        &frames
            .iter()
            .find(|frame| frame.time_ms == time)
            .expect("frame")
            .ledger
    };
    // Mid-flight the row is painted away from its slot and tracks are live.
    let moving = at(120);
    assert!(moving.any_live());
    let slot = moving.bounds("drop-in.slot-0").expect("slot").y;
    let row = moving.bounds("drop-in.row-0").expect("row").y;
    assert!((row - slot).abs() > 1.0, "row {row} slot {slot}");
    // Every published track stays within its budget and lands on target.
    for time in [700, 900, 1_100] {
        let ledger = at(time);
        assert!(!ledger.any_live(), "live at {time}");
        for track in &ledger.tracks {
            assert!((track.value - track.target).abs() < 1e-4, "{track:?}");
        }
        for index in 0..4 {
            let slot = ledger
                .bounds(&format!("drop-in.slot-{index}"))
                .expect("slot");
            let row = ledger.bounds(&format!("drop-in.row-{index}")).expect("row");
            assert_eq!((row.x, row.y), (slot.x, slot.y), "row {index} at {time}");
        }
    }
    // A settled scene asks for no more frames.
    assert_eq!(at(900).frames_requested, at(700).frames_requested);
    assert_eq!(at(1_100).frames_requested, at(900).frames_requested);
    assert!(at(120).frames_requested > at(0).frames_requested);
}

/// Ink extents of one 64 px proof row at 2x: (left, right, ink mass).
fn row_ink(image: &RgbaImage, row: u32) -> (u32, u32, f64) {
    let (mut left, mut right, mut mass) = (u32::MAX, 0, 0.0);
    for y in row * 128..(row + 1) * 128 {
        for x in 0..image.width() {
            let [r, g, b, _] = image.get_pixel(x, y).0;
            let luma = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
            if luma > 60 {
                left = left.min(x);
                right = right.max(x);
            }
            mass += f64::from(luma.saturating_sub(20)) / 235.0;
        }
    }
    (left, right, mass)
}

/// The horizontal shear that best stands a row's strokes upright.
fn best_shear(image: &RgbaImage, row: u32) -> f64 {
    let centre = f64::from(row * 128 + 64);
    let mut best = (f64::MIN, 0.0);
    for step in -20..=20 {
        let shear = f64::from(step) * 0.02;
        let mut columns = vec![0.0_f64; image.width() as usize + 200];
        for y in row * 128..(row + 1) * 128 {
            for x in 0..image.width() {
                let [r, ..] = image.get_pixel(x, y).0;
                if r > 60 {
                    let moved = f64::from(x) + shear * (f64::from(y) - centre) + 100.0;
                    columns[moved.round().max(0.0) as usize] += f64::from(r);
                }
            }
        }
        let score: f64 = columns.iter().map(|c| c * c).sum();
        if score > best.0 {
            best = (score, shear);
        }
    }
    best.1
}

#[test]
fn cuts_render_the_weights_widths_and_slant_chrome_renders() {
    let _platform = platform();
    let frames = capture("type-proof", &[0], |_| {});
    let image = &frames[0].image;
    // Rows (64 px each): 0 hero, 1 display-xl, 2 display, 3 section,
    // 4 title, 5 book, 6-8 geist 400/500/600, 9-11 mono 400/500/600,
    // 12 serif italic, 13 serif upright. A new row moves these: fail on
    // the layout, not on a misleading width.
    assert_eq!(
        image.height(),
        14 * 128,
        "type-proof no longer has 14 rows; update the row map above"
    );
    let width = |row| {
        let (left, right, _) = row_ink(image, row);
        right - left
    };
    // Reference ink widths, measured on Chrome rendering the variable masters
    // at the board's axes (2x). GPUI must land within 1 %.
    for (row, name, chrome) in [
        (0, "hero 640 wdth 92 opsz 96", 419),
        (1, "display-xl 640 opsz 96", 406),
        (2, "display 620 opsz 96", 409),
        (5, "book 640 wdth 95", 164),
        (6, "geist 400 (kerned)", 504),
        (9, "geist mono, liga off", 644),
        (12, "newsreader italic opsz 19", 769),
    ] {
        let got = width(row);
        assert!(
            (f64::from(got) - f64::from(chrome)).abs() <= f64::from(chrome) * 0.01,
            "{name}: {got} px, Chrome {chrome} px"
        );
    }
    // Geist 400 < 500 < 600 by clear steps of ink.
    let mass = |row| row_ink(image, row).2;
    assert!(
        mass(7) > mass(6) * 1.08,
        "500 {} vs 400 {}",
        mass(7),
        mass(6)
    );
    assert!(
        mass(8) > mass(7) * 1.08,
        "600 {} vs 500 {}",
        mass(8),
        mass(7)
    );
    // The serif italic really slants; the upright does not.
    let italic = best_shear(image, 12);
    let upright = best_shear(image, 13);
    assert!(italic.abs() > 0.12, "italic shear {italic}");
    assert!(upright.abs() < 0.05, "upright shear {upright}");
}

fn bench(times: &[u64], script: Option<&str>) -> Vec<Frame> {
    capture("harness-bench", times, |shot| {
        shot.probe = true;
        shot.scale = 1;
        shot.script = script.map(|source| {
            backend_gui_harness::Script::parse(source).expect("script parses")
        });
    })
}

fn target<'a>(frame: &'a Frame, key: &str) -> &'a facet::probe::TargetSample {
    frame
        .ledger
        .targets
        .iter()
        .find(|target| target.key == key)
        .unwrap_or_else(|| panic!("{key} not published at t={}", frame.time_ms))
}

#[test]
fn scripted_input_is_deterministic_and_reaches_the_scene() {
    let _platform = platform();
    let script = "move 270,114 @0; down 270,114 @100; up 270,114 @140; key cmd-k @200; hold alt @300";
    let times = [0, 120, 180, 260, 320];
    let first = bench(&times, Some(script));
    let second = bench(&times, Some(script));
    for (a, b) in first.iter().zip(&second) {
        assert!(same(&a.image, &b.image), "t={} differs between runs", a.time_ms);
    }
    // The pointer over beta is a hover the scene believes and paints.
    assert!(target(&first[0], "bench.plate-1").state.hovered);
    assert!(!target(&first[0], "bench.plate-0").state.hovered);
    // Held down at 120, released (and focused by the press) at 180.
    assert!(target(&first[1], "bench.plate-1").state.pressed);
    let released = target(&first[2], "bench.plate-1").state;
    assert!(!released.pressed && released.focused, "{released:?}");
    // ⌘K opened a popup: the stack says so and the pixels changed.
    let stack = |frame: &Frame| frame.ledger.stacks.last().map_or(0, |stack| stack.entries.len());
    assert_eq!(stack(&first[2]), 0);
    assert_eq!(stack(&first[3]), 1);
    // ⌥ raised x-ray captions, published as text.
    assert!(
        first[4]
            .ledger
            .texts
            .iter()
            .any(|text| text.content == "beta · 14 uses"),
        "no x-ray caption at t=320"
    );
    // Without the script, the same times paint a different (idle) scene.
    let idle = bench(&times, Some(""));
    assert!(!same(&idle[3].image, &first[3].image));
    assert!(!target(&idle[0], "bench.plate-1").state.hovered);
}

#[test]
fn a_resize_reflows_the_layout_instead_of_cropping_it() {
    let _platform = platform();
    let frames = bench(&[0, 40], Some("resize 400x440 @20"));
    assert_eq!(frames[0].image.dimensions(), (720, 440));
    assert_eq!(frames[1].image.dimensions(), (400, 440));
    assert_eq!(frames[1].drawn.viewport.width, 400);
    // The title stretches across the padded root: it must follow the new
    // width (a crop of the old layout would keep 640).
    let title = |frame: &Frame| {
        frame
            .ledger
            .texts
            .iter()
            .find(|text| text.key == "bench.title")
            .expect("title published")
            .bounds
            .width
    };
    assert_eq!((title(&frames[0]), title(&frames[1])), (640.0, 320.0));
}

/// Every fast check can fail: each canary (the bench with one deliberate
/// defect) fails exactly the stage and check it was built for, and the
/// healthy bench passes those same stages. If a check goes blind, the
/// canary passes and this test names it.
#[test]
fn every_fast_check_catches_its_canary_and_passes_the_healthy_bench() {
    use facet::gallery::verify::{self, EXPECTED, Outcome};
    let _platform = platform();
    let out = std::env::temp_dir().join("facet-canary-test");
    std::fs::create_dir_all(&out).expect("scratch dir");
    let bench = scene("harness-bench");
    let mut missed = Vec::new();
    for (id, stage, check) in EXPECTED {
        if stage == "storm" {
            continue; // storms are minutes long; `facet-gallery verify` runs them
        }
        let caught = verify::stage(&scene(id), stage, &out).expect("stage runs");
        if !caught.failed_checks.iter().any(|failed| failed == check) {
            missed.push(format!("{id}: {stage}/{check} passed ({})", caught.summary));
        }
        let healthy = verify::stage(&bench, stage, &out).expect("stage runs");
        assert_eq!(
            healthy.outcome,
            Outcome::Pass,
            "the healthy bench fails {stage}: {:?}",
            healthy.details
        );
    }
    assert!(missed.is_empty(), "blind checks: {missed:#?}");
}

/// A view that paints plain GPUI text and a plain clickable box, wrapped in
/// neither `facet::probe::text` nor `probe::target` — exactly what an
/// unwired product shell renders today (see `desktop-symbol`).
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child("plain text, never wrapped in probe::text")
    }
}

fn build_blank(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| Blank).into()
}

const BLANK: Scene = Scene {
    id: "test-fixture-blank",
    title: "test fixture: publishes no probe text or targets (never registered in `all`)",
    size: (200, 120),
    build: build_blank,
};

/// A scene that publishes nothing to the probe must report NOT COVERED from
/// every check that depends on it — never PASS. A zero-work PASS is
/// indistinguishable from a healthy scene on the one-screen verify table,
/// which is exactly the defect class this harness exists to catch (see
/// `desktop-symbol`, which is genuinely uninstrumented today).
#[test]
fn a_scene_with_zero_probe_coverage_reports_not_covered_never_pass() {
    use facet::Density;
    use facet::gallery::matrix::Axes;
    use facet::gallery::verify::{self, Outcome};
    use facet::tokens::Appearance;

    let _platform = platform();
    let out = std::env::temp_dir().join("facet-not-covered-test");
    std::fs::create_dir_all(&out).expect("scratch dir");

    let lint_stage = verify::stage(&BLANK, "lint", &out).expect("lint stage runs");
    assert_eq!(
        lint_stage.outcome,
        Outcome::NotCovered,
        "a scene with no probe text or targets must report NOT COVERED from lint, not {:?}: {}",
        lint_stage.outcome,
        lint_stage.summary
    );

    // `verify::stage`'s own gate always compares a motion-on cell with its
    // reduced-motion twin, which is itself real coverage; to exercise the
    // matrix stage's zero-work path (nothing linted *and* no comparison) a
    // single-motion axes has to go through `scene_report` directly.
    let single_cell = Axes {
        widths: vec![200],
        text_scales: vec![100],
        themes: vec![Appearance::Abyss],
        densities: vec![Density::Comfortable],
        motion: vec![true],
    };
    let report = verify::scene_report(&BLANK, 0, Some(&single_cell), &out);
    let matrix_stage = report
        .stages
        .iter()
        .find(|stage| stage.name == "matrix")
        .expect("matrix stage ran");
    assert_eq!(
        matrix_stage.outcome,
        Outcome::NotCovered,
        "a matrix cell with nothing linted and no settled==reduced comparison must report NOT \
         COVERED, not {:?}: {}",
        matrix_stage.outcome,
        matrix_stage.summary
    );
}
