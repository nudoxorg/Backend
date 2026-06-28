use std::collections::HashMap;

use qdrant_client::{
	Qdrant,
	qdrant::{QueryPointsBuilder, ScoredPoint, Value, value::Kind},
};

use crate::config::PipelineConfig;
use crate::http::error::{AppError, QdrantError};
use crate::ingest::embedding::{EmbeddingProvider, OpenAIEmbeddingProvider};

use super::{SearchResponse, SearchResult};
use super::{require_non_empty, require_qdrant};

pub async fn semantic_search(
	config: &PipelineConfig,
	client: &Qdrant,
	provider: &OpenAIEmbeddingProvider,
	query: &str,
	limit: usize,
) -> Result<SearchResponse, AppError> {
	let results = search_results(config, client, provider, query, limit).await?;
	Ok(SearchResponse { query: query.trim().to_owned(), results })
}

pub(crate) async fn search_results(
	config: &PipelineConfig,
	client: &Qdrant,
	provider: &OpenAIEmbeddingProvider,
	query: &str,
	limit: usize,
) -> Result<Vec<SearchResult>, AppError> {
	let qdrant = require_qdrant(config)?;
	let query = require_non_empty("q", query)?;
	let embedding = provider.embed_text(query).await?;

	// Single global collection (see `ingest::symbols_collection_name`): one
	// `query_points` against one HNSW graph, instead of listing and sequentially
	// querying every per-package-version collection.
	let collection = crate::ingest::symbols_collection_name(&qdrant.collection_prefix);

	let response = client
		.query(
			QueryPointsBuilder::new(&collection)
				.query(embedding)
				.limit(limit.max(1) as u64)
				.with_payload(true),
		)
		.await
		.map_err(|source| AppError::Qdrant(QdrantError::QueryFailed {
			collection: collection.clone(),
			source,
		}))?;

	let mut results: Vec<SearchResult> = response
		.result
		.into_iter()
		.filter_map(|point| parse_search_result(point, &collection))
		.collect();

	results.sort_by(|left, right| right.score.total_cmp(&left.score));
	results.truncate(limit.max(1));
	Ok(results)
}

fn parse_search_result(point: ScoredPoint, collection: &str) -> Option<SearchResult> {
	let mut payload = point.payload;
	let uri = take_string(&mut payload, "uri")?;

	Some(SearchResult {
		uri,
		score: point.score,
		collection: collection.to_owned(),
		fq_name: take_string(&mut payload, "fq_name"),
		language: take_string(&mut payload, "language"),
		package: take_string(&mut payload, "package"),
		version: take_string(&mut payload, "version"),
		symbol_kind: take_string(&mut payload, "symbol_kind"),
	})
}

fn take_string(payload: &mut HashMap<String, Value>, key: &str) -> Option<String> {
	match payload.remove(key)?.kind {
		Some(Kind::StringValue(value)) => Some(value),
		Some(Kind::IntegerValue(value)) => Some(value.to_string()),
		Some(Kind::DoubleValue(value)) => Some(value.to_string()),
		Some(Kind::BoolValue(value)) => Some(value.to_string()),
		_ => None,
	}
}


