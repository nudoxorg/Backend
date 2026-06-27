use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

use nudox_core::EmbeddingPurpose;

pub(crate) const DEFAULT_DIMENSION: usize = 256;

pub(crate) fn hash_purpose(hasher: &mut DefaultHasher, purpose: &EmbeddingPurpose) {
	match purpose {
		EmbeddingPurpose::Code => 0u8.hash(hasher),
		EmbeddingPurpose::Docstring => 1u8.hash(hasher),
		EmbeddingPurpose::Comment => 2u8.hash(hasher),
		EmbeddingPurpose::Other(value) => {
			3u8.hash(hasher);
			value.hash(hasher);
		}
	}
}

pub(crate) fn deterministic_vector(seed: impl Hash, dim: usize) -> Vec<f32> {
	let mut hasher = DefaultHasher::new();
	seed.hash(&mut hasher);
	let seed = hasher.finish();
	let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
	let mut out = Vec::with_capacity(dim);
	for _ in 0..dim {
		state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
		let bits = (state >> 33) as u32;
		out.push((bits as f32 / u32::MAX as f32) * 2.0 - 1.0);
	}
	out
}
