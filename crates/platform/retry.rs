//! The bounded pause schedule shared by every "someone else got there first" retry.
//!
//! A retry that spins without pausing starves behind a hot publisher, and one that waits without a
//! bound hangs behind a permanent holder. Every retry in this crate therefore follows one short,
//! finite schedule, and takes its clock as a parameter so a test can drive it without sleeping.

use std::time::Duration;

/// Pause before each retry. The total (about 320 ms) is far longer than a scan of a small state
/// file and far shorter than any caller's own deadline.
pub(crate) const BACKOFF: [Duration; 10] = [
    Duration::from_millis(1),
    Duration::from_millis(2),
    Duration::from_millis(4),
    Duration::from_millis(8),
    Duration::from_millis(16),
    Duration::from_millis(32),
    Duration::from_millis(64),
    Duration::from_millis(64),
    Duration::from_millis(64),
    Duration::from_millis(64),
];

/// Runs `operation` once per scheduled pause while `again` calls its outcome a lost race, then
/// once more. Whatever the final attempt returns is the result, so a persistent loser sees the
/// last outcome rather than a fabricated one.
pub(crate) fn retry_when<T>(
    mut operation: impl FnMut() -> T,
    again: impl Fn(&T) -> bool,
    backoff: &[Duration],
    mut pause: impl FnMut(Duration),
) -> T {
    for delay in backoff {
        let outcome = operation();
        if !again(&outcome) {
            return outcome;
        }
        pause(*delay);
    }
    operation()
}

#[cfg(test)]
mod tests {
    use super::{BACKOFF, retry_when};
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn a_lost_race_is_retried_with_the_schedule_and_the_last_outcome_is_returned() {
        let attempts = Cell::new(0_usize);
        let slept = Cell::new(Duration::ZERO);
        let outcome = retry_when(
            || {
                attempts.set(attempts.get() + 1);
                attempts.get()
            },
            |_| true,
            &BACKOFF,
            |delay| slept.set(slept.get() + delay),
        );
        assert_eq!(
            outcome,
            BACKOFF.len() + 1,
            "the final attempt is the result"
        );
        assert_eq!(attempts.get(), BACKOFF.len() + 1);
        assert_eq!(slept.get(), BACKOFF.iter().sum::<Duration>());

        attempts.set(0);
        let outcome = retry_when(
            || {
                attempts.set(attempts.get() + 1);
                attempts.get()
            },
            |attempt| *attempt < 3,
            &BACKOFF,
            |_| {},
        );
        assert_eq!(outcome, 3, "the first outcome that is not a lost race wins");
    }

    #[test]
    fn the_schedule_is_bounded() {
        assert!(BACKOFF.iter().sum::<Duration>() < Duration::from_millis(500));
    }
}
