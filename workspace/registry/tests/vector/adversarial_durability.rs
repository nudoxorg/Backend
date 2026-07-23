#![cfg(feature = "local")]
//! Adversarial durability tests — kill-9 shaped abrupt exits, schema
//! tampering, upsert_raw equivalence, and dimension attacks (09c §1.1,
//! §13.5, 09-vector §20.3/§20.6).

mod common;

use common::*;
use vector::{JinaCodeV2, StoreError, VectorStore};
use vector::local::{LocalShardStore, SCHEMA_FILE, open_or_create, upsert_raw};

// ─── Area 1: Kill-9 shaped durability ────────────────────────────────────────

/// Upsert N points, flush, then drop the actor without calling `close()` or
/// `optimize()`.  Simulates an abrupt exit (kill-9 style): the tokio runtime
/// tears down the actor thread before it can do a graceful close.  After
/// reopening, all N points must be searchable exactly.
///
/// Mechanism: the actor thread's graceful close path calls `Drop` on the
/// `EdgeShard`, which Edge guarantees flushes the WAL.  Since we issued an
/// explicit `flush()` before dropping, the WAL is already quiesced; even if
/// `Drop`'s flush is skipped, the data is already durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abrupt_drop_after_flush_all_n_points_survive() {
    let dir = tempfile::tempdir().unwrap();
    const N: usize = 20;

    {
        let store = open_mutable_f32(dir.path()).await;
        store.upsert(corpus(N, "durable")).await.unwrap();
        // Explicit flush: WAL quiesced before any implicit drop.
        store.flush().await.unwrap();
        // Abrupt drop — no `close()`, no `optimize()`.
        // The store actor is dropped with its channel still open.
        drop(store);
    }
    // Give the actor thread time to finish its own Drop.
    settle().await;

    let reopened = open_mutable_f32(dir.path()).await;
    // All N points must be exactly present.
    assert_eq!(
        reopened.count(None).await.unwrap(),
        N as u64,
        "all {N} points must survive after abrupt drop following explicit flush"
    );
    let hits = reopened.search(request(basis(0), N)).await.unwrap();
    assert_eq!(hits.len(), N);
    // Graded corpus: best-first == id ascending.
    let expected: Vec<_> = (0..N as u128).map(pid).collect();
    let got: Vec<_> = hits.iter().map(|h| h.id).collect();
    assert_eq!(
        got, expected,
        "graded order must be preserved exactly after abrupt drop + reopen"
    );
}

/// Upsert N points, flush, then upsert M more WITHOUT flushing — drop abruptly.
/// Reopen and assert the durable set is exactly N (pre-flush baseline) or N+M
/// (WAL recovered), and that searches NEVER return corrupt/partial vectors.
///
/// CONTRACT PINNED: Edge's WAL-recovery path (load without prior flush) either
/// replays the entire second batch or loses it entirely — it never delivers
/// partial or corrupt vectors.  We assert `count ∈ {N, N+M}` and that every
/// returned hit has a valid non-NaN score.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abrupt_drop_unflushed_second_batch_wal_semantics() {
    let dir = tempfile::tempdir().unwrap();
    const N: usize = 15;
    const M: usize = 10;

    {
        let store = open_mutable_f32(dir.path()).await;
        // First batch: flushed (definitely durable).
        store.upsert(corpus(N, "batch1")).await.unwrap();
        store.flush().await.unwrap();
        // Second batch: NOT flushed — WAL only; then abrupt drop.
        let extra: Vec<_> = (N..N + M)
            .map(|i| vector::VectorPoint {
                id: pid(i as u128),
                vector: graded(i),
                payload: payload("rust", "batch2"),
            })
            .collect();
        store.upsert(extra).await.unwrap();
        // No flush. Abrupt drop.
        drop(store);
    }
    settle().await;

    let reopened = open_mutable_f32(dir.path()).await;
    let count = reopened.count(None).await.unwrap() as usize;

    // WAL contract: either the entire second batch was recovered or it was not.
    // No partial state (e.g. 7 out of 10 points) is legal.
    assert!(
        count == N || count == N + M,
        "WAL semantics: count must be exactly {N} (pre-flush baseline) or {N}+{M}={} (fully recovered); got {count}",
        N + M
    );

    // Regardless of which count we observe, every returned hit must be non-corrupt.
    let hits = reopened.search(request(basis(0), N + M)).await.unwrap();
    assert_eq!(
        hits.len(),
        count,
        "search result count must agree with point count"
    );
    for hit in &hits {
        assert!(
            hit.score.is_finite(),
            "corrupt vector would produce non-finite score for hit {}; score = {}",
            hit.id,
            hit.score
        );
        assert!(
            hit.score > 0.0,
            "cosine against basis(0) of a graded corpus must be positive; hit {} score = {}",
            hit.id,
            hit.score
        );
    }
}

// ─── Area 2: schema.json tampering ───────────────────────────────────────────

/// After close, overwrite schema.json with a different `model_id`.
/// Reopen must fail with `StoreError::Corrupt`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_tampered_model_id_rejected_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    // Read the schema, change model_id, write it back.
    let schema_path = dir.path().join(SCHEMA_FILE);
    let raw = std::fs::read_to_string(&schema_path).unwrap();
    // Inject a different model_id string in the JSON.
    let tampered = raw.replace(
        "jina-embeddings-v2-base-code",
        "voyage/voyage-code-3-tampered",
    );
    // If replacement didn't change anything, forcibly mutate via serde_json.
    let tampered = if tampered == raw {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["model_id"] = serde_json::json!("voyage/voyage-code-3-tampered");
        serde_json::to_string_pretty(&v).unwrap()
    } else {
        tampered
    };
    std::fs::write(&schema_path, tampered).unwrap();

    let err = LocalShardStore::open_read_only(dir.path(), f32_schema()).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(_)),
        "tampered model_id must yield Corrupt, got {err:?}"
    );
}

/// After close, overwrite schema.json with a different `dim`.
/// Reopen must fail with `StoreError::Corrupt`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_tampered_dim_rejected_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    let schema_path = dir.path().join(SCHEMA_FILE);
    let raw = std::fs::read_to_string(&schema_path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    // Write a dimensionality that differs from the real shard's dim.
    v["dim"] = serde_json::json!(384u64);
    std::fs::write(&schema_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let err = LocalShardStore::open_read_only(dir.path(), f32_schema()).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(_)),
        "tampered dim must yield Corrupt, got {err:?}"
    );
}

/// After close, overwrite schema.json with a flipped `quant_profile`.
///
/// CONTRACT PINNED: `validate_schema` only checks `format_version`, `model_id`,
/// and `dim` — it does NOT validate `quant_profile`.  A quant_profile mismatch
/// therefore opens without error (the shard geometry is still correct).  This
/// test pins that observed behavior.  If schema validation is tightened to
/// include `quant_profile`, this test will fail and should be updated to
/// assert `Corrupt`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_tampered_quant_profile_behavior_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    let schema_path = dir.path().join(SCHEMA_FILE);
    let raw = std::fs::read_to_string(&schema_path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    // Flip quant_profile from "None" to a scalar int8 variant.
    v["quant_profile"] = serde_json::json!({"ScalarInt8": {"quantile": 0.99, "always_ram": false}});
    std::fs::write(&schema_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    // As coded: quant_profile mismatch is NOT detected by validate_schema.
    // The shard opens because the structural geometry (dim, model, format) matches.
    // This is the current contract — pinned here so any tightening is deliberate.
    let result = LocalShardStore::open_read_only(dir.path(), f32_schema()).await;
    assert!(
        result.is_ok(),
        "quant_profile mismatch currently opens without error (validate_schema does not check it); \
         if this assertion fails it means validation was tightened — update test accordingly. \
         Got: {result:?}"
    );
}

/// After close, overwrite schema.json with invalid JSON.
/// Reopen must fail with `StoreError::Corrupt`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_invalid_json_rejected_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    let schema_path = dir.path().join(SCHEMA_FILE);
    std::fs::write(&schema_path, b"{ this is not valid JSON }}}").unwrap();

    let err = LocalShardStore::open_read_only(dir.path(), f32_schema()).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(_)),
        "invalid JSON schema.json must yield Corrupt, got {err:?}"
    );
}

/// After close, DELETE schema.json entirely.
///
/// CONTRACT PINNED (from shard.rs recovery path): if Edge data is present
/// (`segments/` or `wal/` directory exists) but `schema.json` is missing, the
/// code re-creates it from the `expected` schema passed by the caller — the
/// torn-creation recovery path.  So deletion of schema.json when Edge data is
/// already present SUCCEEDS and recovers the shard, not an error.
///
/// This test pins that contract.  If the recovery path is removed (so that a
/// missing schema.json is always `Corrupt`), update this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_deleted_with_edge_data_present_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_mutable_f32(dir.path()).await;
    store.upsert(corpus(5, "acme")).await.unwrap();
    store.close().await.unwrap();
    settle().await;

    // Remove schema.json; Edge data (segments/, wal/) remains.
    std::fs::remove_file(dir.path().join(SCHEMA_FILE)).unwrap();
    // Confirm Edge data is present (recovery path precondition).
    assert!(
        dir.path().join("segments").is_dir() || dir.path().join("wal").is_dir(),
        "test precondition: Edge data must be present for the recovery path to trigger"
    );

    // CONTRACT: recovery re-derives schema.json and loads the shard.
    let result = LocalShardStore::open_read_only(dir.path(), f32_schema()).await;
    assert!(
        result.is_ok(),
        "missing schema.json with Edge data present triggers recovery (not Corrupt); \
         if this fails, the recovery path was removed — update test to assert Corrupt. \
         Got: {result:?}"
    );
    // The recovered shard must still answer searches correctly.
    let recovered = result.unwrap();
    assert_eq!(recovered.count(None).await.unwrap(), 5);
}

// ─── Area 3: upsert_raw vs LocalShardStore::upsert equivalence ───────────────

/// Same points via `upsert_raw` (server bakery path) and via
/// `LocalShardStore::upsert` into two identical-schema shards.
/// Search results must be identical: same ids, same order, exact f32 scores.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upsert_raw_and_upsert_give_identical_search_results() {
    use vector::VectorPoint;

    let dir_actor = tempfile::tempdir().unwrap();
    let dir_raw = tempfile::tempdir().unwrap();
    const N: usize = 30;

    // Path A: via the actor/VectorStore::upsert.
    let store_actor = open_mutable_f32(dir_actor.path()).await;
    store_actor.upsert(corpus(N, "pkg")).await.unwrap();
    store_actor.flush().await.unwrap();

    // Path B: via upsert_raw directly on an EdgeShard (server bakery path).
    // We must open the shard on a blocking thread because EdgeShard is !Send.
    {
        let raw_dir = dir_raw.path().to_path_buf();
        let schema = f32_schema();
        tokio::task::spawn_blocking(move || {
            let shard = open_or_create(&raw_dir, &schema).expect("open_or_create for raw shard");
            for i in 0..N {
                let id = pid(i as u128);
                let vector = graded(i).as_slice().to_vec();
                let pl = payload(if i % 2 == 0 { "rust" } else { "python" }, "pkg");
                upsert_raw(&shard, id, vector, pl)
                    .expect("upsert_raw must succeed for valid vector");
            }
            shard.flush();
        })
        .await
        .unwrap();
    }

    // Compare search results: ids, order, and exact f32 scores.
    let hits_actor = store_actor.search(request(basis(0), N)).await.unwrap();
    let store_raw = LocalShardStore::open_read_only(dir_raw.path(), f32_schema())
        .await
        .unwrap();
    let hits_raw = store_raw.search(request(basis(0), N)).await.unwrap();

    assert_eq!(hits_actor.len(), N);
    assert_eq!(hits_raw.len(), N);

    for (i, (a, b)) in hits_actor.iter().zip(&hits_raw).enumerate() {
        assert_eq!(
            a.id, b.id,
            "upsert_raw equivalence: position {i} id mismatch: actor={} raw={}",
            a.id, b.id
        );
        assert_eq!(
            a.score, b.score,
            "upsert_raw equivalence: position {i} score mismatch: actor={} raw={}",
            a.score, b.score
        );
        assert_eq!(
            a.payload, b.payload,
            "upsert_raw equivalence: position {i} payload mismatch"
        );
    }
}

// ─── Area 4: upsert_raw dimension attack ─────────────────────────────────────

/// upsert_raw a 767-length vector (one short of 768) into a 768 shard.
/// Must return an error; shard remains usable for a subsequent valid upsert + search.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upsert_raw_dim_767_rejected_shard_remains_usable() {
    let dir = tempfile::tempdir().unwrap();
    let schema = f32_schema();

    // Spawn blocking: EdgeShard is !Send.
    let dir_path = dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let shard = open_or_create(&dir_path, &schema).expect("create shard");

        // 767-dim: one short.
        let short_vec: Vec<f32> = vec![1.0f32; 767];
        let err = upsert_raw(&shard, pid(0), short_vec, Default::default())
            .expect_err("767-dim vector into 768 shard must fail");
        // Error must encode as a Backend variant (Edge rejects the wrong dim).
        assert!(
            matches!(err, StoreError::Backend(_)),
            "wrong-dim rejection must be StoreError::Backend, got {err:?}"
        );

        // Shard remains usable: a valid 768-dim upsert and search succeed.
        let ok_vec: Vec<f32> = {
            let mut v = vec![0.0f32; 768];
            v[0] = 1.0;
            v
        };
        upsert_raw(&shard, pid(1), ok_vec, Default::default())
            .expect("valid 768-dim upsert after rejected 767-dim must succeed");
        shard.flush();

        let results = shard
            .search(vector_local_edge_search_request_dim768())
            .expect("search after dim attack must succeed");
        assert_eq!(results.len(), 1, "one valid point must be searchable after dim attack");
    })
    .await
    .unwrap();
}

/// upsert_raw a 4096-length vector (oversized) into a 768 shard.
/// Must return an error; shard remains usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upsert_raw_dim_4096_rejected_shard_remains_usable() {
    let dir = tempfile::tempdir().unwrap();
    let schema = f32_schema();
    let dir_path = dir.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let shard = open_or_create(&dir_path, &schema).expect("create shard");

        // 4096-dim: wildly oversized.
        let big_vec: Vec<f32> = vec![1.0f32; 4096];
        let err = upsert_raw(&shard, pid(0), big_vec, Default::default())
            .expect_err("4096-dim vector into 768 shard must fail");
        assert!(
            matches!(err, StoreError::Backend(_)),
            "oversized-dim rejection must be StoreError::Backend, got {err:?}"
        );

        // Shard remains usable.
        let ok_vec: Vec<f32> = {
            let mut v = vec![0.0f32; 768];
            v[0] = 1.0;
            v
        };
        upsert_raw(&shard, pid(42), ok_vec, Default::default())
            .expect("valid upsert after oversized-dim rejection must succeed");
        shard.flush();

        let results = shard
            .search(vector_local_edge_search_request_dim768())
            .expect("search after dim attack must succeed");
        assert_eq!(results.len(), 1);
    })
    .await
    .unwrap();
}

/// Build a minimal Edge search request for a 768-dim shard (basis vector e0).
/// Used by the blocking-thread dim-attack tests to verify the shard is still usable.
fn vector_local_edge_search_request_dim768() -> qdrant_edge::SearchRequest {
    use qdrant_edge::{NamedQuery, QueryEnum, SearchParams, VectorInternal, WithPayloadInterface, WithVector};
    let mut v = vec![0.0f32; 768];
    v[0] = 1.0;
    qdrant_edge::SearchRequest {
        query: QueryEnum::Nearest(NamedQuery::new(VectorInternal::Dense(v), "sym")),
        filter: None,
        params: Some(SearchParams {
            hnsw_ef: Some(10),
            ..SearchParams::default()
        }),
        limit: 10,
        offset: 0,
        with_payload: Some(WithPayloadInterface::Bool(true)),
        with_vector: Some(WithVector::Bool(false)),
        score_threshold: None,
    }
}
