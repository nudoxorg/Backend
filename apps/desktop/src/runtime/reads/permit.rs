//! Read capacity: one permit per admitted read, owned from admission until
//! the last outcome carrying it is dropped.
//!
//! A read occupies the pool for longer than its worker runs. Its payload
//! waits in the outbox until the UI takes it, and a taken partial page can
//! still be landing while the same read's terminal page is queued. The
//! capacity unit therefore travels with the work:
//!
//! ```text
//! Ledger::admit --> ReadPermit (queued job; reclassable while queued)
//!                     |  ReadPermit::share (the worker starts the read)
//!                     v
//!                  PermitShare --clone--> each partial outcome
//!                     |  moved into the terminal outcome
//!                     v
//!       last share dropped (landed or discarded) --> Ledger::release
//! ```
//!
//! Nothing but [`Drop`] gives capacity back, so a read cannot be released
//! twice, and a payload cannot outlive the capacity that admitted it.

use super::Priority;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// How many reads the pool holds at once, and how many of those a hover
/// prefetch may take. The rest stays reserved for reads a view waits on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadLimits {
    reads: NonZeroUsize,
    prefetch: usize,
}

impl ReadLimits {
    /// The window's limits: 64 reads in flight or awaiting the UI, of which
    /// hover prefetch may hold at most 32.
    pub const DEFAULT: Self = Self {
        reads: NonZeroUsize::MIN.saturating_add(63),
        prefetch: 32,
    };

    /// Limits holding at most `reads` reads, of which at most `prefetch`
    /// are prefetches. The prefetch share is clamped below `reads`, so at
    /// least one admission is always reserved for a read a view waits on.
    #[must_use]
    pub const fn new(reads: NonZeroUsize, prefetch: usize) -> Self {
        let most = reads.get() - 1;
        Self {
            reads,
            prefetch: if prefetch < most { prefetch } else { most },
        }
    }

    /// Reads the pool holds at most.
    #[must_use]
    pub const fn reads(self) -> usize {
        self.reads.get()
    }

    /// Prefetches the pool holds at most.
    #[must_use]
    pub const fn prefetch(self) -> usize {
        self.prefetch
    }
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Admitted reads by class, from admission until their last outcome drops.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Held {
    /// Reads a view waits on (including promoted prefetches).
    pub normal: usize,
    /// Hover prefetches.
    pub prefetch: usize,
}

impl Held {
    /// All admitted reads.
    #[must_use]
    pub const fn total(self) -> usize {
        self.normal + self.prefetch
    }

    fn of(&mut self, class: Priority) -> &mut usize {
        match class {
            Priority::Normal => &mut self.normal,
            Priority::Prefetch => &mut self.prefetch,
        }
    }
}

/// Why the pool cannot admit one more read now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Saturation {
    /// Every admission is held by queued, running, or undelivered reads.
    Reads,
    /// Prefetches hold their whole share; the rest is reserved for views.
    PrefetchShare,
}

/// Identity of one started read, unique within its pool.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct ReadId(u64);

/// The pool's capacity accounts. Its lock is a leaf: releasing capacity
/// never takes another lock. Admissions and reclassification may take it
/// from the queue, but displaced permits and models drop outside scheduler locks.
#[derive(Debug)]
pub(super) struct Ledger {
    limits: ReadLimits,
    held: Mutex<Held>,
    next: AtomicU64,
}

impl Ledger {
    pub(super) fn new(limits: ReadLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            held: Mutex::new(Held::default()),
            next: AtomicU64::new(0),
        })
    }

    pub(super) const fn limits(&self) -> ReadLimits {
        self.limits
    }

    /// What is held now.
    pub(super) fn held(&self) -> Held {
        *self.lock()
    }

    /// A callback regression must fail immediately instead of hanging on
    /// the leaf lock; returns a copied count, never a live guard.
    #[cfg(test)]
    pub(super) fn try_held_for_test(&self) -> Option<Held> {
        self.held.try_lock().ok().map(|held| *held)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one read of `class`, or says which bound refused it.
    pub(super) fn admit(self: &Arc<Self>, class: Priority) -> Result<ReadPermit, Saturation> {
        {
            let mut held = self.lock();
            if held.total() >= self.limits.reads() {
                return Err(Saturation::Reads);
            }
            if class == Priority::Prefetch && held.prefetch >= self.limits.prefetch() {
                return Err(Saturation::PrefetchShare);
            }
            *held.of(class) += 1;
        }
        Ok(ReadPermit {
            ledger: Arc::clone(self),
            class,
        })
    }

    fn release(&self, class: Priority) {
        let mut held = self.lock();
        let count = held.of(class);
        // Each permit is counted once at admission and released once by its
        // own drop; a release without its admission would be a ledger bug,
        // and saturating keeps it from wrapping into "full forever".
        debug_assert!(*count > 0, "released a read the ledger does not hold");
        *count = count.saturating_sub(1);
    }

    fn reclass(&self, from: Priority, to: Priority) -> Result<(), Saturation> {
        if from != to {
            let mut held = self.lock();
            if to == Priority::Prefetch && held.prefetch >= self.limits.prefetch() {
                return Err(Saturation::PrefetchShare);
            }
            let count = held.of(from);
            debug_assert!(*count > 0, "reclassed a read the ledger does not hold");
            *count = count.saturating_sub(1);
            *held.of(to) += 1;
        }
        Ok(())
    }
}

/// One admitted read's capacity, owned by its queued job. It is not
/// cloneable: the read starts sharing it only when a worker runs it. A
/// queued read can hand it to another job (an eviction) without returning
/// it to the ledger in between.
pub(super) struct ReadPermit {
    ledger: Arc<Ledger>,
    class: Priority,
}

impl std::fmt::Debug for ReadPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadPermit")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

impl ReadPermit {
    /// A view now waits on this queued read: it stops counting against the
    /// prefetch share. A normal read stays normal.
    pub(super) fn promote(&mut self) {
        // Normal admission has no class-share ceiling.
        let _ = self.reclass(Priority::Normal);
    }

    /// A same-key queued replacement keeps its admission, but must still
    /// respect the prefetch share if its new request is a hover.
    pub(super) fn reclass(&mut self, class: Priority) -> Result<(), Saturation> {
        self.ledger.reclass(self.class, class)?;
        self.class = class;
        Ok(())
    }

    /// The read starts and gets its identity: from here its outcomes share
    /// this capacity, and it returns to the ledger when the last of them is
    /// dropped.
    pub(super) fn share(self) -> PermitShare {
        let id = ReadId(self.ledger.next.fetch_add(1, Ordering::Relaxed));
        PermitShare(Arc::new(Started { permit: self, id }))
    }
}

impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.ledger.release(self.class);
    }
}

/// A started read: the admission it holds and the identity its outcomes
/// carry.
#[derive(Debug)]
struct Started {
    permit: ReadPermit,
    id: ReadId,
}

/// A running read's capacity, shared by the worker and every outcome it
/// publishes (and by any clone of those outcomes).
#[derive(Clone)]
pub(super) struct PermitShare(Arc<Started>);

impl std::fmt::Debug for PermitShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PermitShare")
            .field(&self.0.id)
            .field(&self.0.permit.class)
            .finish()
    }
}

impl PermitShare {
    /// The started read this share belongs to.
    pub(super) fn id(&self) -> ReadId {
        self.0.id
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn limits(reads: usize, prefetch: usize) -> ReadLimits {
        ReadLimits::new(NonZeroUsize::new(reads).expect("nonzero"), prefetch)
    }

    #[test]
    fn a_prefetch_share_never_covers_the_whole_pool() {
        assert_eq!(limits(4, 9).prefetch(), 3, "one admission stays for a view");
        assert_eq!(limits(1, 1).prefetch(), 0);
        assert_eq!(ReadLimits::DEFAULT.reads(), 64);
        assert_eq!(ReadLimits::DEFAULT.prefetch(), 32);
    }

    #[test]
    fn capacity_returns_only_when_the_last_share_drops() {
        let ledger = Ledger::new(limits(2, 1));
        let first = ledger.admit(Priority::Normal).expect("first");
        let second = ledger.admit(Priority::Normal).expect("second");
        assert_eq!(
            ledger.admit(Priority::Normal).err(),
            Some(Saturation::Reads)
        );
        let share = first.share();
        let partial = share.clone();
        drop(share);
        assert_eq!(
            ledger.held().total(),
            2,
            "a queued partial still holds the read"
        );
        drop(partial);
        assert_eq!(ledger.held().total(), 1);
        drop(second);
        assert_eq!(ledger.held(), Held::default());
    }

    #[test]
    fn prefetches_stop_at_their_share_and_a_promotion_returns_it() {
        let ledger = Ledger::new(limits(3, 1));
        let mut hover = ledger.admit(Priority::Prefetch).expect("hover");
        assert_eq!(
            ledger.admit(Priority::Prefetch).err(),
            Some(Saturation::PrefetchShare)
        );
        let view = ledger
            .admit(Priority::Normal)
            .expect("a view's read uses the reserve");
        hover.promote();
        assert_eq!(
            ledger.held(),
            Held {
                normal: 2,
                prefetch: 0
            }
        );
        let again = ledger
            .admit(Priority::Prefetch)
            .expect("the share is free again");
        assert_eq!(
            ledger.admit(Priority::Normal).err(),
            Some(Saturation::Reads)
        );
        drop((hover, view, again));
        assert_eq!(ledger.held(), Held::default());
    }
}
