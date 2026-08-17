//! Demand-based checkpoint serving.
//!
//! A **checkpoint** is a sealed version: its full IR held as a zerocopy
//! [`SealedArchive`] plus the [`MaterializedIndex`] needed to replay its
//! neighbours off it. The cache is demand-driven, and every decision is gated by
//! the same [`governor`] leaky bucket the simple serve cache uses:
//!
//! - **Seal the hot ones.** A version whose request count crosses a threshold is
//!   sealed once and retained.
//! - **Replay the cold ones off a seal.** A cold version *near* a hot checkpoint
//!   is served by replaying only its delta off that checkpoint — an O(delta)
//!   rebuild ([`materialize_ref_incremental`]), not a full O(state) graph walk.
//! - **Unseal on demand shift.** Retained checkpoints are byte-bounded by
//!   [`moka`]; as new hot versions are sealed, cold checkpoints are evicted.
//!
//! Concretely, for "fifty people on v1, twenty on v2": v1 crosses the threshold
//! and is sealed once; each v2 request replays v2's small delta off the v1 seal
//! instead of walking v2's whole graph. If the crowd later moves to v3, v1's
//! checkpoint ages out and v3's is sealed in its place.
//!
//! [`materialize_ref_incremental`]: crate::repo::IrRepository::materialize_ref_incremental
//! [`SealedArchive`]: crate::archive::SealedArchive
//! [`MaterializedIndex`]: crate::checkout::MaterializedIndex

use std::num::NonZeroU32;
use std::sync::Arc;

use crate::archive::SealedArchive;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use libpijul::changestore::ChangeStore;
use moka::sync::Cache;

use crate::checkout::MaterializedIndex;
use crate::error::Error;
use crate::refs::Ref;
use crate::repo::IrRepository;
use crate::version::VersionState;

// ---------------------------------------------------------------------------
// Checkpoint
// ---------------------------------------------------------------------------

/// A sealed version retained for fast serving and as a replay base.
pub struct Checkpoint {
    /// The version state this checkpoint captures.
    pub state: VersionState,
    /// The materialized index — the replay base (untouched symbols shared by
    /// `Arc` with any neighbour replayed off it).
    pub index: MaterializedIndex,
    /// The sealed zerocopy archive — served directly on a checkpoint hit.
    pub archive: Arc<SealedArchive>,
}

impl Checkpoint {
    /// Conservative memory footprint: the sealed archive plus the replay index's
    /// symbol bytes (they are separate allocations).
    fn weight(&self) -> u32 {
        let index_bytes: usize = self.index.symbols.values().map(|a| a.len()).sum();
        (self.archive.bytes.len() + index_bytes).min(u32::MAX as usize) as u32
    }
}

// ---------------------------------------------------------------------------
// Serve outcome
// ---------------------------------------------------------------------------

/// How a served state's IR was produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServeStrategy {
    /// A checkpoint for this exact state existed — served from its sealed archive
    /// with **no** graph walk.
    CheckpointHit,
    /// No checkpoint for this state; replayed its delta off an existing
    /// checkpoint — O(delta). `symbols_reoutput` is the number of symbols
    /// actually re-materialized (small when the state is near `base`).
    ReplayedFrom {
        base: VersionState,
        symbols_reoutput: usize,
    },
    /// No usable checkpoint yet; a full graph walk — O(state).
    FullWalk,
}

/// Whether the freshly-served state was retained as a checkpoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retention {
    /// Retained as a checkpoint (a hit, or a demand-crossing promotion).
    Checkpointed,
    /// Not retained — demand is still below the promotion threshold.
    ColdDemand { hits: u64 },
    /// Not retained — the leaky bucket throttled the promotion (a seal burst).
    Throttled { hits: u64 },
}

/// The result of a demand-based serve.
#[derive(Clone)]
pub struct Served {
    /// The sealed archive for the requested state (always present).
    pub archive: Arc<SealedArchive>,
    /// The materialized index for the requested state.
    pub index: MaterializedIndex,
    /// The requested state.
    pub state: VersionState,
    /// How the IR was produced.
    pub strategy: ServeStrategy,
    /// Whether it was retained as a checkpoint.
    pub retention: Retention,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Tuning for a [`CheckpointCache`].
#[derive(Clone, Copy, Debug)]
pub struct CheckpointConfig {
    /// Total bytes of retained checkpoints (archive + replay index). Cold
    /// checkpoints are evicted to stay under this — the "unseal" bound.
    pub max_checkpoint_bytes: u64,
    /// Request count at which a state is promoted to a checkpoint.
    pub promote_at_hits: u64,
    /// Leaky-bucket rate: sustained promotions per second.
    pub seals_per_second: NonZeroU32,
    /// Leaky-bucket burst: promotions allowed back-to-back.
    pub seal_burst: NonZeroU32,
    /// Number of distinct states whose demand is tracked (older demand ages out,
    /// so demand is recency-weighted).
    pub demand_capacity: u64,
}

// ---------------------------------------------------------------------------
// CheckpointCache
// ---------------------------------------------------------------------------

/// A demand-based, leaky-bucket-gated cache of sealed checkpoints, keyed by
/// [`VersionState`].
pub struct CheckpointCache {
    checkpoints: Cache<VersionState, Arc<Checkpoint>>,
    demand: Cache<VersionState, u64>,
    admission: DefaultDirectRateLimiter,
    promote_at: u64,
}

impl CheckpointCache {
    /// Build a checkpoint cache from a [`CheckpointConfig`].
    pub fn new(config: CheckpointConfig) -> Self {
        let checkpoints = Cache::builder()
            .max_capacity(config.max_checkpoint_bytes)
            .weigher(|_state: &VersionState, cp: &Arc<Checkpoint>| cp.weight())
            .build();
        let demand = Cache::builder()
            .max_capacity(config.demand_capacity)
            .build();
        let quota = Quota::per_second(config.seals_per_second).allow_burst(config.seal_burst);
        Self {
            checkpoints,
            demand,
            admission: RateLimiter::direct(quota),
            promote_at: config.promote_at_hits,
        }
    }

    /// **Serve a reference through the demand-based checkpoint cache.**
    ///
    /// 1. Bump the state's demand.
    /// 2. If it is already a checkpoint, serve its sealed archive (no walk).
    /// 3. Otherwise replay it off the hottest checkpoint (O(delta)); if there is
    ///    none, do a full walk.
    /// 4. Seal the result; promote it to a checkpoint iff demand has crossed the
    ///    threshold and the leaky bucket admits it.
    ///
    /// Correctness never depends on any cache decision — every call returns the
    /// reference's exact IR.
    pub fn serve<C>(&self, repo: &IrRepository<C>, reference: &Ref) -> Result<Served, Error>
    where
        C: ChangeStore + Clone + Send + 'static,
        C::Error: std::fmt::Display + Send + Sync + 'static,
    {
        let state = repo.resolve_ref(reference)?.state;
        let hits = self.bump_demand(state);

        // 1. Checkpoint hit — serve the sealed archive with no graph walk.
        if let Some(cp) = self.checkpoints.get(&state) {
            return Ok(Served {
                archive: Arc::clone(&cp.archive),
                index: cp.index.clone(),
                state,
                strategy: ServeStrategy::CheckpointHit,
                retention: Retention::Checkpointed,
            });
        }

        // 2. Miss — replay the delta off the hottest checkpoint, or full-walk.
        let (index, strategy) = match self.hottest_checkpoint() {
            Some(base) => {
                let (index, count) =
                    repo.materialize_ref_incremental_counted(reference, &base.index)?;
                let symbols_reoutput = count.unwrap_or(index.len());
                (
                    index,
                    ServeStrategy::ReplayedFrom {
                        base: base.state,
                        symbols_reoutput,
                    },
                )
            }
            None => (repo.materialize_ref(reference)?, ServeStrategy::FullWalk),
        };

        // 3. Seal the result (borrow + memcpy — cheap).
        let archive = Arc::new(repo.seal_from_index(&index)?);

        // 4. Demand-based, leaky-bucket-gated promotion.
        let retention = if hits < self.promote_at {
            Retention::ColdDemand { hits }
        } else if self.admission.check().is_ok() {
            self.checkpoints.insert(
                state,
                Arc::new(Checkpoint {
                    state,
                    index: index.clone(),
                    archive: Arc::clone(&archive),
                }),
            );
            Retention::Checkpointed
        } else {
            Retention::Throttled { hits }
        };

        Ok(Served {
            archive,
            index,
            state,
            strategy,
            retention,
        })
    }

    /// Record one request against `state`'s demand, returning the new count.
    fn bump_demand(&self, state: VersionState) -> u64 {
        let next = self.demand.get(&state).unwrap_or(0) + 1;
        self.demand.insert(state, next);
        next
    }

    /// The retained checkpoint with the highest current demand — the natural
    /// replay base for a neighbouring cold state. `None` if no checkpoint exists.
    fn hottest_checkpoint(&self) -> Option<Arc<Checkpoint>> {
        let mut best: Option<(u64, Arc<Checkpoint>)> = None;
        for (state, cp) in self.checkpoints.iter() {
            let demand = self.demand.get(&*state).unwrap_or(0);
            if best.as_ref().map(|(d, _)| demand > *d).unwrap_or(true) {
                best = Some((demand, cp));
            }
        }
        best.map(|(_, cp)| cp)
    }

    // ----- introspection (tests / observability) -----

    /// Whether `state` is currently a retained checkpoint.
    pub fn is_checkpointed(&self, state: VersionState) -> bool {
        self.checkpoints.contains_key(&state)
    }

    /// The tracked demand for `state`.
    pub fn demand_of(&self, state: VersionState) -> u64 {
        self.demand.get(&state).unwrap_or(0)
    }

    /// Number of retained checkpoints (after [`run_pending_tasks`](Self::run_pending_tasks)).
    pub fn checkpoint_count(&self) -> u64 {
        self.checkpoints.entry_count()
    }

    /// Total retained checkpoint bytes (after [`run_pending_tasks`](Self::run_pending_tasks)).
    pub fn checkpoint_bytes(&self) -> u64 {
        self.checkpoints.weighted_size()
    }

    /// Force pending eviction/insertion housekeeping so introspection is exact.
    pub fn run_pending_tasks(&self) {
        self.checkpoints.run_pending_tasks();
        self.demand.run_pending_tasks();
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
