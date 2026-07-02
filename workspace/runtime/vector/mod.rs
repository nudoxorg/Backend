//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
//! it.
//!
//! Two type-level guarantees live here:
//! - the collection's vector dimension is the const generic `DIM`, so a
//!   wrong-dimension query cannot be formed, and [`Connect`] asserts the live
//!   collection actually has that dimension;
//! - semantic search is reachable only on a [`Live`] store AND only when handed
//!   a [`SemanticGate`], so the heavy path is never taken implicitly.
//!
//! ## Model-migration playbook
//! Vectors are only comparable within one `(ModelId, Generation)` regime.
//! Re-embedding in place would make the collection briefly incoherent (mixed
//! models rank against each other), so migration is a **cutover**:
//! 1. Create a *second* collection named for the new `(ModelId, Generation)`
//!    (see [`CollectionName`]), same `DIM`.
//! 2. Re-embed every symbol with the new model into the new collection, its
//!    payload carrying the new generation stamp.
//! 3. Flip the read path to the new collection once backfill is complete.
//! 4. Retire the old collection after a grace period.
//!
//! Because each point's payload carries its `Generation` and `ModelId`, a query
//! can detect (and refuse) cross-regime skew rather than silently mixing.

use std::{marker::PhantomData, num::NonZeroUsize};

use futures::Stream;
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};

use heart::{
	AccessContext, BackendKind, Cold, Connect, ConnectError, Cursor, Generation, GlobalSymbolId,
	Live, Scored, Sink, BatchSink, Retryable, Symbol,
};

pub mod cache;
pub mod embedding;
pub mod gate;
pub mod language;
pub mod similarity;
pub mod snippet;

pub use cache::{EmbeddingCache, EmbeddingKey};
pub use embedding::{Embedder, Embedding, EmbeddingModel, EmbeddingPurpose, models};
pub use gate::SemanticGate;

use crate::error::VectorError;

/// The keyset key a semantic-search [`Cursor`] resumes from: the last hit's
/// score paired with its id (score alone is not unique). Ordered so pagination
/// is stable across an eventually-consistent index.
pub type SemanticCursorKey = (heart::Score, GlobalSymbolId);

/// A connected [`Semantic`] store — the form query methods live on.
pub type SemanticLive<M> = Semantic<M, Live>;

/// Why a raw collection name was rejected.
#[derive(Debug, thiserror::Error)]
pub enum CollectionNameError {
	/// The name was empty after trimming.
	#[error("collection name is empty")]
	Empty,

	/// The name contained characters qdrant disallows in a collection name.
	#[error("collection name {raw:?} contains illegal characters")]
	Invalid {
		/// The offending raw input.
		raw: String,
	},
}

/// The validated name of a Qdrant collection.
///
/// A single global collection holds every symbol vector; scoping (per-language,
/// per-package) lives in each point's payload, not in separate collections. The
/// exception is model migration, which spins up a *named* second collection
/// (see the module-level playbook) — hence the name is a first-class,
/// constructor-validated value rather than a bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollectionName(String);

impl CollectionName {
	/// Validate and wrap a raw collection name (non-empty, legal charset).
	pub fn new(raw: impl Into<String>) -> Result<Self, CollectionNameError> {
		let _ = raw;
		todo!("trim, reject empty, validate qdrant-legal charset, wrap")
	}

	/// Derive the migration-collection name for a `(ModelId, Generation)` regime.
	pub fn for_regime(model: &heart::ModelId, generation: Generation) -> Self {
		let _ = (model, generation);
		todo!("deterministic name from model id + generation, e.g. sym-<model>-<gen>")
	}

	/// The underlying name.
	pub fn as_str(&self) -> &str { &self.0 }
}

/// The wire/payload shape of one stored vector: the symbol identity plus the
/// stamps that make cross-store joins and migrations safe.
///
/// Uploaded via the [`BatchSink`] impl; the generation stamp lives in the
/// qdrant point payload so version skew is observable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolPoint<M: EmbeddingModel> {
	/// The symbol this vector represents.
	pub symbol: GlobalSymbolId,
	/// The vector itself. Its brand `M` supplies both the dimension and the
	/// producing model, so a separate `model` field is redundant.
	pub embedding: Embedding<M>,
	/// The package generation this vector reflects (skew detection).
	pub generation: Generation,
	/// The ecosystem, mirrored into the payload for language-scoped filtering.
	pub ecosystem: heart::Ecosystem,
	/// What the vector was embedded as (code vs documentation).
	pub purpose: EmbeddingPurpose,
}

impl<M: EmbeddingModel> SymbolPoint<M> {
	/// The producing model's id — the migration key, derived from the brand.
	pub fn model(&self) -> heart::ModelId { M::id() }
}

/// Our semantic/vector embedding database of choice (Qdrant).
///
/// `DIM` is the vector dimension; `S` is the connection state ([`Cold`] until
/// [`Connect::connect`] verifies it, then [`Live`]).
pub struct Semantic<M: EmbeddingModel, S = Cold> {
	/// The gRPC client to the Qdrant instance.
	client: Qdrant,

	/// The collection every symbol vector is upserted into / searched against.
	collection: CollectionName,

	_model: PhantomData<fn() -> M>,
	_state: PhantomData<fn() -> S>,
}

impl<M: EmbeddingModel> Semantic<M, Cold> {
	/// Configure a cold handle from an existing client and (validated) collection.
	/// Reachability and dimension are only checked at [`Connect::connect`].
	pub fn new(client: Qdrant, collection: CollectionName) -> Self {
		Self { client, collection, _model: PhantomData, _state: PhantomData }
	}
}

impl<M: EmbeddingModel> Connect for Semantic<M, Cold> {
	type Live = Semantic<M, Live>;

	/// Ping qdrant and assert the collection's configured vector size equals
	/// `M::DIMENSIONS`, else fail with [`heart::ConnectFailure::DimensionMismatch`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		let _ = (&self.client, &self.collection, BackendKind::Qdrant, M::DIMENSIONS);
		todo!("ping qdrant; read collection info; assert vector size == M::DIMENSIONS else DimensionMismatch")
	}
}

impl<M: EmbeddingModel> Semantic<M, Live> {
	/// The collection this store reads/writes.
	pub fn collection(&self) -> &CollectionName { &self.collection }

	/// Nearest-neighbour semantic search, ranked by cosine.
	///
	/// Gated: the [`SemanticGate`] is **consumed by value**, so one issuance
	/// authorizes exactly one query and cannot be stashed and reused. Every hit
	/// is access-checked against `scope`. `after` resumes a previous page via the
	/// keyset [`Cursor`]; a page served against a newer generation than the
	/// cursor is flagged as [`VectorError::GenerationSkew`].
	///
	/// Returns a stream so large result sets are not materialized at once.
	pub async fn search(
		&self,
		gate: SemanticGate,
		query: &Embedding<M>,
		limit: NonZeroUsize,
		scope: &AccessContext,
		after: Option<Cursor<SemanticCursorKey>>,
	) -> Result<
		impl Stream<Item = Result<Scored<GlobalSymbolId>, VectorError>> + Send,
		VectorError,
	> {
		let _ = (gate, query, limit, scope, after, &self.client);
		todo!("build a qdrant search with access + keyset filters, stream Scored<GlobalSymbolId>");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}
}

/// Upserting vectors is a [`BatchSink`]: symbol points ship to qdrant in bulk,
/// each carrying its generation stamp in the payload. Delivery-with-retry and
/// backoff are inherited from [`heart::Sink`].
impl<M: EmbeddingModel> Sink for Semantic<M, Live> {
	type Item = SymbolPoint<M>;
	type Error = VectorError;

	async fn upload(&self, item: Self::Item) -> Result<(), Self::Error> {
		let _ = (item, &self.client, &self.collection);
		todo!("upsert a single point into the collection")
	}
}

impl<M: EmbeddingModel> BatchSink for Semantic<M, Live> {
	/// Qdrant ingests points in bulk; chunk callers to this ceiling.
	const MAX_BATCH: usize = 256;

	async fn upload_batch(&self, items: Vec<Self::Item>) -> Result<(), Self::Error> {
		let _ = (items, &self.client, &self.collection);
		todo!("upsert a batch of points (<= MAX_BATCH) into the collection")
	}
}

/// Assert the semantic query futures are `Send` (so the store composes into a
/// multi-threaded server) without boxing. Mirrors the graph store's RTN guard.
pub fn assert_semantic_futures_send<M: EmbeddingModel>()
where
	Semantic<M, Cold>: Connect<connect(..): Send>,
{
}

// Ensure the retry classification is wired: the sink machinery consults it.
const _: fn() = || {
	fn assert_retryable<T: Retryable>() {}
	assert_retryable::<VectorError>();
};

// Keep `Symbol` in scope for downstream signatures that shape it into points.
const _: Option<Symbol> = None;
