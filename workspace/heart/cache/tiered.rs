//! L1 (StampedeCache) + L2 (DiskCas) + L3 (any `Cas`, `NoL3` when absent).

use std::time::Duration;

use super::StampedeCache;
use crate::ContentHash;
use bytes::Bytes;

use super::disk::DiskCas;
use super::{Cas, CasError, EvictableCas};

/// Assumed recompute cost when seeding L1 from a lower tier (XFetch bookkeeping).
const PROMOTE_COST: Duration = Duration::from_millis(1);

/// Soft TTL for L1 entries. Cached producer outputs are content-addressed and
/// immutable under a fixed job key; a long TTL just bounds memory residency.
const L1_TTL: Duration = Duration::from_hours(24);

/// Zero-sized sentinel meaning "no L3 configured".
///
/// Used as the default type parameter for [`Tiered`] so that bare `Tiered`
/// resolves to `Tiered<NoL3>` at existing call sites without any annotation.
/// This is the *only* way to express an absent L3 tier — there is no runtime
/// `Option` alongside it. All operations return [`CasError::Unsupported`], which
/// the tiered read/write paths skip via [`CasError::is_unsupported`].
///
/// `NoL3` holds no data; it is not [`EvictableCas`] because it has nothing to
/// evict (and, like any content-addressed L3, is treated as immutable).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoL3;

impl Cas for NoL3 {
    async fn get(&self, _key: ContentHash) -> Result<Option<Bytes>, CasError> {
        Err(CasError::Unsupported("L3 not configured (NoL3)"))
    }

    async fn put(&self, _bytes: Bytes) -> Result<ContentHash, CasError> {
        Err(CasError::Unsupported("L3 not configured (NoL3)"))
    }

    async fn put_keyed(&self, _key: ContentHash, _bytes: Bytes) -> Result<bool, CasError> {
        Err(CasError::Unsupported("L3 not configured (NoL3)"))
    }
}

/// Tiered CAS: in-process stampede cache, optional local disk, an L3 backend.
///
/// `L3` is any [`Cas`] implementation; the default is [`NoL3`], which the
/// composition treats as "tier absent" (all ops return
/// [`CasError::Unsupported`]). Bare `Tiered` therefore resolves to
/// `Tiered<NoL3>` at every existing call site without annotation. The L3 tier is
/// a compile-time type, never a runtime `Option`: absence is `NoL3` and nothing
/// else. (L2 stays a runtime `Option<DiskCas>` — that is a genuine wiring choice,
/// not a type-level redundancy.)
///
/// Read order: L1 → L2 → L3. Hits promote upward.
///
/// Write policy (best-effort lower tiers):
/// - Durable tiers (L2, then L3) are written before L1.
/// - [`CasError::Unsupported`] from L3 is treated as "tier absent" (skip), so
///   a [`NoL3`] tier (or any tier that returns Unsupported) never fails a
///   put/get.
/// - First-write-wins on durable tiers: when L2 already holds the key, L1 is
///   filled from L2 (not from the caller's possibly-different bytes) so the
///   tiers cannot diverge.
pub struct Tiered<L3: Cas = NoL3> {
    l1: StampedeCache<ContentHash, Bytes>,
    l2: Option<DiskCas>,
    l3: L3,
}

impl<L3: Cas> Tiered<L3> {
    /// Build a tiered store with an explicit L3 backend.
    ///
    /// Pass [`NoL3`] to express "no L3"; there is no `Option` around the tier.
    pub fn new(l1_capacity: u64, l2: Option<DiskCas>, l3: L3) -> Self {
        Self {
            l1: StampedeCache::new(l1_capacity, L1_TTL),
            l2,
            l3,
        }
    }

    /// L1 + optional disk + a live L3 backend.
    ///
    /// Use this constructor to wire a real distributed store (e.g.
    /// `registry::StoreCas`) as the L3 tier. The `l2` argument is optional so
    /// callers that want memory + L3 can pass `None`.
    pub fn with_l3(l1_capacity: u64, l2: Option<DiskCas>, l3: L3) -> Self {
        Self::new(l1_capacity, l2, l3)
    }
}

impl Tiered<NoL3> {
    /// Process-default: L1 (256) + disk at `$NUDOX_PARSE_CACHE` / temp, no L3.
    pub fn default_open() -> Result<Self, CasError> {
        Ok(Self::new(256, Some(DiskCas::default_open()?), NoL3))
    }

    /// L1 only (tests / open-failure fallback).
    pub fn memory_only(capacity: u64) -> Self {
        Self::new(capacity, None, NoL3)
    }

    /// L1 + given disk root (tests / explicit wiring).
    pub fn with_disk(capacity: u64, disk: DiskCas) -> Self {
        Self::new(capacity, Some(disk), NoL3)
    }
}

impl<L3: Cas> Cas for Tiered<L3> {
    async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
        if let Some(v) = self.l1.get(&key).await {
            return Ok(Some(v));
        }

        if let Some(l2) = &self.l2 {
            match l2.get(key).await {
                Ok(Some(v)) => {
                    self.l1.insert(key, v.clone(), PROMOTE_COST).await;
                    return Ok(Some(v));
                }
                // `None` and Integrity errors are both misses; the latter has
                // already deleted the blob, so treat it as absent.
                Ok(None) | Err(CasError::Integrity { .. }) => {}
                Err(e) => return Err(e),
            }
        }

        match self.l3.get(key).await {
            Ok(Some(v)) => {
                if let Some(l2) = &self.l2 {
                    let _ = l2.put_keyed(key, v.clone()).await?;
                }
                self.l1.insert(key, v.clone(), PROMOTE_COST).await;
                return Ok(Some(v));
            }
            Ok(None) => {}
            // Unwired / no-op L3 (NoL3) is a soft miss, not a hard failure.
            Err(e) if e.is_unsupported() => {}
            Err(e) => return Err(e),
        }

        Ok(None)
    }

    async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError> {
        let key = ContentHash::of_bytes(&bytes);
        self.put_keyed(key, bytes).await?;
        Ok(key)
    }

    async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
        // Write durable tiers first so a crash after L1 insert still has the blob.
        let mut novel = true;

        if let Some(l2) = &self.l2 {
            novel = l2.put_keyed(key, bytes.clone()).await?;
        } else if self.l1.get(&key).await.is_some() {
            // Memory-only: L1 is the durable face — first-write-wins there too.
            novel = false;
        }

        match self.l3.put_keyed(key, bytes.clone()).await {
            Ok(wrote) => novel = novel && wrote,
            Err(e) if e.is_unsupported() => {}
            Err(e) => return Err(e),
        }

        if novel {
            self.l1.insert(key, bytes, PROMOTE_COST).await;
        } else if self.l1.get(&key).await.is_none() {
            // Durable tier already held the key; promote *that* value into L1
            // so we never let a losing concurrent put poison L1.
            if let Some(l2) = &self.l2
                && let Ok(Some(existing)) = l2.get(key).await
            {
                self.l1.insert(key, existing, PROMOTE_COST).await;
            }
        }

        Ok(novel)
    }
}

/// Eviction drops L1 + L2 only.
///
/// L3 is deliberately untouched: it is content-addressed and immutable (a
/// distributed object store has no per-key delete), so a "wrong" entry is
/// structurally impossible there. `Tiered` implements [`EvictableCas`]
/// unconditionally for any `L3` — the capability depends only on the mutable
/// L1/L2 tiers, never on the L3 type.
impl<L3: Cas> EvictableCas for Tiered<L3> {
    async fn invalidate(&self, key: ContentHash) -> Result<(), CasError> {
        self.l1.invalidate(&key).await;
        if let Some(l2) = &self.l2 {
            l2.invalidate(key).await?;
        }
        Ok(())
    }
}
