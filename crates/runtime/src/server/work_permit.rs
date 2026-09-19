//! Defines work-permit behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the work-permit invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed work-slot permits backed by one atomic bitmap.
#![allow(
    clippy::as_conversions,
    reason = "the bitmap index is bounded by the exact 64-bit source representation"
)]

use core::num::NonZeroU64;
#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU64, Ordering};

use crate::server::slot::SlotIndex;

/// The bitmap representation intentionally bounds one fabric to machine-word slots.
pub(crate) const MAX_WORK_SLOTS: usize = u64::BITS as usize;

/// Unique permission to use one physical payload coordinate and its matching slot state.
#[derive(Debug)]
pub(crate) struct WorkPermit {
    pub(crate) index: SlotIndex,
}

/// Lock-free fixed-capacity issuer. A returned permit restores exactly the bit it consumed.
pub(crate) struct WorkPermits {
    free: AtomicU64,
    retired: AtomicU64,
}

impl WorkPermits {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected atomic constructor is not const"
    )]
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            free: AtomicU64::new(initial_free_mask(capacity)),
            retired: AtomicU64::new(0),
        }
    }

    pub(crate) fn acquire(&self) -> Option<WorkPermit> {
        let mut observed = self.free.load(Ordering::Acquire);
        loop {
            if observed == 0 {
                return None;
            }
            let bits = NonZeroU64::new(observed)?;
            let index = SlotIndex::from_ready_word(bits);
            let claimed = observed & !index.mask();
            match self.free.compare_exchange_weak(
                observed,
                claimed,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(WorkPermit { index }),
                Err(actual) => observed = actual,
            }
        }
    }

    /// Linear ownership makes duplicate restoration unrepresentable within the runtime protocol.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "consuming the non-copy permit proves this exact bitmap bit is restored once"
    )]
    pub(crate) fn restore(&self, permit: WorkPermit) {
        let WorkPermit { index } = permit;
        self.free.fetch_or(index.mask(), Ordering::Release);
    }

    pub(crate) fn available(&self) -> usize {
        self.free.load(Ordering::Acquire).count_ones() as usize
    }

    /// Consumes a permit whose matching coordinate can never receive another ABA-safe epoch.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "consuming the non-copy permit makes retirement mutually exclusive with restoration"
    )]
    pub(crate) fn retire(&self, permit: WorkPermit) {
        let WorkPermit { index } = permit;
        let bit = index.mask();
        // A permit is linear, so this coordinate is absent from `free`. Recording retirement as a
        // bit preserves the exact physical identity and makes restoration impossible by protocol.
        self.retired.fetch_or(bit, Ordering::Release);
    }

    pub(crate) fn retired(&self) -> usize {
        self.retired.load(Ordering::Acquire).count_ones() as usize
    }
}

const fn initial_free_mask(capacity: usize) -> u64 {
    if capacity == MAX_WORK_SLOTS {
        u64::MAX
    } else {
        (1_u64 << capacity) - 1
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod tests {
    use loom::{sync::Arc, thread};
    use thiserror::Error;

    use super::WorkPermits;

    #[test]
    fn loom_retired_coordinate_never_reissues_under_contention() {
        loom::model(|| crate::server::test_report::assert_loom(retirement_transition));
    }

    fn retirement_transition() -> Result<(), PermitTestError> {
        let permits = Arc::new(WorkPermits::new(2));
        let retired = permits
            .acquire()
            .ok_or(PermitTestError::InitialPermitMissing)?;
        let retired_index = retired.index;
        let retiring = Arc::clone(&permits);
        let retire = thread::spawn(move || retiring.retire(retired));
        let competing = Arc::clone(&permits);
        let acquire = thread::spawn(move || competing.acquire());
        crate::server::test_report::join(retire.join())?;
        if let Some(permit) = crate::server::test_report::join(acquire.join())? {
            if permit.index == retired_index {
                return Err(PermitTestError::RetiredCoordinateReissued);
            }
            permits.restore(permit);
        }
        if permits.retired() != 1 || permits.available() != 1 {
            return Err(PermitTestError::Conservation {
                retired: permits.retired(),
                available: permits.available(),
            });
        }
        let survivor = permits.acquire().ok_or(PermitTestError::SurvivorMissing)?;
        if survivor.index == retired_index {
            return Err(PermitTestError::RetiredCoordinateReissued);
        }
        permits.restore(survivor);
        Ok(())
    }

    #[derive(Debug, Error)]
    enum PermitTestError {
        #[error("initial permit was unavailable")]
        InitialPermitMissing,
        #[error("permit test thread panicked")]
        Join(#[from] crate::server::test_report::ThreadPanic),
        #[error("retired coordinate was issued again")]
        RetiredCoordinateReissued,
        #[error("retirement conservation observed retired={retired}, available={available}")]
        Conservation { retired: usize, available: usize },
        #[error("healthy coordinate was unavailable after retirement")]
        SurvivorMissing,
    }
}
