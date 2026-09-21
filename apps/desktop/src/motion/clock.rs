//! A small, allocation-free clock seam shared by live motion and captures.
//!
//! GPUI owns the live frame loop, but a screenshot is a test of a particular
//! frame rather than a race against wall time. `AnimationClock` lets a caller
//! advance the same state by an exact timestamp. The live path calls `tick`,
//! while preview and screenshot harnesses call `advance_to`; both return only
//! the elapsed duration the spring integrator should consume.

use std::time::Duration;

/// Timestamp and ordinal of one rendered animation frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Frame {
    /// Monotonically increasing frame ordinal.
    pub(crate) ordinal: u64,
    /// Elapsed time from the start of the capture or session.
    pub(crate) elapsed: Duration,
}

/// Monotonic clock for animated state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AnimationClock {
    elapsed: Duration,
    ordinal: u64,
}

impl AnimationClock {
    /// Creates a clock at frame zero.
    pub(crate) const fn new() -> Self {
        Self {
            elapsed: Duration::ZERO,
            ordinal: 0,
        }
    }

    /// Returns the current frame stamp without advancing it.
    pub(crate) const fn frame(self) -> Frame {
        Frame {
            ordinal: self.ordinal,
            elapsed: self.elapsed,
        }
    }

    /// Advances by one live frame. Large stalls are clamped to the same 32ms
    /// bound used by [`crate::motion::spring::Spring`] so a resumed window
    /// cannot teleport across an entire transition.
    pub(crate) fn tick(&mut self, delta: Duration) -> Duration {
        let delta = delta.min(Duration::from_millis(32));
        self.elapsed = self.elapsed.saturating_add(delta);
        self.ordinal = self.ordinal.saturating_add(1);
        delta
    }

    /// Advances to an exact capture timestamp and returns the forward delta.
    /// Replaying an earlier timestamp is a no-op, which makes a harness safe
    /// to call while it retries a frame after a transient surface settles.
    pub(crate) fn advance_to(&mut self, timestamp: Duration) -> Duration {
        if timestamp <= self.elapsed {
            return Duration::ZERO;
        }
        let delta = timestamp.saturating_sub(self.elapsed);
        self.elapsed = timestamp;
        self.ordinal = self.ordinal.saturating_add(1);
        delta
    }

    /// Starts a fresh deterministic run without allocating a new clock.
    pub(crate) const fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
        self.ordinal = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::AnimationClock;
    use std::time::Duration;

    #[test]
    fn capture_timestamps_are_monotonic_and_replay_is_idempotent() {
        let mut clock = AnimationClock::new();
        assert_eq!(
            clock.tick(Duration::from_millis(40)),
            Duration::from_millis(32)
        );
        assert_eq!(clock.frame().elapsed, Duration::from_millis(32));
        assert_eq!(
            clock.advance_to(Duration::from_millis(20)),
            Duration::ZERO,
            "a retry of an earlier capture frame must not reverse motion"
        );
        assert_eq!(clock.frame().elapsed, Duration::from_millis(32));
    }

    #[test]
    fn capture_advances_once_to_the_exact_requested_frame() {
        let mut clock = AnimationClock::new();
        assert_eq!(
            clock.advance_to(Duration::from_millis(240)),
            Duration::from_millis(240)
        );
        assert_eq!(
            clock.frame(),
            super::Frame {
                ordinal: 1,
                elapsed: Duration::from_millis(240),
            }
        );
        assert_eq!(clock.advance_to(Duration::from_millis(240)), Duration::ZERO);
        assert_eq!(
            clock.frame().ordinal,
            1,
            "replaying a frame cannot spawn work"
        );
        clock.reset();
        assert_eq!(
            clock.frame(),
            super::Frame {
                ordinal: 0,
                elapsed: Duration::ZERO
            }
        );
    }
}
