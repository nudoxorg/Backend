//! The motion store: tracks keyed by stable ids, sampled inside `render`,
//! and the per-window gate that requests a frame only while one is live.

use super::keys::{Keys, Pose};
use super::spring::{Phase, Spring};
use super::{epoch, now, reduced};
use crate::probe::{self, TrackKind, TrackSample};
use crate::tokens::motion::Bezier;
use gpui::{App, ElementId, EntityId, Global, Window, WindowId};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How a value moves towards a new target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spec {
    /// Jump; nothing animates.
    Snap,
    /// A CSS-transition-like tween. Retargeting mid-flight restarts the full
    /// duration from the current value.
    Tween {
        /// Time from start to target.
        duration: Duration,
        /// Easing (may overshoot).
        curve: Bezier,
        /// Hold before starting.
        delay: Duration,
    },
    /// A spring. Retargeting keeps the current velocity.
    Spring(Spring),
}

impl Spec {
    /// A tween with no delay.
    #[must_use]
    pub const fn tween(duration: Duration, curve: Bezier) -> Self {
        Self::Tween {
            duration,
            curve,
            delay: Duration::ZERO,
        }
    }

    /// The same spec after a hold (springs and snaps ignore it).
    #[must_use]
    pub const fn delayed(self, hold: Duration) -> Self {
        match self {
            Self::Tween {
                duration, curve, ..
            } => Self::Tween {
                duration,
                curve,
                delay: hold,
            },
            other => other,
        }
    }
}

impl From<Spring> for Spec {
    fn from(spring: Spring) -> Self {
        Self::Spring(spring)
    }
}

#[derive(Clone, Debug)]
enum State {
    Still(f32),
    Tween {
        from: f32,
        to: f32,
        start: Instant,
        delay: Duration,
        duration: Duration,
        curve: Bezier,
    },
    Spring {
        spring: Spring,
        target: f32,
        origin: Phase,
        start: Instant,
        rest: f64,
        budget: Duration,
    },
    Keys {
        keys: Keys<Pose>,
        start: Instant,
        done: bool,
    },
}

/// Where a spring comes to rest, in the track's own units: close enough to
/// its target that the last step onto it is below anything a frame shows and
/// within what its velocity accounts for.
const REST: f64 = 1e-3;

#[derive(Clone, Copy, Debug)]
struct Sample {
    value: f32,
    velocity: f32,
    live: bool,
}

impl Sample {
    const fn at_rest(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            live: false,
        }
    }

    const fn held(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            live: true,
        }
    }
}

impl State {
    fn target(&self) -> f32 {
        match self {
            Self::Still(value) => *value,
            Self::Tween { to, .. } => *to,
            Self::Spring { target, .. } => *target,
            Self::Keys { .. } => 0.0,
        }
    }

    fn started(&self) -> Option<Instant> {
        match self {
            Self::Still(_) => None,
            Self::Tween { start, .. } | Self::Spring { start, .. } | Self::Keys { start, .. } => {
                Some(*start)
            }
        }
    }

    fn budget(&self) -> Duration {
        match self {
            Self::Still(_) => Duration::ZERO,
            Self::Tween {
                delay, duration, ..
            } => *delay + *duration,
            Self::Spring { budget, .. } => *budget,
            Self::Keys { keys, .. } => keys.duration,
        }
    }

    /// Samples a scalar track at `now`, settling it in place when done.
    #[allow(clippy::cast_possible_truncation)]
    fn sample(&mut self, now: Instant) -> Sample {
        let (sample, settled) = match *self {
            Self::Still(value) => (Sample::at_rest(value), None),
            Self::Keys { .. } => (Sample::at_rest(0.0), None),
            Self::Tween {
                from,
                to,
                start,
                delay,
                duration,
                curve,
            } => {
                let run = now.saturating_duration_since(start);
                let active = run.saturating_sub(delay);
                if run < delay {
                    (Sample::held(from), None)
                } else if duration.is_zero() || active >= duration {
                    (Sample::at_rest(to), Some(to))
                } else {
                    let span = duration.as_secs_f32();
                    let progress = active.as_secs_f32() / span;
                    let sample = Sample {
                        value: from + (to - from) * curve.ease(progress),
                        velocity: (to - from) * curve.slope(progress) / span,
                        live: true,
                    };
                    (sample, None)
                }
            }
            Self::Spring {
                spring,
                target,
                origin,
                start,
                rest,
                ..
            } => {
                let phase = spring.step(origin, now.saturating_duration_since(start).as_secs_f64());
                if spring.at_rest(phase, rest) {
                    (Sample::at_rest(target), Some(target))
                } else {
                    let sample = Sample {
                        value: target + phase.offset as f32,
                        velocity: phase.velocity as f32,
                        live: true,
                    };
                    (sample, None)
                }
            }
        };
        if let Some(value) = settled {
            *self = Self::Still(value);
        }
        sample
    }

    /// Starts moving from the current sample towards `target`.
    fn retarget(&mut self, current: Sample, target: f32, spec: Spec, now: Instant) {
        *self = match spec {
            Spec::Snap => Self::Still(target),
            Spec::Tween {
                duration,
                curve,
                delay,
            } => Self::Tween {
                from: current.value,
                to: target,
                start: now,
                delay,
                duration,
                curve,
            },
            Spec::Spring(spring) => {
                let origin = Phase {
                    offset: f64::from(current.value - target),
                    velocity: f64::from(current.velocity),
                };
                // An absolute rest: the final snap onto the target is at most
                // 1e-3 units, whatever the size of the move (a threshold
                // relative to the move snapped 0.1 % of it: 0.26 px at 222 px).
                let rest = REST;
                let budget = Duration::from_secs_f64(spring.settle_time(origin, rest));
                Self::Spring {
                    spring,
                    target,
                    origin,
                    start: now,
                    rest,
                    budget,
                }
            }
        };
    }
}

#[derive(Default)]
struct Store {
    tracks: HashMap<ElementId, State>,
}

/// A motion store: every animated value of one view (or one scope), keyed by
/// a stable id rather than by element position, so a remount never replays
/// or restarts a track. Cloning shares the store.
///
/// Sample it inside `render`; each call returns the value to paint this frame
/// and, while the track is live, asks the window's gate for another frame.
#[derive(Clone, Default)]
pub struct Motion {
    store: Rc<RefCell<Store>>,
}

impl Motion {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The store registered under `scope`, shared by every view that asks for
    /// it. Use it when the owning view itself can be rebuilt mid-motion (an
    /// exiting row whose list view is recreated must not replay its exit).
    pub fn scoped(scope: impl Into<ElementId>, cx: &mut App) -> Self {
        cx.default_global::<Scopes>()
            .0
            .entry(scope.into())
            .or_default()
            .clone()
    }

    /// Animates `key` towards `target`. The first sighting of a key is not
    /// animated (like a CSS transition); use [`Motion::animate_from`] for an
    /// entrance.
    pub fn animate(
        &self,
        key: impl Into<ElementId>,
        target: f32,
        spec: impl Into<Spec>,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        self.drive(&key.into(), None, target, spec.into(), window, cx)
    }

    /// Like [`Motion::animate`], but a key seen for the first time starts at
    /// `from` and moves to `target`.
    pub fn animate_from(
        &self,
        key: impl Into<ElementId>,
        from: f32,
        target: f32,
        spec: impl Into<Spec>,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        self.drive(&key.into(), Some(from), target, spec.into(), window, cx)
    }

    fn drive(
        &self,
        key: &ElementId,
        from: Option<f32>,
        target: f32,
        spec: Spec,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        let now = now(cx);
        let spec = if reduced(cx) { Spec::Snap } else { spec };
        let (sample, meta) = {
            let mut store = self.store.borrow_mut();
            let state = store
                .tracks
                .entry(key.clone())
                .or_insert_with(|| match from {
                    Some(from) if (from - target).abs() > f32::EPSILON => {
                        let mut fresh = State::Still(from);
                        fresh.retarget(Sample::at_rest(from), target, spec, now);
                        fresh
                    }
                    _ => State::Still(target),
                });
            if matches!(state, State::Keys { .. }) {
                *state = State::Still(target);
            }
            // The segment being sampled: a sample that settles the track
            // still reports that segment's start and budget to the probe.
            let mut meta = Meta::of(state);
            let mut sample = state.sample(now);
            if (state.target() - target).abs() > f32::EPSILON {
                state.retarget(sample, target, spec, now);
                meta = Meta::of(state);
                sample = state.sample(now);
            }
            (sample, meta)
        };
        if sample.live {
            request_frame(window, cx);
        }
        publish(cx, key, None, sample, target, meta, now);
        sample.value
    }

    /// Plays a keyframe track once under `key`, starting the first time the
    /// key is seen, and returns this frame's pose. A finished track keeps
    /// returning its final pose (it never replays by itself); call
    /// [`Motion::replay`] to run it again.
    pub fn play(
        &self,
        key: impl Into<ElementId>,
        keys: &Keys<Pose>,
        window: &mut Window,
        cx: &mut App,
    ) -> Pose {
        let key = key.into();
        let now = now(cx);
        let reduced = reduced(cx);
        let (pose, live, meta, keys) = {
            let mut store = self.store.borrow_mut();
            let state = store
                .tracks
                .entry(key.clone())
                .or_insert_with(|| State::Keys {
                    keys: keys.clone(),
                    start: now,
                    done: reduced,
                });
            if !matches!(state, State::Keys { .. }) {
                *state = State::Keys {
                    keys: keys.clone(),
                    start: now,
                    done: reduced,
                };
            }
            let meta = Meta::of(state);
            let State::Keys {
                keys: track,
                start,
                done,
            } = state
            else {
                return keys.end();
            };
            let elapsed = now.saturating_duration_since(*start);
            if reduced || elapsed >= track.duration {
                *done = true;
            }
            let pose = if *done {
                track.end()
            } else {
                track.sample(elapsed)
            };
            let keys = probe::enabled(cx).then(|| track.clone());
            (pose, !*done, meta, keys)
        };
        if live {
            request_frame(window, cx);
        }
        if let Some(keys) = keys {
            publish_pose(cx, &key, pose, live, &keys, meta, now);
        }
        pose
    }

    /// Forgets `key`, so the next [`Motion::play`] starts again from 0 and the
    /// next [`Motion::animate`] is a first sighting.
    pub fn replay(&self, key: impl Into<ElementId>) {
        self.store.borrow_mut().tracks.remove(&key.into());
    }

    /// Jumps `key` to `value` without animating.
    pub fn set(&self, key: impl Into<ElementId>, value: f32) {
        self.store
            .borrow_mut()
            .tracks
            .insert(key.into(), State::Still(value));
    }

    /// Whether a keyframe track under `key` has finished (an exit is over and
    /// its element can be dropped). Unknown keys are not done.
    #[must_use]
    pub fn is_done(&self, key: impl Into<ElementId>, cx: &App) -> bool {
        let now = now(cx);
        match self.store.borrow().tracks.get(&key.into()) {
            Some(State::Keys { keys, start, done }) => {
                *done || now.saturating_duration_since(*start) >= keys.duration
            }
            Some(_) | None => false,
        }
    }

    /// Whether any track in the store still moves at the current time.
    #[must_use]
    pub fn is_live(&self, cx: &App) -> bool {
        let now = now(cx);
        self.store
            .borrow()
            .tracks
            .values()
            .any(|state| match state {
                State::Still(_) => false,
                State::Keys { keys, start, done } => {
                    !*done && now.saturating_duration_since(*start) < keys.duration
                }
                State::Tween {
                    start,
                    delay,
                    duration,
                    ..
                } => now.saturating_duration_since(*start) < *delay + *duration,
                State::Spring { .. } => {
                    let mut probe = state.clone();
                    probe.sample(now).live
                }
            })
    }

    /// Settles every track at its target immediately.
    pub fn settle(&self) {
        for state in self.store.borrow_mut().tracks.values_mut() {
            match state {
                State::Keys { done, .. } => *done = true,
                other => *other = State::Still(other.target()),
            }
        }
    }

    /// Keeps only the tracks whose key passes `keep`.
    pub fn retain(&self, mut keep: impl FnMut(&ElementId) -> bool) {
        self.store.borrow_mut().tracks.retain(|key, _| keep(key));
    }

    /// The number of tracks held (live or settled).
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.borrow().tracks.len()
    }

    /// Whether the store holds no tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.borrow().tracks.is_empty()
    }
}

#[derive(Default)]
struct Scopes(HashMap<ElementId, Motion>);

impl Global for Scopes {}

fn millis_since(epoch: Instant, at: Instant) -> f64 {
    at.saturating_duration_since(epoch).as_secs_f64() * 1000.0
}

/// What the probe needs to know about a track besides its sample.
#[derive(Clone, Copy)]
struct Meta {
    kind: TrackKind,
    started: Option<Instant>,
    budget: Duration,
    overshoot: f32,
}

impl Meta {
    fn of(state: &State) -> Self {
        Self {
            kind: match state {
                State::Spring { .. } => TrackKind::Spring,
                State::Keys { .. } => TrackKind::Keys,
                State::Still(_) | State::Tween { .. } => TrackKind::Tween,
            },
            started: state.started(),
            budget: state.budget(),
            overshoot: match state {
                State::Tween { curve, .. } => curve.overshoot(),
                State::Spring { spring, .. } => spring.overshoot_ratio(),
                State::Still(_) | State::Keys { .. } => 0.0,
            },
        }
    }
}

fn publish(
    cx: &mut App,
    key: &ElementId,
    channel: Option<&str>,
    sample: Sample,
    target: f32,
    meta: Meta,
    now: Instant,
) {
    if !probe::enabled(cx) {
        return;
    }
    let epoch = epoch(cx);
    probe::record_track(cx, || TrackSample {
        key: channel.map_or_else(|| key.to_string(), |channel| format!("{key}.{channel}")),
        kind: meta.kind,
        value: sample.value,
        target,
        velocity: sample.velocity,
        started_ms: meta.started.map_or(0.0, |start| millis_since(epoch, start)),
        budget_ms: meta.budget.as_secs_f64() * 1000.0,
        at_ms: millis_since(epoch, now),
        live: sample.live,
        overshoot_ratio: meta.overshoot,
        overshoot_absolute: 0.0,
        group: probe::current_group(),
    });
}

/// A pose channel: its report name and how to read it.
type Channel = (&'static str, fn(&Pose) -> f32);

fn publish_pose(
    cx: &mut App,
    key: &ElementId,
    pose: Pose,
    live: bool,
    keys: &Keys<Pose>,
    meta: Meta,
    now: Instant,
) {
    let elapsed = meta
        .started
        .map_or(Duration::ZERO, |start| now.saturating_duration_since(start));
    let later = keys.sample(elapsed + Duration::from_millis(1));
    let end = keys.end();
    let frames = keys.frames.as_ref();
    let channels: [Channel; 6] = [
        ("x", |p| p.x),
        ("y", |p| p.y),
        ("sx", |p| p.sx),
        ("sy", |p| p.sy),
        ("rotate", |p| p.rotate),
        ("opacity", |p| p.opacity),
    ];
    for (name, read) in channels {
        let first = frames.first().map_or(0.0, |(_, pose)| read(pose));
        if frames
            .iter()
            .all(|(_, pose)| (read(pose) - first).abs() < 1e-6)
        {
            continue;
        }
        let sample = Sample {
            value: read(&pose),
            velocity: if live {
                (read(&later) - read(&pose)) * 1000.0
            } else {
                0.0
            },
            live,
        };
        let meta = Meta {
            overshoot: keyframe_overshoot(frames, read, keys.ease),
            ..meta
        };
        publish(cx, key, Some(name), sample, read(&end), meta, now);
    }
}

/// How far a keyframe channel is designed to travel outside its first-to-last
/// span, as a fraction of that span: the keyframes' own excursions plus what
/// the per-interval ease can add to the widest interval. A channel that ends
/// where it starts but moves in between has no span to measure against and
/// reports `f32::MAX` (its keyframes are the whole allowance).
fn keyframe_overshoot(frames: &[(f32, Pose)], read: fn(&Pose) -> f32, ease: Bezier) -> f32 {
    let (Some((_, first)), Some((_, last))) = (frames.first(), frames.last()) else {
        return 0.0;
    };
    let (first, last) = (read(first), read(last));
    let (low, high) = (first.min(last), first.max(last));
    let outside = frames
        .iter()
        .map(|(_, pose)| {
            let value = read(pose);
            (low - value).max(value - high).max(0.0)
        })
        .fold(0.0_f32, f32::max);
    let widest = frames
        .windows(2)
        .map(|pair| (read(&pair[1].1) - read(&pair[0].1)).abs())
        .fold(0.0_f32, f32::max);
    let excess = outside + ease.overshoot() * widest;
    let span = (last - first).abs();
    if span > 1e-6 {
        excess / span
    } else if excess > 1e-6 {
        f32::MAX
    } else {
        0.0
    }
}

/// The per-window OR-gate: however many tracks are live in however many
/// views, a window schedules one next-frame callback, which notifies exactly
/// the views that asked.
#[derive(Default)]
struct Gate {
    windows: HashMap<WindowId, Pending>,
    requested: u64,
}

#[derive(Default)]
struct Pending {
    views: SmallVec<[EntityId; 4]>,
    scheduled: bool,
}

impl Global for Gate {}

/// Asks for another frame for the view being rendered. Cheap and idempotent
/// within a frame; call it from `render` or `prepaint` only.
pub fn request_frame(window: &mut Window, cx: &mut App) {
    let view = window.current_view();
    let id = window.window_handle().window_id();
    let gate = cx.default_global::<Gate>();
    let pending = gate.windows.entry(id).or_default();
    if !pending.views.contains(&view) {
        pending.views.push(view);
    }
    if pending.scheduled {
        return;
    }
    pending.scheduled = true;
    gate.requested += 1;
    window.on_next_frame(move |_window, cx| {
        let views = cx
            .default_global::<Gate>()
            .windows
            .get_mut(&id)
            .map(|pending| {
                pending.scheduled = false;
                std::mem::take(&mut pending.views)
            })
            .unwrap_or_default();
        for view in views {
            cx.notify(view);
        }
    });
}

/// How many frames the gate has scheduled since the app started.
#[must_use]
pub fn frames_requested(cx: &App) -> u64 {
    cx.try_global::<Gate>().map_or(0, |gate| gate.requested)
}

#[cfg(test)]
mod tests {
    use super::{Sample, Spec, State};
    use crate::motion::spring::BOUNCY;
    use crate::tokens::motion::GLIDE;
    use std::time::{Duration, Instant};

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn spring_retarget_keeps_value_and_velocity_continuous() {
        let start = Instant::now();
        let mut state = State::Still(0.0);
        let rest = Sample {
            value: 0.0,
            velocity: 0.0,
            live: false,
        };
        state.retarget(rest, 100.0, Spec::Spring(BOUNCY), start);
        let before = state.sample(at(start, 90));
        assert!(before.live && before.velocity > 100.0, "{before:?}");
        // Retarget mid-flight, the other way.
        state.retarget(before, -50.0, Spec::Spring(BOUNCY), at(start, 90));
        let after = state.sample(at(start, 90));
        assert!(
            (after.value - before.value).abs() < 1e-3,
            "value jumped: {before:?} -> {after:?}"
        );
        assert!(
            (after.velocity - before.velocity).abs() < 1e-2,
            "velocity jumped: {before:?} -> {after:?}"
        );
        // One millisecond later it is still moving the old way (momentum).
        let next = state.sample(at(start, 91));
        assert!(next.value > after.value, "{after:?} -> {next:?}");
        // And it comes to rest exactly on the new target.
        let settled = state.sample(at(start, 5_000));
        assert!(!settled.live);
        assert_eq!(settled.value.to_bits(), (-50.0_f32).to_bits());
    }

    /// The last step onto the target must be one the spring's own velocity
    /// accounts for, at any size of move: a rest threshold relative to the
    /// move snapped 0.1 % of it (0.26 px on a 222 px shelf, W-Shell's storm;
    /// 0.010 px on a 16 px ring, W-Controls).
    #[test]
    fn a_spring_lands_on_its_target_without_a_snap() {
        use crate::motion::spring::{GENTLE, SNAPPY};
        for spring in [SNAPPY, GENTLE, BOUNCY] {
            for span in [1.0_f32, 16.0, 39.6, 222.0, 500.0] {
                for frame in [8_u64, 16] {
                    let start = Instant::now();
                    let mut state = State::Still(0.0);
                    state.retarget(Sample::at_rest(0.0), span, Spec::Spring(spring), start);
                    let budget = state.budget();
                    let mut last = state.sample(start);
                    let mut t = 0;
                    loop {
                        t += frame;
                        assert!(t < 10_000, "{spring:?} {span}: never settled");
                        let now = state.sample(at(start, t));
                        if now.live {
                            last = now;
                            continue;
                        }
                        let dt = frame as f32 / 1000.0;
                        let step = (now.value - last.value).abs();
                        // The probe's continuity allowance for this step.
                        let allowed = 1.5 * last.velocity.abs() * dt + 0.02 * (span - last.value).abs() + 1e-3;
                        assert!(
                            step <= allowed,
                            "{spring:?} span {span} @{frame} ms frames: snapped {step} ({} -> {}) at {:.2}/s, allowed {allowed}",
                            last.value, now.value, last.velocity
                        );
                        assert!(
                            Duration::from_millis(t) <= budget + Duration::from_millis(frame),
                            "{spring:?} span {span}: settled at {t} ms, budget {budget:?}"
                        );
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn tween_retarget_starts_from_the_current_value() {
        let start = Instant::now();
        let mut state = State::Still(0.0);
        let rest = Sample {
            value: 0.0,
            velocity: 0.0,
            live: false,
        };
        let spec = Spec::tween(Duration::from_millis(200), GLIDE);
        state.retarget(rest, 10.0, spec, start);
        let mid = state.sample(at(start, 100));
        state.retarget(mid, 0.0, spec, at(start, 100));
        let after = state.sample(at(start, 100));
        assert!((after.value - mid.value).abs() < 1e-5);
        assert!(!state.sample(at(start, 300)).live);
        assert_eq!(
            state.sample(at(start, 300)).value.to_bits(),
            0.0_f32.to_bits()
        );
    }

    #[test]
    fn delayed_tweens_hold_then_run() {
        let start = Instant::now();
        let mut state = State::Still(0.0);
        let spec =
            Spec::tween(Duration::from_millis(100), GLIDE).delayed(Duration::from_millis(50));
        state.retarget(
            Sample {
                value: 0.0,
                velocity: 0.0,
                live: false,
            },
            1.0,
            spec,
            start,
        );
        let held = state.sample(at(start, 40));
        assert!(held.live && held.value == 0.0);
        assert!(state.sample(at(start, 100)).value > 0.5);
        assert_eq!(state.budget(), Duration::from_millis(150));
    }
}
