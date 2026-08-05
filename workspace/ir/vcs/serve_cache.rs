//! A leaky-bucket-gated hot cache for sealed serve archives.
//!
//! ## Why a cache at all
//!
//! Historical replay reconstructs any version's IR from the content-shared
//! pijul graph in `O(state)` — a sanakirja B-tree walk. That is age-independent
//! (Zod 2.0 costs the same as Zod 5.0) and partial-read-friendly, but a
//! *repeated full* read of the same hot version re-walks the graph every time.
//! This cache seals such a version once and serves subsequent full reads
//! straight from the zerocopy archive — no graph walk.
//!
//! ## Why a leaky bucket
//!
//! Admission is **scan-resistant**. A burst of distinct cold versions (someone
//! walking the whole history) must not evict the hot working set. A [`governor`]
//! leaky bucket (GCRA) rate-limits cache *admissions* — the same rate-limit
//! primitive the rest of the workspace uses. Under a burst the gate closes and
//! cold versions are served directly (freshly sealed but **not** stored),
//! leaving the hot set intact. Total memory is separately bounded by the
//! [`moka`] weigher (bytes of archive retained).
//!
//! Correctness never depends on the gate: every serve returns the requested
//! archive. The gate only decides whether to *retain* it.

use std::num::NonZeroU32;
use std::sync::Arc;

use crate::archive::SealedArchive;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use moka::sync::Cache;

use crate::version::VersionState;

// ---------------------------------------------------------------------------
// ServeSource
// ---------------------------------------------------------------------------

/// Where a served archive came from — for observability and tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServeSource {
    /// Cache hit: served from the archive with **no** graph walk.
    Cached,
    /// Cache miss, admitted by the leaky bucket: freshly sealed and stored, so
    /// the next read is a hit.
    SealedAndStored,
    /// Cache miss, throttled by the leaky bucket: freshly sealed and returned,
    /// but **not** stored (a scan is in progress; retaining it would evict the
    /// hot set).
    SealedThrottled,
}

/// A served sealed archive together with how it was produced.
#[derive(Clone)]
pub struct ServedArchive {
    /// The sealed archive (mmap-ready zerocopy bytes). Shareable via `Arc`.
    pub archive: Arc<SealedArchive>,
    /// Whether this came from the cache, or was freshly sealed (and whether it
    /// was retained).
    pub source: ServeSource,
}

// ---------------------------------------------------------------------------
// ServeCache
// ---------------------------------------------------------------------------

/// A leaky-bucket-gated, byte-bounded cache of sealed serve archives, keyed by
/// [`VersionState`].
///
/// - **Memory bound:** [`moka`] weighs each entry by its archive byte length;
///   the cache holds at most `max_bytes` total.
/// - **Admission gate:** a [`governor`] direct leaky bucket admits at most
///   `admissions_per_second` new entries (with a configurable burst). Excess
///   admissions during a burst are refused, so the caller serves the version
///   directly without polluting the cache.
pub struct ServeCache {
    archives: Cache<VersionState, Arc<SealedArchive>>,
    admission: DefaultDirectRateLimiter,
}

impl ServeCache {
    /// Build a cache holding up to `max_bytes` of sealed-archive bytes, admitting
    /// at most `admissions_per_second` new versions (allowing short bursts of up
    /// to `burst`).
    pub fn new(max_bytes: u64, admissions_per_second: NonZeroU32, burst: NonZeroU32) -> Self {
        let archives = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_key: &VersionState, value: &Arc<SealedArchive>| {
                // Weigh by archive size so the byte bound is honoured. Clamp to
                // u32 (moka's weight type); a single archive larger than the
                // whole cache is simply never retained.
                u32::try_from(value.bytes.len()).unwrap_or(u32::MAX)
            })
            .build();
        let quota = Quota::per_second(admissions_per_second).allow_burst(burst);
        Self {
            archives,
            admission: RateLimiter::direct(quota),
        }
    }

    /// Peek the cache without touching the admission gate.
    pub fn get(&self, state: VersionState) -> Option<Arc<SealedArchive>> {
        self.archives.get(&state)
    }

    /// Ask the leaky bucket whether a new admission is allowed *now*. Consumes a
    /// token on success (GCRA); returns `false` when throttled.
    pub fn try_admit(&self) -> bool {
        self.admission.check().is_ok()
    }

    /// Store a freshly-sealed archive (the caller has already passed
    /// [`try_admit`](Self::try_admit)).
    pub fn insert(&self, state: VersionState, archive: Arc<SealedArchive>) {
        self.archives.insert(state, archive);
    }

    /// Force any pending eviction/insertion housekeeping to complete. Test-only:
    /// makes [`entry_count`](Self::entry_count) deterministic immediately after
    /// an insert/eviction.
    pub fn run_pending_tasks(&self) {
        self.archives.run_pending_tasks();
    }

    /// Number of retained archives (after [`run_pending_tasks`](Self::run_pending_tasks)).
    pub fn entry_count(&self) -> u64 {
        self.archives.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(n: u8) -> VersionState {
        VersionState::from_bytes([n; 32])
    }

    /// A fresh bucket admits up to its burst, then throttles — the scan-resistant
    /// property, tested deterministically (all calls happen within one refill
    /// period, so no token is replenished).
    #[test]
    fn leaky_bucket_admits_burst_then_throttles() {
        // 1 admission/sec, burst of 2: the first two admits pass, the third is
        // throttled (a full second would be needed to replenish).
        let cache = ServeCache::new(
            1 << 20,
            NonZeroU32::new(1).unwrap(),
            NonZeroU32::new(2).unwrap(),
        );
        assert!(cache.try_admit(), "1st admit within burst");
        assert!(cache.try_admit(), "2nd admit within burst");
        assert!(!cache.try_admit(), "3rd admit throttled");
        assert!(!cache.try_admit(), "still throttled");
    }

    /// `get` never consumes an admission token, and a stored archive is a hit.
    #[test]
    fn get_is_admission_free_and_hits() {
        let cache = ServeCache::new(
            1 << 20,
            NonZeroU32::new(1).unwrap(),
            NonZeroU32::new(1).unwrap(),
        );
        assert!(cache.get(state(1)).is_none(), "cold miss");
        // Peeking must not drain the bucket.
        assert!(cache.try_admit(), "bucket still full after a peek");
    }
}
