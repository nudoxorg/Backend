//! The one bounded wait for tests that race a real thread (R-Open3, phase 2).
//!
//! A test that starts a worker (a read pool, an actor, an owner) cannot park
//! the executor until the worker is done: nothing tells the executor a thread
//! finished. It has to poll. Polling was written a dozen times, each with its
//! own count (`for _ in 0..100 { yield_now() }`, which is a bound on
//! scheduling luck, not on a condition) or its own sleep and deadline.
//!
//! This is the one copy. It waits on the condition, not on a count: it
//! returns the moment `done` holds, backs off from 100 microseconds to 4 ms
//! so an idle wait does not spin, and gives up only after [`HUNG`], which is
//! long on purpose (a loaded machine starves a thread for seconds; a pass
//! never waits for the bound, a hang costs it once).

use std::fmt::Display;
use std::time::{Duration, Instant};

/// How long a test waits for a thread before it calls it hung.
pub(crate) const HUNG: Duration = Duration::from_secs(20);

/// The pause between two looks: 100 microseconds, doubling to 4 ms.
struct Backoff(Duration);

impl Backoff {
    const fn new() -> Self {
        Self(Duration::from_micros(100))
    }

    fn pause(&mut self) {
        std::thread::park_timeout(self.0);
        self.0 = (self.0 * 2).min(Duration::from_millis(4));
    }
}

/// Polls `done` until it holds; `false` when `bound` passes first. `done`
/// runs at least once and may have side effects (draw a frame, drain a
/// queue): it is the whole body of the wait.
#[must_use]
pub(crate) fn poll(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    let mut backoff = Backoff::new();
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        backoff.pause();
    }
}

/// Waits until `done` holds, or fails saying what never happened.
///
/// # Panics
/// After [`HUNG`], with `never: {what}`.
#[track_caller]
pub(crate) fn until(what: impl Display, done: impl FnMut() -> bool) {
    assert!(poll(HUNG, done), "never: {what}");
}

/// Waits until `read` answers, and returns the answer.
///
/// # Panics
/// After [`HUNG`], with `never: {what}`.
#[track_caller]
pub(crate) fn until_some<T>(what: impl Display, mut read: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + HUNG;
    let mut backoff = Backoff::new();
    loop {
        if let Some(found) = read() {
            return found;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        backoff.pause();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_condition_that_holds_returns_at_once_and_runs_the_body_once() {
        let runs = Cell::new(0);
        assert!(poll(Duration::from_secs(5), || {
            runs.set(runs.get() + 1);
            true
        }));
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn a_condition_that_never_holds_gives_up_at_the_bound_and_says_so() {
        let started = Instant::now();
        assert!(!poll(Duration::from_millis(30), || false));
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(30) && waited < Duration::from_secs(2),
            "waited {waited:?}"
        );
    }

    #[test]
    fn it_waits_for_another_thread_not_for_a_count() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let setter = std::sync::Arc::clone(&flag);
        // Later than any count of scheduler yields could cover.
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            setter.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        until("the other thread set the flag", || {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        });
        thread.join().expect("the setter");
        assert_eq!(until_some("an answer", || Some(3)), 3);
    }
}
