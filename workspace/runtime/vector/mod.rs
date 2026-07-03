//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
//! it.
//!
//! Two type-level guarantees live here:
//! - the collection's vector dimension is the brand's `M::DIMENSIONS`, so a
//!   wrong-dimension query cannot be formed, and [`Connect`] asserts the live
//!   collection actually has that dimension;
//! - semantic search is reachable only on a [`Live`] store AND only when handed
//!   a [`SemanticGate`], so the heavy path is never taken implicitly.
//!
//! ## Model-migration playbook
//! Vectors are only comparable within one model regime. Re-embedding in place
//! would make the collection briefly incoherent (mixed models rank against each
//! other), so migration is a **cutover**:
//! 1. Create a *second* collection named for the new model (see
//!    [`CollectionName::for_regime`]), same dimension.
//! 2. Re-embed every symbol with the new model into the new collection.
//! 3. Flip the read path to the new collection once backfill is complete.
//! 4. Retire the old collection after a grace period.

use std::{
	future::Future,
	marker::PhantomData,
	num::NonZeroUsize,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use futures::Stream;
use heart::{
	AccessContext, BackendKind, Cold, Connect, ConnectError, Cursor, Live, Retryable, Scored,
	Symbol, SymbolId,
};
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};

pub mod cache;
pub mod embedding;
pub mod gate;
pub mod language;
pub mod model;
pub mod similarity;
pub mod snippet;

pub use cache::{EmbeddingCache, EmbeddingKey};
pub use embedding::{Embedder, Embedding, EmbeddingPurpose};
pub use gate::SemanticGate;
pub use model::{EmbeddingModel, ModelId, catalog as models};

use crate::error::VectorError;

/// The keyset key a semantic-search [`Cursor`] resumes from: the last hit's
/// score paired with its id (score alone is not unique). Ordered so pagination
/// is stable across an eventually-consistent index.
pub type SemanticCursorKey = (heart::Score, SymbolId);

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

	/// Derive the migration-collection name for a model regime.
	pub fn for_regime(model: &ModelId) -> Self {
		let _ = model;
		todo!("deterministic name from model id, e.g. sym-<model>")
	}

	/// The underlying name.
	pub fn as_str(&self) -> &str { &self.0 }
}

/// The wire/payload shape of one stored vector: the symbol identity plus the
/// stamps that make cross-store joins and migrations safe.
///
/// Uploaded via [`Semantic::batch_sink`]; the producing model is carried by the
/// vector's brand `M`, so no separate `model` field is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolPoint<M: EmbeddingModel> {
	/// The symbol this vector represents.
	pub symbol: SymbolId,
	/// The vector itself. Its brand `M` supplies both the dimension and the
	/// producing model, so a separate `model` field is redundant.
	pub embedding: Embedding<M>,
	/// The ecosystem, mirrored into the payload for language-scoped filtering.
	pub ecosystem: heart::Language,
	/// What the vector was embedded as (code vs documentation).
	pub purpose: EmbeddingPurpose,
}

impl<M: EmbeddingModel> SymbolPoint<M> {
	/// The producing model's id — the migration key, derived from the brand.
	pub fn model(&self) -> ModelId { M::id() }
}

/// Our semantic/vector embedding database of choice (Qdrant).
///
/// `M` is the embedding-model brand; `S` is the connection state ([`Cold`] until
/// [`Connect::connect`] verifies it, then [`Live`]).
pub struct Semantic<M: EmbeddingModel, S = Cold> {
	/// The gRPC client to the Qdrant instance (shared so upload sinks are cheap
	/// to clone).
	client: Arc<Qdrant>,

	/// The collection every symbol vector is upserted into / searched against.
	collection: CollectionName,

	_model: PhantomData<fn() -> M>,
	_state: PhantomData<fn() -> S>,
}

impl<M: EmbeddingModel> Semantic<M, Cold> {
	/// Configure a cold handle from an existing client and (validated) collection.
	/// Reachability and dimension are only checked at [`Connect::connect`].
	pub fn new(client: Qdrant, collection: CollectionName) -> Self {
		Self { client: Arc::new(client), collection, _model: PhantomData, _state: PhantomData }
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
	/// keyset [`Cursor`].
	///
	/// Returns a stream so large result sets are not materialized at once.
	pub async fn search(
		&self,
		gate: SemanticGate,
		query: &Embedding<M>,
		limit: NonZeroUsize,
		scope: &AccessContext,
		after: Option<Cursor<SemanticCursorKey>>,
	) -> Result<impl Stream<Item = Result<Scored<SymbolId>, VectorError>> + Send, VectorError> {
		let _ = (gate, query, limit, scope, after, &self.client);
		todo!("build a qdrant search with access + keyset filters, stream Scored<SymbolId>");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}

	/// A retrying single-point [`heart::Sink`] over this store.
	pub fn sink(&self) -> heart::Sink<VectorSink<M>> { heart::Sink::new(self.uploader()) }

	/// A retrying bulk [`heart::BatchSink`] over this store — qdrant ingests
	/// points in bulk, so prefer this for backfills.
	pub fn batch_sink(&self) -> heart::BatchSink<VectorSink<M>> {
		heart::BatchSink::new(self.uploader())
	}

	fn uploader(&self) -> VectorSink<M> {
		VectorSink {
			client:     Arc::clone(&self.client),
			collection: self.collection.clone(),
			_model:     PhantomData,
		}
	}
}

/// The upsert path expressed as a cloneable [`tower::Service`], so wrapping it in
/// [`heart::Sink`] / [`heart::BatchSink`] adds retry/backoff for free (and any
/// further tower layer — concurrency limit, rate limit — composes at the wrap
/// site). Handles `SymbolPoint<M>` (single) and `Vec<SymbolPoint<M>>` (batch,
/// chunk callers to [`VectorSink::MAX_BATCH`]).
pub struct VectorSink<M: EmbeddingModel> {
	client:     Arc<Qdrant>,
	collection: CollectionName,
	_model:     PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> VectorSink<M> {
	/// Qdrant ingests points in bulk; chunk batch callers to this ceiling.
	pub const MAX_BATCH: usize = 256;
}

// Hand-written so the brand `M` (a zero-sized marker, not `Clone`) doesn't leak a
// spurious `M: Clone` bound.
impl<M: EmbeddingModel> Clone for VectorSink<M> {
	fn clone(&self) -> Self {
		Self { client: Arc::clone(&self.client), collection: self.collection.clone(), _model: PhantomData }
	}
}

type UploadFuture = Pin<Box<dyn Future<Output = Result<(), VectorError>> + Send>>;

impl<M: EmbeddingModel> tower::Service<SymbolPoint<M>> for VectorSink<M> {
	type Response = ();
	type Error = VectorError;
	type Future = UploadFuture;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, point: SymbolPoint<M>) -> Self::Future {
		let (client, collection) = (Arc::clone(&self.client), self.collection.clone());
		Box::pin(async move {
			let _ = (client, collection, point);
			todo!("upsert a single point into the collection")
		})
	}
}

impl<M: EmbeddingModel> tower::Service<Vec<SymbolPoint<M>>> for VectorSink<M> {
	type Response = ();
	type Error = VectorError;
	type Future = UploadFuture;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, points: Vec<SymbolPoint<M>>) -> Self::Future {
		let (client, collection) = (Arc::clone(&self.client), self.collection.clone());
		Box::pin(async move {
			let _ = (client, collection, points);
			todo!("upsert a batch of points (<= MAX_BATCH) into the collection")
		})
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
