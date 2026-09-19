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
