//! One deterministic frame clock and retargetable motion tracks.

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
    pub fn set(&mut self, now: Duration) {
        self.now.set(now);
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
}

/// Retargetable motion primitive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Fixed-duration interpolation.
    Tween {
        /// Duration of the interpolation.
        duration: Duration,
    },
    /// Critically damped-ish spring parameters.
    Spring {
        /// Spring stiffness.
        stiffness: f32,
        /// Velocity damping.
        damping: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Track {
    from: f32,
    to: f32,
    value: f32,
    velocity: f32,
    started: Duration,
    motion: Motion,
}

/// Timeline advanced once per frame by the shell.
#[derive(Debug)]
pub struct AnimationTimeline<C> {
    clock: C,
    reduced_motion: bool,
    tracks: BTreeMap<AnimationId, Track>,
    last_frame: Duration,
}

impl<C: FrameClock> AnimationTimeline<C> {
    /// Creates a timeline with one frame clock.
    #[must_use]
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            reduced_motion: false,
            tracks: BTreeMap::new(),
            last_frame: Duration::ZERO,
        }
    }

    /// Enables reduced motion; active tracks snap to their targets on advance.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
    }

    /// Retargets or creates a tween/spring without restarting unrelated tracks.
    pub fn retarget(&mut self, id: AnimationId, target: f32, motion: Motion) {
        let now = self.clock.now();
        let track = self.tracks.entry(id).or_insert(Track {
            from: target,
            to: target,
            value: target,
            velocity: 0.0,
            started: now,
            motion,
        });
        track.from = track.value;
        track.to = target;
        track.started = now;
        track.motion = motion;
    }

    /// Advances every track from the shared frame timestamp.
    pub fn advance(&mut self) {
        let now = self.clock.now();
        self.advance_at(now);
    }

    /// Advances deterministically at an explicit timestamp.
    pub fn advance_at(&mut self, now: Duration) {
        let dt = now.saturating_sub(self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        for track in self.tracks.values_mut() {
            if self.reduced_motion {
                track.value = track.to;
                track.velocity = 0.0;
                continue;
            }
            match track.motion {
                Motion::Tween { duration } => {
                    let elapsed = now.saturating_sub(track.started).as_secs_f32();
                    let amount =
                        (elapsed / duration.as_secs_f32().max(f32::EPSILON)).clamp(0.0, 1.0);
                    let eased = amount * amount * (3.0 - 2.0 * amount);
                    track.value = track.from + (track.to - track.from) * eased;
                }
                Motion::Spring { stiffness, damping } => {
                    let acceleration =
                        (track.to - track.value) * stiffness - track.velocity * damping;
                    track.velocity += acceleration * dt;
                    track.value += track.velocity * dt;
                    if (track.value - track.to).abs() < 0.001 && track.velocity.abs() < 0.001 {
                        track.value = track.to;
                        track.velocity = 0.0;
                    }
                }
            }
        }
    }

    /// Returns the current value of a track.
    #[must_use]
    pub fn value(&self, id: AnimationId) -> Option<f32> {
        self.tracks.get(&id).map(|track| track.value)
    }

    /// Returns whether any track remains active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.tracks
            .values()
            .any(|track| (track.value - track.to).abs() > 0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_clock_makes_retargeting_repeatable() {
        let mut clock = CaptureFrameClock::default();
        let mut timeline = AnimationTimeline::new(clock.clone());
        timeline.retarget(
            AnimationId::new(1),
            1.0,
            Motion::Tween {
                duration: Duration::from_millis(100),
            },
        );
        clock.set(Duration::from_millis(50));
        timeline.advance();
        let first = timeline.value(AnimationId::new(1)).expect("track");
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(1)), Some(first));
        timeline.set_reduced_motion(true);
        clock.set(Duration::from_millis(60));
        timeline.advance();
        assert_eq!(timeline.value(AnimationId::new(1)), Some(1.0));
    }
}
