//! Per-ecosystem embedding-text shaping and language-scoped similarity.

use std::num::NonZeroUsize;

use heart::{Language, SymbolId, Scored, Symbol};

use crate::{
	error::VectorError,
	vector::{
		Embedding, SemanticLive,
		embedding::EmbeddingPurpose,
		gate::SemanticGate,
		model::EmbeddingModel,
	},
};

/// Shapes a [`Symbol`]'s fields into the text handed to the embedder.
pub trait EmbeddingText: Send + Sync {
	fn ecosystem(&self) -> Language;
	fn shape(&self, symbol: &Symbol, purpose: EmbeddingPurpose) -> String;
}

/// Semantic search restricted to a single `ecosystem`.
///
/// One collection, payload-level scoping: the filter pins each hit's stored
/// `ecosystem` stamp to the requested language, so no per-language collection
/// (and no cross-language leakage) exists.
pub async fn similar_within<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	ecosystem: Language,
	query: &Embedding<M>,
	limit: NonZeroUsize,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	use futures::TryStreamExt;
	use qdrant_client::qdrant::{Condition, Filter};

	let scope = Filter::must([Condition::matches("ecosystem", ecosystem.as_token().to_owned())]);
	store
		.search_with_filter(gate, query, limit, None, Some(scope))
		.await?
		.try_collect()
		.await
}
