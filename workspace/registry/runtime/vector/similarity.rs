//! Vector similarity kernels.

use heart::Score;

use crate::vector::{Embedding, model::EmbeddingModel};

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

