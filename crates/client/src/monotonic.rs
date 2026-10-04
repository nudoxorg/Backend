//! Monotonic time shared by both ends of the publication-lease protocol.
//!
//! Lease expiry on the owner and reset budgets on the observer both compare a
//! reading of a monotonic clock with a deadline. Reading `Instant::now()`
//! inline makes every such rule untestable without real sleeps, so each side
//! reads time through [`MonotonicClock`] and owns a [`Deadline`] rather than a
//! bare `Instant` whose comparison direction a caller could invert.

use std::fmt;
use std::time::{Duration, Instant};

/// A source of monotonic time.
///
/// Implementations must never move backwards. Production code uses
/// [`SystemClock`]; tests inject [`ManualClock`] (behind the `test-support`
/// feature) so expiry is exercised at exact instants.
pub trait MonotonicClock: fmt::Debug + Send + Sync {
    /// Reads the clock.
    fn now(&self) -> Instant;
}

/// The operating system's monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// An instant at which something becomes due.
///
/// A deadline is due at exactly its own instant: an operation arriving at
/// `deadline` is already late. Keeping that rule in [`Self::is_due`] stops the
/// `<` / `<=` choice from being re-made (and re-made differently) at every
/// call site.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Deadline(Instant);

impl Deadline {
    /// Returns the deadline `after` past `now`, or `None` when the clock
    /// cannot represent that instant.
    #[must_use]
    pub fn after(now: Instant, after: Duration) -> Option<Self> {
        now.checked_add(after).map(Self)
    }

    /// Returns whether the deadline has been reached at `at`.
    #[must_use]
    pub fn is_due(self, at: Instant) -> bool {
        at >= self.0
    }

    /// Returns how long remains at `at`; zero once the deadline is due.
    #[must_use]
    pub fn remaining(self, at: Instant) -> Duration {
        self.0.saturating_duration_since(at)
    }

    /// Returns the absolute instant represented by this deadline.
    #[must_use]
    pub fn instant(self) -> Instant {
        self.0
    }
}

/// A clock that moves only when a test advances it.
///
/// It is anchored on one real `Instant` so the values it produces are
/// ordinary, comparable `Instant`s. Elapsed time is capped at ten years so
/// that no platform's `Instant` addition can overflow, however a test
/// advances it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct ManualClock {
    origin: Instant,
    elapsed_nanos: std::sync::atomic::AtomicU64,
}

#[cfg(any(test, feature = "test-support"))]
const MANUAL_CLOCK_CEILING_NANOS: u64 = 10 * 365 * 24 * 3600 * 1_000_000_000;

#[cfg(any(test, feature = "test-support"))]
impl ManualClock {
    /// Creates a clock at its origin.
    #[must_use]
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            origin: Instant::now(),
            elapsed_nanos: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Moves the clock forward by `by`, saturating at the ten-year ceiling.
    pub fn advance(&self, by: Duration) {
        let nanos = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        let _ = self.elapsed_nanos.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |elapsed| {
                Some(
                    elapsed
                        .saturating_add(nanos)
                        .min(MANUAL_CLOCK_CEILING_NANOS),
                )
            },
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
impl MonotonicClock for ManualClock {
    fn now(&self) -> Instant {
        let elapsed = self
            .elapsed_nanos
            .load(std::sync::atomic::Ordering::Acquire);
        self.origin + Duration::from_nanos(elapsed)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_is_due_at_its_own_instant_and_not_a_nanosecond_before() {
        let clock = ManualClock::new();
        let start = clock.now();
        let deadline = Deadline::after(start, Duration::from_secs(10)).expect("representable");
        clock.advance(Duration::from_secs(10) - Duration::from_nanos(1));
        assert!(!deadline.is_due(clock.now()));
        assert_eq!(deadline.remaining(clock.now()), Duration::from_nanos(1));
        clock.advance(Duration::from_nanos(1));
        assert!(deadline.is_due(clock.now()));
        assert_eq!(deadline.remaining(clock.now()), Duration::ZERO);
    }

    #[test]
    fn a_deadline_the_clock_cannot_represent_is_refused_not_wrapped() {
        assert_eq!(Deadline::after(Instant::now(), Duration::MAX), None);
    }

    #[test]
    fn the_manual_clock_never_moves_unless_advanced_and_saturates() {
        let clock = ManualClock::new();
        let first = clock.now();
        assert_eq!(clock.now(), first);
        clock.advance(Duration::from_millis(3));
        assert_eq!(clock.now() - first, Duration::from_millis(3));
        // A saturated advance must neither panic nor wrap the clock backwards.
        clock.advance(Duration::MAX);
        let saturated = clock.now();
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.now(), saturated);
    }
}
