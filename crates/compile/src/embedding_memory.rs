//! One owner credit envelope shared by resident embedding work and its consumers.

use std::fmt;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Finite owner credit controller. This is accounting, not an OS memory ceiling.
#[derive(Clone, Debug)]
pub struct EmbeddingMemoryBudget(Arc<MemoryCredits>);

#[derive(Debug)]
struct MemoryCredits {
    capacity: u64,
    used: AtomicU64,
    optional: Mutex<Vec<Weak<dyn EmbeddingMemoryReclaimer>>>,
    reclaiming: AtomicBool,
}

/// Optional storage that yields its affine credits to required work on the same account.
/// Implementations must not allocate/reserve credits or block on another worker here.
/// A successful return means storage was dropped; admission always rechecks actual credits.
/// The account retains only a weak reference, so this cannot prolong cache or owner lifetime.
pub trait EmbeddingMemoryReclaimer: fmt::Debug + Send + Sync {
    /// Releases optional least-recent storage toward the current credit shortage.
    fn reclaim(&self, shortage: u64) -> bool;
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
            optional: Mutex::new(Vec::new()),
            reclaiming: AtomicBool::new(false),
        }))
    }

    pub(crate) fn shares_account(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
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

    /// Registers optional storage on this exact shared account. Registration is weak;
    /// stale owners are pruned, and no query/project count ceiling is introduced.
    ///
    /// # Errors
    /// Returns an allocator refusal before changing registration.
    pub fn register_optional(
        &self,
        cache: &Arc<dyn EmbeddingMemoryReclaimer>,
    ) -> Result<(), std::collections::TryReserveError> {
        let mut optional = self
            .0
            .optional
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        optional.retain(|owner| owner.strong_count() != 0);
        optional.try_reserve(1)?;
        optional.push(Arc::downgrade(cache));
        Ok(())
    }

    fn reclaim_optional(&self, shortage: u64) -> bool {
        // Neither account registration nor cache locks are held while invoking user code.
        // Reentry and concurrent reclamation refuse this optional attempt without waiting;
        // normal atomic credit admission remains available even during reclamation.
        if self
            .0
            .reclaiming
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        struct ReclaimGuard<'a>(&'a AtomicBool);
        impl Drop for ReclaimGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = ReclaimGuard(&self.0.reclaiming);
        let count = match self.0.optional.try_lock() {
            Ok(optional) => optional.len(),
            Err(_) => return false,
        };
        for at in 0..count {
            // A bounded snapshot position, without allocating a second registry.
            // Concurrent pruning may skip an optional owner, never grant unleased bytes.
            let weak = match self.0.optional.try_lock() {
                Ok(optional) => optional.get(at).cloned(),
                Err(_) => return false,
            };
            if let Some(cache) = weak.and_then(|weak| weak.upgrade()) {
                if cache.reclaim(shortage) {
                    return true;
                }
            }
        }
        false
    }

    /// Reserves before allocating; failure never increments the account.
    ///
    /// # Errors
    /// Returns the actual remaining credits if this request cannot fit.
    pub fn reserve(&self, bytes: u64) -> Result<EmbeddingMemoryReservation, EmbeddingMemoryError> {
        let mut used = self.used();
        loop {
            let available = self.0.capacity.saturating_sub(used);
            let Some(next) = used
                .checked_add(bytes)
                .filter(|next| *next <= self.0.capacity)
            else {
                if bytes <= self.0.capacity
                    && self.reclaim_optional(bytes.saturating_sub(available))
                {
                    let after = self.used();
                    if after < used {
                        used = after;
                        continue;
                    }
                    return Err(EmbeddingMemoryError {
                        requested: bytes,
                        available: self.0.capacity.saturating_sub(after),
                    });
                }
                return Err(EmbeddingMemoryError {
                    requested: bytes,
                    available: self.0.capacity.saturating_sub(self.used()),
                });
            };
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

    #[derive(Debug)]
    struct OptionalTestCache {
        budget: EmbeddingMemoryBudget,
        leases: Mutex<Vec<EmbeddingMemoryReservation>>,
        calls: AtomicU64,
        recurse: bool,
    }
    impl EmbeddingMemoryReclaimer for OptionalTestCache {
        fn reclaim(&self, _: u64) -> bool {
            assert!(
                self.budget.0.optional.try_lock().is_ok(),
                "callback runs without registry lock"
            );
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.recurse {
                assert!(
                    self.budget.reserve(100).is_err(),
                    "reentry is bounded, not recursive callback"
                );
            }
            self.leases.lock().expect("optional cache").pop().is_some()
        }
    }
    #[test]
    fn optional_cache_yields_to_shared_required_reservations_without_reentry_or_account_cycles()
    -> Result<(), Box<dyn std::error::Error>> {
        let budget = EmbeddingMemoryBudget::new(100);
        let required = budget.reserve(50)?;
        let cache = Arc::new(OptionalTestCache {
            budget: budget.clone(),
            leases: Mutex::new(vec![budget.reserve(20)?, budget.reserve(30)?]),
            calls: AtomicU64::new(0),
            recurse: true,
        });
        let erased: Arc<dyn EmbeddingMemoryReclaimer> = cache.clone();
        budget.register_optional(&erased)?;
        let consumer = budget.clone();
        let first = consumer.reserve(25)?;
        assert_eq!(budget.used(), 95, "required+optional+new affine leases");
        let second = consumer.reserve(20)?;
        assert_eq!(budget.used(), 95);
        assert_eq!(cache.calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            consumer.reserve(6).err(),
            Some(EmbeddingMemoryError {
                requested: 6,
                available: 5
            })
        );
        drop(first);
        drop(second);
        drop(required);
        assert_eq!(budget.used(), 0);
        let weak = Arc::downgrade(&cache);
        drop(erased);
        drop(cache);
        assert!(
            weak.upgrade().is_none(),
            "account registry does not retain cache/owner cycle"
        );
        let full = consumer.reserve(100)?;
        assert_eq!(budget.used(), 100);
        drop(full);
        Ok(())
    }

    #[derive(Debug)]
    struct NoProgress(AtomicU64);
    impl EmbeddingMemoryReclaimer for NoProgress {
        fn reclaim(&self, _: u64) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            true
        }
    }
    #[test]
    fn optional_reclamation_must_release_actual_credits_and_never_discards_for_impossible_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let budget = EmbeddingMemoryBudget::new(10);
        let held = budget.reserve(10)?;
        let fake = Arc::new(NoProgress(AtomicU64::new(0)));
        let erased: Arc<dyn EmbeddingMemoryReclaimer> = fake.clone();
        budget.register_optional(&erased)?;
        assert!(budget.reserve(11).is_err());
        assert_eq!(
            fake.0.load(Ordering::Relaxed),
            0,
            "oversized request cannot evict optional data"
        );
        assert_eq!(
            budget.reserve(1).err(),
            Some(EmbeddingMemoryError {
                requested: 1,
                available: 0
            })
        );
        assert_eq!(
            fake.0.load(Ordering::Relaxed),
            1,
            "false progress cannot spin or grant unleased bytes"
        );
        assert_eq!(budget.used(), 10);
        drop(held);
        Ok(())
    }

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
