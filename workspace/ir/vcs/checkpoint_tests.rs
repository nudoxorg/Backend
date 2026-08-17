//! Tests for demand-based checkpoint serving.
use super::*;

use crate::wire::PayloadTable;
use crate::wire::{EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

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
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: Default::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}
/// `n` symbols; the first `changed` of them carry a `_v{v}` suffix so two
/// generations differ by exactly `changed` symbols.
fn table(n: u32, changed: u32, v: u32) -> PayloadTable {
    let mut t = PayloadTable::new();
    for i in 0..n {
        let name = if i < changed {
            format!("s{i}_v{v}")
        } else {
            format!("s{i}")
        };
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
    a.symbols
        .iter()
        .all(|(k, v)| b.symbols.get(k).map(|w| w[..] == v[..]).unwrap_or(false))
}

/// **The headline scenario: hot v1 is sealed once; cold v2 replays its small
/// delta off the v1 seal instead of a full walk.**
#[test]
fn hot_version_seals_and_cold_neighbour_replays_off_it() {
    const N: u32 = 200;
    const DELTA: u32 = 5;

    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(N, 0, 0)).unwrap().unwrap();
    repo.tag_version(&VersionLabel::new("1.0.0").unwrap())
        .unwrap();
    repo.record_generation(&table(N, DELTA, 2))
        .unwrap()
        .unwrap(); // v2 differs by DELTA
    repo.tag_version(&VersionLabel::new("2.0.0").unwrap())
        .unwrap();

    let v1 = repo
        .version_state(&VersionLabel::new("1.0.0").unwrap())
        .unwrap();

    // promote after 3 hits, generous byte + rate budget.
    let cache = CheckpointCache::new(cfg(64 << 20, 3, 1000, 1000));

    // "Fifty people on v1" — first 2 are cold full walks, the 3rd promotes.
    let s1 = cache.serve(&repo, &ver("1.0.0")).unwrap();
    assert_eq!(s1.strategy, ServeStrategy::FullWalk);
    assert!(matches!(s1.retention, Retention::ColdDemand { hits: 1 }));
    cache.serve(&repo, &ver("1.0.0")).unwrap();
    let s3 = cache.serve(&repo, &ver("1.0.0")).unwrap();
    assert!(
        matches!(s3.retention, Retention::Checkpointed),
        "3rd hit promotes v1"
    );
    cache.run_pending_tasks();
    assert!(cache.is_checkpointed(v1), "v1 is now a checkpoint");

    // A later v1 request is a pure checkpoint hit — no walk.
    let s4 = cache.serve(&repo, &ver("1.0.0")).unwrap();
    assert_eq!(s4.strategy, ServeStrategy::CheckpointHit);

    // "Twenty people on v2" — v2 is cold, so it replays off the v1 seal.
    let v2_served = cache.serve(&repo, &ver("2.0.0")).unwrap();
    match v2_served.strategy {
        ServeStrategy::ReplayedFrom {
            base,
            symbols_reoutput,
        } => {
            assert_eq!(base, v1, "v2 replays off the v1 checkpoint");
            assert!(
                symbols_reoutput <= DELTA as usize + 2,
                "v2 re-outputs only its ~{DELTA}-symbol delta, not all {N} (got {symbols_reoutput})"
            );
        }
        other => panic!("expected ReplayedFrom, got {other:?}"),
    }
    assert!(matches!(
        v2_served.retention,
        Retention::ColdDemand { hits: 1 }
    ));

    // Correctness: the replayed v2 equals a full materialize of v2.
    let v2_full = repo
        .materialize_version(&VersionLabel::new("2.0.0").unwrap())
        .unwrap();
    assert!(
        index_eq(&v2_served.index, &v2_full),
        "replayed v2 must equal a full walk"
    );
    assert_eq!(v2_served.index.len(), N as usize);
}

/// The leaky bucket gates promotion: with a burst of 1, the second
/// promotion-eligible version is throttled (served, but not sealed).
#[test]
fn leaky_bucket_throttles_promotion() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();
    repo.record_generation(&table(20, 0, 0)).unwrap().unwrap();
    repo.tag_version(&VersionLabel::new("1.0.0").unwrap())
        .unwrap();
    repo.record_generation(&table(20, 3, 2)).unwrap().unwrap();
    repo.tag_version(&VersionLabel::new("2.0.0").unwrap())
        .unwrap();

    // promote_at=1 so every serve tries to promote; burst=1 so only one seals.
    let cache = CheckpointCache::new(cfg(64 << 20, 1, 1, 1));
    let a = cache.serve(&repo, &ver("1.0.0")).unwrap();
    assert!(
        matches!(a.retention, Retention::Checkpointed),
        "first promotion admitted"
    );
    let b = cache.serve(&repo, &ver("2.0.0")).unwrap();
    assert!(
        matches!(b.retention, Retention::Throttled { .. }),
        "second promotion throttled"
    );

    cache.run_pending_tasks();
    assert!(
        cache.is_checkpointed(
            repo.version_state(&VersionLabel::new("1.0.0").unwrap())
                .unwrap()
        )
    );
    assert!(
        !cache.is_checkpointed(
            repo.version_state(&VersionLabel::new("2.0.0").unwrap())
                .unwrap()
        )
    );
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
            repo.record_generation(&table(120, 4, i as u32 + 1))
                .unwrap()
                .unwrap();
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

    assert!(
        cache.checkpoint_bytes() <= cap,
        "retained bytes stay under the cap"
    );
    assert!(
        cache.checkpoint_count() < 3,
        "cold checkpoints were unsealed under demand"
    );
    assert!(
        cache.checkpoint_count() >= 1,
        "at least one checkpoint retained"
    );
}
