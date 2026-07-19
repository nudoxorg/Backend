//! Real-EdgeShard integration tests for the store/actor/shard/lock planes:
//! branded upserts, filtered search, delete + compact, durability, schema
//! validation, panic poisoning, and the multi-window lock.

mod common;

use std::io::ErrorKind;

use common::*;
use vector_core::{JinaCodeV2, ModelId, StoreError, VectorStore};
use vector_local::LocalShardStore;

/// 100 branded vectors go in; a keyword-filtered search never returns a
/// point of the wrong language, and the unfiltered order is the exact
/// graded order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upsert_100_and_filtered_search_excludes_wrong_language() {
	let dir = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(dir.path()).await;

	store.upsert(corpus(100, "acme")).await.unwrap();
	store.flush().await.unwrap();

	// Unfiltered: all 100, best-first == ascending id (graded corpus).
	let hits = store.search(request(basis(0), 100)).await.unwrap();
	assert_eq!(hits.len(), 100);
	let expected: Vec<_> = (0..100).map(|i| pid(i as u128)).collect();
	let got: Vec<_> = hits.iter().map(|hit| hit.id).collect();
	assert_eq!(got, expected, "graded corpus must come back in strict score order");
	// Scores strictly descending — no accidental ties or NaNs.
	for pair in hits.windows(2) {
		assert!(pair[0].score > pair[1].score, "scores must strictly decrease");
	}

	// Filtered: exactly the 50 rust points, none of the python ones.
	let hits = store
		.search(filtered_request(basis(0), language_filter("rust"), 100))
		.await
		.unwrap();
	assert_eq!(hits.len(), 50);
	for hit in &hits {
		let language = hit.payload.get("language").expect("payload rides along");
		assert_eq!(
			language,
			&vector_core::PayloadValue::Str("rust".into()),
			"filter must exclude every wrong-language point"
		);
	}
	let got: Vec<_> = hits.iter().map(|hit| hit.id).collect();
	let expected: Vec<_> = (0..100).step_by(2).map(|i| pid(i as u128)).collect();
	assert_eq!(got, expected);

	// Filtered count agrees.
	assert_eq!(store.count(Some(&language_filter("rust"))).await.unwrap(), 50);
	assert_eq!(store.count(None).await.unwrap(), 100);
}

/// Deleted points disappear from search immediately and stay gone through
/// compaction; counts track the live set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_then_compact_excludes_deleted() {
	let dir = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(dir.path()).await;

	store.upsert(corpus(100, "acme")).await.unwrap();
	store.delete(&[pid(0), pid(2), pid(4)]).await.unwrap();

	let hits = store.search(request(basis(0), 100)).await.unwrap();
	assert_eq!(hits.len(), 97);
	assert_eq!(hits[0].id, pid(1), "best surviving point takes over the top slot");
	for gone in [pid(0), pid(2), pid(4)] {
		assert!(hits.iter().all(|hit| hit.id != gone), "deleted {gone} must not be served");
	}
	assert_eq!(store.count(None).await.unwrap(), 97);

	store.compact().await.unwrap();

	let hits = store.search(request(basis(0), 100)).await.unwrap();
	assert_eq!(hits.len(), 97);
	for gone in [pid(0), pid(2), pid(4)] {
		assert!(
			hits.iter().all(|hit| hit.id != gone),
			"compaction must not resurrect deleted {gone}"
		);
	}
	assert_eq!(store.count(None).await.unwrap(), 97);
}

/// Close, drop, reopen: every point and its payload survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_and_reload_preserves_points() {
	let dir = tempfile::tempdir().unwrap();

	let store = open_mutable_f32(dir.path()).await;
	store.upsert(corpus(100, "acme")).await.unwrap();
	store.close().await.unwrap();
	settle().await;

	let store = open_mutable_f32(dir.path()).await;
	assert_eq!(store.count(None).await.unwrap(), 100);
	let hits = store.search(request(basis(0), 100)).await.unwrap();
	assert_eq!(hits.len(), 100);
	assert_eq!(hits[0].id, pid(0));
	assert_eq!(
		hits[0].payload.get("package"),
		Some(&vector_core::PayloadValue::Str("acme".into())),
		"payload must survive the reload"
	);
}

/// A shard whose schema.json disagrees with the caller's expectation is
/// rejected with `Corrupt` — wrong dim, wrong model, wrong format version.
/// It must never silently search the wrong geometry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_mismatch_rejected() {
	let dir = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(dir.path()).await;
	store.upsert(corpus(10, "acme")).await.unwrap();
	store.close().await.unwrap();
	settle().await;

	let mut wrong_dim = f32_schema();
	wrong_dim.dim = 512;
	let err = LocalShardStore::open_read_only(dir.path(), wrong_dim).await.unwrap_err();
	assert!(matches!(err, StoreError::Corrupt(_)), "dim mismatch must be Corrupt, got {err:?}");

	let mut wrong_model = f32_schema();
	wrong_model.model_id = ModelId::new("voyage/voyage-code-3");
	let err = LocalShardStore::open_read_only(dir.path(), wrong_model).await.unwrap_err();
	assert!(matches!(err, StoreError::Corrupt(_)), "model mismatch must be Corrupt, got {err:?}");

	let mut wrong_format = f32_schema();
	wrong_format.format_version += 1;
	let err = LocalShardStore::open_read_only(dir.path(), wrong_format).await.unwrap_err();
	assert!(
		matches!(err, StoreError::Corrupt(_)),
		"format-version mismatch must be Corrupt, got {err:?}"
	);

	// The matching schema still opens — the rejections above were not
	// side-effecting.
	let store = LocalShardStore::open_read_only(dir.path(), f32_schema()).await.unwrap();
	assert_eq!(store.count(None).await.unwrap(), 10);
}

/// An Edge panic poisons the actor: the process survives and every
/// subsequent call observes `Closed` — search, write, and flush alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_returns_closed_after_induced_panic() {
	let dir = tempfile::tempdir().unwrap();
	let store = open_mutable_f32(dir.path()).await;
	store.upsert(corpus(10, "acme")).await.unwrap();

	store.induce_panic().await;

	let err = store.search(request(basis(0), 10)).await.unwrap_err();
	assert!(matches!(err, StoreError::Closed), "search after poison: {err:?}");
	let err = store.upsert(corpus(1, "acme")).await.unwrap_err();
	assert!(matches!(err, StoreError::Closed), "upsert after poison: {err:?}");
	let err = store.flush().await.unwrap_err();
	assert!(matches!(err, StoreError::Closed), "flush after poison: {err:?}");
}

/// The multi-window guard: a second mutable opener of the same shard
/// directory is refused with `Io(WouldBlock)` while the first lives, and
/// admitted after the first closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_mutable_opener_is_locked_out() {
	let dir = tempfile::tempdir().unwrap();
	let first = open_mutable_f32(dir.path()).await;

	let err = LocalShardStore::open_mutable(dir.path(), f32_schema()).await.unwrap_err();
	match &err {
		StoreError::Io(io) => {
			assert_eq!(io.kind(), ErrorKind::WouldBlock, "lock contention kind: {io:?}");
			assert!(
				io.to_string().contains("already open"),
				"message must tell the user what to do: {io}"
			);
		}
		other => panic!("second opener must fail with Io(WouldBlock), got {other:?}"),
	}

	// Release and retry: the lock dies with the store.
	first.close().await.unwrap();
	settle().await;
	let reopened = LocalShardStore::open_mutable(dir.path(), f32_schema()).await;
	assert!(reopened.is_ok(), "lock must be released on close: {:?}", reopened.err());
}

/// §20.6 comparability: the same corpus in an f32 shard and an int8 (QP1,
/// rescore=true) shard returns the **same ordering** with matching exact
/// f32 scores — the property that makes raw-score fan-out merging valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn int8_rescored_scores_match_f32() {
	let f32_dir = tempfile::tempdir().unwrap();
	let int8_dir = tempfile::tempdir().unwrap();

	let f32_store = open_mutable_f32(f32_dir.path()).await;
	let int8_store =
		LocalShardStore::open_mutable(int8_dir.path(), int8_schema()).await.unwrap();

	f32_store.upsert(corpus(50, "acme")).await.unwrap();
	int8_store.upsert(corpus(50, "acme")).await.unwrap();

	let f32_hits =
		VectorStore::<JinaCodeV2>::search(&f32_store, request(basis(0), 50)).await.unwrap();
	let int8_hits =
		VectorStore::<JinaCodeV2>::search(&int8_store, request(basis(0), 50)).await.unwrap();

	assert_eq!(f32_hits.len(), 50);
	assert_eq!(int8_hits.len(), 50);
	let f32_ids: Vec<_> = f32_hits.iter().map(|hit| hit.id).collect();
	let int8_ids: Vec<_> = int8_hits.iter().map(|hit| hit.id).collect();
	assert_eq!(f32_ids, int8_ids, "rescored int8 ordering must equal f32 ordering");
	for (a, b) in f32_hits.iter().zip(&int8_hits) {
		assert!(
			(a.score - b.score).abs() < 1e-4,
			"rescored score must be the exact f32 cosine: {} vs {} for {}",
			a.score,
			b.score,
			a.id,
		);
	}
}
