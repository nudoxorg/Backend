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
pub async fn similar_within<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	ecosystem: Language,
	query: &Embedding<M>,
	limit: NonZeroUsize,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	let _ = (store, gate, ecosystem, query, limit);
	todo!("k-NN search with a payload filter pinning ecosystem == the given one")
}
