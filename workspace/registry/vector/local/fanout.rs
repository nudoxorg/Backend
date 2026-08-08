//! Cross-shard fan-out over the working set (09-vector §20.6).
//!
//! Edge has no built-in multi-shard search, so the client owns the fan-out:
//! the [`WorkingSet`] is **one mutable project shard ∪ N read-only baked
//! dep shards** (each behind its own store actor), and
//! [`WorkingSet::search_all`] queries every shard concurrently and merges
//! top-k.
//!
//! # Why merging by raw score is valid
//!
//! Every quantized shard searches under the frozen §20.6 comparability
//! rule — `rescore = true, oversampling = 2.0` — enforced *by construction*
//! in [`super::store`]'s request builder from the shard's own schema
//! ([`LocalShardStore::rescore_policy`]). Rescoring re-scores the
//! oversampled candidates against the f32 originals on disk, so every score
//! leaving any shard is an exact f32 cosine in the same model geometry;
//! the f32 project shard needs no rescore. Cross-shard raw-score merging
//! is therefore exact, with a deterministic tie-break (score descending,
//! then [`PointId`] ascending).
//!
//! Hits carry their payload through the merge — notably the `"package"`
//! facet, which the UI uses to label where each result came from.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::vector::core::{JinaCodeV2, SearchHit, SearchRequest, StoreError, VectorStore};
use heart::PackageId;
use tokio::sync::RwLock;

use super::store::LocalShardStore;

/// The working set shared between searchers (read) and the install/evict
/// flow (write). Writers only ever swap shard registrations — a search
/// holds its own `Arc` clones, so an in-flight query is never interrupted
/// by an install (evictions additionally wait for idle, §20.4).
pub type SharedWorkingSet = Arc<RwLock<WorkingSet>>;

/// One mutable project store plus the admitted read-only dep-shard stores,
/// keyed (and iterated) in deterministic [`PackageId`] order.
pub struct WorkingSet {
    project: Arc<LocalShardStore>,
    deps: BTreeMap<PackageId, Arc<LocalShardStore>>,
}

impl WorkingSet {
    /// A working set of just the project shard; dep shards register as the
    /// install flow admits them.
    pub fn new(project: Arc<LocalShardStore>) -> Self {
        Self {
            project,
            deps: BTreeMap::new(),
        }
    }

    /// Wrap into the shared handle used across the search / install planes.
    pub fn into_shared(self) -> SharedWorkingSet {
        Arc::new(RwLock::new(self))
    }

    /// The always-resident mutable project store.
    pub fn project(&self) -> &Arc<LocalShardStore> {
        &self.project
    }

    /// Register (or upgrade — whole-shard swap) a read-only dep shard.
    /// Returns the store it replaced, if any, so the caller can close it.
    pub fn insert_dep(
        &mut self,
        package: PackageId,
        store: Arc<LocalShardStore>,
    ) -> Option<Arc<LocalShardStore>> {
        self.deps.insert(package, store)
    }

    /// Deregister a dep shard (eviction). Returns the removed store; the
    /// caller closes it and deletes its directory (see [`super::depshard`]).
    pub fn remove_dep(&mut self, package: &PackageId) -> Option<Arc<LocalShardStore>> {
        self.deps.remove(package)
    }

    /// The resident dep packages — the "installed" input to
    /// [`super::hotset::diff_plan`].
    pub fn resident_packages(&self) -> BTreeSet<PackageId> {
        self.deps.keys().copied().collect()
    }

    /// Number of resident dep shards (UI: "project + N hot packages").
    pub fn dep_count(&self) -> usize {
        self.deps.len()
    }

    /// Fan the request out to the project shard and every dep shard
    /// **concurrently** (one task per shard actor), then merge by exact raw
    /// score (see module docs) and truncate to `request.limit`.
    ///
    /// Any shard failure fails the whole search — a silently narrower scope
    /// is forbidden (§20.4/§20.9); the caller decides whether to retry or
    /// remote-route.
    pub async fn search_all(
        &self,
        request: SearchRequest<JinaCodeV2>,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let limit = request.limit;
        let mut tasks = tokio::task::JoinSet::new();
        for store in std::iter::once(&self.project).chain(self.deps.values()) {
            let store = Arc::clone(store);
            let request = request.clone();
            tasks.spawn(async move { VectorStore::<JinaCodeV2>::search(&*store, request).await });
        }

        let mut per_shard = Vec::with_capacity(1 + self.deps.len());
        while let Some(joined) = tasks.join_next().await {
            let hits = joined
                .map_err(|err| StoreError::Backend(format!("fan-out task failed: {err}")))??;
            per_shard.push(hits);
        }
        Ok(merge_hits(per_shard, limit))
    }
}

/// Merge per-shard result lists into one best-first list of at most `limit`
/// hits.
///
/// Ordering is fully deterministic: score **descending**, ties broken by
/// [`PointId`] **ascending**. No dedup is performed — point ids are UUIDv5
/// of symbol ids and a symbol lives in exactly one shard.
///
/// Non-finite scores are handled by [`compare_hits`] rather than a bare
/// `f32::total_cmp`: a real embedding backend never emits NaN, but a
/// poisoned or malformed shard could, and `total_cmp`'s IEEE total order
/// places a positive-signed NaN *above* `+Infinity` — meaning a raw
/// `total_cmp` sort would let a broken score win the top rank over a
/// legitimate, if extreme, one. NaN carries no similarity information at
/// all, so it sinks to the worst rank instead.
pub fn merge_hits(per_shard: Vec<Vec<SearchHit>>, limit: usize) -> Vec<SearchHit> {
    let mut merged: Vec<SearchHit> = per_shard.into_iter().flatten().collect();
    merged.sort_by(compare_hits);
    merged.truncate(limit);
    merged
}

/// Total order over [`SearchHit`]s: score descending, NaN scores sunk to
/// the worst rank (see [`merge_hits`]), [`PointId`] ascending as the final
/// tiebreak so no two distinct hits ever compare `Equal`.
fn compare_hits(a: &SearchHit, b: &SearchHit) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (a.score.is_nan(), b.score.is_nan()) {
        (false, false) => b.score.total_cmp(&a.score),
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater, // a is NaN → a ranks worse → sorts after b
        (false, true) => Ordering::Less,    // b is NaN → b ranks worse → a sorts before b
    }
    .then_with(|| a.id.cmp(&b.id))
}
