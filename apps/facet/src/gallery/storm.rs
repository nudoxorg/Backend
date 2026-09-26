//! Storms on gallery scenes: a seeded random script (hover sweeps, clicks,
//! presses, key chords, modifier holds, resizes, text-scale/density/theme/
//! contrast/motion flips, many acts per frame), then a neutral tail (buttons
//! and modifiers up, pointer out, Esc x4) and a settle.
//!
//! Per frame: no panic; focus only on an element that is still painted, and
//! at most one element showing focus, on screen; no element showing hover
//! the pointer is not over, or press with no button held; every overlay
//! stack consistent (unique keys, parents below children, one tip at most,
//! cards inside the viewport); the frame's draw within budget; no text
//! clipped without an ellipsis. Over the run: the motion alignment checks
//! ([`super::align`]). At the end: nothing left open but pinned floats,
//! nothing still leaving, zero frames requested, and **settle == fresh**: the
//! settled pixels equal a fresh boot that calmly replays the storm's
//! activations (clicks, keys, holds, resizes, settings; no pointer wandering).
//!
//! A failing storm is shrunk to the shortest script that fails the same
//! check, printed in the script syntax so `capture --input-file` replays it.

use super::align::{self, Observed, Tolerance};
use super::{GalleryError, RootFocus, Scene, Shot};
use crate::probe::StackPhase;
use backend_gui_harness::Script;
use backend_gui_harness::storm::{self, Vocabulary};
use gpui::{App, Global, Window};
use image::RgbaImage;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Duration;

/// A storm's knobs.
#[derive(Clone, Debug, PartialEq)]
pub struct StormConfig {
    /// The seed; with the scene, a complete description of the storm.
    pub seed: u64,
    /// How many acts.
    pub acts: usize,
    /// Over roughly how long.
    pub span_ms: u64,
    /// Settle time after the neutral tail.
    pub settle_ms: u64,
    /// Per-frame draw budget.
    pub budget: Duration,
    /// Compare the settled frame with a fresh boot's calm replay.
    pub fresh: bool,
    /// Spacing of the calm replay's acts.
    pub calm_gap_ms: u64,
    /// Runs the shrinker may spend.
    pub shrink_runs: usize,
}

impl StormConfig {
    /// Defaults for `seed`: 120 acts over 3 s, 1.5 s settle, a 16.7 ms
    /// budget in release (x6 in debug builds), settle == fresh on.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            acts: 120,
            span_ms: 3_000,
            settle_ms: 1_500,
            budget: if cfg!(debug_assertions) {
                Duration::from_micros(16_667 * 6)
            } else {
                Duration::from_micros(16_667)
            },
            fresh: true,
            calm_gap_ms: 450,
            shrink_runs: 48,
        }
    }
}

/// One broken invariant.
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    /// The check (`panic`, `focus`, `hover`, `press`, `stack`, `budget`,
    /// `clip`, a motion check name, `fresh`).
    pub check: &'static str,
    /// When.
    pub at_ms: u64,
    /// Which element, group or layer.
    pub key: String,
    /// What, with operands.
    pub detail: String,
}

/// One storm's result.
#[derive(Clone, Debug)]
pub struct StormRun {
    /// Everything that was played (storm + tail).
    pub script: Script,
    /// Broken invariants, first first.
    pub violations: Vec<Violation>,
    /// Frames drawn.
    pub frames: usize,
    /// The slowest draw.
    pub worst_draw: Duration,
    /// The settled pixels.
    pub settled: Option<RgbaImage>,
    /// The fresh boot's settled pixels (when compared).
    pub fresh: Option<RgbaImage>,
}

impl StormRun {
    /// Whether every invariant held.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// The first violation.
    #[must_use]
    pub fn first(&self) -> Option<&Violation> {
        self.violations.first()
    }
}

#[derive(Default)]
struct DeclaredKeys(Vec<&'static str>);

impl Global for DeclaredKeys {}

#[derive(Default)]
struct Allowed(Vec<&'static str>);

impl Global for Allowed {}

/// Declares motion checks a scene's storms may break by design (call from
/// the scene's build function), e.g. `&["replay"]` for a demo whose click
/// restarts its entrance. The storm report still counts them; they do not
/// fail the storm. Use sparingly: every entry is a hole in the gate.
pub fn allow(checks: &[&'static str], cx: &mut App) {
    cx.set_global(Allowed(checks.to_vec()));
}

/// Declares extra key chords a scene answers to, so storms press them (call
/// from the scene's build function).
pub fn declare_keys(chords: &[&'static str], cx: &mut App) {
    cx.set_global(DeclaredKeys(chords.to_vec()));
}

/// The storm vocabulary for `scene` under `base`: the default mix, aimed at
/// the centres of every target and text the scene publishes, plus the
/// chords it declares.
///
/// # Errors
/// A capture failure.
pub fn vocabulary(scene: &Scene, base: &Shot) -> Result<Vocabulary, GalleryError> {
    let mut shot = base.clone();
    shot.times = vec![0];
    shot.probe = true;
    shot.script = Some(Script::new());
    shot.frame_ms = 0;
    let mut vocabulary = Vocabulary::new(shot.size);
    let mut chords = Vec::new();
    super::run(scene, &shot, &mut |tick, _, cx| {
        for target in &tick.ledger.targets {
            let b = &target.bounds;
            vocabulary
                .targets
                .push((b.x + b.width / 2.0, b.y + b.height / 2.0));
        }
        for text in &tick.ledger.texts {
            let b = &text.bounds;
            vocabulary
                .targets
                .push((b.x + b.width / 2.0, b.y + b.height / 2.0));
        }
        if let Some(declared) = cx.try_global::<DeclaredKeys>() {
            chords.clone_from(&declared.0);
        }
        Ok(())
    })?;
    vocabulary
        .chords
        .extend(chords.into_iter().map(str::to_owned));
    Ok(vocabulary)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

/// Checks one frame's invariants inside the window.
fn frame_checks(
    tick: &super::Tick<'_>,
    budget: Duration,
    window: &mut Window,
    cx: &mut App,
    out: &mut Vec<Violation>,
) {
    let at_ms = tick.drawn.at_ms;
    let viewport = tick.drawn.viewport;
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (viewport.width as f32, viewport.height as f32);
    let mut push = |check: &'static str, key: &str, detail: String| {
        out.push(Violation {
            check,
            at_ms,
            key: key.to_owned(),
            detail,
        });
    };
    // Focus lives on a painted element.
    if window.focused(cx).is_some()
        && let Some(root) = cx.try_global::<RootFocus>()
        && !root.0.contains_focused(window, cx)
    {
        push(
            "focus",
            "window",
            "keyboard focus is on an element that is no longer painted".to_owned(),
        );
    }
    let focused = tick
        .ledger
        .targets
        .iter()
        .filter(|target| target.state.focused)
        .collect::<Vec<_>>();
    if focused.len() > 1 {
        push(
            "focus",
            &focused[1].key,
            format!(
                "{} elements show focus: {}",
                focused.len(),
                focused
                    .iter()
                    .map(|target| target.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    for target in &focused {
        let b = &target.bounds;
        if b.x + b.width <= 0.0 || b.y + b.height <= 0.0 || b.x >= width || b.y >= height {
            push(
                "focus",
                &target.key,
                format!(
                    "focused element at ({:.0}, {:.0}) {:.0}x{:.0} is off a {width:.0}x{height:.0} window",
                    b.x, b.y, b.width, b.height
                ),
            );
        }
    }
    // Hover and press beliefs match the pointer and the buttons.
    for target in &tick.ledger.targets {
        let b = &target.bounds;
        if target.state.hovered {
            let inside = tick.pointer.is_some_and(|(x, y)| {
                x >= b.x - 1.0
                    && x <= b.x + b.width + 1.0
                    && y >= b.y - 1.0
                    && y <= b.y + b.height + 1.0
            });
            if !inside {
                push(
                    "hover",
                    &target.key,
                    format!(
                        "shows hover but the pointer is {} (element at ({:.0}, {:.0}) {:.0}x{:.0})",
                        tick.pointer.map_or_else(
                            || "outside the window".to_owned(),
                            |(x, y)| format!("at ({x:.0}, {y:.0})")
                        ),
                        b.x,
                        b.y,
                        b.width,
                        b.height
                    ),
                );
            }
        }
        if target.state.pressed && !tick.pressed {
            push(
                "press",
                &target.key,
                "shows pressed but no button is held".to_owned(),
            );
        }
    }
    // Overlay stacks are consistent.
    for stack in &tick.ledger.stacks {
        let mut seen: Vec<&str> = Vec::new();
        let mut tips = 0;
        for entry in &stack.entries {
            if seen.contains(&entry.key.as_str()) {
                push(
                    "stack",
                    &entry.key,
                    format!("{} holds `{}` twice", stack.layer, entry.key),
                );
            }
            if let Some(parent) = &entry.parent
                && !seen.contains(&parent.as_str())
            {
                push(
                    "stack",
                    &entry.key,
                    format!(
                        "{}: `{}` is chained from `{parent}`, which is not below it",
                        stack.layer, entry.key
                    ),
                );
            }
            if entry.kind == "tip" && entry.phase != StackPhase::Leaving {
                tips += 1;
            }
            if let Some(b) = &entry.bounds
                && !b.within(width + 0.5, height + 0.5)
            {
                push(
                    "stack",
                    &entry.key,
                    format!(
                        "card at ({:.0}, {:.0}) {:.0}x{:.0} leaves the {width:.0}x{height:.0} window",
                        b.x, b.y, b.width, b.height
                    ),
                );
            }
            seen.push(&entry.key);
        }
        if tips > 1 {
            push(
                "stack",
                &stack.layer,
                format!("{tips} tooltips open at once"),
            );
        }
    }
    // The frame fit its budget.
    if tick.drawn.cpu > budget {
        push(
            "budget",
            "window",
            format!(
                "draw took {:.1} ms (budget {:.1} ms)",
                tick.drawn.cpu.as_secs_f64() * 1000.0,
                budget.as_secs_f64() * 1000.0
            ),
        );
    }
    // No text clipped without an ellipsis.
    for text in &tick.ledger.texts {
        if text.clipped_without_ellipsis() {
            push(
                "clip",
                &text.key,
                format!(
                    "`{}` needs {:.1} px but its box is {:.1} px and it clips without an ellipsis",
                    text.content, text.natural_width, text.bounds.width
                ),
            );
        }
    }
}

/// What one played script produced.
struct Played {
    violations: Vec<Violation>,
    allowed: Vec<&'static str>,
    observed: Vec<Observed>,
    settled: Option<RgbaImage>,
    worst: Duration,
}

/// Plays `script` (already including its tail) on `scene` and checks it.
fn play(scene: &Scene, base: &Shot, script: &Script, config: &StormConfig) -> Played {
    let end = script.end_ms() + config.settle_ms;
    let mut shot = base.clone();
    shot.script = Some(script.clone());
    shot.probe = true;
    shot.times = vec![end];
    shot.until_ms = end;
    if shot.frame_ms == 0 {
        shot.frame_ms = 16;
    }
    let violations = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::new(RefCell::new(Vec::new()));
    let settled = Rc::new(RefCell::new(None));
    let last_at = Rc::new(RefCell::new(0_u64));
    let worst = Rc::new(RefCell::new(Duration::ZERO));
    let allowed = Rc::new(RefCell::new(Vec::new()));
    let budget = config.budget;
    // A storm that panics is a finding, not a crash: keep the message, keep
    // stderr quiet while the shrinker replays it.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(|| {
        super::run(scene, &shot, &mut |tick, window, cx| {
            *last_at.borrow_mut() = tick.drawn.at_ms;
            {
                let mut worst = worst.borrow_mut();
                *worst = (*worst).max(tick.drawn.cpu);
            }
            frame_checks(tick, budget, window, cx, &mut violations.borrow_mut());
            if let Some(allow) = cx.try_global::<Allowed>() {
                allowed.borrow_mut().clone_from(&allow.0);
            }
            observed.borrow_mut().push(Observed {
                drawn: tick.drawn,
                ledger: tick.ledger.clone(),
                events: tick.events.len(),
                state: tick.state.clone(),
            });
            if let Some(image) = tick.image {
                *settled.borrow_mut() = Some(image.clone());
            }
            Ok(())
        })
    }));
    std::panic::set_hook(hook);
    let mut violations = std::mem::take(&mut *violations.borrow_mut());
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => violations.push(Violation {
            check: "panic",
            at_ms: *last_at.borrow(),
            key: "run".to_owned(),
            detail: format!("the run failed: {error}"),
        }),
        Err(payload) => violations.push(Violation {
            check: "panic",
            at_ms: *last_at.borrow(),
            key: "run".to_owned(),
            detail: format!("panicked: {}", panic_message(payload.as_ref())),
        }),
    }
    let observed = std::mem::take(&mut *observed.borrow_mut());
    let settled = settled.borrow_mut().take();
    let worst = *worst.borrow();
    let allowed = allowed.borrow().clone();
    Played {
        violations,
        allowed,
        observed,
        settled,
        worst,
    }
}

/// The storm script for `config` plus its tail (buttons up, modifiers up,
/// pointer out, Esc x4).
#[must_use]
pub fn with_tail(storm: &Script) -> Script {
    let at = storm.end_ms() + 50;
    storm
        .then(&storm::release_buttons(storm, at))
        .then(&storm::neutral_tail(at, 4, 60))
}

/// Checks a played storm: per-frame violations, motion alignment, the end
/// state, and (when asked) settle == fresh.
fn check(scene: &Scene, base: &Shot, storm: &Script, config: &StormConfig) -> StormRun {
    let script = with_tail(storm);
    let played = play(scene, base, &script, config);
    let mut violations = played.violations;
    // A slow frame counts only if the same frame is slow again on a replay
    // of the same script (the machine is shared; one spike is noise).
    if violations
        .iter()
        .any(|violation| violation.check == "budget")
    {
        let again = play(scene, base, &script, config);
        violations.retain(|violation| {
            violation.check != "budget"
                || again
                    .violations
                    .iter()
                    .any(|other| other.check == "budget" && other.at_ms == violation.at_ms)
        });
    }
    let panicked = violations
        .iter()
        .any(|violation| violation.check == "panic");
    if !panicked {
        let alignment = align::analyze(&played.observed, Tolerance::default());
        let allowed = played.allowed.clone();
        violations.extend(
            alignment
                .findings
                .iter()
                .filter(|finding| !allowed.iter().any(|allow| *allow == finding.check.name()))
                .map(|finding| Violation {
                    check: finding.check.name(),
                    at_ms: finding.at_ms,
                    key: finding.key.clone(),
                    detail: finding.detail.clone(),
                }),
        );
        // The end state: only pinned floats remain, nothing still leaving.
        if let Some(last) = played.observed.last() {
            for stack in &last.ledger.stacks {
                for entry in &stack.entries {
                    if entry.phase == StackPhase::Leaving {
                        violations.push(Violation {
                            check: "stack",
                            at_ms: last.drawn.at_ms,
                            key: entry.key.clone(),
                            detail: format!(
                                "still leaving {} ms after the storm settled",
                                config.settle_ms
                            ),
                        });
                    } else if !entry.pinned {
                        violations.push(Violation {
                            check: "stack",
                            at_ms: last.drawn.at_ms,
                            key: entry.key.clone(),
                            detail: "still open after Esc x4 and the pointer leaving".to_owned(),
                        });
                    }
                }
            }
        }
    }
    let mut fresh = None;
    if config.fresh && !panicked && violations.is_empty() {
        let calm = storm::calm(storm, config.calm_gap_ms);
        let calm_script = with_tail(&calm);
        let replay = play(scene, base, &calm_script, config);
        if let Some(error) = replay.violations.iter().find(|v| v.check == "panic") {
            violations.push(Violation {
                check: "fresh",
                at_ms: 0,
                key: "calm replay".to_owned(),
                detail: format!("the calm replay failed: {}", error.detail),
            });
        }
        if let (Some(a), Some(b)) = (&played.settled, &replay.settled) {
            if let Some(detail) = difference(a, b) {
                violations.push(Violation {
                    check: "fresh",
                    at_ms: played.observed.last().map_or(0, |frame| frame.drawn.at_ms),
                    key: "window".to_owned(),
                    detail: format!(
                        "settled pixels differ from a fresh boot calmly replaying the storm's {} activations: {detail}",
                        calm.events.len()
                    ),
                });
            }
        }
        fresh = replay.settled;
    }
    violations.sort_by_key(|violation| violation.at_ms);
    StormRun {
        script,
        violations,
        frames: played.observed.len(),
        worst_draw: played.worst,
        settled: played.settled,
        fresh,
    }
}

/// Where two images differ: size, pixel count and bounding box.
#[must_use]
pub fn difference(a: &RgbaImage, b: &RgbaImage) -> Option<String> {
    if a.dimensions() != b.dimensions() {
        return Some(format!(
            "sizes differ: {:?} vs {:?}",
            a.dimensions(),
            b.dimensions()
        ));
    }
    let (mut count, mut left, mut top, mut right, mut bottom) = (0_u64, u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in a.enumerate_pixels() {
        if pixel != b.get_pixel(x, y) {
            count += 1;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    (count > 0).then(|| {
        format!("{count} px differ inside ({left}, {top})-({right}, {bottom}) physical px")
    })
}

/// One seed's storm, shrunk on failure.
#[derive(Clone, Debug)]
pub struct Report {
    /// The config.
    pub config: StormConfig,
    /// The full run.
    pub run: StormRun,
    /// The shortest failing storm (without its tail), if it failed.
    pub minimal: Option<Script>,
    /// Shrinker runs spent.
    pub shrink_runs: usize,
}

/// Generates, plays and checks one storm; on failure shrinks it to the
/// shortest script that fails the same check.
///
/// # Errors
/// A capture failure outside the storm (the vocabulary probe).
pub fn storm(scene: &Scene, base: &Shot, config: &StormConfig) -> Result<Report, GalleryError> {
    let vocabulary = vocabulary(scene, base)?;
    let script = storm::generate(config.seed, &vocabulary, config.acts, config.span_ms);
    let run = check(scene, base, &script, config);
    let (minimal, runs) = match run.first() {
        Some(first) if config.shrink_runs > 0 => {
            let wanted = first.check;
            let mut small = config.clone();
            // Shrinking one check does not need the others' extra runs.
            small.fresh = wanted == "fresh";
            let (minimal, runs) = storm::shrink(&script, config.shrink_runs, &mut |candidate| {
                check(scene, base, candidate, &small)
                    .violations
                    .iter()
                    .any(|violation| violation.check == wanted)
            });
            (Some(minimal), runs)
        }
        _ => (None, 0),
    };
    Ok(Report {
        config: config.clone(),
        run,
        minimal,
        shrink_runs: runs,
    })
}

/// Replays a given storm script (e.g. a shrunk one) with the same checks.
#[must_use]
pub fn replay(scene: &Scene, base: &Shot, storm: &Script, config: &StormConfig) -> StormRun {
    check(scene, base, storm, config)
}
