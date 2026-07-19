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

/// OpenAI `text-embedding-3-small` — 1536-dim.
pub struct OpenAi3Small;
impl Sealed for OpenAi3Small {}
impl EmbeddingModel for OpenAi3Small {
	const DIMENSIONS: usize = 1536;
	fn id() -> ModelId { model_id("openai/text-embedding-3-small") }
}

/// `jinaai/jina-embeddings-v2-base-code` — 768-dim.
pub struct JinaCodeV2;
impl Sealed for JinaCodeV2 {}
impl EmbeddingModel for JinaCodeV2 {
	const DIMENSIONS: usize = 768;
	fn id() -> ModelId { model_id("jinaai/jina-embeddings-v2-base-code") }
}

/// `voyage/voyage-code-3` — 1024-dim.
pub struct VoyageCode3;
impl Sealed for VoyageCode3 {}
impl EmbeddingModel for VoyageCode3 {
	const DIMENSIONS: usize = 1024;
	fn id() -> ModelId { model_id("voyage/voyage-code-3") }
}

