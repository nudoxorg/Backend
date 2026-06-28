use std::collections::HashMap;

use qdrant_client::{
	Qdrant,
	qdrant::{
		ListCollectionsResponse, QueryPointsBuilder, ScoredPoint, Value, value::Kind,
	},
};

use crate::config::{PipelineConfig, QdrantSettings};
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

	let collections = backend_collections(client, qdrant).await?;

	let mut results = Vec::new();
	for collection in collections {
		let response = client
			.query(
				QueryPointsBuilder::new(&collection)
					.query(embedding.clone())
					.limit(limit.max(1) as u64)
					.with_payload(true),
			)
			.await
			.map_err(|source| AppError::Qdrant(QdrantError::QueryFailed {
				collection: collection.clone(),
				source,
			}))?;

		results.extend(
			response.result.into_iter().filter_map(|point| parse_search_result(point, &collection)),
		);
	}

	results.sort_by(|left, right| right.score.total_cmp(&left.score));
	results.truncate(limit.max(1));
	Ok(results)
}

async fn backend_collections(
	client: &Qdrant,
	settings: &QdrantSettings,
) -> Result<Vec<String>, AppError> {
	let ListCollectionsResponse { collections, .. } =
		client.list_collections().await.map_err(|source| AppError::Qdrant(QdrantError::ListCollectionsFailed {
			prefix: settings.collection_prefix.clone(),
			source,
		}))?;
	let prefix = format!("{}_", settings.collection_prefix);

	let mut names: Vec<String> = collections
		.into_iter()
		.map(|collection| collection.name)
		.filter(|name| name.starts_with(&prefix))
		.collect();
	names.sort();
	Ok(names)
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


