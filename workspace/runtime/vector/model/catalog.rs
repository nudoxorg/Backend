//! The concrete, sealed embedding-model brands the system supports. Adding a
//! model is adding a zero-sized type here; nothing else needs a numeric literal.

use super::{EmbeddingModel, ModelId, sealed::Sealed};

/// Build a static, known-non-empty [`ModelId`].
fn model_id(name: &str) -> ModelId {
	ModelId::try_new(name).expect("static model id is non-empty")
}

/// `intfloat/e5-small-v2` — 384-dim.
pub struct E5Small;
impl Sealed for E5Small {}
impl EmbeddingModel for E5Small {
	const DIMENSIONS: usize = 384;
	fn id() -> ModelId { model_id("intfloat/e5-small-v2") }
}

/// `Qwen/Qwen3-Embedding` — 1024-dim.
pub struct Qwen3;
impl Sealed for Qwen3 {}
impl EmbeddingModel for Qwen3 {
	const DIMENSIONS: usize = 1024;
	fn id() -> ModelId { model_id("Qwen/Qwen3-Embedding") }
}

/// OpenAI `text-embedding-3-small` — 1536-dim.
pub struct OpenAi3Small;
impl Sealed for OpenAi3Small {}
impl EmbeddingModel for OpenAi3Small {
	const DIMENSIONS: usize = 1536;
	fn id() -> ModelId { model_id("openai/text-embedding-3-small") }
}

/// OpenAI `text-embedding-3-large` — 3072-dim.
pub struct OpenAi3Large;
impl Sealed for OpenAi3Large {}
impl EmbeddingModel for OpenAi3Large {
	const DIMENSIONS: usize = 3072;
	fn id() -> ModelId { model_id("openai/text-embedding-3-large") }
}
