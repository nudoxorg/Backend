//! Motion alignment: reads the probe ledger of every drawn frame of a run and
//! checks that the animations line up.
//!
//! | Check | Holds when |
//! |---|---|
//! | continuity | no track moves further between two frames than its velocity allows (a retarget starts where the old motion was) |
//! | overshoot | a segment stays within `[from, target]` widened by its own trajectory's relative and absolute bounds |
//! | settle | a segment stops being live within its budget (plus one frame) |
//! | slot | once nothing moves, every `paint:K` bounds equal their `slot:K` bounds (the settled value is the laid-out position) |
//! | lockstep | tracks tagged with one [`crate::probe::grouped`] group are at the same normalised progress in every frame |
//! | idle | after the last input and the last live track, no frame is requested (ambient pulse frames excepted, at most 12 fps) |
//! | unrequested | nothing changes value or bounds in a frame nobody asked for (a real window would not have drawn it: the motion would freeze) |
//!
//! Every check reports how much it evaluated, so a check that saw nothing
//! reads `n/a`, never `ok`.

use super::json::Json;
use crate::probe::{BoundsSample, Ledger, TrackKind, TrackSample};
use backend_gui_harness::Drawn;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One drawn frame of a run.
#[derive(Clone, Debug)]
pub struct Observed {
    /// What the frame carried.
    pub drawn: Drawn,
    /// What the probe saw.
    pub ledger: Ledger,
    /// How many acts were delivered at this instant.
    pub events: usize,
    /// Report-only state from the scene's declared sampler.
    pub state: Option<Json>,
}

/// A motion check.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Check {
    /// No jumps between frames.
    Continuity,
    /// Overshoot within the segment's own allowance.
    Overshoot,
    /// Settles within budget.
    Settle,
    /// Settled paint equals the laid-out slot.
    Slot,
    /// Grouped tracks move together.
    Lockstep,
    /// Zero frames after settle.
    Idle,
    /// No motion in unrequested frames.
    Unrequested,
    /// A one-shot keyframe track restarted from its first frame while painted.
    Replay,
}

impl Check {
    /// Every check, in report order.
    pub const ALL: [Self; 8] = [
        Self::Replay,
        Self::Continuity,
        Self::Overshoot,
        Self::Settle,
        Self::Slot,
        Self::Lockstep,
        Self::Idle,
        Self::Unrequested,
    ];

    /// A stable name for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Continuity => "continuity",
            Self::Overshoot => "overshoot",
            Self::Settle => "settle",
            Self::Slot => "slot",
            Self::Lockstep => "lockstep",
            Self::Idle => "idle",
            Self::Unrequested => "unrequested",
            Self::Replay => "replay",
        }
    }
}

/// One failed expectation.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// Which check.
    pub check: Check,
    /// The track, group or bounds key.
    pub key: String,
    /// When (virtual ms).
    pub at_ms: u64,
    /// What happened, with the operands.
    pub detail: String,
}

/// How much a check evaluated and how close it came to failing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    /// Comparisons made.
    pub evaluated: usize,
    /// Worst observed / allowed (1.0 = at the limit).
    pub worst: f32,
    /// Where the worst ratio was seen.
    pub worst_at: Option<(String, u64)>,
    /// Comparisons skipped, with why (e.g. spring momentum).
    pub skipped: usize,
}

impl Stat {
    fn see(&mut self, ratio: f32, key: &str, at_ms: u64) {
        self.evaluated += 1;
        if ratio.is_finite() && ratio > self.worst {
            self.worst = ratio;
            self.worst_at = Some((key.to_owned(), at_ms));
        }
    }
}

/// Tolerances (the defaults are what `verify` uses).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    /// Allowed step = `slack` x |velocity| x dt + the terms below.
    pub slack: f32,
    /// Relative slack, as a fraction of the segment's span.
    pub relative: f32,
    /// Absolute slack in track units.
    pub absolute: f32,
    /// Slot/paint difference allowed, in logical px.
    pub slot_px: f32,
    /// Normalised progress spread allowed within a group.
    pub lockstep: f32,
    /// Ambient (pulse) frames per second allowed while idle.
    pub ambient_fps: f32,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            slack: 1.5,
            relative: 0.02,
            absolute: 1e-3,
            slot_px: 0.5,
            lockstep: 0.05,
            ambient_fps: 12.5,
        }
    }
}

/// One track key over the run.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSummary {
    /// The key.
    pub key: String,
    /// The engine.
    pub kind: TrackKind,
    /// Samples seen.
    pub samples: usize,
    /// Distinct segments (retargets + 1).
    pub segments: usize,
    /// Largest value change between two frames.
    pub max_step: f32,
    /// The value in the last frame.
    pub final_value: f32,
    /// The target in the last frame.
    pub final_target: f32,
    /// Whether it was live in the last frame.
    pub live_at_end: bool,
    /// Its alignment group.
    pub group: Option<String>,
}

/// The alignment of one run.
#[derive(Clone, Debug, Default)]
pub struct Alignment {
    /// Frames analysed.
    pub frames: usize,
    /// First and last frame times.
    pub span_ms: (u64, u64),
    /// Failed expectations, in time order.
    pub findings: Vec<Finding>,
    /// Per-check coverage.
    pub stats: BTreeMap<Check, Stat>,
    /// Per-key summaries.
    pub tracks: Vec<TrackSummary>,
    /// When the scene went idle (after the last input and the last live track).
    pub idle_from_ms: Option<u64>,
    /// Frames requested after idle, not counting ambient pulse frames.
    pub requested_after_idle: usize,
    /// Ambient pulse frames after idle.
    pub ambient_after_idle: usize,
}

impl Alignment {
    /// Whether every check held.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }

    /// Findings of one check.
    pub fn of(&self, check: Check) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.check == check)
    }
}

/// A sample with the frame it was drawn in.
#[derive(Clone, Copy)]
struct At<'a> {
    frame: usize,
    at_ms: u64,
    sample: &'a TrackSample,
}

fn at_rest(sample: &TrackSample) -> bool {
    !sample.live && sample.budget_ms <= 0.0
}

fn same_segment(a: &TrackSample, b: &TrackSample) -> bool {
    !at_rest(a) && !at_rest(b) && (a.started_ms - b.started_ms).abs() < 1e-6
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ms(value: f64) -> u64 {
    value.max(0.0).round() as u64
}

/// Analyses a run's frames.
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
pub fn analyze(frames: &[Observed], tolerance: Tolerance) -> Alignment {
    let mut out = Alignment {
        frames: frames.len(),
        span_ms: (
            frames.first().map_or(0, |frame| frame.drawn.at_ms),
            frames.last().map_or(0, |frame| frame.drawn.at_ms),
        ),
        ..Alignment::default()
    };
    for check in Check::ALL {
        out.stats.insert(check, Stat::default());
    }
    let mut findings = Vec::new();

    // Every non-pulse key's samples in sample-time order. A frame's ledger
    // can hold samples from more than one draw: GPUI draws inside event
    // dispatch when the window is dirty, so a retarget can happen in a draw
    // no frame record names. Each sample keeps its own time; of two samples
    // at one instant the later-published wins.
    let mut keys: BTreeMap<&str, Vec<At<'_>>> = BTreeMap::new();
    for (index, frame) in frames.iter().enumerate() {
        for sample in &frame.ledger.tracks {
            if sample.kind == TrackKind::Pulse {
                continue;
            }
            let sequence = keys.entry(sample.key.as_str()).or_default();
            let at = At {
                frame: index,
                at_ms: ms(sample.at_ms),
                sample,
            };
            match sequence.last_mut() {
                // Two draws at one instant saw the same segment: keep the
                // later. A retarget at that instant is a new segment and is
                // kept beside the old segment's last sample.
                Some(last)
                    if (last.sample.at_ms - sample.at_ms).abs() < 1e-6
                        && (same_segment(last.sample, sample)
                            || (at_rest(last.sample) && at_rest(sample))) =>
                {
                    *last = at;
                }
                _ => sequence.push(at),
            }
        }
    }

    // A segment's starting value, when the frame that started it was seen.
    let from_of = |sequence: &[At<'_>], index: usize| -> Option<f32> {
        let sample = sequence[index].sample;
        if at_rest(sample) {
            return None;
        }
        sequence[..=index]
            .iter()
            .rev()
            .take_while(|at| same_segment(at.sample, sample))
            .last()
            .filter(|first| (first.sample.at_ms - first.sample.started_ms).abs() < 0.5)
            .map(|first| first.sample.value)
    };

    for (key, sequence) in &keys {
        let last = sequence.last().map(|at| at.sample);
        let mut summary = TrackSummary {
            key: (*key).to_owned(),
            kind: last.map_or(TrackKind::Tween, |sample| sample.kind),
            samples: sequence.len(),
            segments: 0,
            max_step: 0.0,
            final_value: last.map_or(0.0, |sample| sample.value),
            final_target: last.map_or(0.0, |sample| sample.target),
            live_at_end: last.is_some_and(|sample| sample.live),
            group: last.and_then(|sample| sample.group.clone()),
        };
        // Continuity (snaps under reduced motion are the design).
        for pair in sequence.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if frames[b.frame].ledger.reduced_motion {
                out.stats.entry(Check::Continuity).or_default().skipped += 1;
                continue;
            }
            let dt = ((b.sample.at_ms - a.sample.at_ms) / 1000.0).max(0.0) as f32;
            let same = same_segment(a.sample, b.sample);
            let speed = if same {
                a.sample.velocity.abs().max(b.sample.velocity.abs())
            } else {
                a.sample.velocity.abs()
            };
            let scale = (a.sample.target - a.sample.value)
                .abs()
                .max((b.sample.target - a.sample.value).abs());
            let allowed =
                tolerance.slack * speed * dt + tolerance.relative * scale + tolerance.absolute;
            let step = (b.sample.value - a.sample.value).abs();
            summary.max_step = summary.max_step.max(step);
            out.stats
                .entry(Check::Continuity)
                .or_default()
                .see(step / allowed, key, b.at_ms);
            // A keyframe track is one-shot: a new segment of one is a
            // restart (its store forgot it), not a retarget.
            let replay = b.sample.kind == TrackKind::Keys && !same && !at_rest(b.sample);
            if replay {
                out.stats
                    .entry(Check::Replay)
                    .or_default()
                    .see(step / allowed, key, b.at_ms);
            }
            if step > allowed && replay {
                findings.push(Finding {
                    check: Check::Replay,
                    key: (*key).to_owned(),
                    at_ms: b.at_ms,
                    detail: format!(
                        "keyframe track restarted from its first frame: {:.3} -> {:.3} while painted",
                        a.sample.value, b.sample.value
                    ),
                });
            } else if step > allowed {
                findings.push(Finding {
                    check: Check::Continuity,
                    key: (*key).to_owned(),
                    at_ms: b.at_ms,
                    detail: format!(
                        "jumped {:.3} ({:.3} -> {:.3}) in {:.0} ms; velocity {:.2}/s allows {:.3}{}",
                        step,
                        a.sample.value,
                        b.sample.value,
                        dt * 1000.0,
                        speed,
                        allowed,
                        if same { "" } else { " across a retarget" }
                    ),
                });
            }
        }
        // Segments: overshoot and settle.
        let mut index = 0;
        while index < sequence.len() {
            let sample = sequence[index].sample;
            let mut end = index + 1;
            while end < sequence.len() && same_segment(sequence[end].sample, sample) {
                end += 1;
            }
            if at_rest(sample) {
                index = end;
                continue;
            }
            summary.segments += 1;
            let segment = &sequence[index..end];
            let first = segment[0].sample;
            // Overshoot.
            match from_of(sequence, index) {
                Some(from) if first.overshoot_ratio < f32::MAX / 2.0 => {
                    if !first.overshoot_absolute.is_finite()
                        || first.overshoot_absolute < 0.0
                        || first.overshoot_ratio < 0.0
                    {
                        findings.push(Finding {
                            check: Check::Overshoot,
                            key: (*key).to_owned(),
                            at_ms: segment[0].at_ms,
                            detail: format!(
                                "invalid trajectory envelope: relative {} absolute {}",
                                first.overshoot_ratio, first.overshoot_absolute
                            ),
                        });
                    }
                    let momentum = first.kind == TrackKind::Spring
                        && first.velocity.abs() > tolerance.absolute;
                    if momentum {
                        // A spring retargeted mid-flight carries velocity in;
                        // its step-response bound does not apply.
                        out.stats.entry(Check::Overshoot).or_default().skipped += 1;
                    } else {
                        let target = first.target;
                        let span = (target - from).abs();
                        let (low, high) = (from.min(target), from.max(target));
                        let allowed = (first.overshoot_ratio * span + first.overshoot_absolute)
                            * (1.0 + tolerance.relative)
                            + tolerance.absolute;
                        for at in segment {
                            let excess =
                                (low - at.sample.value).max(at.sample.value - high).max(0.0);
                            out.stats.entry(Check::Overshoot).or_default().see(
                                excess / allowed,
                                key,
                                at.at_ms,
                            );
                            if excess > allowed {
                                findings.push(Finding {
                                    check: Check::Overshoot,
                                    key: (*key).to_owned(),
                                    at_ms: at.at_ms,
                                    detail: format!(
                                        "{:.3} is {:.3} past [{:.3}, {:.3}]; its curve allows {:.3} ({:.1} % of {:.3} + {:.3} absolute)",
                                        at.sample.value,
                                        excess,
                                        low,
                                        high,
                                        allowed,
                                        first.overshoot_ratio * 100.0,
                                        span,
                                        first.overshoot_absolute
                                    ),
                                });
                            }
                        }
                    }
                }
                _ => out.stats.entry(Check::Overshoot).or_default().skipped += 1,
            }
            // Settle.
            let deadline = first.started_ms + first.budget_ms;
            let interrupted = end < sequence.len() && !at_rest(sequence[end].sample);
            let settled = segment
                .iter()
                .find(|at| !at.sample.live)
                .or_else(|| sequence.get(end).filter(|next| at_rest(next.sample)));
            let frame_gap = |at: &At<'_>| {
                at.frame
                    .checked_sub(1)
                    .and_then(|previous| frames.get(previous))
                    .map_or(0, |previous| at.at_ms.saturating_sub(previous.drawn.at_ms))
            };
            match settled {
                Some(at) => {
                    let slack = frame_gap(at) as f64 + 1.0;
                    let late = at.sample.at_ms - deadline;
                    let budget = first.budget_ms.max(1.0);
                    out.stats.entry(Check::Settle).or_default().see(
                        ((at.sample.at_ms - first.started_ms) / (budget + slack)) as f32,
                        key,
                        at.at_ms,
                    );
                    if late > slack {
                        findings.push(Finding {
                            check: Check::Settle,
                            key: (*key).to_owned(),
                            at_ms: at.at_ms,
                            detail: format!(
                                "settled {:.0} ms after it started; budget {:.0} ms (+{slack:.0} ms frame slack)",
                                at.sample.at_ms - first.started_ms,
                                first.budget_ms
                            ),
                        });
                    }
                }
                None if interrupted => {
                    out.stats.entry(Check::Settle).or_default().skipped += 1;
                }
                None => {
                    let last_seen = segment.last().map_or(0.0, |at| at.sample.at_ms);
                    let run_end = frames.last().map_or(0, |frame| frame.drawn.at_ms);
                    #[allow(clippy::cast_precision_loss)]
                    let run_end = run_end as f64;
                    if run_end > deadline + 32.0 {
                        out.stats.entry(Check::Settle).or_default().see(
                            f32::INFINITY,
                            key,
                            ms(last_seen),
                        );
                        findings.push(Finding {
                            check: Check::Settle,
                            key: (*key).to_owned(),
                            at_ms: ms(last_seen),
                            detail: format!(
                                "still live at {last_seen:.0} ms; its budget ended at {deadline:.0} ms"
                            ),
                        });
                    } else {
                        out.stats.entry(Check::Settle).or_default().skipped += 1;
                    }
                }
            }
            index = end;
        }
        out.tracks.push(summary);
    }

    // Lockstep: per draw instant, per group, normalised progress spread.
    let mut instants: BTreeMap<(u64, &str), Vec<(&str, f32)>> = BTreeMap::new();
    for (key, sequence) in &keys {
        for (position, at) in sequence.iter().enumerate() {
            let Some(group) = at.sample.group.as_deref() else {
                continue;
            };
            let Some(from) = from_of(sequence, position) else {
                continue;
            };
            let span = at.sample.target - from;
            if span.abs() <= tolerance.absolute {
                continue;
            }
            let members = instants.entry((at.at_ms, group)).or_default();
            let progress = (at.sample.value - from) / span;
            // A retarget at this instant supersedes the old segment's last
            // sample of the same key.
            match members.iter_mut().find(|(member, _)| member == key) {
                Some(member) => member.1 = progress,
                None => members.push((key, progress)),
            }
        }
    }
    for ((at_ms, group), members) in instants {
        if members.len() < 2 {
            continue;
        }
        let (low, high) = members.iter().fold((f32::MAX, f32::MIN), |acc, (_, p)| {
            (acc.0.min(*p), acc.1.max(*p))
        });
        let spread = high - low;
        out.stats.entry(Check::Lockstep).or_default().see(
            spread / tolerance.lockstep,
            group,
            at_ms,
        );
        if spread > tolerance.lockstep {
            findings.push(Finding {
                check: Check::Lockstep,
                key: group.to_owned(),
                at_ms,
                detail: format!(
                    "progress spread {spread:.3} (allowed {:.3}): {}",
                    tolerance.lockstep,
                    members
                        .iter()
                        .map(|(key, p)| format!("{key} {p:.3}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    // Slots: settled paint equals layout.
    let live = |frame: &Observed| {
        frame
            .ledger
            .tracks
            .iter()
            .any(|track| track.live && track.kind != TrackKind::Pulse)
    };
    for frame in frames.iter().filter(|frame| !live(frame)) {
        let mut slots: BTreeMap<&str, &BoundsSample> = BTreeMap::new();
        for bounds in &frame.ledger.bounds {
            if let Some(name) = bounds.key.strip_prefix("slot:") {
                slots.insert(name, bounds);
            }
        }
        for bounds in &frame.ledger.bounds {
            let Some(name) = bounds.key.strip_prefix("paint:") else {
                continue;
            };
            let Some(slot) = slots.get(name) else {
                continue;
            };
            let delta = (bounds.x - slot.x)
                .abs()
                .max((bounds.y - slot.y).abs())
                .max((bounds.width - slot.width).abs())
                .max((bounds.height - slot.height).abs());
            out.stats.entry(Check::Slot).or_default().see(
                delta / tolerance.slot_px,
                name,
                frame.drawn.at_ms,
            );
            if delta > tolerance.slot_px {
                findings.push(Finding {
                    check: Check::Slot,
                    key: name.to_owned(),
                    at_ms: frame.drawn.at_ms,
                    detail: format!(
                        "settled at ({:.1}, {:.1}) {:.1}x{:.1}; its slot is ({:.1}, {:.1}) {:.1}x{:.1}",
                        bounds.x,
                        bounds.y,
                        bounds.width,
                        bounds.height,
                        slot.x,
                        slot.y,
                        slot.width,
                        slot.height
                    ),
                });
            }
        }
    }

    // Idle: after the last input and the last live frame, nothing asks.
    let last_input = frames.iter().rposition(|frame| frame.events > 0);
    let last_live = frames.iter().rposition(|frame| live(frame));
    let calm_from = match (last_input, last_live) {
        (Some(a), Some(b)) => a.max(b) + 1,
        (Some(a), None) => a + 1,
        (None, Some(b)) => b + 1,
        (None, None) => 0,
    };
    // The frame right after the last live one was requested by it.
    let idle_start = if last_live.is_some_and(|b| b + 1 == calm_from) {
        calm_from + 1
    } else {
        calm_from
    };
    if idle_start < frames.len() {
        out.idle_from_ms = Some(frames[idle_start].drawn.at_ms);
        let mut ambient_times = Vec::new();
        for frame in &frames[idle_start..] {
            out.stats.entry(Check::Idle).or_default().see(
                if frame.drawn.requested() { 1.0 } else { 0.0 },
                "window",
                frame.drawn.at_ms,
            );
            if !frame.drawn.requested() {
                continue;
            }
            let ambient = frame
                .ledger
                .tracks
                .iter()
                .any(|track| track.kind == TrackKind::Pulse);
            if ambient {
                out.ambient_after_idle += 1;
                ambient_times.push(frame.drawn.at_ms);
            } else {
                out.requested_after_idle += 1;
                findings.push(Finding {
                    check: Check::Idle,
                    key: "window".to_owned(),
                    at_ms: frame.drawn.at_ms,
                    detail: format!(
                        "frame requested after idle ({} invalidations, {} frame callbacks)",
                        frame.drawn.invalidations, frame.drawn.callbacks
                    ),
                });
            }
        }
        if let (Some(first), Some(last)) = (ambient_times.first(), ambient_times.last()) {
            let seconds = (last - first) as f32 / 1000.0;
            #[allow(clippy::cast_precision_loss)]
            let fps = if seconds > 0.0 {
                (ambient_times.len() - 1) as f32 / seconds
            } else {
                0.0
            };
            if fps > tolerance.ambient_fps {
                findings.push(Finding {
                    check: Check::Idle,
                    key: "pulse".to_owned(),
                    at_ms: *last,
                    detail: format!(
                        "ambient frames at {fps:.1} fps (allowed {:.1})",
                        tolerance.ambient_fps
                    ),
                });
            }
        }
    } else if !frames.is_empty() {
        findings.push(Finding {
            check: Check::Idle,
            key: "window".to_owned(),
            at_ms: out.span_ms.1,
            detail:
                "never went idle before the run ended (a track stayed live or input kept arriving)"
                    .to_owned(),
        });
        out.stats
            .entry(Check::Idle)
            .or_default()
            .see(f32::INFINITY, "window", out.span_ms.1);
    }

    // Unrequested motion: a frame nobody asked for must look like the last.
    for pair in frames.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if b.drawn.requested() || b.events > 0 {
            continue;
        }
        let mut moved = Vec::new();
        for sample in &b.ledger.tracks {
            if sample.kind == TrackKind::Pulse {
                continue;
            }
            if let Some(before) = a.ledger.track(&sample.key)
                && (before.value - sample.value).abs() > tolerance.absolute
            {
                moved.push(format!(
                    "{} {:.3} -> {:.3}",
                    sample.key, before.value, sample.value
                ));
            }
        }
        for bounds in &b.ledger.bounds {
            if let Some(before) = a.ledger.bounds(&bounds.key)
                && ((before.x - bounds.x).abs() > 0.01
                    || (before.y - bounds.y).abs() > 0.01
                    || (before.width - bounds.width).abs() > 0.01
                    || (before.height - bounds.height).abs() > 0.01)
            {
                moved.push(format!("{} bounds", bounds.key));
            }
        }
        out.stats.entry(Check::Unrequested).or_default().see(
            if moved.is_empty() { 0.0 } else { f32::INFINITY },
            "window",
            b.drawn.at_ms,
        );
        if !moved.is_empty() {
            findings.push(Finding {
                check: Check::Unrequested,
                key: moved[0].split(' ').next().unwrap_or("window").to_owned(),
                at_ms: b.drawn.at_ms,
                detail: format!(
                    "changed in a frame nobody requested (a real window shows the old frame): {}",
                    moved.join(", ")
                ),
            });
        }
    }

    findings.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then(a.check.cmp(&b.check)));
    out.findings = findings;
    out
}

/// A readable report.
#[must_use]
pub fn text(scene: &str, alignment: &Alignment, limit: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "motion alignment  {scene}  {} frames  {}..{} ms  {}",
        alignment.frames,
        alignment.span_ms.0,
        alignment.span_ms.1,
        if alignment.passed() { "PASS" } else { "FAIL" }
    );
    let segments: usize = alignment.tracks.iter().map(|track| track.segments).sum();
    let _ = writeln!(
        out,
        "  {} track keys, {segments} segments, idle from {}, {} requested / {} ambient frames after idle",
        alignment.tracks.len(),
        alignment
            .idle_from_ms
            .map_or_else(|| "never".to_owned(), |at| format!("{at} ms")),
        alignment.requested_after_idle,
        alignment.ambient_after_idle
    );
    for check in Check::ALL {
        let stat = alignment.stats.get(&check).cloned().unwrap_or_default();
        let failures = alignment.of(check).count();
        let verdict = if failures > 0 {
            "FAIL"
        } else if stat.evaluated == 0 {
            "n/a "
        } else {
            "ok  "
        };
        let worst = stat
            .worst_at
            .as_ref()
            .map_or_else(String::new, |(key, at)| {
                format!("  worst {:.2} of allowed ({key} @ {at} ms)", stat.worst)
            });
        let skipped = if stat.skipped > 0 {
            format!("  {} skipped", stat.skipped)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  {:<12} {verdict} {:>5} evaluated  {failures} failed{worst}{skipped}",
            check.name(),
            stat.evaluated
        );
    }
    for finding in alignment.findings.iter().take(limit) {
        let _ = writeln!(
            out,
            "  FAIL {:<11} {:>6} ms  {}: {}",
            finding.check.name(),
            finding.at_ms,
            finding.key,
            finding.detail
        );
    }
    if alignment.findings.len() > limit {
        let _ = writeln!(
            out,
            "  … {} more findings",
            alignment.findings.len() - limit
        );
    }
    out
}

/// The report as JSON.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn json(scene: &str, alignment: &Alignment) -> Json {
    Json::obj([
        ("scene", Json::str(scene)),
        ("pass", Json::Bool(alignment.passed())),
        ("frames", Json::num(alignment.frames as f64)),
        (
            "span_ms",
            Json::Arr(vec![
                Json::num(alignment.span_ms.0 as f64),
                Json::num(alignment.span_ms.1 as f64),
            ]),
        ),
        (
            "idle_from_ms",
            Json::opt(alignment.idle_from_ms.map(|at| at as f64)),
        ),
        (
            "requested_after_idle",
            Json::num(alignment.requested_after_idle as f64),
        ),
        (
            "ambient_after_idle",
            Json::num(alignment.ambient_after_idle as f64),
        ),
        (
            "checks",
            Json::Obj(
                Check::ALL
                    .iter()
                    .map(|check| {
                        let stat = alignment.stats.get(check).cloned().unwrap_or_default();
                        (
                            check.name().to_owned(),
                            Json::obj([
                                ("evaluated", Json::num(stat.evaluated as f64)),
                                ("skipped", Json::num(stat.skipped as f64)),
                                ("failed", Json::num(alignment.of(*check).count() as f64)),
                                ("worst", Json::num(f64::from(stat.worst))),
                                (
                                    "worst_at",
                                    stat.worst_at.map_or(Json::Null, |(key, at)| {
                                        Json::obj([
                                            ("key", Json::str(key)),
                                            ("at_ms", Json::num(at as f64)),
                                        ])
                                    }),
                                ),
                            ]),
                        )
                    })
                    .collect(),
            ),
        ),
        (
            "findings",
            Json::Arr(
                alignment
                    .findings
                    .iter()
                    .map(|finding| {
                        Json::obj([
                            ("check", Json::str(finding.check.name())),
                            ("key", Json::str(finding.key.clone())),
                            ("at_ms", Json::num(finding.at_ms as f64)),
                            ("detail", Json::str(finding.detail.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "tracks",
            Json::Arr(
                alignment
                    .tracks
                    .iter()
                    .map(|track| {
                        Json::obj([
                            ("key", Json::str(track.key.clone())),
                            ("kind", Json::str(track.kind.name())),
                            ("samples", Json::num(track.samples as f64)),
                            ("segments", Json::num(track.segments as f64)),
                            ("max_step", Json::num(f64::from(track.max_step))),
                            ("final_value", Json::num(f64::from(track.final_value))),
                            ("final_target", Json::num(f64::from(track.final_target))),
                            ("live_at_end", Json::Bool(track.live_at_end)),
                            ("group", track.group.clone().map_or(Json::Null, Json::Str)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

#[cfg(test)]
mod envelope_canaries {
    use super::{Check, Observed, Tolerance, analyze};
    use crate::probe::{Ledger, TrackKind, TrackSample};
    use backend_gui_harness::{Drawn, Viewport};
    use std::time::Duration;

    // An independently specified equal-endpoint trajectory. A renderer may
    // bend up to 2 channel units; a 3-unit bend is a defect even though both
    // endpoints are 10. Velocity is deliberately generous so this isolates
    // the envelope check rather than triggering continuity too.
    fn trajectory(peak: f32) -> Vec<Observed> {
        [(0, 10.0), (16, 10.0), (32, peak), (48, 10.0), (64, 10.0)]
            .into_iter()
            .map(|(at_ms, value)| {
                let live = at_ms == 16 || at_ms == 32;
                let active = (16..=48).contains(&at_ms);
                Observed {
                    drawn: Drawn {
                        at_ms,
                        invalidations: u64::from(at_ms < 64),
                        callbacks: 0,
                        cpu: Duration::ZERO,
                        input_cpu: Duration::ZERO,
                        input_events: 0,
                        input_max: Duration::ZERO,
                        viewport: Viewport {
                            width: 100,
                            height: 100,
                            scale: 1,
                        },
                        captured: false,
                    },
                    ledger: Ledger {
                        tracks: vec![TrackSample {
                            key: "equal-endpoint-flight".into(),
                            kind: TrackKind::Tween,
                            value,
                            target: 10.0,
                            velocity: if live { 600.0 } else { 0.0 },
                            started_ms: if active { 16.0 } else { at_ms as f64 },
                            budget_ms: if active { 32.0 } else { 0.0 },
                            at_ms: at_ms as f64,
                            live,
                            overshoot_ratio: 0.0,
                            overshoot_absolute: if active { 2.0 } else { 0.0 },
                            group: None,
                        }],
                        ..Ledger::default()
                    },
                    events: usize::from(at_ms == 0),
                    state: None,
                }
            })
            .collect()
    }

    #[test]
    fn finite_absolute_envelope_accepts_legitimate_bulge_rejects_deformation() {
        let healthy = analyze(&trajectory(11.5), Tolerance::default());
        assert!(
            healthy.passed(),
            "healthy equal-endpoint path: {:?}",
            healthy.findings
        );
        assert!(
            healthy.stats[&Check::Overshoot].evaluated >= 3,
            "equal-endpoint path escaped verification"
        );
        let broken = analyze(&trajectory(13.0), Tolerance::default());
        assert!(
            broken.of(Check::Overshoot).any(|f| f.at_ms == 32),
            "out-of-envelope path was accepted: {:?}",
            broken.findings
        );
        assert_eq!(
            broken.findings.len(),
            1,
            "canary must isolate envelope checking: {:?}",
            broken.findings
        );
    }
    #[test]
    fn an_infinite_absolute_envelope_cannot_turn_a_deformation_into_pass() {
        let mut frames = trajectory(13.0);
        for frame in &mut frames {
            if frame.ledger.tracks[0].budget_ms > 0.0 {
                frame.ledger.tracks[0].overshoot_absolute = f32::INFINITY;
            }
        }
        let broken = analyze(&frames, Tolerance::default());
        assert!(
            broken
                .of(Check::Overshoot)
                .any(|f| f.detail.contains("invalid trajectory envelope")),
            "nonfinite envelopes must fail, not suppress verification: {:?}",
            broken.findings
        );
    }
}
