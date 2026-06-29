//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
//! it.

pub mod embedding;
pub mod language;
pub mod similarity;
pub mod snippet;

pub use embedding::{Embedding, EmbeddingPurpose};

/// Our semantic/vector embedding database of choice (Qdrant)
pub struct Semantic {

}
