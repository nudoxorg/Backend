//! Adversarial tests for hotset (admission flap, unknown-fields state file),
//! lock (release on drop, exact error variant), and compact (threshold
//! boundary exactness, shrink verification).
//!
//! Spec anchors: 09-vector §20.4 (hotset), §13.5 (failure modes),
//! 09c §1.1 (single-writer lock).

mod common;

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

use common::*;
use heart::PackageId;
use vector_core::{AdmissionBudget, StoreError, VectorStore, NAMESPACE_NUDOX};
use vector_local::{
    AdmissionState, HotSetManager, InstallPlan, LocalShardStore, PackageStats,
    COMPACT_DELETED_RATIO, COMPACT_IDLE, COMPACT_UPSERT_THRESHOLD, CompactPolicy, ShardLock,
    diff_plan,
};

fn pkg(name: &str) -> PackageId {
    PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
}

// ─── Area 9: hotset admission flap ───────────────────────────────────────────

/// `diff_plan` between identical admission outcomes and identical installed sets
/// must be the empty plan (idempotent).
#[test]
fn diff_plan_identical_states_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let budget = AdmissionBudget { budget_bytes: 10_000, project_ram: 0 };
    let mut manager = HotSetManager::open(dir.path().join("hotset.json"), budget);

    let now = 5_000_000u64;
    manager.upsert_package(pkg("a"), 400, true, 0.5, now);
    manager.upsert_package(pkg("b"), 400, true, 0.4, now);

    let installed = BTreeSet::from([pkg("a"), pkg("b")]);

    let plan1 = manager.plan(now, &installed);
    let plan2 = manager.plan(now, &installed);

    assert!(
        plan1.is_empty(),
        "plan for already-installed admitted packages must be empty: {plan1:?}"
    );
    assert_eq!(plan1, plan2, "diff_plan must be idempotent for identical states");
}

/// Oscillating stats (alternating between two configurations) must not thrash:
/// the plan computed from each state is correct for THAT state, but applying
/// it brings the installed set into agreement so the NEXT plan is empty.
#[test]
fn diff_plan_oscillating_stats_no_thrash() {
    let dir = tempfile::tempdir().unwrap();
    let budget = AdmissionBudget { budget_bytes: 500, project_ram: 0 };
    let now = 6_000_000u64;

    // State A: `a` admitted (score wins), `b` not (score loses).
    let mut manager_a = HotSetManager::open(dir.path().join("hotset_a.json"), budget);
    manager_a.upsert_package(pkg("a"), 400, true, 0.9, now);
    manager_a.upsert_package(pkg("b"), 400, false, 0.0, now);

    // State B: `b` admitted, `a` not.
    let mut manager_b = HotSetManager::open(dir.path().join("hotset_b.json"), budget);
    manager_b.upsert_package(pkg("a"), 400, false, 0.0, now);
    manager_b.upsert_package(pkg("b"), 400, true, 0.9, now);

    let plan_a_empty = manager_a.plan(now, &BTreeSet::from([pkg("a")]));
    let plan_b_empty = manager_b.plan(now, &BTreeSet::from([pkg("b")]));

    // Converged state for each: plan must be empty.
    assert!(
        plan_a_empty.is_empty(),
        "state A converged (a installed, a admitted): plan must be empty: {plan_a_empty:?}"
    );
    assert!(
        plan_b_empty.is_empty(),
        "state B converged (b installed, b admitted): plan must be empty: {plan_b_empty:?}"
    );

    // Non-converged state: install the wrong package → plan says to swap.
    let plan_a_wrong = manager_a.plan(now, &BTreeSet::from([pkg("b")]));
    assert_eq!(plan_a_wrong.install, vec![pkg("a")], "should install a");
    assert_eq!(plan_a_wrong.evict, vec![pkg("b")], "should evict b");

    // After applying the swap, re-planning is empty (no further thrash).
    let plan_after_swap = manager_a.plan(now, &BTreeSet::from([pkg("a")]));
    assert!(plan_after_swap.is_empty(), "after applying the swap plan, next plan must be empty");
}

/// State file with UNKNOWN JSON fields written by a "newer version".
///
/// CONTRACT PINNED: `serde_json::from_slice` with `#[serde(deny_unknown_fields)]`
/// would fail; without it (the current implementation) unknown fields are
/// silently ignored.  This test pins the current behavior — unknown fields
/// are tolerated and the known fields load correctly.
///
/// If `deny_unknown_fields` is ever added, the corrupt-state path would be
/// triggered, returning empty stats.  Update this test accordingly.
#[test]
fn hotset_state_file_with_unknown_fields_loads_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("hotset.json");
    let budget = AdmissionBudget { budget_bytes: 10_000, project_ram: 0 };

    let now = 7_000_000u64;

    // Write a state file with both known fields and unknown fields that a
    // future version might have added.
    let raw_json = format!(
        r#"{{
            "stats": {{
                "{pkg_key}": {{
                    "ram_estimate": 400,
                    "is_direct": true,
                    "ref_density": 0.5,
                    "query_hit_ema": {{"last_update_secs": {now}, "value": 0.1}},
                    "pinned": false,
                    "UNKNOWN_FUTURE_FIELD": "value from newer version",
                    "ANOTHER_UNKNOWN": 42
                }}
            }},
            "SCHEMA_VERSION_FIELD_FROM_FUTURE": 99,
            "new_global_setting": true
        }}"#,
        pkg_key = pkg("a"),
        now = now,
    );
    std::fs::write(&state_path, raw_json.as_bytes()).unwrap();

    let manager = HotSetManager::open(state_path, budget);

    // CONTRACT: unknown fields tolerated — known package `a` loads correctly.
    assert!(
        manager.state().stats.contains_key(&pkg("a")),
        "package a must load correctly despite unknown JSON fields in state file"
    );
    let stats = &manager.state().stats[&pkg("a")];
    assert_eq!(stats.ram_estimate, 400);
    assert!(stats.is_direct);
    assert!((stats.ref_density - 0.5).abs() < 1e-6);
    assert!(!stats.pinned);
}

/// `apply_plan` with an empty plan is a no-op: working set unchanged, no errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_plan_empty_plan_is_noop() {
    use std::collections::BTreeMap;
    use vector_local::{InstallPlan, apply_plan};

    let project_dir = tempfile::tempdir().unwrap();
    let project = open_mutable_f32(project_dir.path()).await;
    let ws = common::settle_and_wrap(project).await;

    let empty_plan = InstallPlan { install: vec![], evict: vec![], over_budget: false };
    let result = apply_plan(
        &empty_plan,
        &BTreeMap::new(),
        &NoopFetcher,
        &f32_schema(),
        project_dir.path(),
        &ws,
    )
    .await
    .unwrap();

    assert!(result.is_empty(), "empty plan apply must return empty outcomes");
    assert!(ws.read().await.resident_packages().is_empty(), "working set must be unchanged");
}

struct NoopFetcher;
#[async_trait::async_trait]
impl vector_local::ArtifactFetcher for NoopFetcher {
    async fn fetch(&self, h: &heart::ContentHash) -> Result<Vec<u8>, vector_local::FetchError> {
        Err(vector_local::FetchError::NotFound(*h))
    }
}

// ─── Area 10: lock ────────────────────────────────────────────────────────────

/// Lock is released on drop: a second opener succeeds after the first lock
/// drops (without calling `close()` on a store).
#[test]
fn lock_released_on_drop_second_opener_succeeds() {
    let dir = tempfile::tempdir().unwrap();

    {
        let lock = ShardLock::acquire(dir.path()).expect("first acquire must succeed");
        // Second attempt while first is held: must fail.
        let err = ShardLock::acquire(dir.path()).unwrap_err();
        match &err {
            StoreError::Io(io) => {
                assert_eq!(
                    io.kind(),
                    ErrorKind::WouldBlock,
                    "held lock must be WouldBlock: {io:?}"
                );
            }
            other => panic!("held lock must be Io(WouldBlock), got {other:?}"),
        }
        drop(lock); // Explicit drop; OS releases the advisory lock.
    }

    // After drop, the second opener must succeed.
    let second = ShardLock::acquire(dir.path());
    assert!(
        second.is_ok(),
        "lock must be released on drop; second opener failed: {:?}",
        second.err()
    );
}

/// Lock held → error is exactly `StoreError::Io(WouldBlock)` and the
/// error message contains the path.
#[test]
fn lock_held_error_is_io_would_block_with_path() {
    let dir = tempfile::tempdir().unwrap();
    let _first = ShardLock::acquire(dir.path()).expect("first acquire");

    let err = ShardLock::acquire(dir.path()).unwrap_err();
    match &err {
        StoreError::Io(io) => {
            assert_eq!(
                io.kind(),
                ErrorKind::WouldBlock,
                "lock contention must be WouldBlock, got {io:?}"
            );
            // The error message must mention the path so the user knows what to do.
            let msg = io.to_string();
            let path_str = dir.path().to_string_lossy();
            assert!(
                msg.contains(path_str.as_ref()),
                "lock error message must contain the shard path {:?}; got: {msg:?}",
                dir.path()
            );
        }
        other => panic!("lock contention must be StoreError::Io(WouldBlock), got {other:?}"),
    }
}

/// The lock file itself is not part of the shard data — it exists in the
/// directory but must be absent from a packed artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_file_excluded_from_packed_artifact() {
    use vector_local::{LOCK_FILE, pack_shard};

    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    // The lock file is created by opening mutable; it should exist on disk.
    assert!(
        dir.path().join(LOCK_FILE).exists(),
        "lock file must exist in the shard dir after open"
    );

    let (artifact, hash) = pack_shard(dir.path()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("baked");
    vector_local::unpack_shard(&artifact, &hash, &dest).unwrap();

    assert!(
        !dest.join(LOCK_FILE).exists(),
        "lock file must NOT be present in the unpacked artifact"
    );
}

// ─── Area 11: compact policy threshold boundary exactness ────────────────────

/// `should_compact` at exactly COMPACT_UPSERT_THRESHOLD - 1 upserts (one below)
/// must NOT trigger even after COMPACT_IDLE has elapsed.
#[test]
fn compact_threshold_4999_below_never_triggers() {
    let start = Instant::now();
    let mut policy = CompactPolicy::default();
    policy.record_upserts(COMPACT_UPSERT_THRESHOLD - 1, start);

    // Well past the idle window.
    assert!(
        !policy.should_compact(start + COMPACT_IDLE * 10),
        "4999 upserts must NOT trigger compact even after long idle"
    );
}

/// `should_compact` at exactly COMPACT_UPSERT_THRESHOLD (5000) upserts MUST
/// trigger once COMPACT_IDLE has elapsed.
#[test]
fn compact_threshold_5000_exact_triggers_after_idle() {
    let start = Instant::now();
    let mut policy = CompactPolicy::default();
    policy.record_upserts(COMPACT_UPSERT_THRESHOLD, start);

    // Just before idle window: must NOT trigger.
    assert!(
        !policy.should_compact(start + Duration::from_secs(29)),
        "5000 upserts before idle window must NOT trigger"
    );
    // At the idle boundary: must trigger.
    assert!(
        policy.should_compact(start + COMPACT_IDLE),
        "5000 upserts at idle boundary must trigger compact"
    );
}

/// deleted_ratio just below 0.2 (e.g., 199/999+199 = 199/1199 ≈ 0.166): must NOT trigger.
#[test]
fn compact_deleted_ratio_below_threshold_no_trigger() {
    let start = Instant::now();
    let mut policy = CompactPolicy::default();
    // live_points = 1000, deletes = 199 → ratio = 199/1199 ≈ 0.166 < 0.2
    policy.note_compacted(1000);
    policy.record_deletes(199, start);

    let ratio = policy.deleted_ratio();
    assert!(
        ratio < COMPACT_DELETED_RATIO,
        "ratio {ratio} must be below threshold {COMPACT_DELETED_RATIO}"
    );
    assert!(
        !policy.should_compact(start),
        "deleted_ratio below 0.2 must NOT trigger compact (even without idle)"
    );
}

/// deleted_ratio exactly at 0.2 (live=1000, deletes=250 → 250/1250=0.2): MUST trigger.
#[test]
fn compact_deleted_ratio_at_threshold_triggers_immediately() {
    let start = Instant::now();
    let mut policy = CompactPolicy::default();
    // live_points = 1000, deletes = 250 → 250/1250 = 0.2 exactly.
    policy.note_compacted(1000);
    policy.record_deletes(250, start);

    let ratio = policy.deleted_ratio();
    assert!(
        (ratio - 0.2).abs() < 1e-9,
        "ratio must be exactly 0.2; got {ratio}"
    );
    assert!(
        policy.should_compact(start),
        "deleted_ratio at exactly 0.2 must trigger compact (no idle required)"
    );
}

/// After mass delete + compact() on a real shard, count() must return the
/// exact number of survivors, not a stale pre-compact value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_after_mass_delete_count_is_exact_survivors() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;

    // Insert 30 points.
    store.upsert(corpus(30, "acme")).await.unwrap();
    assert_eq!(store.count(None).await.unwrap(), 30);

    // Delete 20 of them (every point with id ≥ 10).
    let to_delete: Vec<_> = (10..30u128).map(pid).collect();
    store.delete(&to_delete).await.unwrap();

    // Pre-compact: tombstones reduce count immediately.
    assert_eq!(
        store.count(None).await.unwrap(),
        10,
        "count must reflect deletions immediately (before compact)"
    );

    // Compact.
    store.compact().await.unwrap();

    // Post-compact: exactly 10 survivors remain.
    assert_eq!(
        store.count(None).await.unwrap(),
        10,
        "count must be exactly 10 survivors after compact (tombstones purged)"
    );

    // Search must also return exactly 10.
    let hits = store.search(request(basis(0), 30)).await.unwrap();
    assert_eq!(
        hits.len(),
        10,
        "search after compact must return exactly 10 survivors"
    );

    // None of the deleted ids must appear.
    for deleted_id in (10..30u128).map(pid) {
        assert!(
            hits.iter().all(|h| h.id != deleted_id),
            "deleted point {deleted_id} must not appear after compact"
        );
    }

    // The surviving 10 must be exactly pids 0..9.
    let surviving_ids: BTreeSet<_> = hits.iter().map(|h| h.id).collect();
    let expected: BTreeSet<_> = (0..10u128).map(pid).collect();
    assert_eq!(
        surviving_ids, expected,
        "exactly pids 0..9 must survive mass delete + compact"
    );
}
