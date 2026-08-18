#![cfg(feature = "local")]
//! Adversarial fan-out tests: partial failure propagation, exact score-merge
//! with cross-shard ties, and score-merge scale correctness (09-vector §20.6,
//! §13.5, 09c §1.1).

mod common;

use std::sync::Arc;

use common::*;
use heart::PackageId;
use registry::vector::NAMESPACE_NUDOX;
use registry::vector::local::{LocalShardStore, WorkingSet, merge_hits};
use registry::vector::{Payload, PayloadValue, SearchHit, SourceTag, VectorStore};

fn pkg(name: &str) -> PackageId {
    PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
}

fn hit(id: u128, score: f32) -> SearchHit {
    SearchHit {
        id: pid(id),
        score,
        payload: Payload::default(),
        source: SourceTag::Local,
    }
}

// ─── Area 5: Fan-out under partial failure ────────────────────────────────────

/// WorkingSet: one healthy shard, one whose actor was poisoned (induced panic).
///
/// CONTRACT (§20.4/§20.9 — no silent narrowing): `search_all` must return
/// an error — not silently narrow the scope to the healthy shard.
///
/// After the poisoned shard is EVICTED and a replacement healthy shard is
/// inserted in its place, `search_all` must succeed and return results from
/// the remaining shards only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_partial_failure_propagates_not_narrows() {
    // Project shard (healthy).
    let project_dir = tempfile::tempdir().unwrap();
    let project = open_mutable_f32(project_dir.path()).await;
    project.upsert(corpus(5, "proj")).await.unwrap();

    // Dep shard (will be poisoned).
    let dep_dir = tempfile::tempdir().unwrap();
    let dep = open_mutable_f32(dep_dir.path()).await;
    dep.upsert(corpus(5, "dep")).await.unwrap();

    // Poison the dep's actor.
    dep.induce_panic().await;

    let working_set = WorkingSet::new(Arc::new(project));
    let mut working_set = working_set;
    let poisoned_package = pkg("poisoned-dep");
    working_set.insert_dep(poisoned_package, Arc::new(dep));

    // Search MUST fail — silently dropping a shard is forbidden.
    let err = working_set
        .search_all(request(basis(0), 10))
        .await
        .unwrap_err();
    // The error must be a backend error (propagated Closed from the actor).
    assert!(
        matches!(err, registry::vector::StoreError::Closed)
            || matches!(err, registry::vector::StoreError::Backend(_)),
        "partial failure must propagate as Closed or Backend, not silently narrow; got {err:?}"
    );

    // After removing the poisoned dep, search over the remaining shards succeeds.
    let _removed = working_set.remove_dep(&poisoned_package);

    let hits = working_set.search_all(request(basis(0), 5)).await.unwrap();
    assert_eq!(
        hits.len(),
        5,
        "after evicting poisoned dep, search over project shard alone must succeed"
    );
    // Results must come from the project shard.
    for hit in &hits {
        assert!(
            hit.payload.get("package") == Some(&PayloadValue::Str("proj".into())),
            "after poisoned dep eviction, only project hits should remain; got {:?}",
            hit.payload
        );
    }
}

// ─── Area 6: Fan-out score-merge exactness at scale ──────────────────────────

/// 3 shards × interleaved graded scores, with EXACT expected global order
/// including a cross-shard exact tie.
///
/// Setup:
///   - Shard A (project): points 0..10 with graded vectors (scores strictly decreasing).
///   - Shard B (dep1):    points 100..110, all with graded(20..30) (weaker).
///   - Shard C (dep2):    one special tie point with pid(9999) scoring exactly
///                        equal to shard A's pid(0) (both use basis(0)), plus
///                        graded points 200..205.
///
/// Expected merged order:
///   1. The tie pair: pid(9999) < pid(0)? No — pid(9999) = 0x1000+9999 is
///      LARGER than pid(0) = 0x1000+0 — so pid(0) wins the tie, then pid(9999).
///      Actually: PointId ordering is UUID order; 0x1000+0 < 0x1000+9999,
///      so ascending id = 0 first, 9999 second.
///   2. Points 1..9 from shard A (strictly decreasing).
///   3. Points 200..205 from shard C (graded(30..35) — weaker).
///   4. Points 100..110 from shard B (graded(20..30) — weaker than the above).
///
/// `limit` is set to cut exactly at the tie boundary (limit=2) and at the
/// natural boundary between groups.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_3_shards_exact_global_order_with_cross_shard_tie() {
    // Shard A: project — pid(0) is an exact match (basis(0)), so it ties
    // with dep2's pid(9999) below; pid(1..10) are graded (scores strictly
    // decreasing, per `graded`'s doc — no internal ties). `corpus(10, ..)`
    // is deliberately NOT used here: it drives every point (including 0)
    // through `graded`, whose own formula (`e0 + (i+1)·0.05·e_{i+1}`) gives
    // `graded(0)` a nonzero weight — score ≈0.99875, not the exact 1.0 this
    // test's tie requires.
    let proj_dir = tempfile::tempdir().unwrap();
    let project = open_mutable_f32(proj_dir.path()).await;
    let mut proj_points = vec![registry::vector::VectorPoint {
        id: pid(0),
        vector: basis(0),
        payload: payload("rust", "proj"),
    }];
    proj_points.extend((1..10u128).map(|i| registry::vector::VectorPoint {
        id: pid(i),
        vector: graded(i as usize),
        payload: payload(if i % 2 == 0 { "rust" } else { "python" }, "proj"),
    }));
    project.upsert(proj_points).await.unwrap();

    // Shard B: dep1 — graded 20..30 (weaker than A's 0..10).
    let dep1_dir = tempfile::tempdir().unwrap();
    {
        let builder = open_mutable_f32(dep1_dir.path()).await;
        let points: Vec<_> = (100..110u128)
            .map(|i| registry::vector::VectorPoint {
                id: pid(i),
                vector: graded((i - 100 + 20) as usize),
                payload: payload("rust", "dep1"),
            })
            .collect();
        builder.upsert(points).await.unwrap();
        builder.close().await.unwrap();
    }
    settle().await;

    // Shard C: dep2 — one tie point (basis(0) same as query = score 1.0),
    // plus graded 30..35.
    let dep2_dir = tempfile::tempdir().unwrap();
    {
        let builder = open_mutable_f32(dep2_dir.path()).await;
        let mut points = vec![registry::vector::VectorPoint {
            id: pid(9999),
            vector: basis(0), // Exact match → score 1.0, ties with proj's pid(0).
            payload: payload("rust", "dep2-tie"),
        }];
        points.extend((200..205u128).map(|i| registry::vector::VectorPoint {
            id: pid(i),
            vector: graded((i - 200 + 30) as usize),
            payload: payload("rust", "dep2"),
        }));
        builder.upsert(points).await.unwrap();
        builder.close().await.unwrap();
    }
    settle().await;

    let dep1 = LocalShardStore::open_read_only(dep1_dir.path(), f32_schema())
        .await
        .unwrap();
    let dep2 = LocalShardStore::open_read_only(dep2_dir.path(), f32_schema())
        .await
        .unwrap();

    let mut ws = WorkingSet::new(Arc::new(project));
    ws.insert_dep(pkg("dep1"), Arc::new(dep1));
    ws.insert_dep(pkg("dep2"), Arc::new(dep2));

    // Full merge: limit large enough to include all 10+10+6 = 26 points.
    let all = ws.search_all(request(basis(0), 30)).await.unwrap();

    // Scores must be in non-increasing order (total_cmp ordering).
    for pair in all.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "merged result must be in non-increasing score order: \
             position has score {} then {}",
            pair[0].score,
            pair[1].score
        );
    }

    // The top 2 must be the exact tie pair: pid(0) < pid(9999) by UUID order.
    assert_eq!(
        all[0].id,
        pid(0),
        "cross-shard tie: smaller id must win; expected pid(0) at rank 0"
    );
    assert_eq!(
        all[1].id,
        pid(9999),
        "cross-shard tie: larger id is second; expected pid(9999) at rank 1"
    );
    assert_eq!(
        all[0].score, all[1].score,
        "both score-1.0 hits must have equal scores (identical vectors)"
    );

    // Limit=2 cuts exactly AT the tie — both tie points included.
    let top2 = ws.search_all(request(basis(0), 2)).await.unwrap();
    assert_eq!(top2.len(), 2);
    assert_eq!(top2[0].id, pid(0));
    assert_eq!(top2[1].id, pid(9999));

    // Limit=1 cuts INSIDE the tie — only the smaller id survives.
    let top1 = ws.search_all(request(basis(0), 1)).await.unwrap();
    assert_eq!(top1.len(), 1);
    assert_eq!(top1[0].id, pid(0));

    // After the tie, the project's graded points (1..9) must come before dep2's (200..205).
    // graded(1) weight = 2*0.05=0.10 → score 1/√(1+0.01) ≈ 0.995
    // graded(30) weight = 31*0.05=1.55 → score 1/√(1+2.4025) ≈ 0.541
    // So all A's graded points beat dep2's.
    let proj_ids: Vec<_> = all
        .iter()
        .map(|h| h.id)
        .filter(|&id| id == pid(1) || id == pid(2))
        .collect();
    let dep2_non_tie_ids: Vec<_> = all
        .iter()
        .map(|h| h.id)
        .filter(|&id| id == pid(200))
        .collect();
    if !proj_ids.is_empty() && !dep2_non_tie_ids.is_empty() {
        let proj_rank = all.iter().position(|h| h.id == pid(1)).unwrap();
        let dep2_rank = all.iter().position(|h| h.id == pid(200)).unwrap();
        assert!(
            proj_rank < dep2_rank,
            "graded(1) from proj must rank above graded(30) from dep2; \
             proj rank={proj_rank}, dep2 rank={dep2_rank}"
        );
    }

    // Total count must be exactly 10 (proj) + 10 (dep1) + 6 (dep2: 1 tie + 5 graded).
    assert_eq!(
        all.len(),
        26,
        "merged result must include all 26 points from 3 shards"
    );
}

/// Pure `merge_hits` tie-break exactness at high scale:
/// two shards each contributing 500 hits, with every other score being a tie.
/// The merged result must be exactly score-descending, ties broken by id ascending.
#[test]
fn merge_hits_large_interleaved_tie_break_is_exact() {
    // Shard A: ids 0,2,4,...998 with scores 1.0, 0.99, 0.98, ... (even ids)
    // Shard B: ids 1,3,5,...999 with scores 1.0, 0.99, 0.98, ... (odd ids, same scores → ties)
    let shard_a: Vec<SearchHit> = (0..500)
        .map(|i| hit(i as u128 * 2, (i as f32).mul_add(-0.001, 1.0)))
        .collect();
    let shard_b: Vec<SearchHit> = (0..500)
        .map(|i| hit(i as u128 * 2 + 1, (i as f32).mul_add(-0.001, 1.0)))
        .collect();

    let merged = merge_hits(vec![shard_a, shard_b], 1000);
    assert_eq!(merged.len(), 1000);

    // Every pair must be (even_id, odd_id) because even < odd for the same score.
    for (i, pair) in merged.chunks(2).enumerate() {
        let expected_score = (i as f32).mul_add(-0.001, 1.0);
        assert!(
            (pair[0].score - expected_score).abs() < 1e-6,
            "pair {i}: expected score {expected_score}, got {}",
            pair[0].score
        );
        assert_eq!(
            pair[0].score, pair[1].score,
            "pair {i}: both should have equal scores (tied)"
        );
        // Even id < odd id → even wins tie.
        let even_id = pid(i as u128 * 2);
        let odd_id = pid(i as u128 * 2 + 1);
        assert_eq!(
            pair[0].id, even_id,
            "pair {i}: smaller (even) id must come first in tie"
        );
        assert_eq!(
            pair[1].id, odd_id,
            "pair {i}: larger (odd) id must come second in tie"
        );
    }

    // Idempotent: same inputs same outputs.
    let shard_a2: Vec<SearchHit> = (0..500)
        .map(|i| hit(i as u128 * 2, (i as f32).mul_add(-0.001, 1.0)))
        .collect();
    let shard_b2: Vec<SearchHit> = (0..500)
        .map(|i| hit(i as u128 * 2 + 1, (i as f32).mul_add(-0.001, 1.0)))
        .collect();
    let merged2 = merge_hits(vec![shard_a2, shard_b2], 1000);
    assert_eq!(
        merged, merged2,
        "merge_hits must be idempotent for identical inputs"
    );
}

/// Score-merge edge case: all hits have the same score.
/// Result must be sorted by id ascending (full tie).
#[test]
fn merge_hits_all_same_score_sorted_by_id() {
    let shard_a: Vec<SearchHit> = vec![hit(5, 0.5), hit(3, 0.5), hit(9, 0.5)];
    let shard_b: Vec<SearchHit> = vec![hit(1, 0.5), hit(7, 0.5), hit(2, 0.5)];

    let merged = merge_hits(vec![shard_a, shard_b], 10);
    let ids: Vec<_> = merged.iter().map(|h| h.id).collect();
    assert_eq!(
        ids,
        vec![pid(1), pid(2), pid(3), pid(5), pid(7), pid(9)],
        "all-same-score merge must be sorted by id ascending"
    );
}

/// Score-merge with non-finite scores (NaN / ±inf) must never panic, and a
/// NaN score — which carries no similarity information — must never
/// outrank a legitimate one, however extreme. This is deliberately NOT "sort
/// by raw `f32::total_cmp` end to end": IEEE total order places a
/// positive-signed NaN *above* `+Infinity`, so a bare `total_cmp` sort would
/// let a broken score win the top rank over a real, if extreme, match. Note
/// this also means a single "every adjacent pair satisfies raw
/// `total_cmp() != Less`" loop cannot express this invariant — that
/// property and "NaN ranks worst" are mutually exclusive whenever a NaN and
/// a non-NaN score are both present (nothing but another top-ranked NaN can
/// legally precede a NaN under raw `total_cmp`, since NaN is that order's
/// maximum element), so the full order is pinned exactly instead.
#[test]
fn merge_hits_non_finite_scores_do_not_panic() {
    let shard_a: Vec<SearchHit> = vec![
        hit(1, f32::NAN),
        hit(2, f32::INFINITY),
        hit(3, f32::NEG_INFINITY),
    ];
    let shard_b: Vec<SearchHit> = vec![hit(4, 0.5)];

    // Must not panic — the comparator handles all f32 values, including NaN.
    let merged = merge_hits(vec![shard_a, shard_b], 10);
    assert_eq!(
        merged.len(),
        4,
        "non-finite scores must not be silently dropped"
    );

    // INFINITY must come first (highest legitimate score); NaN sinks to the
    // worst rank instead of floating to the top. Exact order, pinned:
    // +Infinity, then 0.5, then -Infinity, then NaN last.
    let ids: Vec<_> = merged.iter().map(|h| h.id).collect();
    assert_eq!(
        ids,
        vec![pid(2), pid(4), pid(3), pid(1)],
        "NaN must rank worst; the remaining scores must be total_cmp descending"
    );
}
