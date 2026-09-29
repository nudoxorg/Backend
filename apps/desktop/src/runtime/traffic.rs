//! Reads first: the window's reads to the owner, counted, so the worker that
//! adds packages lets them through before it starts the next compile.
//!
//! The owner answers one request at a time, and indexing a package holds it
//! for the whole compile. Adding a project's packages back to back therefore
//! made every page read wait behind the next compile, and a page read that
//! takes four round trips (the Library's) waited behind four of them, so the
//! Library did not grow while its packages arrived. Between two packages the
//! worker now waits until no read has been in flight for a moment (the UI's
//! own reads after a package lands start within that moment), bounded so a
//! window that never stops reading cannot stall an install.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Reads waiting on one owner, and when their count last changed.
pub(crate) struct Traffic {
    in_flight: AtomicUsize,
    /// Nanoseconds since [`epoch`].
    changed: AtomicU64,
}

/// The window's reads (one window per process).
static WINDOW: Traffic = Traffic::new();

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

impl Traffic {
    const fn new() -> Self {
        Self { in_flight: AtomicUsize::new(0), changed: AtomicU64::new(0) }
    }

    fn mark(&self) {
        let nanos = u64::try_from(epoch().elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.changed.store(nanos, Ordering::Release);
    }

    fn begin(&'static self) -> Reading {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.mark();
        Reading(self)
    }

    /// Blocks the calling (worker) thread until no read has been in flight
    /// for `calm`, or `deadline` passes. Returns whether the reads went quiet.
    fn yield_to_reads(&self, calm: Duration, deadline: Duration) -> bool {
        let started = Instant::now();
        loop {
            let changed = Duration::from_nanos(self.changed.load(Ordering::Acquire));
            let quiet_since = epoch().elapsed().saturating_sub(changed);
            if self.in_flight.load(Ordering::Acquire) == 0 && quiet_since >= calm {
                return true;
            }
            if started.elapsed() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// One read in flight to the owner, for as long as it lives.
#[must_use = "a read counts only while its guard lives"]
pub(crate) struct Reading(&'static Traffic);

impl Reading {
    /// Counts one of the window's reads from now until the guard drops.
    pub(crate) fn begin() -> Self {
        WINDOW.begin()
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.0.mark();
    }
}

/// [`Traffic::yield_to_reads`] for the window's reads.
pub(crate) fn yield_to_reads(calm: Duration, deadline: Duration) -> bool {
    WINDOW.yield_to_reads(calm, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_waits_while_a_read_is_in_flight_and_goes_once_the_reads_are_quiet() {
        // Its own counter: the window's is shared by every test in the process.
        static TRAFFIC: Traffic = Traffic::new();
        let calm = Duration::from_millis(60);
        let reading = TRAFFIC.begin();
        let started = Instant::now();
        assert!(!TRAFFIC.yield_to_reads(calm, Duration::from_millis(200)), "a read in flight holds the worker until the deadline");
        assert!(started.elapsed() >= Duration::from_millis(200));
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(reading);
        });
        let started = Instant::now();
        assert!(TRAFFIC.yield_to_reads(calm, Duration::from_secs(5)), "the reads went quiet");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(150), "it waited for the read and then a calm moment: {waited:?}");
        release.join().expect("release");
    }
}
