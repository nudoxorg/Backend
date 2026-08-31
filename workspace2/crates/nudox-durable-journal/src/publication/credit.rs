#[cfg(all(test, feature = "loom-model"))]
use loom::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
#[cfg(not(all(test, feature = "loom-model")))]
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

const ACTIVE: u8 = 1;
const CANCELLED: u8 = 2;
const COMPLETED: u8 = 3;
const COMMITTING: u8 = 4;

#[derive(Debug)]
pub(super) struct CreditPool {
    capacity: usize,
    active: AtomicUsize,
    active_high_water: AtomicUsize,
}

impl CreditPool {
    pub(super) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            active: AtomicUsize::new(0),
            active_high_water: AtomicUsize::new(0),
        })
    }

    pub(super) fn reserve(pool: &Arc<Self>) -> Option<Arc<CreditLease>> {
        let mut active = pool.active.load(Ordering::Acquire);
        loop {
            if active >= pool.capacity {
                return None;
            }
            match pool.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => active = observed,
            }
        }
        update_high_water(&pool.active_high_water, active + 1);
        Some(Arc::new(CreditLease {
            pool: Arc::clone(pool),
            state: AtomicU8::new(ACTIVE),
        }))
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn high_water(&self) -> usize {
        self.active_high_water.load(Ordering::Acquire)
    }

    fn release_counts(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(super) struct CreditLease {
    pool: Arc<CreditPool>,
    state: AtomicU8,
}

impl CreditLease {
    pub(super) fn cancel(&self) -> bool {
        self.state
            .compare_exchange(ACTIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn claim(&self) -> bool {
        self.state
            .compare_exchange(ACTIVE, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == CANCELLED
    }

    pub(super) fn is_committing(&self) -> bool {
        self.state.load(Ordering::Acquire) == COMMITTING
    }

    pub(super) fn complete(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        while state == ACTIVE || state == CANCELLED || state == COMMITTING {
            match self
                .state
                .compare_exchange(state, COMPLETED, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }
}

impl Drop for CreditLease {
    fn drop(&mut self) {
        self.pool.release_counts();
    }
}

/// The pending-side guard cancels only while the command is still ACTIVE. It never releases the
/// shared lease; the queued command remains its owner until the worker observes a terminal.
pub(super) struct PendingLease(pub(super) Arc<CreditLease>);

impl PendingLease {
    pub(super) fn arc(&self) -> Arc<CreditLease> {
        Arc::clone(&self.0)
    }
}

impl Drop for PendingLease {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn update_high_water(high_water: &AtomicUsize, observed: usize) {
    let mut current = high_water.load(Ordering::Relaxed);
    while observed > current {
        match high_water.compare_exchange_weak(
            current,
            observed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[cfg(feature = "loom-model")]
    use loom::{sync::Arc, thread};
    #[cfg(not(feature = "loom-model"))]
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    fn take_lease(pool: &Arc<CreditPool>) -> io::Result<Arc<CreditLease>> {
        CreditPool::reserve(pool).ok_or_else(|| io::Error::other("bounded lease unavailable"))
    }

    #[cfg(not(feature = "loom-model"))]
    #[test]
    fn credit_lease_retains_capacity_until_command_and_pending_drop() -> io::Result<()> {
        let pool = CreditPool::new(1);
        let lease = take_lease(&pool)?;
        let command_lease = Arc::clone(&lease);
        let pending = PendingLease(Arc::clone(&lease));
        drop(lease);

        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        drop(pending);
        assert!(command_lease.is_cancelled());
        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        drop(command_lease);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(not(feature = "loom-model"))]
    #[test]
    fn committing_lease_rejects_cancel_until_terminal_completion() -> io::Result<()> {
        let pool = CreditPool::new(1);
        let lease = take_lease(&pool)?;

        assert!(lease.claim());
        assert!(!lease.cancel());
        lease.complete();
        assert!(!lease.is_cancelled());
        drop(lease);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(not(feature = "loom-model"))]
    #[test]
    fn active_counter_is_the_only_admission_owner() -> io::Result<()> {
        let pool = CreditPool::new(1);
        let lease = take_lease(&pool)?;
        assert!(CreditPool::reserve(&pool).is_none());
        drop(lease);
        drop(take_lease(&pool)?);
        Ok(())
    }

    #[cfg(not(feature = "loom-model"))]
    #[test]
    fn active_high_water_tracks_the_peak_without_a_second_resource_counter() -> io::Result<()> {
        let pool = CreditPool::new(3);
        let first = take_lease(&pool)?;
        let second = take_lease(&pool)?;
        let third = take_lease(&pool)?;
        assert!(CreditPool::reserve(&pool).is_none());
        assert_eq!(pool.active_high_water.load(Ordering::Acquire), 3);
        drop(first);
        drop(second);
        drop(third);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(not(feature = "loom-model"))]
    #[test]
    fn production_credit_lease_model_covers_claim_cancel_drop_and_capacity_races()
    -> Result<(), Box<dyn std::error::Error>> {
        let pool = CreditPool::new(2);
        let first = take_lease(&pool)?;
        let second = take_lease(&pool)?;
        assert!(CreditPool::reserve(&pool).is_none());
        assert_eq!(pool.active_high_water.load(Ordering::Acquire), 2);

        // This deterministic schedule exercises cancellation before claim and proves that a
        // cancelled command still needs the owner-side terminal completion transition.
        assert!(first.cancel());
        assert!(!first.claim());
        assert!(first.is_cancelled());
        first.complete();
        assert!(!first.is_committing());

        // The two threads call the production AtomicU8 transitions on one shared lease. Exactly
        // one can claim ACTIVE; the losing operation must observe the winner's state.
        let barrier = Arc::new(Barrier::new(3));
        let cancel_barrier = Arc::clone(&barrier);
        let claim_barrier = Arc::clone(&barrier);
        let cancel_lease = Arc::clone(&second);
        let claim_lease = Arc::clone(&second);
        let cancel = thread::spawn(move || {
            cancel_barrier.wait();
            cancel_lease.cancel()
        });
        let claim = thread::spawn(move || {
            claim_barrier.wait();
            claim_lease.claim()
        });
        barrier.wait();
        let cancel_won = cancel
            .join()
            .map_err(|_| io::Error::other("cancel transition thread panicked"))?;
        let claim_won = claim
            .join()
            .map_err(|_| io::Error::other("claim transition thread panicked"))?;
        if cancel_won == claim_won {
            return Err(io::Error::other("claim/cancel race had no unique winner").into());
        }
        if cancel_won {
            assert!(second.is_cancelled());
        } else {
            assert!(second.is_committing());
        }
        second.complete();
        drop(first);
        drop(second);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);

        // A released active lease can be admitted again, while the production high-water mark
        // remains the peak of the bounded pool rather than a second live counter.
        let replacement = take_lease(&pool)?;
        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        assert_eq!(pool.active_high_water.load(Ordering::Acquire), 2);
        drop(replacement);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[cfg(feature = "loom-model")]
    #[test]
    fn loom_credit_lease_model_exercises_production_transitions() {
        loom::model(|| {
            let pool = CreditPool::new(2);
            let first = CreditPool::reserve(&pool);
            let second = CreditPool::reserve(&pool);
            assert!(first.is_some());
            assert!(second.is_some());
            assert!(CreditPool::reserve(&pool).is_none());
            assert_eq!(pool.active.load(Ordering::Acquire), 2);
            assert_eq!(pool.active_high_water.load(Ordering::Acquire), 2);

            let (Some(first), Some(second)) = (first, second) else {
                return;
            };
            assert!(first.cancel());
            assert!(!first.claim());
            assert!(first.is_cancelled());
            first.complete();

            let cancel_lease = Arc::clone(&second);
            let claim_lease = Arc::clone(&second);
            let cancel = thread::spawn(move || cancel_lease.cancel());
            let claim = thread::spawn(move || claim_lease.claim());
            let cancel_won = cancel
                .join()
                .unwrap_or_else(|panic_payload| std::panic::resume_unwind(panic_payload));
            let claim_won = claim
                .join()
                .unwrap_or_else(|panic_payload| std::panic::resume_unwind(panic_payload));
            assert_ne!(cancel_won, claim_won);
            if cancel_won {
                assert!(second.is_cancelled());
            } else {
                assert!(second.is_committing());
            }
            second.complete();
            drop(first);
            drop(second);
            assert_eq!(pool.active.load(Ordering::Acquire), 0);

            let replacement = CreditPool::reserve(&pool);
            assert!(replacement.is_some());
            assert_eq!(pool.active.load(Ordering::Acquire), 1);
            assert_eq!(pool.active_high_water.load(Ordering::Acquire), 2);
            drop(replacement);
            assert_eq!(pool.active.load(Ordering::Acquire), 0);
        });
    }
}
