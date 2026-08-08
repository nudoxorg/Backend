//! Shared fixtures for the vector-local integration tests.
//!
//! All tests run against the real in-process `qdrant-edge` engine in
//! tempdirs — no mocks of the store plane.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use smol_str::SmolStr;
use uuid::Uuid;
pub use registry::vector::VectorStore;
use registry::vector::{
	Embedding, FilterClause, JinaCodeV2, Payload, PayloadValue, PointId, QP1, QuantProfile,
	SearchFilter, SearchRequest, ShardSchema, VectorPoint,
};
use registry::vector::local::{LocalShardStore, schema_for};

/// JinaCodeV2 dimensionality (the only local-plane model).
pub const DIM: usize = 768;

pub fn f32_schema() -> ShardSchema {
	schema_for::<JinaCodeV2>(QuantProfile::None)
}

pub fn int8_schema() -> ShardSchema {
	schema_for::<JinaCodeV2>(QP1)
}

/// The `i`-th standard basis vector.
pub fn basis(i: usize) -> Embedding<JinaCodeV2> {
	assert!(i < DIM);
	let mut v = vec![0.0f32; DIM];
	v[i] = 1.0;
	Embedding::from_vec(v).expect("basis vector is valid")
}

/// A vector whose cosine against `basis(0)` decreases **strictly** with
/// `i`: `e0 + (i+1)·0.05·e_{i+1}` scores `1/√(1+w²)`. No ties, so
/// best-first order over a graded corpus is exactly ascending id order.
pub fn graded(i: usize) -> Embedding<JinaCodeV2> {
	assert!(i + 1 < DIM);
	let mut v = vec![0.0f32; DIM];
	v[0] = 1.0;
	v[i + 1] = (i as f32 + 1.0) * 0.05;
	Embedding::from_vec(v).expect("graded vector is valid")
}

/// Deterministic point ids, ordered like their index.
pub fn pid(i: u128) -> PointId {
	PointId::from_uuid(Uuid::from_u128(0x1000 + i))
}

pub fn payload(language: &str, package: &str) -> Payload {
	[
		(SmolStr::new("language"), PayloadValue::Str(SmolStr::new(language))),
		(SmolStr::new("package"), PayloadValue::Str(SmolStr::new(package))),
		(SmolStr::new("kind"), PayloadValue::Str(SmolStr::new_static("fn"))),
	]
	.into_iter()
	.collect()
}

/// `n` graded points (ascending-id = best-first): even indices are
/// `language=rust`, odd are `language=python`; all carry `package`.
pub fn corpus(n: usize, package: &str) -> Vec<VectorPoint<JinaCodeV2>> {
	(0..n)
		.map(|i| VectorPoint {
			id: pid(i as u128),
			vector: graded(i),
			payload: payload(if i % 2 == 0 { "rust" } else { "python" }, package),
		})
		.collect()
}

pub fn request(vector: Embedding<JinaCodeV2>, limit: usize) -> SearchRequest<JinaCodeV2> {
	SearchRequest { vector, filter: SearchFilter::default(), limit, score_threshold: None }
}

pub fn filtered_request(
	vector: Embedding<JinaCodeV2>,
	filter: SearchFilter,
	limit: usize,
) -> SearchRequest<JinaCodeV2> {
	SearchRequest { vector, filter, limit, score_threshold: None }
}

pub fn language_filter(language: &str) -> SearchFilter {
	SearchFilter {
		must: vec![FilterClause::Eq {
			key: SmolStr::new("language"),
			value: PayloadValue::Str(SmolStr::new(language)),
		}],
	}
}

pub async fn open_mutable_f32(dir: &Path) -> LocalShardStore {
	LocalShardStore::open_mutable(dir, f32_schema()).await.expect("open mutable f32 shard")
}

/// Let a closed store's actor thread finish its flush-and-drop before the
/// directory is reopened or packed (the actor exits asynchronously after
/// the last handle drops).
pub async fn settle() {
	tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Wrap a `LocalShardStore` into a `SharedWorkingSet` for tests that need
/// the shared read/write lock around the working set.
pub async fn settle_and_wrap(project: LocalShardStore) -> registry::vector::local::SharedWorkingSet {
	use std::sync::Arc;
	use registry::vector::local::WorkingSet;
	WorkingSet::new(Arc::new(project)).into_shared()
}
