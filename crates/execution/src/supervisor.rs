use std::sync::atomic::{AtomicU64, Ordering};

/// A point-in-time count of scheduler lifecycle events.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SupervisorSnapshot {
    /// Number of routes admitted.
    pub admitted: u64,
    /// Number of routes rejected before execution.
    pub rejected: u64,
    /// Number of outputs accepted and published.
    pub completed: u64,
    /// Number of routes that failed.
    pub failed: u64,
    /// Number of routes cancelled before publication.
    pub cancelled: u64,
    /// Number of late or stale worker results.
    pub stale: u64,
    /// Number of requests coalesced behind an existing leader.
    pub coalesced: u64,
}

/// Lock-free event counters for scheduler observability.
pub struct Supervisor {
    admitted: AtomicU64,
    rejected: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    stale: AtomicU64,
    coalesced: AtomicU64,
}

impl Supervisor {
    /// Creates an empty event counter set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            admitted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            stale: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
        }
    }

    /// Records an admitted route.
    pub fn admitted(&self) {
        self.admitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a rejected route.
    pub fn rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an accepted output.
    pub fn completed(&self) {
        self.completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a failed route.
    pub fn failed(&self) {
        self.failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a cancelled route.
    pub fn cancelled(&self) {
        self.cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a stale worker result.
    pub fn stale(&self) {
        self.stale.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a coalesced request.
    pub fn coalesced(&self) {
        self.coalesced.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads all counters with relaxed ordering suitable for metrics.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        SupervisorSnapshot {
            admitted: self.admitted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            stale: self.stale.load(Ordering::Relaxed),
            coalesced: self.coalesced.load(Ordering::Relaxed),
        }
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}
