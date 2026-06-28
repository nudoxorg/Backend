use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

use nudox_core::{Embedding, EmbeddingPurpose};

pub(crate) const DEFAULT_DIMENSION: usize = 256;

/// LCG multiplier (Knuth/Newlib, period 2^64).
const LCG_MUL: u64 = 6364136223846793005;
/// LCG addend.
const LCG_ADD: u64 = 1442695040888963407;

pub(crate) fn hash_purpose(hasher: &mut DefaultHasher, purpose: &EmbeddingPurpose) {
	match purpose {
		EmbeddingPurpose::Code => 0u8.hash(hasher),
		EmbeddingPurpose::Docstring => 1u8.hash(hasher),
		EmbeddingPurpose::Comment => 2u8.hash(hasher),
		EmbeddingPurpose::Other(value) => {
			3u8.hash(hasher);
			value.hash(hasher);
		}
		_ => 255u8.hash(hasher),
	}
}

pub(crate) fn deterministic_vector(seed: impl Hash, dim: usize) -> Embedding {
	let mut hasher = DefaultHasher::new();
	seed.hash(&mut hasher);
	let seed = hasher.finish();
	let mut state = seed.wrapping_mul(LCG_MUL).wrapping_add(LCG_ADD);
	let mut out = Vec::with_capacity(dim);
	for _ in 0..dim {
		state = state.wrapping_mul(LCG_MUL).wrapping_add(LCG_ADD);
		let bits = (state >> 33) as u32;
		out.push((bits as f32 / u32::MAX as f32) * 2.0 - 1.0);
	}
	Embedding::new(out).expect("deterministic_vector: dim must be > 0")
}
