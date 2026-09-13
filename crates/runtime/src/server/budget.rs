//! Defines budget behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the budget invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Quantized, lock-free physical byte-credit reservation.
#![allow(
    missing_docs,
    reason = "compact configuration types have complete type-level documentation"
)]
#![allow(
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    reason = "linear reservation tokens are intentionally consumed"
)]

use core::ops::Deref;

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

/// Exact logical retained bytes reported by concrete work.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RetainedBytes(usize);

impl From<usize> for RetainedBytes {
    fn from(bytes: usize) -> Self {
        Self(bytes)
    }
}
impl Deref for RetainedBytes {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Concrete queued or active work whose retained memory is explicitly bounded.
pub trait BoundedWork {
    /// Exact retained bytes while owning runtime credits.
    fn retained_bytes(&self) -> RetainedBytes;
}

impl BoundedWork for RetainedBytes {
    fn retained_bytes(&self) -> RetainedBytes {
        *self
    }
}

/// Non-zero granularity of one byte-credit slab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ByteQuantum(usize);

impl TryFrom<usize> for ByteQuantum {
    type Error = ByteBudgetError;
    fn try_from(bytes: usize) -> Result<Self, Self::Error> {
        if bytes == 0 {
            Err(ByteBudgetError::ZeroQuantum)
        } else {
            Ok(Self(bytes))
        }
    }
}
impl Deref for ByteQuantum {
    type Target = usize;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Fixed logical bytes represented by an integral number of physical credits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteBudget(ByteBudgetFacts);

/// Validated byte-budget geometry exposed without repeating accessor boilerplate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteBudgetFacts {
    pub total: RetainedBytes,
    pub quantum: ByteQuantum,
    pub credits: usize,
}

impl Deref for ByteBudget {
    type Target = ByteBudgetFacts;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl ByteBudget {
    /// Creates an exact integral budget: partial physical credits are rejected.
    pub fn new(total: RetainedBytes, quantum: ByteQuantum) -> Result<Self, ByteBudgetError> {
        if *total == 0 {
            return Err(ByteBudgetError::ZeroBudget);
        }
        if !(*total).is_multiple_of(*quantum) {
            return Err(ByteBudgetError::NonIntegralCredits);
        }
        Ok(Self(ByteBudgetFacts {
            total,
            quantum,
            credits: *total / *quantum,
        }))
    }
}

/// Invalid physical byte-credit configuration.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ByteBudgetError {
    #[error("a byte-credit quantum must be non-zero")]
    ZeroQuantum,
    #[error("a byte budget must be non-zero")]
    ZeroBudget,
    #[error("a byte budget must be an integral number of quantums")]
    NonIntegralCredits,
}

/// Compact, linear proof of `count` reserved credits. It is never copyable.
#[derive(Debug)]
pub(crate) struct ReservedCredits {
    count: usize,
}

pub(crate) fn credits_for(bytes: RetainedBytes, quantum: ByteQuantum) -> usize {
    (*bytes).div_ceil(*quantum)
}

/// A counted resource with O(1) reservation and O(1) reclamation, not one queue cell per byte.
pub(crate) struct CreditPoolCore {
    available: AtomicUsize,
}
pub(crate) type CreditPool = CreditPoolCore;

impl CreditPoolCore {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            available: AtomicUsize::new(capacity),
        }
    }
    pub(crate) fn reserve(&self, count: usize) -> Option<ReservedCredits> {
        if count == 0 {
            return Some(ReservedCredits { count });
        }
        // This atomic owns only the numerical credit invariant. Payload initialization,
        // publication, and coordinate reuse synchronize independently through the slot and
        // ready-bit state machines, so reserving a byte count does not acquire any memory.
        let mut observed = self.available.load(Ordering::Relaxed);
        loop {
            if observed < count {
                return None;
            }
            match self.available.compare_exchange_weak(
                observed,
                observed - count,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(ReservedCredits { count }),
                Err(actual) => observed = actual,
            }
        }
    }
    pub(crate) fn release(&self, reservation: ReservedCredits) {
        if reservation.count == 0 {
            return;
        }
        // A reservation is issued by this exact pool and cannot be duplicated. Under that linear
        // proof, restoration cannot exceed capacity, so no rollback branch is an API state. The
        // RMW needs only the atomic's modification order: it publishes no payload memory.
        let _previous = self
            .available
            .fetch_add(reservation.count, Ordering::Relaxed);
    }
    pub(crate) fn available(&self) -> usize {
        // Structural metrics and waiter selection are explicitly eventually consistent. A waiter
        // always rechecks ordinary admission after registering/arming, so a stale observation can
        // cause only a benign delayed or spurious wake, never lost ownership or over-admission.
        self.available.load(Ordering::Relaxed)
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod tests {
    use loom::{sync::Arc, thread};
    use thiserror::Error;

    use super::CreditPoolCore;

    #[test]
    fn loom_credit_reservation_conserves_capacity() {
        loom::model(|| crate::server::test_report::assert_loom(loom_transition));
    }

    fn loom_transition() -> Result<(), BudgetTestError> {
        let credits = Arc::new(CreditPoolCore::new(1));
        let empty = credits
            .reserve(0)
            .ok_or(BudgetTestError::ZeroReservationMissing)?;
        credits.release(empty);
        if credits.available() != 1 {
            return Err(BudgetTestError::ZeroReservationChangedCapacity);
        }
        let other = Arc::clone(&credits);
        let first = thread::spawn(move || {
            if let Some(reservation) = other.reserve(1) {
                other.release(reservation);
            }
        });
        if let Some(reservation) = credits.reserve(1) {
            credits.release(reservation);
        }
        crate::server::test_report::join(first.join())?;
        let available = credits.available();
        if available != 1 {
            return Err(BudgetTestError::Conservation {
                observed: available,
            });
        }
        if let Some(reservation) = credits.reserve(2) {
            credits.release(reservation);
            return Err(BudgetTestError::OverReservation);
        }
        let available = credits.available();
        if available != 1 {
            return Err(BudgetTestError::Conservation {
                observed: available,
            });
        }
        Ok(())
    }

    #[derive(Debug, Error)]
    enum BudgetTestError {
        #[error("credit test thread panicked")]
        Join(#[from] crate::server::test_report::ThreadPanic),
        #[error("expected one available credit, observed {observed}")]
        Conservation { observed: usize },
        #[error("a two-credit reservation succeeded against one-credit capacity")]
        OverReservation,
        #[error("a zero-credit reservation was unexpectedly unavailable")]
        ZeroReservationMissing,
        #[error("a zero-credit reservation changed pool capacity")]
        ZeroReservationChangedCapacity,
    }
}
