//! Pacing shared by every loop that asks an owner again after it was lost.

use std::time::Duration;

/// Exponential pacing: each step doubles the previous one up to a ceiling.
///
/// The type only counts. The loop that owns it sleeps, because each loop has
/// its own deadline and its own cancellation to observe while it waits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Backoff {
    next: Duration,
    ceiling: Duration,
}

impl Backoff {
    /// Starts at `first` and never exceeds `ceiling`.
    #[must_use]
    pub const fn new(first: Duration, ceiling: Duration) -> Self {
        Self {
            next: first,
            ceiling,
        }
    }

    /// Returns the current step and doubles it, up to the ceiling.
    pub fn step(&mut self) -> Duration {
        let step = self.next.min(self.ceiling);
        self.next = step.saturating_mul(2).min(self.ceiling);
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_double_to_the_ceiling_and_stay_there() {
        let mut backoff = Backoff::new(Duration::from_millis(25), Duration::from_millis(500));
        let steps: Vec<_> = (0..8).map(|_| backoff.step().as_millis()).collect();
        assert_eq!(steps, [25, 50, 100, 200, 400, 500, 500, 500]);
    }

    #[test]
    fn a_first_step_above_the_ceiling_is_clamped() {
        let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(1));
        assert_eq!(backoff.step(), Duration::from_secs(1));
        assert_eq!(backoff.step(), Duration::from_secs(1));
    }
}
