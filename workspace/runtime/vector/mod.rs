//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
//! it.
//!
//! Two type-level guarantees live here:
//! - the collection's vector dimension is the const generic `DIM`, so a
//!   wrong-dimension query cannot be formed;
//! - semantic search is reachable only on a [`Live`] store AND only when handed
//!   a [`SemanticGate`], so the heavy path is never taken implicitly.

use std::{marker::PhantomData, num::NonZeroUsize};

use heart::{Cold, Guid, Live, Score};
use qdrant_client::Qdrant;

pub mod embedding;
pub mod gate;
pub mod language;
pub mod similarity;
pub mod snippet;

pub use embedding::{Embedder, Embedding, EmbeddingPurpose};
pub use gate::SemanticGate;

/// A value paired with its (provably finite) relevance score.
pub struct Scored<T> {
	pub value: T,
	pub score: Score,
}

/// The name of the single global Qdrant collection symbols are upserted into.
/// Scoping (per-language, per-package) lives in each point's payload, not in
/// separate collections.
pub struct CollectionName(String);

/// Raised when a `Cold` store fails to come up (or its collection's dimension
/// disagrees with `DIM`).
#[derive(Debug)]
pub struct ConnectError;

/// Our semantic/vector embedding database of choice (Qdrant).
///
/// `DIM` is the vector dimension; `S` is the connection state ([`Cold`] until
/// [`connect`](Semantic::connect) verifies it, then [`Live`]).
pub struct Semantic<const DIM: usize, S = Cold> {
	/// The gRPC client to the Qdrant instance.
	client: Qdrant,

	/// The collection every symbol vector is upserted into / searched against.
	collection: CollectionName,

	_state: PhantomData<S>,
}

impl<const DIM: usize> Semantic<DIM, Cold> {
	/// Verify the Qdrant collection exists and has dimension `DIM`, promoting the
	/// handle to [`Live`]. Query methods exist only on the `Live` form.
	pub async fn connect(self) -> Result<Semantic<DIM, Live>, ConnectError> {
		let _ = (&self.client, &self.collection);
		todo!("ping qdrant + assert collection dim == DIM, then go Live")
	}
}

impl<const DIM: usize> Semantic<DIM, Live> {
	pub async fn search(
		&self,
		_gate: &SemanticGate,
		_query: &Embedding<DIM>,
		_limit: NonZeroUsize,
	) -> Vec<Scored<Guid>> {
		todo!("query qdrant for the top-k nearest points")
	}
}
