//! Vector similarity kernels and similarity-from-a-symbol queries.
//!
//! The ranking metric for the semantic store is cosine similarity. The kernel
//! below is a hot path over `DIM`-length vectors; the eventual implementation is
//! a SIMD target (e.g. `std::simd` / `wide`), so it is isolated here behind a
//! stable signature.

use std::num::NonZeroUsize;

use heart::{AccessContext, GlobalSymbolId, Score, Scored};

use crate::{
	error::VectorError,
	vector::{embedding::{Embedding, EmbeddingModel}, gate::SemanticGate, SemanticLive},
};

/// Cosine similarity of two embeddings of the same model brand, as a
/// provably-finite [`Score`]. Same brand ⇒ same dimension by construction, so no
/// length check is needed — and two *different* models can't be compared at all.
///
/// SIMD KERNEL TARGET: this is the inner loop of every semantic query; it will
/// be lowered to explicit SIMD once the shape stabilizes.
pub fn cosine<M: EmbeddingModel>(a: &Embedding<M>, b: &Embedding<M>) -> Score {
	let _ = (a, b);
	todo!("dot(a,b) / (||a|| * ||b||), clamped into a finite Score")
}

/// Find symbols semantically near a *stored* symbol: fetch its vector, then
/// nearest-neighbour on the store, excluding the symbol itself.
///
/// Gated (semantic path) and access-scoped like every semantic query.
pub async fn similar_to<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	symbol: GlobalSymbolId,
	limit: NonZeroUsize,
	scope: &AccessContext,
) -> Result<Vec<Scored<GlobalSymbolId>>, VectorError> {
	let _ = (store, gate, symbol, limit, scope);
	todo!("look up symbol's stored vector, k-NN search, drop the self-hit")
}
