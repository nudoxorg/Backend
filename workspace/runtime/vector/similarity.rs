//! Vector similarity kernels and similarity-from-a-symbol queries.

use std::num::NonZeroUsize;

use heart::{SymbolId, Score, Scored};

use crate::{
	error::VectorError,
	vector::{Embedding, SemanticLive, gate::SemanticGate, model::EmbeddingModel},
};

/// Cosine similarity of two embeddings of the same model brand.
pub fn cosine<M: EmbeddingModel>(a: &Embedding<M>, b: &Embedding<M>) -> Score {
	let _ = (a, b);
	todo!("dot(a,b) / (||a|| * ||b||), clamped into a finite Score")
}

/// Find symbols semantically near a *stored* symbol.
pub async fn similar_to<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	symbol: SymbolId,
	limit: NonZeroUsize,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	let _ = (store, gate, symbol, limit);
	todo!("look up symbol's stored vector, k-NN search, drop the self-hit")
}
