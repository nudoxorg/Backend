//! The serving-side vector plane.
//!
//! The pure vector machinery — model brands, embeddings, the `VectorStore` /
//! `Embedder` traits, quantization, routing, admission, fusion — lives in the
//! `vector-core` crate (and its `vector-local` / `vector-remote` / `vector-embed`
//! sibling planes). This module holds only the two *serving* concerns that are
//! not part of that pure plane:
//!
//! - [`gate::SemanticGate`] — the capability token that gates the expensive
//!   semantic-search path (issued by the server's query planner).
//! - [`cache::EmbeddingCache`] — the content-addressed query/document embedding
//!   cache in front of the (network) embedder.
//!
//! Both are built on `vector_core` types; there is no second embedding/model
//! vocabulary here (the former `registry::runtime::vector` duplicate is gone).

pub mod cache;
pub mod gate;

pub use cache::{EmbeddingCache, EmbeddingKey};
pub use gate::SemanticGate;

// The serving plane's single re-export hub for the pure vector vocabulary, so
// consumers say `registry::vector::X` for both the serving pieces above and the
// `vector-core` essentials — there is no second vocabulary.
pub use vector_core::{
	AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel,
	EmbeddingPurpose, JinaCodeV2, ModelId, VoyageCode3, l2_normalize,
};
pub use vector_core::store::{
	Payload, PayloadValue, PointId, SearchFilter, SearchHit, SearchRequest, SourceTag, VectorPoint,
	VectorStore,
};
