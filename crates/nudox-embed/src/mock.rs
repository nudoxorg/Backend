use async_trait::async_trait;
use nudox_core::{Embedder, EmbeddingPurpose, ModelType, Result, SourceChunk};

/// A deterministic mock embedder for use in tests.
/// Returns a fixed-dimension vector derived from the input length.
pub struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    /// Create a mock embedder that returns vectors of the given dimension.
    pub fn new(dim: usize) -> Self {
        MockEmbedder { dim }
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    fn model_id(&self) -> &str {
        "mock"
    }

    fn model_type(&self) -> ModelType {
        ModelType::Mock
    }

    async fn embed(&self, _chunk: &SourceChunk, _purpose: EmbeddingPurpose) -> Result<Vec<f32>> {
        Ok(vec![0.1_f32; self.dim])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_core::{ByteSpan, SourceChunk, TreesitterRepr};

    #[tokio::test]
    async fn mock_embedder_returns_correct_dimension() {
        let embedder = MockEmbedder::new(128);
        let chunk = SourceChunk {
            raw_code: "fn foo() {}".to_string(),
            treesitter_repr: TreesitterRepr(vec![]),
            symbol_span: ByteSpan { start: 0, end: 1 },
        };
        let result = embedder.embed(&chunk, EmbeddingPurpose::Code).await.unwrap();
        assert_eq!(result.len(), 128);
        assert!(result.iter().all(|&v| (v - 0.1_f32).abs() < f32::EPSILON));
    }

    #[tokio::test]
    async fn mock_embedder_model_id_is_mock() {
        let embedder = MockEmbedder::new(64);
        assert_eq!(embedder.model_id(), "mock");
    }
}
