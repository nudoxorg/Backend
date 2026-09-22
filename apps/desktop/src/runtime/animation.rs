//! Deterministic, retargetable animation tracks.
//!
//! The timeline owns render values only. Product state remains in the immutable
//! [`crate::model::AppSnapshot`], while this module supplies the small amount of
//! transient geometry/opacity needed between two admitted snapshots. Every
//! track is keyed by a stable semantic id and carries a monotonically increasing
//! version, so a late result cannot restart a newer visual transition.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// One clock abstraction shared by every timeline track.
pub trait FrameClock {
    /// Returns the current frame timestamp.
    fn now(&self) -> Duration;
}

/// Wall-clock frame source for live windows.
#[derive(Clone, Debug)]
pub struct LiveFrameClock {
    started: Instant,
}

impl Default for LiveFrameClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl FrameClock for LiveFrameClock {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Deterministic capture clock owned by a harness.
#[derive(Clone, Debug, Default)]
pub struct CaptureFrameClock {
    now: Rc<Cell<Duration>>,
}

impl CaptureFrameClock {
    /// Sets the exact timestamp used by the next frame.
    ///
    /// This setter is intentionally exact for a replay. Callers driving a
    /// monotonic capture should prefer [`Self::advance_to`], which ignores a
    /// stale retry rather than moving the clock backwards.
    pub fn set(&mut self, now: Duration) {
        self.now.set(now);
    }

    /// Advances to a timestamp without ever reversing a capture.
    pub fn advance_to(&mut self, now: Duration) -> Duration {
        let current = self.now.get();
        if now <= current {
            return Duration::ZERO;
        }
        self.now.set(now);
        now.saturating_sub(current)
    }

    /// Returns the current capture time.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.now.get()
    }

    /// Resets the clock for a fresh replay.
    pub fn reset(&mut self) {
        self.now.set(Duration::ZERO);
    }
}

impl FrameClock for CaptureFrameClock {
    fn now(&self) -> Duration {
        self.now.get()
    }
}

/// Stable animation track identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnimationId(u64);

impl AnimationId {
    /// Creates an animation ID.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the stable numeric identity used in harness traces.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Semantic channels shared by the shell and screenshot harness.
///
/// Channels make it possible to retarget a render value without coupling the
/// transition to a view instance or an event callback. Product state still
/// owns the target; this enum only gives transient render state a stable key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AnimationChannel {
    /// Route changes are normally an immediate content swap.
    Route,
    /// Shelf/context pane width collapse and expansion.
    PaneCollapse,
    /// Disclosure content reveal.
    Disclosure,
    /// Hover state.
    Hover,
    /// Pressed state.
    Press,
    /// Selection highlight.
    Selection,
    /// Search/command palette opacity and offset.
    SearchOverlay,
    /// Skeleton-to-content crossfade.
    SkeletonContent,
    /// Toast/status visibility.
    ToastStatus,
    /// Graph camera/layout interpolation.
    GraphCamera,
}

impl AnimationChannel {
    /// Returns the stable numeric track identity for this channel.
    #[must_use]
    pub const fn id(self) -> AnimationId {
        AnimationId::new(match self {
            Self::Route => 0x524f_5554_4500_0001,
            Self::PaneCollapse => 0x5041_4e45_0000_0002,
            Self::Disclosure => 0x4449_5343_0000_0003,
            Self::Hover => 0x484f_5645_5200_0004,
            Self::Press => 0x5052_4553_5300_0005,
            Self::Selection => 0x5345_4c45_4300_0006,
            Self::SearchOverlay => 0x5345_4152_4348_0007,
            Self::SkeletonContent => 0x534b_454c_0000_0008,
            Self::ToastStatus => 0x544f_4153_5400_0009,
            Self::GraphCamera => 0x4752_4150_4800_000a,
        })
    }

    /// Returns the design beat used by this channel when it is retargeted.
    #[must_use]
    pub const fn beat(self) -> Beat {
        match self {
            Self::Route | Self::GraphCamera => Beat::Scene,
            Self::PaneCollapse | Self::SearchOverlay | Self::SkeletonContent => Beat::Reveal,
            Self::Disclosure => Beat::Unfold,
            Self::Hover | Self::Press | Self::Selection => Beat::Touch,
            Self::ToastStatus => Beat::Emphasis,
        }
    }
}

/// A monotonically increasing producer/version identity for one track.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimelineVersion(u64);

impl TimelineVersion {
    /// Creates a version from a persisted or event-owned sequence.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying sequence value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the next version, saturating at the representable maximum.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Easing functions used by semantic transition tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Easing {
    /// Constant-rate interpolation for reduced-motion and progress values.
    Linear,
    /// Nudox `--snap`: the short, decisive control response.
    Snap,
    /// Nudox `--glide`: the quiet ease-out used by reveals.
    Glide,
    /// Nudox bounce: the measured overshooting sheet/emphasis response.
    Bounce,
    /// Smoothstep for a quiet two-way control transition.
    SmoothStep,
    /// Fast arrival with a short, readable tail.
    EaseOutQuint,
    /// Soft popover translation/opacity easing.
    EaseOutCubic,
}

impl Easing {
    fn sample(self, amount: f32) -> f32 {
        let amount = amount.clamp(0.0, 1.0);
        match self {
            Self::Linear => amount,
            Self::Snap => cubic_bezier(amount, 0.3, 0.0, 0.0, 1.0),
            Self::Glide => cubic_bezier(amount, 0.22, 1.0, 0.36, 1.0),
            Self::Bounce => cubic_bezier(amount, 0.34, 1.56, 0.64, 1.0),
            Self::SmoothStep => amount * amount * (3.0 - 2.0 * amount),
            Self::EaseOutQuint => 1.0 - (1.0 - amount).powi(5),
            Self::EaseOutCubic => 1.0 - (1.0 - amount).powi(3),
        }
    }
}

/// Product motion beats from the design contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Beat {
    /// Hover, pressed, and selection color changes.
    Touch,
    /// Popovers and route-adjacent reveals.
    Reveal,
    /// User-opened disclosure or sheet.
    Unfold,
    /// Toast and status emphasis.
    Emphasis,
    /// Route-scale scene choreography.
    Scene,
}

impl Beat {
    /// Returns the duration, with reduced motion collapsing to an exact snap.
    #[must_use]
    pub const fn duration(self, reduced_motion: bool) -> Duration {
        if reduced_motion {
            return Duration::ZERO;
        }
        Duration::from_millis(match self {
            Self::Touch => 90,
            Self::Reveal => 160,
            Self::Unfold => 240,
            Self::Emphasis => 380,
            Self::Scene => 620,
        })
    }

    /// Returns the easing assigned to the beat.
    #[must_use]
    pub const fn easing(self) -> Easing {
        match self {
            Self::Touch => Easing::Snap,
            Self::Reveal => Easing::Glide,
            Self::Unfold | Self::Emphasis => Easing::Bounce,
            Self::Scene => Easing::Glide,
        }
    }
}

/// Retargetable motion primitive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Fixed-duration interpolation with the default quiet easing.
    Tween {
        /// Duration of the interpolation.
        duration: Duration,
    },
    /// Fixed-duration interpolation with an explicit design easing.
    TweenEased {
        /// Duration of the interpolation.
        duration: Duration,
        /// Easing curve used for the value.
        easing: Easing,
    },
    /// Critically damped-ish spring parameters.
    Spring {
        /// Spring stiffness.
        stiffness: f32,
        /// Velocity damping.
        damping: f32,
    },
}

impl Motion {
    /// Creates the motion token for a design beat.
    #[must_use]
    pub const fn beat(beat: Beat, reduced_motion: bool) -> Self {
        Self::TweenEased {
            duration: beat.duration(reduced_motion),
            easing: beat.easing(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Track {
    from: f32,
    to: f32,
    value: f32,
    velocity: f32,
    started: Duration,
    version: TimelineVersion,
    motion: Motion,
    settled: bool,
}

impl Track {
    fn active(self) -> bool {
        match self.motion {
            Motion::Tween { .. } | Motion::TweenEased { .. } => {
                // Do not retire a tween on a visual epsilon: the next frame
                // must still be admitted at its exact terminal timestamp.
                !self.settled
            }
            Motion::Spring { .. } => {
                (self.value - self.to).abs() > VALUE_EPSILON
                    || self.velocity.abs() > VELOCITY_EPSILON
            }
        }
    }
}

/// Read-only state of one track, useful for semantic probes and frame tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSnapshot {
    /// Current interpolated value.
    pub value: f32,
    /// Current target value.
    pub target: f32,
    /// Current spring velocity.
    pub velocity: f32,
    /// Producer version that owns the target.
    pub version: TimelineVersion,
    /// Whether the track still needs a frame.
    pub active: bool,
}

const VALUE_EPSILON: f32 = 0.001;
const VELOCITY_EPSILON: f32 = 0.001;
const SUBSTEP: f32 = 1.0 / 240.0;
const MAX_FRAME_DELTA: f32 = 0.25;

/// Timeline advanced once per frame by the shell.
#[derive(Debug)]
pub struct AnimationTimeline<C> {
    clock: C,
    reduced_motion: bool,
    tracks: BTreeMap<AnimationId, Track>,
    last_frame: Option<Duration>,
    next_version: TimelineVersion,
}

impl<C: FrameClock> AnimationTimeline<C> {
    /// Creates a timeline with one frame clock.
    #[must_use]
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            reduced_motion: false,
            tracks: BTreeMap::new(),
            last_frame: None,
            next_version: TimelineVersion::new(1),
        }
    }

    /// Enables reduced motion; active tracks snap to exact terminal values.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
        if reduced {
            for track in self.tracks.values_mut() {
                track.value = track.to;
                track.velocity = 0.0;
                track.settled = true;
            }
        }
    }

    /// Returns whether reduced motion is enabled.
    #[must_use]
    pub const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Retargets or creates a motion track with a fresh version.
    pub fn retarget(&mut self, id: AnimationId, target: f32, motion: Motion) -> TimelineVersion {
        self.retarget_at(id, target, motion, self.clock.now())
    }

    /// Retargets at an explicit monotonic timestamp.
    ///
    /// Live windows use [`Self::retarget`]. Deterministic screenshot adapters
    /// use this boundary so a virtual frame timestamp owns both the track's
    /// start and its subsequent samples; no wall-clock read can leak into a
    /// replay when a resize or input lands between frames.
    pub fn retarget_at(
        &mut self,
        id: AnimationId,
        target: f32,
        motion: Motion,
        now: Duration,
    ) -> TimelineVersion {
        let version = self.next_version;
        self.next_version = self.next_version.next();
        let _ = self.retarget_if_newer_at(id, version, target, motion, now);
        version
    }

    /// Retargets a track only when the supplied producer version is current.
    ///
    /// Returns `false` for a stale result. Retargeting preserves the current
    /// value (and spring velocity), making interruption and reversal continuous.
    pub fn retarget_if_newer(
        &mut self,
        id: AnimationId,
        version: TimelineVersion,
        target: f32,
        motion: Motion,
    ) -> bool {
        self.retarget_if_newer_at(id, version, target, motion, self.clock.now())
    }

    /// Version-gated retargeting at an explicit monotonic timestamp.
    pub fn retarget_if_newer_at(
        &mut self,
        id: AnimationId,
        version: TimelineVersion,
        target: f32,
        motion: Motion,
        now: Duration,
    ) -> bool {
        if self
            .tracks
            .get(&id)
            .is_some_and(|track| version <= track.version)
        {
            return false;
        }
        let now = self.last_frame.map_or(now, |previous| now.max(previous));
        let track = self.tracks.entry(id).or_insert(Track {
            from: target,
            to: target,
            value: target,
            velocity: 0.0,
            started: now,
            version,
            motion,
            settled: true,
        });
        track.from = track.value;
        track.to = target;
        track.started = now;
        track.version = version;
        track.motion = motion;
        let zero_duration = match motion {
            Motion::Tween { duration } | Motion::TweenEased { duration, .. } => duration.is_zero(),
            Motion::Spring { .. } => false,
        };
        track.settled =
            self.reduced_motion || (track.from - target).abs() <= VALUE_EPSILON || zero_duration;
        if self.reduced_motion || (track.from - target).abs() <= VALUE_EPSILON {
            track.value = target;
            track.velocity = 0.0;
        }
        true
    }

    /// Advances every track from the shared frame timestamp.
    pub fn advance(&mut self) {
        let now = self.clock.now();
        self.advance_at(now);
    }

    /// Advances deterministically at an explicit timestamp.
    ///
    /// Earlier timestamps are ignored. This is what makes a screenshot retry
    /// safe when a platform draw callback is replayed after a resize.
    pub fn advance_at(&mut self, now: Duration) {
        let Some(previous) = self.last_frame else {
            self.last_frame = Some(now);
            // The first draw may happen after a capture clock has already
            // advanced. Use each track's own start time so a late-created
            // spring does not inherit the elapsed time of an older track.
            self.advance_tracks_from_start(now);
            return;
        };
        if now < previous {
            return;
        }
        let delta = now.saturating_sub(previous);
        self.last_frame = Some(now);
        self.advance_tracks(now, delta);
    }

    fn advance_tracks(&mut self, now: Duration, delta: Duration) {
        let dt = delta.as_secs_f32();
        for track in self.tracks.values_mut() {
            advance_track(track, now, dt, self.reduced_motion);
        }
    }

    fn advance_tracks_from_start(&mut self, now: Duration) {
        for track in self.tracks.values_mut() {
            let dt = now.saturating_sub(track.started).as_secs_f32();
            advance_track(track, now, dt, self.reduced_motion);
        }
    }

    /// Snaps one track to its target and returns whether it existed.
    pub fn settle(&mut self, id: AnimationId) -> bool {
        let Some(track) = self.tracks.get_mut(&id) else {
            return false;
        };
        track.value = track.to;
        track.velocity = 0.0;
        track.settled = true;
        true
    }

    /// Snaps every active render track to its exact terminal frame.
    pub fn settle_all(&mut self) {
        for track in self.tracks.values_mut() {
            track.value = track.to;
            track.velocity = 0.0;
            track.settled = true;
        }
    }

    /// Removes a track that belongs to an unmounted semantic control.
    pub fn remove(&mut self, id: AnimationId) -> bool {
        self.tracks.remove(&id).is_some()
    }

    /// Returns the current value of a track.
    #[must_use]
    pub fn value(&self, id: AnimationId) -> Option<f32> {
        self.tracks.get(&id).map(|track| track.value)
    }

    /// Returns a stable snapshot of a track.
    #[must_use]
    pub fn snapshot(&self, id: AnimationId) -> Option<TrackSnapshot> {
        self.tracks.get(&id).map(|track| TrackSnapshot {
            value: track.value,
            target: track.to,
            velocity: track.velocity,
            version: track.version,
            active: track.active(),
        })
    }

    /// Returns the version currently owning a track.
    #[must_use]
    pub fn version(&self, id: AnimationId) -> Option<TimelineVersion> {
        self.tracks.get(&id).map(|track| track.version)
    }

    /// Returns whether any track remains active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.tracks.values().copied().any(Track::active)
    }

    /// Returns the number of live tracks.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

fn advance_track(track: &mut Track, now: Duration, delta: f32, reduced_motion: bool) {
    if reduced_motion {
        track.value = track.to;
        track.velocity = 0.0;
        track.settled = true;
        return;
    }
    match track.motion {
        Motion::Tween { duration } => {
            advance_tween(track, now, duration, Easing::Glide);
        }
        Motion::TweenEased { duration, easing } => {
            advance_tween(track, now, duration, easing);
        }
        Motion::Spring { stiffness, damping } => {
            advance_spring(track, delta, stiffness.max(0.0), damping.max(0.0));
        }
    }
}

/// Samples a CSS-compatible cubic-bezier curve at normalized time `x`.
///
/// Design-system easing is specified in CSS terms, where the horizontal
/// component is time and the vertical component is progress. Eight Newton
/// steps followed by a bounded binary search keep the result deterministic
/// without relying on a platform animation primitive.
fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let sample = |t: f32, first: f32, second: f32| {
        let one = 1.0 - t;
        3.0 * one * one * t * first + 3.0 * one * t * t * second + t * t * t
    };
    let derivative = |t: f32, first: f32, second: f32| {
        let one = 1.0 - t;
        3.0 * one * one * first + 6.0 * one * t * (second - first) + 3.0 * t * t * (1.0 - second)
    };
    let mut t = x;
    for _ in 0..8 {
        let error = sample(t, x1, x2) - x;
        if error.abs() <= 1e-6 {
            return sample(t, y1, y2);
        }
        let slope = derivative(t, x1, x2);
        if slope.abs() <= 1e-6 {
            break;
        }
        t = (t - error / slope).clamp(0.0, 1.0);
    }
    let mut lower = 0.0;
    let mut upper = 1.0;
    for _ in 0..20 {
        let sample_x = sample(t, x1, x2);
        if (sample_x - x).abs() <= 1e-6 {
            break;
        }
        if sample_x < x {
            lower = t;
        } else {
            upper = t;
        }
        t = (lower + upper) * 0.5;
    }
    sample(t, y1, y2)
}

fn advance_tween(track: &mut Track, now: Duration, duration: Duration, easing: Easing) {
    if duration.is_zero() {
        track.value = track.to;
        track.velocity = 0.0;
        track.settled = true;
        return;
    }
    let elapsed = now.saturating_sub(track.started);
    let amount = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    if amount >= 1.0 {
        track.value = track.to;
        track.velocity = 0.0;
        track.settled = true;
        return;
    }
    track.settled = false;
    track.value = track.from + (track.to - track.from) * easing.sample(amount);
    track.velocity = 0.0;
}

fn advance_spring(track: &mut Track, delta: f32, stiffness: f32, damping: f32) {
    if !delta.is_finite() || delta <= 0.0 {
        return;
    }
    // A stalled window can report a multi-second gap. Bounded integration
    // keeps a late draw responsive; deterministic captures can call
    // `settle`/`settle_all` when they need the exact terminal frame.
    let mut remaining = delta.min(MAX_FRAME_DELTA);
    while remaining > 0.0 {
        let step = remaining.min(SUBSTEP);
        let acceleration = (track.to - track.value) * stiffness - track.velocity * damping;
        track.velocity += acceleration * step;
        track.value += track.velocity * step;
        remaining -= step;
    }
    if (track.value - track.to).abs() <= VALUE_EPSILON && track.velocity.abs() <= VELOCITY_EPSILON {
        track.value = track.to;
        track.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWEEN: Motion = Motion::Tween {
        duration: Duration::from_millis(100),
    };

    #[test]
    fn capture_clock_makes_retargeting_repeatable() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        timeline.retarget(AnimationId::new(1), 1.0, TWEEN);
        clock.set(Duration::from_millis(50));
        timeline.advance();
        let first = timeline.value(AnimationId::new(1)).expect("track");
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(1)), Some(first));
        timeline.set_reduced_motion(true);
        clock.set(Duration::from_millis(60));
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(1)), Some(1.0));
        assert!(!timeline.is_active());
    }

    #[test]
    fn earlier_capture_timestamps_are_ignored() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        timeline.retarget(AnimationId::new(1), 1.0, TWEEN);
        clock.set(Duration::from_millis(75));
        timeline.advance();
        let value = timeline.value(AnimationId::new(1)).expect("track");
        clock.set(Duration::from_millis(25));
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(1)), Some(value));
    }

    #[test]
    fn retargeting_preserves_continuity_and_rejects_stale_versions() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        let first = timeline.retarget(AnimationId::new(7), 1.0, TWEEN);
        clock.set(Duration::from_millis(50));
        timeline.advance();
        let before = timeline.value(AnimationId::new(7)).expect("track");
        let second = timeline.retarget(AnimationId::new(7), 0.0, TWEEN);
        assert!(second > first);
        assert_eq!(timeline.value(AnimationId::new(7)), Some(before));
        assert!(!timeline.retarget_if_newer(AnimationId::new(7), first, 1.0, TWEEN));
        clock.set(Duration::from_millis(150));
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(7)), Some(0.0));
    }

    #[test]
    fn explicit_retarget_timestamp_anchors_virtual_replay_after_a_wall_clock_gap() {
        let mut timeline = AnimationTimeline::new(LiveFrameClock::default());
        let id = AnimationId::new(70);
        timeline.retarget_at(
            id,
            1.0,
            Motion::TweenEased {
                duration: Duration::from_millis(100),
                easing: Easing::Linear,
            },
            Duration::from_millis(400),
        );
        timeline.advance_at(Duration::from_millis(450));
        assert_eq!(timeline.value(id), Some(0.5));
        timeline.advance_at(Duration::from_millis(500));
        assert_eq!(timeline.value(id), Some(1.0));
    }

    #[test]
    fn stale_retarget_timestamp_cannot_move_the_track_clock_backwards() {
        let mut timeline = AnimationTimeline::new(LiveFrameClock::default());
        let id = AnimationId::new(71);
        timeline.retarget_at(
            id,
            1.0,
            Motion::TweenEased {
                duration: Duration::from_millis(100),
                easing: Easing::Linear,
            },
            Duration::from_millis(100),
        );
        timeline.advance_at(Duration::from_millis(150));
        timeline.retarget_at(
            id,
            0.0,
            Motion::TweenEased {
                duration: Duration::from_millis(100),
                easing: Easing::Linear,
            },
            Duration::from_millis(120),
        );
        timeline.advance_at(Duration::from_millis(200));
        assert_eq!(timeline.value(id), Some(0.5));
    }

    #[test]
    fn terminal_frame_is_exact_for_every_tween_easing() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        for (number, easing) in [
            Easing::Linear,
            Easing::Snap,
            Easing::Glide,
            Easing::Bounce,
            Easing::SmoothStep,
            Easing::EaseOutQuint,
            Easing::EaseOutCubic,
        ]
        .into_iter()
        .enumerate()
        {
            let id = AnimationId::new(number as u64);
            timeline.retarget(
                id,
                1.0,
                Motion::TweenEased {
                    duration: Duration::from_millis(100),
                    easing,
                },
            );
        }
        clock.set(Duration::from_millis(100));
        timeline.advance();
        for number in 0..7 {
            assert_eq!(timeline.value(AnimationId::new(number)), Some(1.0));
        }
        assert!(!timeline.is_active());
    }

    #[test]
    fn tween_stays_live_until_the_exact_terminal_timestamp() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        let id = AnimationId::new(8);
        timeline.retarget(id, 1.0, TWEEN);
        clock.set(Duration::from_micros(99_999));
        timeline.advance();
        assert!(timeline.is_active());
        clock.set(Duration::from_millis(100));
        timeline.advance();
        assert_eq!(timeline.value(id), Some(1.0));
        assert!(!timeline.is_active());
    }

    #[test]
    fn reduced_motion_snaps_without_waiting_for_a_frame() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        timeline.retarget(AnimationId::new(1), 1.0, TWEEN);
        clock.set(Duration::from_millis(25));
        timeline.advance();
        timeline.set_reduced_motion(true);
        assert_eq!(timeline.value(AnimationId::new(1)), Some(1.0));
        assert!(!timeline.is_active());
    }

    #[test]
    fn spring_reversal_has_no_value_jump_and_settles_exactly() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        let id = AnimationId::new(2);
        timeline.retarget(
            id,
            1.0,
            Motion::Spring {
                stiffness: 220.0,
                damping: 28.0,
            },
        );
        clock.set(Duration::from_millis(100));
        timeline.advance();
        let before = timeline.value(id).expect("spring");
        timeline.retarget(
            id,
            0.0,
            Motion::Spring {
                stiffness: 220.0,
                damping: 28.0,
            },
        );
        assert_eq!(timeline.value(id), Some(before));
        timeline.settle(id);
        assert_eq!(timeline.value(id), Some(0.0));
        assert!(!timeline.is_active());
    }

    #[test]
    fn beat_tokens_follow_the_design_timing() {
        assert_eq!(Beat::Touch.duration(false), Duration::from_millis(90));
        assert_eq!(Beat::Reveal.duration(false), Duration::from_millis(160));
        assert_eq!(Beat::Unfold.duration(false), Duration::from_millis(240));
        assert_eq!(Beat::Emphasis.duration(false), Duration::from_millis(380));
        assert_eq!(Beat::Scene.duration(false), Duration::from_millis(620));
        assert_eq!(Beat::Unfold.duration(true), Duration::ZERO);
        assert_eq!(Beat::Touch.easing(), Easing::Snap);
        assert_eq!(Beat::Reveal.easing(), Easing::Glide);
        assert_eq!(Beat::Unfold.easing(), Easing::Bounce);
        assert_eq!(Beat::Scene.easing(), Easing::Glide);
    }

    #[test]
    fn semantic_channels_have_unique_trace_ids() {
        let channels = [
            AnimationChannel::Route,
            AnimationChannel::PaneCollapse,
            AnimationChannel::Disclosure,
            AnimationChannel::Hover,
            AnimationChannel::Press,
            AnimationChannel::Selection,
            AnimationChannel::SearchOverlay,
            AnimationChannel::SkeletonContent,
            AnimationChannel::ToastStatus,
            AnimationChannel::GraphCamera,
        ];
        let expected = channels.len();
        let mut ids = channels
            .into_iter()
            .map(AnimationChannel::id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), expected);
        assert_eq!(AnimationChannel::PaneCollapse.beat(), Beat::Reveal);
        assert_eq!(AnimationChannel::Disclosure.beat(), Beat::Unfold);
        assert_eq!(AnimationChannel::Hover.beat(), Beat::Touch);
        assert_eq!(AnimationChannel::ToastStatus.beat(), Beat::Emphasis);
        assert_eq!(AnimationChannel::GraphCamera.beat(), Beat::Scene);
    }
}
