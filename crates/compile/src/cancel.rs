//! Cooperative cancellation with an explicit observation/control split.

use crate::errors::ProcessError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Error returned when a cancelled operation is asked to continue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationError;

impl std::fmt::Display for CancellationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation was cancelled")
    }
}

impl std::error::Error for CancellationError {}

/// A cloneable, read-only cancellation observation for one operation.
#[derive(Clone, Debug)]
pub struct Cancellation {
    cancelled: Arc<AtomicBool>,
}

/// Borrowed, read-only cancellation observation accepted by process
/// supervisors and other synchronous operations.
///
/// Implementations should be cheap and side-effect free because a supervisor
/// may poll the observer while a child is running. A borrowed [`AtomicBool`]
/// can be used directly without creating a relay thread or copying the caller's
/// cancellation state.
pub trait CancellationObserver {
    /// Returns whether cancellation has been requested.
    fn is_cancelled(&self) -> bool;

    /// Returns the exact cancellation terminal when cancellation was requested.
    fn checkpoint(&self) -> Result<(), CancellationError> {
        if self.is_cancelled() {
            Err(CancellationError)
        } else {
            Ok(())
        }
    }
}

/// The control capability that can request cancellation of an operation.
#[derive(Clone, Debug)]
pub struct CancelHandle {
    cancelled: Arc<AtomicBool>,
}

impl Cancellation {
    /// Creates an observation and its corresponding control handle.
    #[must_use = "retain the cancel handle while its operation is active"]
    pub fn new() -> (Self, CancelHandle) {
        let cancelled = Arc::new(AtomicBool::new(false));
        (
            Self {
                cancelled: Arc::clone(&cancelled),
            },
            CancelHandle { cancelled },
        )
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Fails when cancellation has been requested.
    ///
    /// # Errors
    ///
    /// Returns [`CancellationError`] when the associated handle has requested
    /// cancellation.
    pub fn checkpoint(&self) -> Result<(), CancellationError> {
        if self.is_cancelled() {
            Err(CancellationError)
        } else {
            Ok(())
        }
    }
}

impl CancellationObserver for Cancellation {
    fn is_cancelled(&self) -> bool {
        Cancellation::is_cancelled(self)
    }
}

impl CancellationObserver for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

impl CancelHandle {
    /// Requests cancellation.  Physical interruption remains best effort;
    /// publication must always check the shared observation before accepting a
    /// result.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation has been requested through this handle.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl From<CancellationError> for ProcessError {
    fn from(_: CancellationError) -> Self {
        Self::Cancelled
    }
}

#[cfg(test)]
mod observer_tests {
    use super::{CancelHandle, Cancellation, CancellationError, CancellationObserver};
    use std::sync::atomic::AtomicBool;

    fn observes_cancelled(observer: &impl CancellationObserver) -> bool {
        observer.is_cancelled()
    }

    #[test]
    fn owned_and_borrowed_observers_share_the_same_read_only_contract() {
        let flag = AtomicBool::new(false);
        let (cancellation, handle) = Cancellation::new();
        assert!(!observes_cancelled(&flag));
        assert!(!observes_cancelled(&cancellation));
        assert_eq!(
            CancellationObserver::checkpoint(&flag),
            Ok::<(), CancellationError>(())
        );
        assert_eq!(cancellation.checkpoint(), Ok(()));

        flag.store(true, std::sync::atomic::Ordering::Release);
        CancelHandle::cancel(&handle);
        assert!(observes_cancelled(&flag));
        assert!(observes_cancelled(&cancellation));
        assert_eq!(
            CancellationObserver::checkpoint(&flag),
            Err(CancellationError)
        );
        assert_eq!(cancellation.checkpoint(), Err(CancellationError));
    }
}
