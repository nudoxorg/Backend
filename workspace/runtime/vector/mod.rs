//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
//! it.

use std::num::NonZeroU64;

use qdrant_client::Qdrant;

pub mod embedding;
pub mod language;
pub mod similarity;
pub mod snippet;

pub use embedding::{Embedding, EmbeddingPurpose};

/// A value paired with its relevance score, as returned by a search target.
// NOTE: glue placeholder so the relocated search sketches typecheck; mirrors
// `heart::Hit` and will converge with it as the migration lands.
pub struct Scored<T> {
	pub value: T,
	pub score: f32,
}

/// The name of the single global Qdrant collection symbols are upserted into.
/// Scoping (per-language, per-package) lives in each point's payload, not in
/// separate collections.
pub struct CollectionName(String);

/// Our semantic/vector embedding database of choice (Qdrant)
pub struct Semantic {
	/// The live gRPC client to the Qdrant instance.
	client: Qdrant,

	/// The collection every symbol vector is upserted into / searched against.
	collection: CollectionName,

	/// The dimensionality every embedding in `collection` must have.
	vector_dimensions: NonZeroU64,
}
