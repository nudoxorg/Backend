//! [`RemoteStore<M>`]: `VectorStore<M>` over Qdrant (09b §5.2/5.3).
//!
//! Collection schemas (frozen):
//! - Parity:  `"symbols__jina_v2_code_768"`  — 768-dim Cosine, named vec "sym",
//!            hnsw m=0/payload_m=16 (per-tenant graphs), scalar int8 q=0.99
//!            always_ram, on_disk vectors; payload indexes language/kind/package
//!            (package is tenant, is_tenant=true).
//! - Premium: `"symbols__voyage_code3_1024"` — 1024-dim Cosine, same quant.
//!
//! Search: always sends quantization search params rescore=true oversampling=2.0
//! (per 09b §5.3 / QP1 rescore policy).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
	self as q,
	Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, Distance, FieldType,
	Filter, HnswConfigDiffBuilder, KeywordIndexParamsBuilder, QuantizationSearchParams,
	ScalarQuantizationBuilder, SearchParams, SearchPointsBuilder, UpsertPointsBuilder,
	VectorParamsBuilder, VectorsConfig, payload_index_params,
	quantization_config,
};
use thiserror::Error;
use vector_core::{
	EmbeddingModel, FilterClause, Payload, PayloadValue, PointId, SearchFilter, SearchHit,
	SearchRequest, SourceTag, StoreCapabilities, StoreError, VectorPoint, VectorStore,
};

/// Which collection schema a `RemoteStore` instance is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionConfig {
	/// Parity: jina-embeddings-v2-base-code, 768-dim.
	JinaParity,
	/// Premium: voyage-code-3, 1024-dim.
	VoyagePremium,
}

impl CollectionConfig {
	/// The canonical collection name (frozen; changing it is a migration).
	pub fn collection_name(self) -> &'static str {
		match self {
			CollectionConfig::JinaParity => "symbols__jina_v2_code_768",
			CollectionConfig::VoyagePremium => "symbols__voyage_code3_1024",
		}
	}

	/// The named vector inside the collection (09b §5.2: always "sym" for the
	/// primary code vector; other facet vectors ("sig", "body") are separate
	/// named entries in the same collection if ever added).
	pub fn vector_name(self) -> &'static str { "sym" }

	/// Which [`SourceTag`] to stamp on hits from this collection.
	pub fn source_tag(self) -> SourceTag {
		match self {
			CollectionConfig::JinaParity => SourceTag::IndexJina,
			CollectionConfig::VoyagePremium => SourceTag::IndexVoyage,
		}
	}

	/// Number of dimensions for this collection.
	pub fn dimensions(self) -> u64 {
		match self {
			CollectionConfig::JinaParity => 768,
			CollectionConfig::VoyagePremium => 1024,
		}
	}
}

/// Errors specific to the remote store that are not already covered by
/// [`StoreError`].
#[derive(Debug, Error)]
pub enum RemoteStoreError {
	#[error("qdrant error: {0}")]
	Qdrant(#[from] qdrant_client::QdrantError),

	#[error("payload encoding error: {0}")]
	Payload(String),
}

impl From<RemoteStoreError> for StoreError {
	fn from(err: RemoteStoreError) -> Self {
		StoreError::Backend(err.to_string())
	}
}

/// A `VectorStore<M>` backed by Qdrant, bound to one collection schema.
///
/// The collection must already exist or have been bootstrapped via
/// [`ensure_collection`] before any operations are performed; `RemoteStore`
/// itself does not create collections lazily.
pub struct RemoteStore<M: EmbeddingModel> {
	client: Arc<Qdrant>,
	config: CollectionConfig,
	_model: std::marker::PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> RemoteStore<M> {
	/// Create a store handle. Call [`ensure_collection`] separately to
	/// bootstrap the collection if it may not exist.
	pub fn new(client: Arc<Qdrant>, config: CollectionConfig) -> Self {
		Self { client, config, _model: std::marker::PhantomData }
	}

	/// The Qdrant client (for use in [`ensure_collection`]).
	pub fn client(&self) -> &Arc<Qdrant> { &self.client }

	/// The bound collection configuration.
	pub fn collection_config(&self) -> CollectionConfig { self.config }
}

// Hand-written Clone/Debug so M need not be Clone/Debug.
impl<M: EmbeddingModel> Clone for RemoteStore<M> {
	fn clone(&self) -> Self {
		Self { client: Arc::clone(&self.client), config: self.config, _model: std::marker::PhantomData }
	}
}
impl<M: EmbeddingModel> std::fmt::Debug for RemoteStore<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RemoteStore")
			.field("collection", &self.config.collection_name())
			.finish_non_exhaustive()
	}
}

#[async_trait]
impl<M: EmbeddingModel + 'static> VectorStore<M> for RemoteStore<M> {
	async fn upsert(&self, points: Vec<VectorPoint<M>>) -> Result<(), StoreError> {
		if points.is_empty() {
			return Ok(());
		}
		let collection = self.config.collection_name();
		let vector_name = self.config.vector_name();

		let mut qdrant_points = Vec::with_capacity(points.len());
		for point in points {
			let mut payload_map = qdrant_client::Payload::new();
			for (key, value) in &point.payload {
				match value {
					PayloadValue::Str(s) => payload_map.insert(key.as_str(), s.as_str()),
					PayloadValue::Int(i) => payload_map.insert(key.as_str(), *i),
					PayloadValue::Bool(b) => payload_map.insert(key.as_str(), *b),
				}
			}

			let mut named_vectors: HashMap<String, Vec<f32>> = HashMap::new();
			named_vectors.insert(vector_name.to_owned(), point.vector.as_slice().to_vec());

			let qdrant_point = q::PointStruct::new(
				point.id.as_uuid().to_string(),
				named_vectors,
				payload_map,
			);
			qdrant_points.push(qdrant_point);
		}

		self.client
			.upsert_points(
				UpsertPointsBuilder::new(collection, qdrant_points).wait(true),
			)
			.await
			.map_err(|e| StoreError::Backend(e.to_string()))?;

		Ok(())
	}

	async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError> {
		if ids.is_empty() {
			return Ok(());
		}
		let collection = self.config.collection_name();
		use qdrant_client::qdrant::{DeletePointsBuilder, PointsIdsList, points_selector};

		let point_ids: Vec<q::PointId> = ids
			.iter()
			.map(|id| q::PointId {
				point_id_options: Some(q::point_id::PointIdOptions::Uuid(
					id.as_uuid().to_string(),
				)),
			})
			.collect();

		self.client
			.delete_points(
				DeletePointsBuilder::new(collection)
					.points(points_selector::PointsSelectorOneOf::Points(PointsIdsList {
						ids: point_ids,
					}))
					.wait(true),
			)
			.await
			.map_err(|e| StoreError::Backend(e.to_string()))?;

		Ok(())
	}

	async fn search(&self, request: SearchRequest<M>) -> Result<Vec<SearchHit>, StoreError> {
		let collection = self.config.collection_name();
		let vector_name = self.config.vector_name();
		let source_tag = self.config.source_tag();

		let qdrant_filter = compile_filter(&request.filter);

		let quant_params = QuantizationSearchParams {
			rescore: Some(true),
			oversampling: Some(2.0),
			..Default::default()
		};
		let search_params = SearchParams {
			quantization: Some(quant_params),
			..Default::default()
		};

		let mut builder = SearchPointsBuilder::new(
			collection,
			request.vector.as_slice().to_vec(),
			request.limit as u64,
		)
		.vector_name(vector_name)
		.params(search_params)
		.with_payload(true);

		if let Some(f) = qdrant_filter {
			builder = builder.filter(f);
		}

		if let Some(threshold) = request.score_threshold {
			builder = builder.score_threshold(threshold);
		}

		let reply = self
			.client
			.search_points(builder)
			.await
			.map_err(|e| StoreError::Backend(e.to_string()))?;

		let hits = reply
			.result
			.into_iter()
			.map(|scored_point| {
				let id = parse_point_id(&scored_point)
					.ok_or_else(|| StoreError::Corrupt("hit with invalid point id".into()))?;
				let payload = decode_payload(scored_point.payload);
				Ok(SearchHit { id, score: scored_point.score, payload, source: source_tag })
			})
			.collect::<Result<Vec<_>, StoreError>>()?;

		Ok(hits)
	}

	async fn count(&self, filter: Option<&SearchFilter>) -> Result<u64, StoreError> {
		use qdrant_client::qdrant::CountPointsBuilder;

		let collection = self.config.collection_name();
		let qdrant_filter = filter.and_then(compile_filter);

		let mut builder = CountPointsBuilder::new(collection).exact(true);
		if let Some(f) = qdrant_filter {
			builder = builder.filter(f);
		}

		let reply = self
			.client
			.count(builder)
			.await
			.map_err(|e| StoreError::Backend(e.to_string()))?;

		Ok(reply.result.map(|r| r.count).unwrap_or(0))
	}

	async fn flush(&self) -> Result<(), StoreError> {
		Ok(())
	}

	async fn compact(&self) -> Result<(), StoreError> {
		Ok(())
	}

	fn capabilities(&self) -> StoreCapabilities {
		StoreCapabilities {
			filtered_search: true,
			disk_resident: true,
			exact_count: true,
			backend: "qdrant".into(),
		}
	}
}

/// Compile a [`SearchFilter`] into a Qdrant [`Filter`], or `None` if the
/// filter is empty (no unnecessary wrapping).
fn compile_filter(filter: &SearchFilter) -> Option<Filter> {
	if filter.must.is_empty() {
		return None;
	}
	let conditions = filter.must.iter().map(compile_clause).collect::<Vec<_>>();
	Some(Filter::must(conditions))
}

/// Compile one [`FilterClause`] into a qdrant [`Condition`].
fn compile_clause(clause: &FilterClause) -> Condition {
	match clause {
		FilterClause::Eq { key, value } => {
			let match_value = payload_value_to_match(value);
			Condition::matches(key.as_str(), match_value)
		}
		FilterClause::Any { key, values } => {
			// For all-string values use Qdrant's efficient multi-keyword match;
			// for mixed/int/bool values build an OR (should) filter.
			let str_values: Vec<String> = values
				.iter()
				.filter_map(|v| {
					if let PayloadValue::Str(s) = v { Some(s.as_str().to_owned()) } else { None }
				})
				.collect();

			if str_values.len() == values.len() {
				// All strings — use the efficient match-any form.
				Condition::matches(key.as_str(), str_values)
			} else {
				// Mixed/int/bool — wrap each value in an individual condition,
				// then nest them under a should (OR) filter.
				let inner: Vec<Condition> = values
					.iter()
					.map(|v| Condition::matches(key.as_str(), payload_value_to_match(v)))
					.collect();
				// Nest a should (OR) filter as a condition: any one value
				// matching is sufficient.
				Filter::should(inner).into()
			}
		}
	}
}

/// Map a [`PayloadValue`] to the scalar qdrant match value type.
fn payload_value_to_match(value: &PayloadValue) -> q::r#match::MatchValue {
	match value {
		PayloadValue::Str(s) => q::r#match::MatchValue::Keyword(s.as_str().to_owned()),
		PayloadValue::Int(i) => q::r#match::MatchValue::Integer(*i),
		PayloadValue::Bool(b) => q::r#match::MatchValue::Boolean(*b),
	}
}

/// Extract the [`PointId`] from a Qdrant [`ScoredPoint`].
fn parse_point_id(point: &q::ScoredPoint) -> Option<PointId> {
	use q::point_id::PointIdOptions;
	match point.id.as_ref()?.point_id_options.as_ref()? {
		PointIdOptions::Uuid(raw) => raw.parse::<uuid::Uuid>().ok().map(PointId::from_uuid),
		PointIdOptions::Num(_) => None,
	}
}

/// Convert the Qdrant payload map into a `vector_core::Payload`.
fn decode_payload(raw: HashMap<String, q::Value>) -> Payload {
	raw.into_iter()
		.filter_map(|(k, v)| {
			use q::value::Kind;
			let pv = match v.kind? {
				Kind::StringValue(s) => PayloadValue::Str(s.into()),
				Kind::IntegerValue(i) => PayloadValue::Int(i),
				Kind::BoolValue(b) => PayloadValue::Bool(b),
				_ => return None,
			};
			Some((k.into(), pv))
		})
		.collect()
}

/// Bootstrap a Qdrant collection for the given [`CollectionConfig`] if it does
/// not already exist.
///
/// Schema per 09b §5.2/5.3:
/// - Named vector "sym" at the configured dimension, Cosine distance.
/// - HNSW: m=0 (Qdrant default), payload_m=16 for per-tenant graphs.
/// - Scalar int8 quantization: quantile=0.99, always_ram=true (QP1).
/// - on_disk=true for vectors (mmap; reduces resident memory).
/// - Payload keyword indexes: `language`, `kind`, `package` (tenant-optimized).
pub async fn ensure_collection(
	client: &Qdrant,
	config: CollectionConfig,
) -> Result<(), RemoteStoreError> {
	let name = config.collection_name();

	// Idempotent: if the collection already exists, skip creation.
	if let Ok(info) = client.collection_info(name).await {
		if info.result.is_some() {
			tracing::debug!(collection = name, "collection already exists; skipping bootstrap");
			return Ok(());
		}
	}

	let dim = config.dimensions();
	let vector_name = config.vector_name();

	// Named-vector params: cosine, hnsw per-tenant, int8 quant, on_disk.
	let hnsw = HnswConfigDiffBuilder::default().m(0u64).payload_m(16u64).build();
	let scalar_quant = ScalarQuantizationBuilder::default()
		.quantile(0.99_f32)
		.always_ram(true)
		.build();
	let quant_config = quantization_config::Quantization::Scalar(scalar_quant);

	let vector_params: qdrant_client::qdrant::VectorParams = VectorParamsBuilder::new(dim, Distance::Cosine)
		.hnsw_config(hnsw)
		.quantization_config(quant_config)
		.on_disk(true)
		.build();

	let named_map = q::VectorParamsMap {
		map: std::iter::once((vector_name.to_owned(), vector_params)).collect(),
	};
	let vectors_config = VectorsConfig {
		config: Some(q::vectors_config::Config::ParamsMap(named_map)),
	};

	client
		.create_collection(
			CreateCollectionBuilder::new(name).vectors_config(vectors_config),
		)
		.await?;

	tracing::info!(collection = name, dimensions = dim, "collection bootstrapped");

	// Payload keyword indexes.
	for field in ["language", "kind"] {
		client
			.create_field_index(
				CreateFieldIndexCollectionBuilder::new(name, field, FieldType::Keyword),
			)
			.await?;
	}

	// Package field with is_tenant=true for graph-partitioned HNSW.
	client
		.create_field_index(
			CreateFieldIndexCollectionBuilder::new(name, "package", FieldType::Keyword)
				.field_index_params(payload_index_params::IndexParams::KeywordIndexParams(
					KeywordIndexParamsBuilder::default().is_tenant(true).build(),
				)),
		)
		.await?;

	tracing::debug!(collection = name, "payload indexes created");
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use vector_core::{FilterClause, PayloadValue, SearchFilter};

	/// compile_filter on an empty filter returns None (no wrapping).
	#[test]
	fn empty_filter_is_none() {
		let filter = SearchFilter::default();
		assert!(compile_filter(&filter).is_none());
	}

	/// An Eq clause compiles to a Condition without panic.
	#[test]
	fn eq_clause_compiles() {
		let filter = SearchFilter {
			must: vec![FilterClause::Eq {
				key: "language".into(),
				value: PayloadValue::Str("rust".into()),
			}],
		};
		let compiled = compile_filter(&filter);
		assert!(compiled.is_some());
	}

	/// An Any clause with all-string values compiles.
	#[test]
	fn any_str_clause_compiles() {
		let filter = SearchFilter {
			must: vec![FilterClause::Any {
				key: "kind".into(),
				values: vec![
					PayloadValue::Str("fn".into()),
					PayloadValue::Str("struct".into()),
				],
			}],
		};
		let compiled = compile_filter(&filter);
		assert!(compiled.is_some());
	}

	/// An Eq clause on an integer compiles.
	#[test]
	fn eq_int_clause_compiles() {
		let filter = SearchFilter {
			must: vec![FilterClause::Eq {
				key: "score_bucket".into(),
				value: PayloadValue::Int(3),
			}],
		};
		assert!(compile_filter(&filter).is_some());
	}

	/// Multiple clauses produce a must-filter.
	#[test]
	fn multiple_clauses_produce_must_filter() {
		let filter = SearchFilter {
			must: vec![
				FilterClause::Eq { key: "language".into(), value: PayloadValue::Str("go".into()) },
				FilterClause::Eq { key: "kind".into(), value: PayloadValue::Str("fn".into()) },
			],
		};
		let compiled = compile_filter(&filter);
		assert!(compiled.is_some());
	}

	/// CollectionConfig constants are correct.
	#[test]
	fn collection_config_names_and_dims() {
		assert_eq!(
			CollectionConfig::JinaParity.collection_name(),
			"symbols__jina_v2_code_768"
		);
		assert_eq!(
			CollectionConfig::VoyagePremium.collection_name(),
			"symbols__voyage_code3_1024"
		);
		assert_eq!(CollectionConfig::JinaParity.dimensions(), 768);
		assert_eq!(CollectionConfig::VoyagePremium.dimensions(), 1024);
		assert_eq!(CollectionConfig::JinaParity.source_tag(), SourceTag::IndexJina);
		assert_eq!(CollectionConfig::VoyagePremium.source_tag(), SourceTag::IndexVoyage);
		assert_eq!(CollectionConfig::JinaParity.vector_name(), "sym");
	}
}
