//! One owner credit envelope shared by resident embedding work and its consumers.

use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Finite owner credit controller. This is accounting, not an OS memory ceiling.
#[derive(Clone, Debug)]
pub struct EmbeddingMemoryBudget(Arc<MemoryCredits>);

#[derive(Debug)]
struct MemoryCredits {
    capacity: u64,
    used: AtomicU64,
}

/// A refused reservation, retaining bounded numeric evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingMemoryError {
    /// Credits requested by this allocation envelope.
    pub requested: u64,
    /// Credits remaining when admission refused it.
    pub available: u64,
}

impl fmt::Display for EmbeddingMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "embedding memory requires {} credits; {} remain",
            self.requested, self.available
        )
    }
}
impl std::error::Error for EmbeddingMemoryError {}

/// Affine reservation. Keep it with the allocation until its final shared owner drops.
#[derive(Debug)]
pub struct EmbeddingMemoryReservation {
    budget: EmbeddingMemoryBudget,
    bytes: u64,
}

impl EmbeddingMemoryBudget {
    /// Creates the one explicitly admitted envelope shared by all runtime consumers.
    #[must_use]
    pub fn new(capacity: u64) -> Self {
        Self(Arc::new(MemoryCredits {
            capacity,
            used: AtomicU64::new(0),
        }))
    }

    /// Fixed capacity admitted by the owner.
    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.0.capacity
    }

    /// Current reservations, including retained coordinate/cache/projection owners.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.0.used.load(Ordering::Acquire)
    }

    /// Reserves before allocating; failure never increments the account.
    ///
    /// # Errors
    /// Returns the actual remaining credits if this request cannot fit.
    pub fn reserve(&self, bytes: u64) -> Result<EmbeddingMemoryReservation, EmbeddingMemoryError> {
        let mut used = self.used();
        loop {
            let available = self.0.capacity.saturating_sub(used);
            let next = used
                .checked_add(bytes)
                .filter(|next| *next <= self.0.capacity)
                .ok_or(EmbeddingMemoryError {
                    requested: bytes,
                    available,
                })?;
            match self
                .0
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(EmbeddingMemoryReservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(observed) => used = observed,
            }
        }
    }
}

impl EmbeddingMemoryReservation {
    /// Credits owned by this affine lease.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for EmbeddingMemoryReservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

// The shared version primitive owns a closed immutable slice and its final Drop owner.
// Accounting remains here in compile; the version crate knows no runtime or credit policy.
pub use backend_version::SharedCoordinates as EmbeddingCoordinateValues;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_refuse_overflow_and_release_only_after_final_storage_owner()
    -> Result<(), EmbeddingMemoryError> {
        let budget = EmbeddingMemoryBudget::new(12);
        let reservation = Arc::new(budget.reserve(8)?);
        let values = EmbeddingCoordinateValues::retain(Arc::from([1.0, 2.0]), reservation);
        let retained = values.clone();
        assert!(values.shares_storage(&retained));
        assert_eq!(budget.used(), 8);
        assert_eq!(
            budget.reserve(5).err(),
            Some(EmbeddingMemoryError {
                requested: 5,
                available: 4
            })
        );
        assert!(budget.reserve(u64::MAX).is_err());
        drop(values);
        assert_eq!(budget.used(), 8);
        drop(retained);
        assert_eq!(budget.used(), 0);
        Ok(())
    }

    #[test]
    fn shared_consumers_use_one_account_and_error_paths_release() -> Result<(), EmbeddingMemoryError>
    {
        let budget = EmbeddingMemoryBudget::new(100);
        let compiler = budget.reserve(70)?;
        let query = budget.clone();
        assert!(query.reserve(31).is_err());
        let page = query.reserve(30)?;
        assert_eq!(budget.used(), 100);
        drop(page);
        assert_eq!(query.used(), 70);
        drop(compiler);
        assert_eq!(query.used(), 0);
        Ok(())
    }
    #[test]
    fn concurrent_reservations_never_exceed_capacity_and_hold_until_all_workers_release() {
        const WORKERS: usize = 16;
        let budget = EmbeddingMemoryBudget::new(7);
        let start = Arc::new(std::sync::Barrier::new(WORKERS + 1));
        let held = Arc::new(std::sync::Barrier::new(WORKERS + 1));
        let release = Arc::new(std::sync::Barrier::new(WORKERS + 1));
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..WORKERS {
                let (budget, start, held, release) =
                    (budget.clone(), start.clone(), held.clone(), release.clone());
                workers.push(scope.spawn(move || {
                    start.wait();
                    let reservation = budget.reserve(1);
                    held.wait();
                    release.wait();
                    reservation.is_ok()
                }));
            }
            start.wait();
            held.wait();
            let held = budget.used();
            let refused = budget.reserve(1).is_err();
            release.wait();
            let admitted = workers
                .into_iter()
                .map(|worker| worker.join().expect("worker"))
                .filter(|admitted| *admitted)
                .count();
            assert_eq!(held, 7);
            assert!(refused);
            assert_eq!(admitted, 7);
        });
        assert_eq!(budget.used(), 0);
    }
}
