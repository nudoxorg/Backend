//! In-process embedder backed by a local model directory.
//!
//! Until real inference (candle/ort) lands, returns deterministic
//! placeholder vectors derived from the model id, chunk content, and purpose.

use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}, path::Path};

use async_trait::async_trait;
use nudox_core::{Embedder, EmbedderError, EmbeddingPurpose, ModelType, Result, SourceChunk};

use crate::hash_utils;

/// Embedder that runs a model in-process (via candle or ort).
/// Until real inference lands, this returns deterministic placeholder vectors.
pub struct InProcessEmbedder {
	model_id: String,
	dim:      usize,
}

impl InProcessEmbedder {
	/// Load an in-process embedder from a model directory.
	///
	/// For now this validates the path shape and derives a stable model id from
	/// the final path component; real model loading is deferred.
	pub fn load(model_path: &Path) -> Result<Self> {
		if model_path.as_os_str().is_empty() {
			return Err(EmbedderError::EmptyPath.into());
		}

		let model_id = model_path
			.file_name()
			.and_then(|value| value.to_str())
			.filter(|value| !value.is_empty())
			.map(ToOwned::to_owned)
			.unwrap_or_else(|| model_path.display().to_string());

		Ok(Self { model_id, dim: hash_utils::DEFAULT_DIMENSION })
	}
}

#[async_trait]
impl Embedder for InProcessEmbedder {
	fn model_id(&self) -> &str { &self.model_id }

	fn model_type(&self) -> ModelType { ModelType::InProcess }

	async fn embed(&self, chunk: &SourceChunk, purpose: EmbeddingPurpose) -> Result<Vec<f32>> {
		let mut hasher = DefaultHasher::new();
		self.model_id.hash(&mut hasher);
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
			symbol_span:     ByteSpan { start: 0, end: code.len() },
		}
	}

	#[test]
	fn in_process_load_derives_model_id_from_path() {
		let embedder = InProcessEmbedder::load(Path::new("/models/nomic-embed")).unwrap();
		assert_eq!(embedder.model_id(), "nomic-embed");
		assert_eq!(embedder.model_type(), ModelType::InProcess);
	}

	#[test]
	fn in_process_load_rejects_empty_path() {
		let result = InProcessEmbedder::load(Path::new(""));
		assert!(matches!(result, Err(nudox_core::Error::Embedder(_))));
	}

	#[tokio::test]
	async fn in_process_embed_is_deterministic() {
		let embedder = InProcessEmbedder::load(Path::new("/models/p1")).unwrap();
		let a = embedder.embed(&chunk("fn x() {}"), EmbeddingPurpose::Code).await.unwrap();
		let b = embedder.embed(&chunk("fn x() {}"), EmbeddingPurpose::Code).await.unwrap();
		assert_eq!(a, b);
		assert_eq!(a.len(), hash_utils::DEFAULT_DIMENSION);
	}
}
