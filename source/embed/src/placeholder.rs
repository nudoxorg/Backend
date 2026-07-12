//! Placeholder embedder for development and end-to-end testing.
//!
//! Produces deterministic, content-derived vectors with no external
//! dependencies. Real provider integration (OpenAI, self-hosted models, etc.)
//! is intentionally out of scope for this crate — swap in a different
//! `Embedder` impl when ready.

use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

use async_trait::async_trait;
use nudox_core::{Embedder, Embedding, EmbeddingPurpose, ModelId, ModelType, Result, SourceChunk};

use crate::hash_utils;

/// A deterministic, content-derived embedder for development use.
///
/// Vectors are derived by hashing the input chunk and expanding the seed
/// through a simple LCG. Same input → same vector; different inputs →
/// almost certainly different vectors. NOT a real embedding model — when
/// production embeddings are required, swap in a different `Embedder` impl.
pub struct PlaceholderEmbedder {
	model_id: ModelId,
	dim:      usize,
}

impl PlaceholderEmbedder {
	/// Create a placeholder embedder with the given model id and vector
	/// dimension.
	pub fn new(model_id: impl Into<String>, dim: usize) -> Self {
		PlaceholderEmbedder { model_id: ModelId::new(model_id), dim }
	}
}

#[async_trait]
impl Embedder for PlaceholderEmbedder {
	fn model_id(&self) -> &ModelId { &self.model_id }

	fn model_type(&self) -> ModelType { ModelType::Placeholder }

	async fn embed(&self, chunk: &SourceChunk, purpose: EmbeddingPurpose) -> Result<Embedding> {
		let mut hasher = DefaultHasher::new();
		chunk.raw_code.hash(&mut hasher);
		hash_utils::hash_purpose(&mut hasher, &purpose);
		Ok(hash_utils::deterministic_vector(hasher.finish(), self.dim))
	}
}

#[cfg(test)]
mod tests {
	use nudox_core::{ByteSpan};

	use super::*;

	fn chunk(code: &str) -> SourceChunk {
		SourceChunk {
			raw_code:        code.into(),
			treesitter_repr: None,
			symbol_span:     ByteSpan::covering(0, code.len()),
		}
	}

	#[tokio::test]
	async fn vectors_have_configured_dimension() {
		let e = PlaceholderEmbedder::new("placeholder-v1", 16);
		let v = e.embed(&chunk("fn foo()"), EmbeddingPurpose::Code).await.unwrap();
		assert_eq!(v.len(), 16);
		assert_eq!(e.model_id().as_str(), "placeholder-v1");
	}

	#[tokio::test]
	async fn same_input_yields_same_vector() {
		let e = PlaceholderEmbedder::new("p", 32);
		let v1 = e.embed(&chunk("hello"), EmbeddingPurpose::Code).await.unwrap();
		let v2 = e.embed(&chunk("hello"), EmbeddingPurpose::Code).await.unwrap();
		assert_eq!(v1.as_slice(), v2.as_slice());
	}

	#[tokio::test]
	async fn different_input_yields_different_vector() {
		let e = PlaceholderEmbedder::new("p", 32);
		let v1 = e.embed(&chunk("hello"), EmbeddingPurpose::Code).await.unwrap();
		let v2 = e.embed(&chunk("world"), EmbeddingPurpose::Code).await.unwrap();
		assert_ne!(v1.as_slice(), v2.as_slice());
	}

	#[tokio::test]
	async fn same_text_different_purpose_yields_different_vector() {
		let e = PlaceholderEmbedder::new("p", 32);
		let v1 = e.embed(&chunk("hello"), EmbeddingPurpose::Code).await.unwrap();
		let v2 = e.embed(&chunk("hello"), EmbeddingPurpose::Docstring).await.unwrap();
		assert_ne!(v1.as_slice(), v2.as_slice());
	}

	#[tokio::test]
	async fn values_are_in_unit_range() {
		let e = PlaceholderEmbedder::new("p", 256);
		let v = e.embed(&chunk("some code"), EmbeddingPurpose::Code).await.unwrap();
		assert!(v.iter().all(|f| *f >= -1.0 && *f <= 1.0));
	}
}
