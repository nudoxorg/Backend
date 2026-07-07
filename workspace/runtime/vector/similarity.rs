//! Vector similarity kernels and similarity-from-a-symbol queries.

use std::num::NonZeroUsize;

use heart::{SymbolId, Score, Scored};

use crate::{
	error::VectorError,
	vector::{Embedding, SemanticLive, gate::SemanticGate, model::EmbeddingModel},
};

/// Cosine similarity of two embeddings of the same model brand.
///
/// Accumulates in `f64` for stability, clamps into `[-1, 1]` (rounding can
/// nudge past the bound), and maps a zero vector to `0.0` — so the result is
/// always a finite [`Score`].
pub fn cosine<M: EmbeddingModel>(a: &Embedding<M>, b: &Embedding<M>) -> Score {
	let (mut dot, mut norm_a, mut norm_b) = (0.0f64, 0.0f64, 0.0f64);
	for (x, y) in a.as_slice().iter().zip(b.as_slice()) {
		let (x, y) = (f64::from(*x), f64::from(*y));
		dot += x * y;
		norm_a += x * x;
		norm_b += y * y;
	}
	let denominator = norm_a.sqrt() * norm_b.sqrt();
	let value = if denominator == 0.0 { 0.0 } else { (dot / denominator).clamp(-1.0, 1.0) };
	Score::try_new(value as f32).expect("a clamped cosine is finite by construction")
}

/// Find symbols semantically near a *stored* symbol.
pub async fn similar_to<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	symbol: SymbolId,
	limit: NonZeroUsize,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	use futures::TryStreamExt;

	// The query vector is the symbol's own stored point.
	let Some(query) = store.stored_embedding(symbol).await? else {
		// No stored vector means no meaningful neighborhood; an empty result is
		// the honest answer (the symbol simply is not in the semantic surface).
		tracing::warn!(%symbol, "similar_to on a symbol with no stored vector");
		return Ok(Vec::new());
	};
	// Over-fetch by one: the symbol itself is (almost always) its own top hit.
	let widened = NonZeroUsize::new(limit.get().saturating_add(1))
		.expect("a nonzero limit plus one is nonzero");
	let hits: Vec<_> = store
		.search_with_filter(gate, &query, widened, None, None)
		.await?
		.try_collect()
		.await?;
	Ok(hits.into_iter().filter(|hit| hit.value != symbol).take(limit.get()).collect())
}
