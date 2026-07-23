//! [`LocalShardStore`] — `crate::vector::core::VectorStore<JinaCodeV2>` over one
//! Edge shard behind the single-writer actor.
//!
//! Conversions between the vector-core vocabulary and Edge's are all local
//! to this module:
//!
//! - `VectorPoint` → `PointStructPersisted` (named vector `"sym"`, JSON
//!   object payload),
//! - `FilterClause::Eq` → keyword `Match::Value`, `FilterClause::Any` →
//!   `Match::Any` over strings,
//! - search carries the frozen HNSW/rescore parameters: `hnsw_ef =
//!   search_ef(limit, oversampling)` and, on quantized shards,
//!   `rescore=true, oversampling=2.0` so every score leaving the shard is
//!   an exact f32 cosine (09-vector §20.6 comparability rule),
//! - `compact()` drives Edge's `optimize()` to a fixed point.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use qdrant_edge::{
	Condition, CountRequest, FieldCondition, Filter, JsonPath, Match, NamedQuery, PointId as EdgePointId,
	PointInsertOperations, PointOperations, PointStruct, QuantizationSearchParams, QueryEnum,
	ScoredPoint, SearchParams, SearchRequest as EdgeSearchRequest, UpdateOperation, ValueVariants,
	Vectors, VectorInternal, WithPayloadInterface, WithVector,
	external::serde_json::{Map as JsonMap, Value as JsonValue},
};
use smol_str::SmolStr;
use uuid::Uuid;
use crate::vector::core::{
	FilterClause, JinaCodeV2, Payload, PayloadValue, PointId, RescorePolicy, SearchFilter,
	SearchHit, SearchRequest, ShardSchema, SourceTag, StoreCapabilities, StoreError, VectorPoint,
	VectorStore, search_ef,
};

use super::actor::StoreHandle;
use super::compact::CompactPolicy;
use super::lock::ShardLock;
use super::shard::{self, VECTOR_NAME};
use super::backend_error;

/// `optimize()` is one pass; a handful of passes reaches the fixed point of
/// any realistic backlog without letting a pathological loop spin forever.
const MAX_OPTIMIZE_PASSES: usize = 8;

/// One Edge shard behind its store actor, implementing the shared
/// [`VectorStore`] contract for the local plane.
pub struct LocalShardStore {
	handle: StoreHandle,
	rescore: RescorePolicy,
	policy: Arc<Mutex<CompactPolicy>>,
	/// Held for the life of a *mutable* store (09c §1.1); `None` for baked
	/// read-only dep shards.
	_lock: Option<ShardLock>,
}

impl std::fmt::Debug for LocalShardStore {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LocalShardStore")
			.field("rescore", &self.rescore)
			.field("locked", &self._lock.is_some())
			.finish_non_exhaustive()
	}
}

impl LocalShardStore {
	/// Open the mutable project shard: takes the multi-window advisory lock
	/// first, then opens or creates the shard on the actor thread.
	pub async fn open_mutable(dir: &Path, schema: ShardSchema) -> Result<Self, StoreError> {
		let lock = ShardLock::acquire(dir)?;
		Self::open(dir, schema, Some(lock)).await
	}

	/// Open a read-only baked dep shard (no lock; never written after
	/// install per 09-vector §20.3).
	pub async fn open_read_only(dir: &Path, schema: ShardSchema) -> Result<Self, StoreError> {
		Self::open(dir, schema, None).await
	}

	async fn open(
		dir: &Path,
		schema: ShardSchema,
		lock: Option<ShardLock>,
	) -> Result<Self, StoreError> {
		let rescore = RescorePolicy::for_profile(&schema.quant_profile);
		let name = dir
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_else(|| "shard".to_owned());
		let dir = dir.to_path_buf();
		let handle =
			StoreHandle::spawn(&name, move || shard::open_or_create(&dir, &schema)).await?;
		Ok(Self {
			handle,
			rescore,
			policy: Arc::new(Mutex::new(CompactPolicy::default())),
			_lock: lock,
		})
	}

	/// Graceful close: flush, then drop the last handle so the actor exits
	/// (Edge flushes again on `Drop`).
	pub async fn close(self) -> Result<(), StoreError> {
		self.handle.flush().await
	}

	/// The compact-policy counters, shared with the idle scheduler
	/// ([`super::compact::spawn_compactor`]).
	pub fn compact_policy(&self) -> Arc<Mutex<CompactPolicy>> {
		Arc::clone(&self.policy)
	}

	/// The §20.6 rescore policy this shard searches under (derived from its
	/// schema's quant profile; quantized shards always rescore at 2×, which
	/// is what makes raw-score fan-out merging valid).
	pub fn rescore_policy(&self) -> RescorePolicy {
		self.rescore
	}

	/// Test hook: panic the actor thread; subsequent calls return `Closed`.
	#[doc(hidden)]
	pub async fn induce_panic(&self) {
		self.handle.induce_panic().await;
	}

	fn lock_policy(&self) -> std::sync::MutexGuard<'_, CompactPolicy> {
		// A poisoned counter mutex only means a panicked reader; the counts
		// themselves are always valid.
		self.policy.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
	}
}

#[async_trait]
impl VectorStore<JinaCodeV2> for LocalShardStore {
	async fn upsert(&self, points: Vec<VectorPoint<JinaCodeV2>>) -> Result<(), StoreError> {
		let count = points.len();
		let points = points.into_iter().map(to_edge_point).collect();
		let op = UpdateOperation::PointOperation(PointOperations::UpsertPoints(
			PointInsertOperations::PointsList(points),
		));
		self.handle.update(op).await?;
		self.lock_policy().record_upserts(count as u64, Instant::now());
		Ok(())
	}

	async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError> {
		let count = ids.len();
		let ids = ids.iter().map(|id| EdgePointId::Uuid(*id.as_uuid())).collect();
		let op = UpdateOperation::PointOperation(PointOperations::DeletePoints { ids });
		self.handle.update(op).await?;
		self.lock_policy().record_deletes(count as u64, Instant::now());
		Ok(())
	}

	async fn search(&self, request: SearchRequest<JinaCodeV2>) -> Result<Vec<SearchHit>, StoreError> {
		let edge_request = to_edge_search(&request, &self.rescore)?;
		let scored = self.handle.search(edge_request).await?;
		Ok(scored.into_iter().map(to_hit).collect())
	}

	async fn count(&self, filter: Option<&SearchFilter>) -> Result<u64, StoreError> {
		let filter = match filter {
			Some(filter) => to_edge_filter(filter)?,
			None => None,
		};
		let count = self.handle.count(CountRequest { filter, exact: true }).await?;
		Ok(count as u64)
	}

	async fn flush(&self) -> Result<(), StoreError> {
		self.handle.flush().await
	}

	async fn compact(&self) -> Result<(), StoreError> {
		for _ in 0..MAX_OPTIMIZE_PASSES {
			if !self.handle.optimize().await? {
				break;
			}
		}
		self.handle.flush().await?;
		let live = self.handle.count(CountRequest { filter: None, exact: false }).await?;
		self.lock_policy().note_compacted(live as u64);
		Ok(())
	}

	fn capabilities(&self) -> StoreCapabilities {
		StoreCapabilities {
			filtered_search: true,
			disk_resident: true,
			exact_count: true,
			backend: SmolStr::new_static("qdrant-edge"),
		}
	}
}

/// `VectorPoint` → Edge persisted point: UUID id, single named `"sym"`
/// dense vector, payload as a JSON object of keyword/int/bool facets.
fn to_edge_point(point: VectorPoint<JinaCodeV2>) -> qdrant_edge::PointStructPersisted {
	let payload: JsonMap<String, JsonValue> = point
		.payload
		.iter()
		.map(|(key, value)| (key.to_string(), payload_value_to_json(value)))
		.collect();
	PointStruct::new(
		EdgePointId::Uuid(*point.id.as_uuid()),
		Vectors::new_named([(VECTOR_NAME, point.vector.as_slice().to_vec())]),
		JsonValue::Object(payload),
	)
	.into()
}

fn payload_value_to_json(value: &PayloadValue) -> JsonValue {
	match value {
		PayloadValue::Str(s) => JsonValue::String(s.to_string()),
		PayloadValue::Int(i) => JsonValue::from(*i),
		PayloadValue::Bool(b) => JsonValue::Bool(*b),
	}
}

/// Upsert one decomposed `(id, raw f32 vector, payload)` point directly on a
/// synchronous [`qdrant_edge::EdgeShard`] — the server bakery path (§20.3),
/// where the caller owns a blocking thread and no actor exists. The vector's
/// dimension is validated by Edge against the shard schema at update time, so
/// this stays correct for any model brand the shard was created for.
pub fn upsert_raw(
	shard: &qdrant_edge::EdgeShard,
	id: PointId,
	vector: Vec<f32>,
	payload: Payload,
) -> Result<(), StoreError> {
	let json: JsonMap<String, JsonValue> = payload
		.iter()
		.map(|(key, value)| (key.to_string(), payload_value_to_json(value)))
		.collect();
	let point: qdrant_edge::PointStructPersisted = PointStruct::new(
		EdgePointId::Uuid(*id.as_uuid()),
		Vectors::new_named([(VECTOR_NAME, vector)]),
		JsonValue::Object(json),
	)
	.into();
	shard
		.update(UpdateOperation::PointOperation(PointOperations::UpsertPoints(
			PointInsertOperations::PointsList(vec![point]),
		)))
		.map(|_| ())
		.map_err(backend_error)
}

/// Build the Edge search request with the frozen §20.6 parameters.
fn to_edge_search(
	request: &SearchRequest<JinaCodeV2>,
	policy: &RescorePolicy,
) -> Result<EdgeSearchRequest, StoreError> {
	// Quantized shards oversample and rescore against the f32 originals so
	// cross-shard raw-score merges stay exact; f32 shards need neither.
	let oversampling = if policy.rescore { policy.oversampling } else { 1.0 };
	let params = SearchParams {
		hnsw_ef: Some(search_ef(request.limit, oversampling)),
		quantization: policy.rescore.then(|| QuantizationSearchParams {
			ignore: false,
			rescore: Some(true),
			oversampling: Some(f64::from(policy.oversampling)),
		}),
		..SearchParams::default()
	};

	Ok(EdgeSearchRequest {
		query: QueryEnum::Nearest(NamedQuery::new(
			VectorInternal::Dense(request.vector.as_slice().to_vec()),
			VECTOR_NAME,
		)),
		filter: to_edge_filter(&request.filter)?,
		params: Some(params),
		limit: request.limit,
		offset: 0,
		with_payload: Some(WithPayloadInterface::Bool(true)),
		with_vector: Some(WithVector::Bool(false)),
		score_threshold: request.score_threshold,
	})
}

/// The minimal frozen filter AST (`Eq`, `Any`, implicit `Must` — 09c §1.1)
/// mapped onto Edge conditions. An empty `must` means no filter.
pub(crate) fn to_edge_filter(filter: &SearchFilter) -> Result<Option<Filter>, StoreError> {
	if filter.must.is_empty() {
		return Ok(None);
	}
	let must = filter
		.must
		.iter()
		.map(clause_to_condition)
		.collect::<Result<Vec<_>, _>>()?;
	Ok(Some(Filter { must: Some(must), should: None, min_should: None, must_not: None }))
}

fn clause_to_condition(clause: &FilterClause) -> Result<Condition, StoreError> {
	match clause {
		FilterClause::Eq { key, value } => Ok(Condition::Field(FieldCondition::new_match(
			json_path(key)?,
			Match::new_value(to_value_variant(value)),
		))),
		FilterClause::Any { key, values } => {
			let keywords = values
				.iter()
				.map(|value| match value {
					PayloadValue::Str(s) => Ok(s.to_string()),
					other => Err(backend_error(format!(
						"Any filter over non-string value {other:?} is unsupported by the local plane"
					))),
				})
				.collect::<Result<Vec<String>, _>>()?;
			Ok(Condition::Field(FieldCondition::new_match(
				json_path(key)?,
				Match::from(keywords),
			)))
		}
	}
}

fn json_path(key: &SmolStr) -> Result<JsonPath, StoreError> {
	key.parse()
		.map_err(|()| backend_error(format!("invalid payload filter key {key:?}")))
}

fn to_value_variant(value: &PayloadValue) -> ValueVariants {
	match value {
		PayloadValue::Str(s) => ValueVariants::String(s.to_string()),
		PayloadValue::Int(i) => ValueVariants::Integer(*i),
		PayloadValue::Bool(b) => ValueVariants::Bool(*b),
	}
}

/// Edge scored point → shared hit. All local hits are tagged
/// [`SourceTag::Local`]; payload facets (notably `"package"`) ride along
/// for fan-out labeling.
fn to_hit(point: ScoredPoint) -> SearchHit {
	let id = match point.id {
		EdgePointId::Uuid(uuid) => PointId::from_uuid(uuid),
		// The local plane only ever writes UUID ids; a numeric id would be
		// foreign data. Preserve it injectively rather than dropping the hit.
		EdgePointId::NumId(n) => PointId::from_uuid(Uuid::from_u128(u128::from(n))),
	};
	let payload = point.payload.map(payload_from_edge).unwrap_or_default();
	SearchHit { id, score: point.score, payload, source: SourceTag::Local }
}

fn payload_from_edge(payload: qdrant_edge::Payload) -> Payload {
	payload
		.0
		.into_iter()
		.filter_map(|(key, value)| {
			let value = match value {
				JsonValue::String(s) => PayloadValue::Str(s.into()),
				JsonValue::Number(n) => PayloadValue::Int(n.as_i64()?),
				JsonValue::Bool(b) => PayloadValue::Bool(b),
				// Nested / array payloads are outside the local-plane facet
				// vocabulary; drop rather than misrepresent.
				_ => return None,
			};
			Some((SmolStr::new(key), value))
		})
		.collect()
}
