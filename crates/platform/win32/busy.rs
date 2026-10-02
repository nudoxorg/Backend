//! Bounded retry for files that another process holds for a moment.
//!
//! A virus scanner, the search indexer, or a backup agent routinely opens a
//! file that was just written, without sharing delete access, for as long as it
//! takes to scan it. While it does, renaming that file, replacing it, or opening
//! it with delete access fails with `ERROR_ACCESS_DENIED` or
//! `ERROR_SHARING_VIOLATION` even though nothing is wrong with the request. On
//! a machine with real-time protection about one atomic replace in a hundred
//! hits the window, so a state file that is republished in a loop fails within
//! seconds. Unix has no equivalent: a rename never waits on a reader.
//!
//! The wait is short and bounded. A holder that outlasts it is reported with
//! the original operating-system error, so a genuine permission problem or a
//! genuinely long-lived handle still fails closed instead of hanging.

use std::io;
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
};

/// Pause before each retry. The total (about 320 ms) is far longer than a scan
/// of a small state file and far shorter than any caller's own deadline.
const BACKOFF: [Duration; 10] = [
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

/// Whether `error` is one of the codes Windows reports while a file is held
/// open by another handle that does not share what the caller needs.
pub(crate) fn is_busy(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        .is_some_and(|code| {
            matches!(
                code,
                ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION
            )
        })
}

/// Runs `operation`, repeating it after a short pause while it fails because
/// the file is busy. The operation must have no effect when it fails, which
/// holds for every open and rename in this crate: the kernel either performs
/// the call or refuses it before changing anything.
pub(crate) fn retry_while_busy<T>(operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_with(operation, &BACKOFF, thread::sleep)
}

/// The retry loop with its schedule and clock injected, so tests can drive it
/// without sleeping.
fn retry_with<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    backoff: &[Duration],
    mut pause: impl FnMut(Duration),
) -> io::Result<T> {
    for delay in backoff {
        match operation() {
            Err(error) if is_busy(&error) => pause(*delay),
            outcome => return outcome,
        }
    }
    operation()
}

#[cfg(test)]
mod tests {
    use super::{BACKOFF, is_busy, retry_while_busy, retry_with};
    use std::cell::Cell;
    use std::io;
    use std::time::Duration;

    fn busy() -> io::Error {
        io::Error::from_raw_os_error(5)
    }

    #[test]
    fn busy_codes_are_recognised_and_other_errors_are_not() {
        for code in [5, 32, 33] {
            assert!(is_busy(&io::Error::from_raw_os_error(code)), "{code}");
        }
        for code in [2, 3, 80, 87, 183] {
            assert!(!is_busy(&io::Error::from_raw_os_error(code)), "{code}");
        }
        assert!(!is_busy(&io::Error::other("no os code")));
        assert!(!is_busy(&io::Error::from_raw_os_error(-5)));
    }

    #[test]
    fn a_busy_file_is_retried_with_the_scheduled_pauses_until_it_frees() {
        let attempts = Cell::new(0_usize);
        let pauses = Cell::new(Vec::new());
        let outcome = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 4 {
                    Err(busy())
                } else {
                    Ok("published")
                }
            },
            &BACKOFF,
            |delay| {
                let mut seen = pauses.take();
                seen.push(delay);
                pauses.set(seen);
            },
        );
        assert_eq!(outcome.expect("freed on the fourth attempt"), "published");
        assert_eq!(attempts.get(), 4);
        assert_eq!(
            pauses.take(),
            [
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(4)
            ]
        );
    }

    #[test]
    fn a_failure_that_is_not_busy_is_returned_at_once() {
        let attempts = Cell::new(0_usize);
        let error = retry_with(
            || -> io::Result<()> {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(2))
            },
            &BACKOFF,
            |_| panic!("a missing file must not be waited on"),
        )
        .expect_err("not found stays not found");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn a_holder_that_outlasts_the_schedule_is_reported_with_the_original_error() {
        let attempts = Cell::new(0_usize);
        let slept = Cell::new(Duration::ZERO);
        let error = retry_with(
            || -> io::Result<()> {
                attempts.set(attempts.get() + 1);
                Err(busy())
            },
            &BACKOFF,
            |delay| slept.set(slept.get() + delay),
        )
        .expect_err("a permanent holder is an error");
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(attempts.get(), BACKOFF.len() + 1, "bounded attempts");
        assert_eq!(
            slept.get(),
            BACKOFF.iter().sum::<Duration>(),
            "bounded wait"
        );
        assert!(slept.get() < Duration::from_millis(500));
    }

    #[test]
    fn the_real_clock_waits_out_a_short_hold() {
        let started = std::time::Instant::now();
        let remaining = Cell::new(2_usize);
        retry_while_busy(|| {
            if remaining.get() > 0 {
                remaining.set(remaining.get() - 1);
                Err(busy())
            } else {
                Ok(())
            }
        })
        .expect("two busy answers then success");
        assert!(started.elapsed() >= Duration::from_millis(3));
    }
}
