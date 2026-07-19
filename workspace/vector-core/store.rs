//! The backend-agnostic vector-store surface (09-vector §6): points, filters,
//! hits, capabilities, and the sealed-by-brand `VectorStore` trait implemented
//! by the local (edge) store and the remote (index) client alike.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use uuid::Uuid;

use crate::{embedding::Embedding, model::EmbeddingModel};

/// The nudox UUID-v5 namespace: `uuid5(NAMESPACE_DNS, "nudox.dev")`, computed
/// once and frozen (see `namespace_derivation` test). Every deterministic
/// point id hangs off this root, so both planes mint identical ids for the
/// same symbol (I3).
pub const NAMESPACE_NUDOX: Uuid = Uuid::from_u128(0xfb2ac7b7_3b21_5d1e_93bc_abbd016f210a);

/// A store point id — deterministic UUID v5 of the symbol id under
/// [`NAMESPACE_NUDOX`], so upserts are idempotent and both planes agree (I3).
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct PointId(Uuid);

impl PointId {
	/// Derive the (stable) point id for a symbol.
	pub fn from_symbol(symbol: &heart::SymbolId) -> Self {
		Self(Uuid::new_v5(&NAMESPACE_NUDOX, symbol.as_uuid().as_bytes()))
	}

	/// Wrap a raw UUID read back from a store.
	pub const fn from_uuid(uuid: Uuid) -> Self { Self(uuid) }

	/// The raw UUID for wire encoding.
	pub const fn as_uuid(&self) -> &Uuid { &self.0 }
}

impl std::fmt::Display for PointId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(&self.0, f)
	}
}

/// A typed payload value — the only value shapes the payload schema admits
/// (no stringly floats/nested blobs; filters stay index-friendly).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PayloadValue {
	Str(SmolStr),
	Int(i64),
	Bool(bool),
}

impl From<&str> for PayloadValue {
	fn from(value: &str) -> Self { Self::Str(SmolStr::new(value)) }
}
impl From<String> for PayloadValue {
	fn from(value: String) -> Self { Self::Str(SmolStr::new(value)) }
}
impl From<SmolStr> for PayloadValue {
	fn from(value: SmolStr) -> Self { Self::Str(value) }
}
impl From<i64> for PayloadValue {
	fn from(value: i64) -> Self { Self::Int(value) }
}
impl From<bool> for PayloadValue {
	fn from(value: bool) -> Self { Self::Bool(value) }
}

/// A point's payload: sorted keys for deterministic serialization.
pub type Payload = BTreeMap<SmolStr, PayloadValue>;

/// One upsertable point: id + branded vector + payload. The brand ties the
/// point to exactly one store family (I11).
///
/// `serde(bound = "")`: the brand is phantom — no `M: Serialize` requirement
/// leaks onto the zero-sized model type. Clone/Debug/PartialEq are
/// hand-written for the same reason.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct VectorPoint<M: EmbeddingModel> {
	pub id: PointId,
	pub vector: Embedding<M>,
	pub payload: Payload,
}

impl<M: EmbeddingModel> Clone for VectorPoint<M> {
	fn clone(&self) -> Self {
		Self { id: self.id, vector: self.vector.clone(), payload: self.payload.clone() }
	}
}
impl<M: EmbeddingModel> PartialEq for VectorPoint<M> {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id && self.vector == other.vector && self.payload == other.payload
	}
}
impl<M: EmbeddingModel> std::fmt::Debug for VectorPoint<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("VectorPoint")
			.field("id", &self.id)
			.field("vector", &self.vector)
			.field("payload", &self.payload)
			.finish()
	}
}

/// Which plane/model family produced a hit — carried through fusion so
/// provenance survives merging (09-vector §20.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceTag {
	/// The local edge store.
	Local,
	/// The remote index, jina parity collection.
	IndexJina,
	/// The remote index, voyage premium collection.
	IndexVoyage,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
	pub id: PointId,
	/// Backend similarity score (metric-space, *not* comparable across
	/// models — cross-source merging must go through rank-based fusion, R6).
	pub score: f32,
	pub payload: Payload,
	pub source: SourceTag,
}

/// A conjunctive payload filter (all clauses must hold).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchFilter {
	pub must: Vec<FilterClause>,
}

/// One filter clause over a payload key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterClause {
	/// `payload[key] == value`.
	Eq { key: SmolStr, value: PayloadValue },
	/// `payload[key] ∈ values`.
	Any { key: SmolStr, values: Vec<PayloadValue> },
}

/// A dense search request against one model family.
///
/// `serde(bound = "")` + hand-written impls: see [`VectorPoint`].
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct SearchRequest<M: EmbeddingModel> {
	pub vector: Embedding<M>,
	pub filter: SearchFilter,
	pub limit: usize,
	/// Drop hits below this backend score (same caveat as [`SearchHit::score`]).
	pub score_threshold: Option<f32>,
}

impl<M: EmbeddingModel> Clone for SearchRequest<M> {
	fn clone(&self) -> Self {
		Self {
			vector: self.vector.clone(),
			filter: self.filter.clone(),
			limit: self.limit,
			score_threshold: self.score_threshold,
		}
	}
}
impl<M: EmbeddingModel> std::fmt::Debug for SearchRequest<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SearchRequest")
			.field("vector", &self.vector)
			.field("filter", &self.filter)
			.field("limit", &self.limit)
			.field("score_threshold", &self.score_threshold)
			.finish()
	}
}

/// What a concrete store can actually do — callers branch on capabilities
/// instead of downcasting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCapabilities {
	/// Payload-filtered dense search is supported natively.
	pub filtered_search: bool,
	/// Vectors may live on disk (mmap) rather than resident RAM.
	pub disk_resident: bool,
	/// `count` is exact (vs. an estimate).
	pub exact_count: bool,
	/// Human-readable backend tag (`"edge-hnsw"`, `"qdrant"`, …).
	pub backend: SmolStr,
}

/// Store failures.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
	#[error("store I/O failure: {0}")]
	Io(#[from] std::io::Error),

	/// On-disk shard bytes fail checksum/schema validation (I8: corrupt
	/// shards are rejected, never partially served).
	#[error("store corrupt: {0}")]
	Corrupt(String),

	/// A vector's dimension disagrees with the collection schema — should be
	/// unreachable through branded [`VectorPoint`]s; guards raw ingestion.
	#[error("store dimension mismatch: expected {expected}, got {got}")]
	DimensionMismatch { expected: usize, got: usize },

	#[error("store backend failure: {0}")]
	Backend(String),

	/// The store was closed/evicted underneath the caller.
	#[error("store closed")]
	Closed,

	/// Another process/window holds the shard's writer lock (09c §1.1:
	/// single-writer per shard path; the second opener must not spin).
	#[error("shard is locked by another process: {path}")]
	Locked { path: String },
}

/// A dense vector store over one model family (09-vector §6). Both planes
/// implement this: the local edge store and the remote index client — the
/// router composes them without knowing which is which.
#[async_trait::async_trait]
pub trait VectorStore<M: EmbeddingModel>: Send + Sync {
	/// Idempotently insert-or-replace points (ids are deterministic — I3).
	async fn upsert(&self, points: Vec<VectorPoint<M>>) -> Result<(), StoreError>;

	/// Delete by id (missing ids are not an error; deletes are idempotent).
	async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError>;

	/// Dense search, best-first.
	async fn search(&self, request: SearchRequest<M>) -> Result<Vec<SearchHit>, StoreError>;

	/// Point count, optionally filtered (exactness per
	/// [`StoreCapabilities::exact_count`]).
	async fn count(&self, filter: Option<&SearchFilter>) -> Result<u64, StoreError>;

	/// Make all prior writes durable.
	async fn flush(&self) -> Result<(), StoreError>;

	/// Reclaim space / rebuild indexes after heavy delete churn.
	async fn compact(&self) -> Result<(), StoreError>;

	/// What this store can do.
	fn capabilities(&self) -> StoreCapabilities;
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The frozen namespace really is `uuid5(NAMESPACE_DNS, "nudox.dev")`.
	#[test]
	fn namespace_derivation() {
		assert_eq!(NAMESPACE_NUDOX, Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"nudox.dev"));
	}

	// ── adversarial: PayloadValue ordering stability ─────────────────────────

	/// PayloadValue derives Ord. The discriminant order is Bool < Int < Str.
	/// This matters for BTreeMap key determinism in Payload.
	#[test]
	fn payload_value_ordering_stability_btreemap_determinism() {
		use smol_str::SmolStr;
		let bool_false = PayloadValue::Bool(false);
		let bool_true = PayloadValue::Bool(true);
		let int_neg = PayloadValue::Int(-1);
		let int_zero = PayloadValue::Int(0);
		let int_pos = PayloadValue::Int(1);
		let str_a = PayloadValue::Str(SmolStr::new("a"));
		let str_z = PayloadValue::Str(SmolStr::new("z"));

		// Bool < Int < Str (enum variant order = discriminant order with #[derive(Ord)]).
		assert!(bool_false < int_neg, "Bool < Int");
		assert!(bool_true < int_neg, "Bool(true) < Int(-1)");
		assert!(int_pos < str_a, "Int < Str");
		assert!(int_neg < int_zero, "Int ordering within variant");
		assert!(str_a < str_z, "Str ordering within variant");

		// Confirm in a BTreeMap: iteration order must be deterministic.
		let mut map: std::collections::BTreeMap<PayloadValue, &str> =
			std::collections::BTreeMap::new();
		map.insert(str_a.clone(), "str_a");
		map.insert(int_zero.clone(), "int_zero");
		map.insert(bool_false.clone(), "bool_false");
		map.insert(bool_true.clone(), "bool_true");

		let keys: Vec<_> = map.keys().cloned().collect();
		// Must be in ascending order.
		for w in keys.windows(2) {
			assert!(w[0] < w[1], "BTreeMap must iterate in ascending PayloadValue order");
		}
	}

	// ── adversarial: SearchFilter serde roundtrip ─────────────────────────────

	#[test]
	fn search_filter_serde_roundtrip_empty() {
		let filter = SearchFilter::default();
		let json = serde_json::to_string(&filter).unwrap();
		let restored: SearchFilter = serde_json::from_str(&json).unwrap();
		assert_eq!(filter, restored, "empty SearchFilter must roundtrip");
	}

	#[test]
	fn search_filter_serde_roundtrip_with_clauses() {
		use smol_str::SmolStr;
		let filter = SearchFilter {
			must: vec![
				FilterClause::Eq {
					key: SmolStr::new("language"),
					value: PayloadValue::Str(SmolStr::new("rust")),
				},
				FilterClause::Any {
					key: SmolStr::new("kind"),
					values: vec![
						PayloadValue::Str(SmolStr::new("fn")),
						PayloadValue::Str(SmolStr::new("struct")),
					],
				},
				FilterClause::Eq {
					key: SmolStr::new("count"),
					value: PayloadValue::Int(42),
				},
				FilterClause::Eq {
					key: SmolStr::new("direct"),
					value: PayloadValue::Bool(true),
				},
			],
		};
		let json = serde_json::to_string(&filter).unwrap();
		let restored: SearchFilter = serde_json::from_str(&json).unwrap();
		assert_eq!(filter, restored, "SearchFilter with all clause types must roundtrip");
	}

	#[test]
	fn point_id_is_stable_and_injective_per_symbol() {
		let sym_a = heart::SymbolId::from_name(&NAMESPACE_NUDOX, b"sym-a");
		let sym_b = heart::SymbolId::from_name(&NAMESPACE_NUDOX, b"sym-b");
		// Stable: same symbol → same point id, forever (I3).
		assert_eq!(PointId::from_symbol(&sym_a), PointId::from_symbol(&sym_a));
		// Distinct symbols → distinct points.
		assert_ne!(PointId::from_symbol(&sym_a), PointId::from_symbol(&sym_b));
		// And the id is *derived*, not the raw symbol uuid (namespaced v5).
		assert_ne!(PointId::from_symbol(&sym_a).as_uuid(), sym_a.as_uuid());
	}
}
