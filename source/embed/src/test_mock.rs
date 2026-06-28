use async_trait::async_trait;
use nudox_core::{Embedder, Embedding, EmbeddingPurpose, ModelId, ModelType, Result, SourceChunk};

/// A deterministic mock embedder for use in tests.
/// Returns a fixed-dimension vector derived from the input length.
pub struct MockEmbedder {
	model_id: ModelId,
	dim:      usize,
}

impl MockEmbedder {
	/// Create a mock embedder that returns vectors of the given dimension.
	pub fn new(dim: usize) -> Self { MockEmbedder { model_id: ModelId::new("mock"), dim } }
}

#[async_trait]
impl Embedder for MockEmbedder {
	fn model_id(&self) -> &ModelId { &self.model_id }

	fn model_type(&self) -> ModelType { ModelType::Mock }

	async fn embed(&self, _chunk: &SourceChunk, _purpose: EmbeddingPurpose) -> Result<Embedding> {
		Ok(Embedding::new(vec![0.1_f32; self.dim]).expect("mock dim must be > 0"))
	}
}

#[cfg(test)]
mod tests {
	use nudox_core::{ByteSpan, SourceChunk};

	use super::*;

	#[tokio::test]
	async fn mock_embedder_returns_correct_dimension() {
		let embedder = MockEmbedder::new(128);
		let chunk = SourceChunk {
			raw_code:        "fn foo() {}".into(),
			treesitter_repr: None,
			symbol_span:     ByteSpan::covering(0, 1),
		};
		let result = embedder.embed(&chunk, EmbeddingPurpose::Code).await.unwrap();
		assert_eq!(result.len(), 128);
		assert!(result.iter().all(|&v| (v - 0.1_f32).abs() < f32::EPSILON));
	}

	#[tokio::test]
	async fn mock_embedder_model_id_is_mock() {
		let embedder = MockEmbedder::new(64);
		assert_eq!(embedder.model_id().as_str(), "mock");
	}
}
