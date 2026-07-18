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
//! [`SealedArchive`]: nudox_ir_archive::SealedArchive
//! [`MaterializedIndex`]: crate::checkout::MaterializedIndex

use std::num::NonZeroU32;
use std::sync::Arc;

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use libpijul::changestore::ChangeStore;
use moka::sync::Cache;
use nudox_ir_archive::SealedArchive;

use crate::checkout::MaterializedIndex;
use crate::error::VcsError;
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
    ReplayedFrom { base: VersionState, symbols_reoutput: usize },
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
        let demand = Cache::builder().max_capacity(config.demand_capacity).build();
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
    pub fn serve<C>(&self, repo: &IrRepository<C>, reference: &Ref) -> Result<Served, VcsError>
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
                (index, ServeStrategy::ReplayedFrom { base: base.state, symbols_reoutput })
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
                Arc::new(Checkpoint { state, index: index.clone(), archive: Arc::clone(&archive) }),
            );
            Retention::Checkpointed
        } else {
            Retention::Throttled { hits }
        };

        Ok(Served { archive, index, state, strategy, retention })
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
mod tests {
    use super::*;

    use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName};
    use nudox_ir::apply::PristineIntroTable;
    use nudox_ir::kind::KindDiscriminant;
    use nudox_ir::symbol::Visibility;
    use nudox_ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};

    use crate::version::VersionLabel;

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
    }
    fn intro_n(n: u32) -> IntroId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_le_bytes());
        IntroId::from_raw(b)
    }
    fn func(name: &str) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".to_owned(),
            span_start: 0,
            span_end: name.len() as u32,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        };
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]), sig: Default::default(), generics: Box::new([]), wheres: Box::new([]) }),
            EntryPayloadFlags::default(),
        )
    }
    /// `n` symbols; the first `changed` of them carry a `_v{v}` suffix so two
    /// generations differ by exactly `changed` symbols.
    fn table(n: u32, changed: u32, v: u32) -> PristineIntroTable {
        let mut t = PristineIntroTable::new();
        for i in 0..n {
            let name = if i < changed { format!("s{i}_v{v}") } else { format!("s{i}") };
            t.insert_live(intro_n(i), func(&name), None);
        }
        t
    }
    fn ver(s: &str) -> Ref {
        Ref::version(s).unwrap()
    }
    fn cfg(max_bytes: u64, promote_at: u64, per_sec: u32, burst: u32) -> CheckpointConfig {
        CheckpointConfig {
            max_checkpoint_bytes: max_bytes,
            promote_at_hits: promote_at,
            seals_per_second: NonZeroU32::new(per_sec).unwrap(),
            seal_burst: NonZeroU32::new(burst).unwrap(),
            demand_capacity: 1024,
        }
    }

    fn index_eq(a: &MaterializedIndex, b: &MaterializedIndex) -> bool {
        if a.symbols.len() != b.symbols.len() {
            return false;
        }
        a.symbols.iter().all(|(k, v)| b.symbols.get(k).map(|w| w[..] == v[..]).unwrap_or(false))
    }

    /// **The headline scenario: hot v1 is sealed once; cold v2 replays its small
    /// delta off the v1 seal instead of a full walk.**
    #[test]
    fn hot_version_seals_and_cold_neighbour_replays_off_it() {
        const N: u32 = 200;
        const DELTA: u32 = 5;

        let repo = IrRepository::in_memory(pkg(), "main").unwrap();
        repo.record_generation(&table(N, 0, 0)).unwrap().unwrap();
        repo.tag_version(&VersionLabel::new("1.0.0").unwrap()).unwrap();
        repo.record_generation(&table(N, DELTA, 2)).unwrap().unwrap(); // v2 differs by DELTA
        repo.tag_version(&VersionLabel::new("2.0.0").unwrap()).unwrap();

        let v1 = repo.version_state(&VersionLabel::new("1.0.0").unwrap()).unwrap();

        // promote after 3 hits, generous byte + rate budget.
        let cache = CheckpointCache::new(cfg(64 << 20, 3, 1000, 1000));

        // "Fifty people on v1" — first 2 are cold full walks, the 3rd promotes.
        let s1 = cache.serve(&repo, &ver("1.0.0")).unwrap();
        assert_eq!(s1.strategy, ServeStrategy::FullWalk);
        assert!(matches!(s1.retention, Retention::ColdDemand { hits: 1 }));
        cache.serve(&repo, &ver("1.0.0")).unwrap();
        let s3 = cache.serve(&repo, &ver("1.0.0")).unwrap();
        assert!(matches!(s3.retention, Retention::Checkpointed), "3rd hit promotes v1");
        cache.run_pending_tasks();
        assert!(cache.is_checkpointed(v1), "v1 is now a checkpoint");

        // A later v1 request is a pure checkpoint hit — no walk.
        let s4 = cache.serve(&repo, &ver("1.0.0")).unwrap();
        assert_eq!(s4.strategy, ServeStrategy::CheckpointHit);

        // "Twenty people on v2" — v2 is cold, so it replays off the v1 seal.
        let v2_served = cache.serve(&repo, &ver("2.0.0")).unwrap();
        match v2_served.strategy {
            ServeStrategy::ReplayedFrom { base, symbols_reoutput } => {
                assert_eq!(base, v1, "v2 replays off the v1 checkpoint");
                assert!(
                    symbols_reoutput <= DELTA as usize + 2,
                    "v2 re-outputs only its ~{DELTA}-symbol delta, not all {N} (got {symbols_reoutput})"
                );
            }
            other => panic!("expected ReplayedFrom, got {other:?}"),
        }
        assert!(matches!(v2_served.retention, Retention::ColdDemand { hits: 1 }));

        // Correctness: the replayed v2 equals a full materialize of v2.
        let v2_full = repo.materialize_version(&VersionLabel::new("2.0.0").unwrap()).unwrap();
        assert!(index_eq(&v2_served.index, &v2_full), "replayed v2 must equal a full walk");
        assert_eq!(v2_served.index.len(), N as usize);
    }

    /// The leaky bucket gates promotion: with a burst of 1, the second
    /// promotion-eligible version is throttled (served, but not sealed).
    #[test]
    fn leaky_bucket_throttles_promotion() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();
        repo.record_generation(&table(20, 0, 0)).unwrap().unwrap();
        repo.tag_version(&VersionLabel::new("1.0.0").unwrap()).unwrap();
        repo.record_generation(&table(20, 3, 2)).unwrap().unwrap();
        repo.tag_version(&VersionLabel::new("2.0.0").unwrap()).unwrap();

        // promote_at=1 so every serve tries to promote; burst=1 so only one seals.
        let cache = CheckpointCache::new(cfg(64 << 20, 1, 1, 1));
        let a = cache.serve(&repo, &ver("1.0.0")).unwrap();
        assert!(matches!(a.retention, Retention::Checkpointed), "first promotion admitted");
        let b = cache.serve(&repo, &ver("2.0.0")).unwrap();
        assert!(matches!(b.retention, Retention::Throttled { .. }), "second promotion throttled");

        cache.run_pending_tasks();
        assert!(cache.is_checkpointed(repo.version_state(&VersionLabel::new("1.0.0").unwrap()).unwrap()));
        assert!(!cache.is_checkpointed(repo.version_state(&VersionLabel::new("2.0.0").unwrap()).unwrap()));
        // The throttled version was still served correctly.
        assert_eq!(b.index.len(), 20);
    }

    /// Demand shift unseals: under a byte cap that fits one checkpoint, promoting
    /// several distinct versions keeps total retained bytes bounded — cold
    /// checkpoints are evicted.
    #[test]
    fn demand_shift_evicts_cold_checkpoints() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();
        repo.record_generation(&table(120, 0, 0)).unwrap().unwrap();
        for (i, v) in ["1.0.0", "2.0.0", "3.0.0"].iter().enumerate() {
            if i > 0 {
                repo.record_generation(&table(120, 4, i as u32 + 1)).unwrap().unwrap();
            }
            repo.tag_version(&VersionLabel::new(*v).unwrap()).unwrap();
        }

        // Measure one checkpoint's weight with a huge cap.
        let probe = CheckpointCache::new(cfg(1 << 30, 1, 1000, 1000));
        probe.serve(&repo, &ver("1.0.0")).unwrap();
        probe.run_pending_tasks();
        let one = probe.checkpoint_bytes();
        assert!(one > 0);

        // A cap that fits ~1.5 checkpoints. Promote all three (promote_at=1).
        let cap = one + one / 2;
        let cache = CheckpointCache::new(cfg(cap, 1, 1000, 1000));
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            cache.serve(&repo, &ver(v)).unwrap();
        }
        cache.run_pending_tasks();

        assert!(cache.checkpoint_bytes() <= cap, "retained bytes stay under the cap");
        assert!(cache.checkpoint_count() < 3, "cold checkpoints were unsealed under demand");
        assert!(cache.checkpoint_count() >= 1, "at least one checkpoint retained");
    }
}
