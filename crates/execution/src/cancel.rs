use crate::{Interned, WorkInterner, WorkKey};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A cancellation observation shared by one caller and its execution owner.
#[derive(Clone, Debug)]
pub struct Cancellation {
    cancelled: Arc<AtomicBool>,
}

/// The affine control handle for a [`Cancellation`].
#[must_use = "retain the cancel handle while the associated demand is active"]
pub struct CancelHandle {
    cancelled: Arc<AtomicBool>,
}

impl Cancellation {
    /// Creates a cancellation observation and its one control handle.
    #[must_use = "use both the cancellation observation and its control handle"]
    pub fn new() -> (Self, CancelHandle) {
        let cancelled = Arc::new(AtomicBool::new(false));
        (
            Self {
                cancelled: Arc::clone(&cancelled),
            },
            CancelHandle { cancelled },
        )
    }

    /// Returns the latest cancellation state.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl CancelHandle {
    /// Requests cancellation. Physical interruption remains best effort, but
    /// publication rights are checked against this flag before acceptance.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Failure while registering or observing a bounded waiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaiterError {
    /// The bounded follower envelope is full.
    Full,
    /// No live work exists for the requested key.
    Unknown,
    /// The demand was cancelled before registration completed.
    Cancelled,
    /// The bounded generation or global demand counter overflowed.
    Overflow,
}

impl std::fmt::Display for WaiterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "waiter error: {self:?}")
    }
}

impl std::error::Error for WaiterError {}

/// One bounded read-only demand on coalesced work.
#[must_use = "retain the waiter until its demand is released or completes"]
pub struct Waiter {
    demand: Interned,
}

impl std::fmt::Debug for Waiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiter")
            .field("key", &self.key())
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Waiter {
    /// Returns the work key observed by this waiter.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.demand.key()
    }

    /// Returns a cloneable cancellation observation.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.demand.cancellation()
    }

    /// Requests cancellation of this caller's demand only.
    pub fn cancel(&self) {
        self.demand.cancel();
    }

    /// Returns whether this caller has cancelled its demand.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.demand.is_cancelled()
    }

    /// Consumes the waiter and releases its bounded follower slot.
    pub fn release(self) {
        drop(self);
    }
}

/// Bounded waiter registry. Waiters own their registration through RAII.
pub struct WaiterTable {
    /// The waiter view shares the same versioned per-work demand state and
    /// terminal fanout implementation as [`WorkInterner`].  Keeping this
    /// wrapper preserves the small legacy API without maintaining a second
    /// counter/map lifecycle.
    inner: Arc<WorkInterner>,
}

impl std::fmt::Debug for WaiterTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaiterTable")
            .field("waiters", &self.len())
            .finish()
    }
}

impl WaiterTable {
    /// Creates a table with a global bound on active follower demand.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: WorkInterner::new(capacity, capacity),
        }
    }

    /// Registers one waiter for a work key.
    /// # Errors
    ///
    /// Returns [`WaiterError::Full`] when the bounded table has no remaining
    /// demand slot or [`WaiterError::Overflow`] when its checked counters
    /// cannot advance. Registration is atomic with respect to other callers.
    pub fn register(&self, key: WorkKey) -> Result<Waiter, WaiterError> {
        self.inner
            .register_external_waiter(key)
            .map(|demand| Waiter { demand })
    }

    /// Returns the number of active waiters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.follower_len()
    }

    /// Returns whether no waiter demand is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
