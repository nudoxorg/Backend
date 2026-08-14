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
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    self as qdrant, Condition, CountPointsBuilder, CreateCollectionBuilder,
    CreateFieldIndexCollectionBuilder, DeletePointsBuilder, Distance, FieldType, Filter,
    HnswConfigDiffBuilder, KeywordIndexParamsBuilder, PointsIdsList, QuantizationSearchParams,
    ScalarQuantizationBuilder, SearchParams, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParams, VectorParamsBuilder, VectorParamsMap, VectorsConfig, payload_index_params,
    points_selector, quantization_config, vectors_config,
};
use thiserror::Error;

use crate::vector::core::{
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
    /// Dev-only: `nomic-embed-text` via a local Ollama instance, 768-dim.
    /// Not a product schema — see `registry::vector::core::model::NomicEmbedText`.
    /// Named `__dev_` (rather than reusing the parity name) precisely so it
    /// can never collide with a real `JinaParity` deployment's collection on
    /// a shared qdrant instance.
    NomicDev,
}

impl CollectionConfig {
    /// The canonical collection name (frozen; changing it is a migration).
    pub fn collection_name(self) -> &'static str {
        match self {
            CollectionConfig::JinaParity => "symbols__jina_v2_code_768",
            CollectionConfig::VoyagePremium => "symbols__voyage_code3_1024",
            CollectionConfig::NomicDev => "symbols__dev_nomic_embed_text_768",
        }
    }

    /// The named vector inside the collection.
    pub fn vector_name(self) -> &'static str {
        "sym"
    }

    /// Which [`SourceTag`] to stamp on hits from this collection.
    pub fn source_tag(self) -> SourceTag {
        match self {
            CollectionConfig::JinaParity => SourceTag::IndexJina,
            CollectionConfig::VoyagePremium => SourceTag::IndexVoyage,
            // Dev-only stand-in for the parity tier; there is no dedicated
            // `SourceTag` for it and adding one would ripple into every
            // exhaustive match over `SourceTag` for a brand that exists only
            // to unblock local dev without provisioned ONNX weights.
            CollectionConfig::NomicDev => SourceTag::IndexJina,
        }
    }

    /// Number of dimensions for this collection.
    pub fn dimensions(self) -> u64 {
        match self {
            CollectionConfig::JinaParity => 768,
            CollectionConfig::VoyagePremium => 1024,
            CollectionConfig::NomicDev => 768,
        }
    }

    /// The collection schema for the compiled-in model brand `M`.
    pub fn for_model<M: EmbeddingModel>() -> Self {
        match M::id().as_str() {
            "jinaai/jina-embeddings-v2-base-code" => CollectionConfig::JinaParity,
            "voyage/voyage-code-3" => CollectionConfig::VoyagePremium,
            "nomic-embed-text" => CollectionConfig::NomicDev,
            other => panic!("no qdrant collection schema is defined for model brand {other:?}"),
        }
    }
}

/// Errors specific to the remote store that are not already covered by [`StoreError`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("qdrant error: {0}")]
    Qdrant(#[from] qdrant_client::QdrantError),

    #[error("payload encoding error: {0}")]
    Payload(String),
}

impl From<Error> for StoreError {
    fn from(error: Error) -> Self {
        StoreError::Backend(error.to_string())
    }
}

/// A `VectorStore<M>` backed by Qdrant, bound to one collection schema.
pub struct RemoteStore<M: EmbeddingModel> {
    client: Arc<Qdrant>,
    config: CollectionConfig,
    _model: PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> RemoteStore<M> {
    /// Create a store handle.
    pub fn new(client: Arc<Qdrant>, config: CollectionConfig) -> Self {
        Self {
            client,
            config,
            _model: PhantomData,
        }
    }

    /// The Qdrant client handle.
    pub fn client(&self) -> &Arc<Qdrant> {
        &self.client
    }

    /// The bound collection configuration.
    pub fn collection_config(&self) -> CollectionConfig {
        self.config
    }

    /// Delete every point whose `package` payload field equals `package`.
    pub async fn delete_by_package(&self, package: &str) -> Result<(), StoreError> {
        let collection = self.config.collection_name();
        let filter = Filter::must([Condition::matches("package", package.to_owned())]);

        self.client
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(points_selector::PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        Ok(())
    }
}

impl<M: EmbeddingModel> Clone for RemoteStore<M> {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            config: self.config,
            _model: PhantomData,
        }
    }
}

impl<M: EmbeddingModel> Debug for RemoteStore<M> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter
            .debug_struct("RemoteStore")
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

        let vector_name = self.config.vector_name();
        let qdrant_points: Vec<qdrant::PointStruct> = points
            .into_iter()
            .map(|point| convert_vector_point(point, vector_name))
            .collect();

        self.client
            .upsert_points(
                UpsertPointsBuilder::new(self.config.collection_name(), qdrant_points).wait(true),
            )
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        Ok(())
    }

    async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }

        let point_ids: Vec<qdrant::PointId> = ids.iter().map(convert_point_id).collect();
        let selector =
            points_selector::PointsSelectorOneOf::Points(PointsIdsList { ids: point_ids });

        self.client
            .delete_points(
                DeletePointsBuilder::new(self.config.collection_name())
                    .points(selector)
                    .wait(true),
            )
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        Ok(())
    }

    async fn search(&self, request: SearchRequest<M>) -> Result<Vec<SearchHit>, StoreError> {
        let mut builder = SearchPointsBuilder::new(
            self.config.collection_name(),
            request.vector.as_slice().to_vec(),
            request.limit as u64,
        )
        .vector_name(self.config.vector_name())
        .params(build_search_params())
        .with_payload(true);

        if let Some(filter) = compile_filter(&request.filter) {
            builder = builder.filter(filter);
        }

        if let Some(threshold) = request.score_threshold {
            builder = builder.score_threshold(threshold);
        }

        let reply = self
            .client
            .search_points(builder)
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        let source_tag = self.config.source_tag();
        reply
            .result
            .into_iter()
            .map(|point| parse_search_hit(point, source_tag))
            .collect()
    }

    async fn count(&self, filter: Option<&SearchFilter>) -> Result<u64, StoreError> {
        let mut builder = CountPointsBuilder::new(self.config.collection_name()).exact(true);

        if let Some(compiled_filter) = filter.and_then(compile_filter) {
            builder = builder.filter(compiled_filter);
        }

        let reply = self
            .client
            .count(builder)
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        Ok(reply.result.map(|result| result.count).unwrap_or(0))
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

impl<M: EmbeddingModel + 'static> heart::health::Probeable for RemoteStore<M> {
    fn backend(&self) -> heart::BackendKind {
        heart::BackendKind::Qdrant
    }

    async fn probe(&self) -> heart::health::Probe {
        heart::health::timed(heart::BackendKind::Qdrant, async {
            match self.client.health_check().await {
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            }
        })
        .await
    }
}

// =============================================================================
// Helper Functions: Conversions and Compilations
// =============================================================================

fn convert_payload(payload: &Payload) -> qdrant_client::Payload {
    let mut payload_map = qdrant_client::Payload::new();
    for (key, value) in payload {
        match value {
            PayloadValue::Str(string_val) => payload_map.insert(key.as_str(), string_val.as_str()),
            PayloadValue::Int(integer_val) => payload_map.insert(key.as_str(), *integer_val),
            PayloadValue::Bool(boolean_val) => payload_map.insert(key.as_str(), *boolean_val),
        };
    }
    payload_map
}

fn convert_vector_point<M: EmbeddingModel>(
    point: VectorPoint<M>,
    vector_name: &str,
) -> qdrant::PointStruct {
    let payload_map = convert_payload(&point.payload);
    let named_vectors = HashMap::from([(vector_name.to_owned(), point.vector.as_slice().to_vec())]);

    qdrant::PointStruct::new(point.id.as_uuid().to_string(), named_vectors, payload_map)
}

fn convert_point_id(id: &PointId) -> qdrant::PointId {
    qdrant::PointId {
        point_id_options: Some(qdrant::point_id::PointIdOptions::Uuid(
            id.as_uuid().to_string(),
        )),
    }
}

fn parse_search_hit(
    scored_point: qdrant::ScoredPoint,
    source_tag: SourceTag,
) -> Result<SearchHit, StoreError> {
    let id = parse_point_id(&scored_point)
        .ok_or_else(|| StoreError::Corrupt("hit with invalid point id".into()))?;
    let payload = decode_payload(scored_point.payload);

    Ok(SearchHit {
        id,
        score: scored_point.score,
        payload,
        source: source_tag,
    })
}

fn parse_point_id(point: &qdrant::ScoredPoint) -> Option<PointId> {
    match point.id.as_ref()?.point_id_options.as_ref()? {
        qdrant::point_id::PointIdOptions::Uuid(raw_uuid) => {
            raw_uuid.parse::<uuid::Uuid>().ok().map(PointId::from_uuid)
        }
        qdrant::point_id::PointIdOptions::Num(_) => None,
    }
}

fn decode_payload(raw_payload: HashMap<String, qdrant::Value>) -> Payload {
    raw_payload
        .into_iter()
        .filter_map(|(key, value)| {
            let payload_value = match value.kind? {
                qdrant::value::Kind::StringValue(string_val) => {
                    PayloadValue::Str(string_val.into())
                }
                qdrant::value::Kind::IntegerValue(integer_val) => PayloadValue::Int(integer_val),
                qdrant::value::Kind::BoolValue(boolean_val) => PayloadValue::Bool(boolean_val),
                _ => return None,
            };
            Some((key.into(), payload_value))
        })
        .collect()
}

fn build_search_params() -> SearchParams {
    let quantization_params = QuantizationSearchParams {
        rescore: Some(true),
        oversampling: Some(2.0),
        ..Default::default()
    };
    SearchParams {
        quantization: Some(quantization_params),
        ..Default::default()
    }
}

fn compile_filter(filter: &SearchFilter) -> Option<Filter> {
    if filter.must.is_empty() {
        return None;
    }
    let conditions: Vec<Condition> = filter.must.iter().map(compile_clause).collect();
    Some(Filter::must(conditions))
}

fn compile_clause(clause: &FilterClause) -> Condition {
    match clause {
        FilterClause::Eq { key, value } => {
            Condition::matches(key.as_str(), payload_value_to_match(value))
        }
        FilterClause::Any { key, values } => {
            let string_values: Vec<String> = values
                .iter()
                .filter_map(|val| match val {
                    PayloadValue::Str(string_val) => Some(string_val.as_str().to_owned()),
                    _ => None,
                })
                .collect();

            if string_values.len() == values.len() {
                Condition::matches(key.as_str(), string_values)
            } else {
                let inner_conditions: Vec<Condition> = values
                    .iter()
                    .map(|val| Condition::matches(key.as_str(), payload_value_to_match(val)))
                    .collect();
                Filter::should(inner_conditions).into()
            }
        }
    }
}

fn payload_value_to_match(value: &PayloadValue) -> qdrant::r#match::MatchValue {
    match value {
        PayloadValue::Str(string_val) => {
            qdrant::r#match::MatchValue::Keyword(string_val.as_str().to_owned())
        }
        PayloadValue::Int(integer_val) => qdrant::r#match::MatchValue::Integer(*integer_val),
        PayloadValue::Bool(boolean_val) => qdrant::r#match::MatchValue::Boolean(*boolean_val),
    }
}

// =============================================================================
// Collection Initialization Helpers
// =============================================================================

/// Bootstrap a Qdrant collection for the given [`CollectionConfig`] if missing.
pub async fn ensure_collection(
    client: &Qdrant,
    config: CollectionConfig,
) -> Result<(), Error> {
    let collection_name = config.collection_name();

    if is_collection_present(client, collection_name).await? {
        tracing::debug!(
            collection = collection_name,
            "collection already exists; skipping bootstrap"
        );
        return Ok(());
    }

    let dimensions = config.dimensions();
    let vectors_config = build_vectors_config(dimensions, config.vector_name());

    client
        .create_collection(
            CreateCollectionBuilder::new(collection_name).vectors_config(vectors_config),
        )
        .await?;

    tracing::info!(
        collection = collection_name,
        dimensions,
        "collection bootstrapped"
    );

    create_payload_indexes(client, collection_name).await?;
    tracing::debug!(collection = collection_name, "payload indexes created");

    Ok(())
}

async fn is_collection_present(
    client: &Qdrant,
    collection_name: &str,
) -> Result<bool, Error> {
    if let Ok(info) = client.collection_info(collection_name).await {
        if info.result.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn build_vectors_config(dimensions: u64, vector_name: &str) -> VectorsConfig {
    let hnsw_config = HnswConfigDiffBuilder::default()
        .m(0u64)
        .payload_m(16u64)
        .build();

    let scalar_quantization = ScalarQuantizationBuilder::default()
        .quantile(0.99_f32)
        .always_ram(true)
        .build();

    let vector_params: VectorParams = VectorParamsBuilder::new(dimensions, Distance::Cosine)
        .hnsw_config(hnsw_config)
        .quantization_config(quantization_config::Quantization::Scalar(
            scalar_quantization,
        ))
        .on_disk(true)
        .build();

    let named_vector_map = VectorParamsMap {
        map: HashMap::from([(vector_name.to_owned(), vector_params)]),
    };

    VectorsConfig {
        config: Some(vectors_config::Config::ParamsMap(named_vector_map)),
    }
}

async fn create_payload_indexes(
    client: &Qdrant,
    collection_name: &str,
) -> Result<(), Error> {
    for field_name in ["language", "kind"] {
        client
            .create_field_index(CreateFieldIndexCollectionBuilder::new(
                collection_name,
                field_name,
                FieldType::Keyword,
            ))
            .await?;
    }

    let tenant_params = payload_index_params::IndexParams::KeywordIndexParams(
        KeywordIndexParamsBuilder::default().is_tenant(true).build(),
    );

    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(collection_name, "package", FieldType::Keyword)
                .field_index_params(tenant_params),
        )
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::core::{FilterClause, PayloadValue, SearchFilter};

    #[test]
    fn empty_filter_is_none() {
        let filter = SearchFilter::default();
        assert!(compile_filter(&filter).is_none());
    }

    #[test]
    fn eq_clause_compiles() {
        let filter = SearchFilter {
            must: vec![FilterClause::Eq {
                key: "language".into(),
                value: PayloadValue::Str("rust".into()),
            }],
        };
        assert!(compile_filter(&filter).is_some());
    }

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
        assert!(compile_filter(&filter).is_some());
    }

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

    #[test]
    fn multiple_clauses_produce_must_filter() {
        let filter = SearchFilter {
            must: vec![
                FilterClause::Eq {
                    key: "language".into(),
                    value: PayloadValue::Str("go".into()),
                },
                FilterClause::Eq {
                    key: "kind".into(),
                    value: PayloadValue::Str("fn".into()),
                },
            ],
        };
        assert!(compile_filter(&filter).is_some());
    }

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
        assert_eq!(
            CollectionConfig::JinaParity.source_tag(),
            SourceTag::IndexJina
        );
        assert_eq!(
            CollectionConfig::VoyagePremium.source_tag(),
            SourceTag::IndexVoyage
        );
        assert_eq!(CollectionConfig::JinaParity.vector_name(), "sym");
    }
}
