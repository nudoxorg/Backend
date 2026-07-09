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
	BackendKind, Cold, Connect, ConnectError, Cursor, Live, Retryable, Scored,
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

/// Whether qdrant accepts `character` in a collection name — the conservative
/// portable subset (alphanumerics plus `-`, `_`, `.`).
fn qdrant_legal(character: char) -> bool {
	character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

impl CollectionName {
	/// Validate and wrap a raw collection name (non-empty, legal charset).
	pub fn new(raw: impl Into<String>) -> Result<Self, CollectionNameError> {
		let raw = raw.into();
		let trimmed = raw.trim();
		if trimmed.is_empty() {
			return Err(CollectionNameError::Empty);
		}
		if trimmed.chars().all(qdrant_legal) {
			Ok(Self(trimmed.to_owned()))
		} else {
			Err(CollectionNameError::Invalid { raw })
		}
	}

	/// Derive the migration-collection name for a model regime: `sym-<model>`,
	/// with any qdrant-illegal characters in the model id folded to `-`.
	/// Deterministic, so the same model always cuts over to the same collection.
	pub fn for_regime(model: &ModelId) -> Self {
		let sanitized: String = model
			.as_ref()
			.chars()
			.map(|character| if qdrant_legal(character) { character } else { '-' })
			.collect();
		Self(format!("sym-{sanitized}"))
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

/// Dig the configured (unnamed) dense-vector size out of a collection-info
/// reply; `None` when any layer is absent or the collection uses named vectors.
fn configured_dimension(reply: qdrant_client::qdrant::GetCollectionInfoResponse) -> Option<u64> {
	use qdrant_client::qdrant::vectors_config::Config;
	match reply.result?.config?.params?.vectors_config?.config? {
		Config::Params(parameters) => Some(parameters.size),
		Config::ParamsMap(_) => None,
	}
}

impl<M: EmbeddingModel> Connect for Semantic<M, Cold> {
	type Live = Semantic<M, Live>;

	/// Ping qdrant and assert the collection's configured vector size equals
	/// `M::DIMENSIONS`, else fail with [`heart::ConnectFailure::DimensionMismatch`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		use heart::ConnectFailure;
		let fail = |kind| ConnectError::new(BackendKind::Qdrant, kind);

		self.client
			.health_check()
			.await
			.map_err(|error| fail(ConnectFailure::Other(error.into())))?;

		let info = self
			.client
			.collection_info(self.collection.as_str())
			.await
			.map_err(|error| fail(ConnectFailure::Other(error.into())))?;
		// A missing/named-vector layout cannot serve `Embedding<M>` queries at all.
		let found = configured_dimension(info).ok_or_else(|| fail(ConnectFailure::SchemaMismatch))?;
		if found as usize != M::DIMENSIONS {
			return Err(fail(ConnectFailure::DimensionMismatch {
				expected: M::DIMENSIONS,
				found: found as usize,
			}));
		}

		tracing::info!(
			collection = self.collection.as_str(),
			model = %M::id(),
			dimensions = M::DIMENSIONS,
			"vector store verified; promoting to Live"
		);
		Ok(Semantic {
			client: self.client,
			collection: self.collection,
			_model: PhantomData,
			_state: PhantomData,
		})
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
		after: Option<Cursor<SemanticCursorKey>>,
	) -> Result<impl Stream<Item = Result<Scored<SymbolId>, VectorError>> + Send, VectorError> {
		self.search_with_filter(gate, query, limit, after, None).await
	}

	/// The shared gated k-NN body: [`search`](Self::search) plus an optional
	/// qdrant payload `filter` (how language scoping stays inside one
	/// collection). Crate-internal so the gate remains the only public door.
	///
	/// Keyset resume: qdrant cannot filter on the *computed* similarity score,
	/// so pages after a cursor over-fetch (doubling, capped) and skip past the
	/// `(score, id)` key client-side. The cursor's snapshot hash is advisory
	/// here — an eventually-consistent ANN index has no cheap content hash — so
	/// it is carried but not enforced.
	pub(crate) async fn search_with_filter(
		&self,
		gate: SemanticGate,
		query: &Embedding<M>,
		limit: NonZeroUsize,
		after: Option<Cursor<SemanticCursorKey>>,
		filter: Option<qdrant_client::qdrant::Filter>,
	) -> Result<impl Stream<Item = Result<Scored<SymbolId>, VectorError>> + Send, VectorError> {
		use qdrant_client::qdrant::SearchPointsBuilder;

		/// The deepest a keyset resume will dig before truncating the page.
		const MAX_FETCH: u64 = 4096;

		tracing::debug!(
			reason = gate.reason(),
			collection = self.collection.as_str(),
			limit = limit.get(),
			resumed = after.is_some(),
			"semantic search authorized"
		);

		let target = limit.get();
		let mut fetch = match &after {
			None => target as u64,
			Some(_) => (target as u64 * 2).min(MAX_FETCH),
		};
		loop {
			let mut request = SearchPointsBuilder::new(
				self.collection.as_str(),
				query.as_slice().to_vec(),
				fetch,
			)
			.with_payload(false);
			if let Some(filter) = filter.clone() {
				request = request.filter(filter);
			}
			let reply =
				self.client.search_points(request).await.map_err(VectorError::Transport)?;

			let exhausted = (reply.result.len() as u64) < fetch;
			let mut hits = reply
				.result
				.into_iter()
				.map(scored_symbol)
				.collect::<Result<Vec<_>, _>>()?;
			// Stable keyset order: qdrant sorts by score; ties get an id tiebreak.
			hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.cmp(&b.value)));

			let page: Vec<_> = hits
				.into_iter()
				.filter(|hit| {
					after.as_ref().is_none_or(|cursor| {
						let (score, id) = &cursor.after;
						hit.score < *score || (hit.score == *score && hit.value > *id)
					})
				})
				.take(target)
				.collect();

			if page.len() == target || exhausted || fetch >= MAX_FETCH {
				if page.len() < target && !exhausted && fetch >= MAX_FETCH {
					tracing::warn!(fetched = fetch, "keyset resume truncated at fetch ceiling");
				}
				return Ok(futures::stream::iter(page.into_iter().map(Ok)));
			}
			fetch = (fetch * 2).min(MAX_FETCH);
		}
	}

	/// The stored embedding for `symbol`, if the collection holds its point —
	/// the lookup behind similarity-from-a-symbol. Not gated: reading one point
	/// back is cheap; only the k-NN scan is the heavy, gated operation.
	pub(crate) async fn stored_embedding(
		&self,
		symbol: SymbolId,
	) -> Result<Option<Embedding<M>>, VectorError> {
		use qdrant_client::qdrant::{GetPointsBuilder, PointId, vectors_output::VectorsOptions};

		let request = GetPointsBuilder::new(
			self.collection.as_str(),
			vec![PointId::from(symbol.as_uuid().to_string())],
		)
		.with_vectors(true);
		let reply = self.client.get_points(request).await.map_err(VectorError::Transport)?;

		let Some(point) = reply.result.into_iter().next() else { return Ok(None) };
		let values = match point.vectors.and_then(|vectors| vectors.vectors_options) {
			Some(VectorsOptions::Vector(vector)) => vector.data,
			Some(_) => {
				return Err(payload_error(format_args!(
					"point {symbol} stores named vectors, expected a single dense vector"
				)));
			}
			None => return Ok(None),
		};
		Embedding::from_vec(values).map(Some).map_err(VectorError::Embed)
	}

	/// The tower [`Service`] for this store; use [`heart::sink::SinkExt`] methods
	/// to get retry-aware delivery.
	pub fn uploader(&self) -> VectorSink<M> {
		VectorSink {
			client:     Arc::clone(&self.client),
			collection: self.collection.clone(),
			_model:     PhantomData,
		}
	}

}

/// A malformed point reported through [`VectorError::Payload`] with a message
/// naming exactly what was wrong.
fn payload_error(message: impl std::fmt::Display) -> VectorError {
	VectorError::Payload(<serde_json::Error as serde::de::Error>::custom(message))
}

/// Decode one qdrant hit into a [`Scored<SymbolId>`]: the point id **is** the
/// symbol's uuid, so no payload round-trip is needed on the hot path.
fn scored_symbol(point: qdrant_client::qdrant::ScoredPoint) -> Result<Scored<SymbolId>, VectorError> {
	use qdrant_client::qdrant::point_id::PointIdOptions;

	let id = match point.id.and_then(|id| id.point_id_options) {
		Some(PointIdOptions::Uuid(raw)) => raw
			.parse::<heart::Guid>()
			.map(SymbolId::from_uuid)
			.map_err(|_| payload_error(format_args!("point id {raw:?} is not a uuid")))?,
		Some(PointIdOptions::Num(number)) => {
			return Err(payload_error(format_args!("numeric point id {number} is not a symbol")));
		}
		None => return Err(payload_error("hit without a point id")),
	};
	let score = heart::Score::try_new(point.score)
		.map_err(|_| payload_error(format_args!("non-finite score for point {id}")))?;
	Ok(Scored::new(id, score))
}

/// Shape one [`SymbolPoint`] into the qdrant wire form. The payload carries the
/// filterable stamps (`ecosystem`, `purpose`); the vector's model rides on the
/// brand and the collection name, not on every point.
fn point_struct<M: EmbeddingModel>(
	point: SymbolPoint<M>,
) -> qdrant_client::qdrant::PointStruct {
	let mut payload = qdrant_client::Payload::new();
	payload.insert("ecosystem", point.ecosystem.as_token());
	payload.insert("purpose", match point.purpose {
		EmbeddingPurpose::Code => "code",
		EmbeddingPurpose::Documentation => "documentation",
	});
	qdrant_client::qdrant::PointStruct::new(
		point.symbol.as_uuid().to_string(),
		point.embedding.as_slice().to_vec(),
		payload,
	)
}

/// Upsert one chunk (≤ [`VectorSink::MAX_BATCH`]) of points, waiting for the
/// write to be applied so a delivered batch is immediately searchable.
async fn upsert_chunk(
	client: &Qdrant,
	collection: &CollectionName,
	points: Vec<qdrant_client::qdrant::PointStruct>,
) -> Result<(), VectorError> {
	use qdrant_client::qdrant::UpsertPointsBuilder;

	let count = points.len();
	client
		.upsert_points(UpsertPointsBuilder::new(collection.as_str(), points).wait(true))
		.await
		.map_err(VectorError::Transport)?;
	tracing::debug!(collection = collection.as_str(), points = count, "points upserted");
	Ok(())
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
		Box::pin(async move { upsert_chunk(&client, &collection, vec![point_struct(point)]).await })
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
			// Callers are asked to chunk to MAX_BATCH; over-large batches are
			// still delivered correctly by chunking here rather than rejected.
			let mut shaped = points.into_iter().map(point_struct).collect::<Vec<_>>();
			while !shaped.is_empty() {
				let tail = shaped.split_off(shaped.len().min(Self::MAX_BATCH));
				upsert_chunk(&client, &collection, std::mem::replace(&mut shaped, tail)).await?;
			}
			Ok(())
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

impl<M: EmbeddingModel> heart::Probeable for Semantic<M, Live> {
	fn backend(&self) -> BackendKind {
		BackendKind::Qdrant
	}

	async fn probe(&self) -> heart::Probe {
		use crate::error::VectorError;
		heart::timed_probe(BackendKind::Qdrant, async {
			// Cheap one-hit zero-vector query; connectivity is the signal.
			// Gate issued here solely for readiness — not a user-facing search.
			let gate = SemanticGate::issue("readiness probe");
			let query = Embedding::<M>::zeroed();
			match self
				.search(gate, &query, NonZeroUsize::MIN, None)
				.await
			{
				Ok(_) => None,
				Err(error @ (VectorError::Connect(_) | VectorError::Transport(_))) => {
					Some(error.to_string())
				}
				Err(_) => None,
			}
		})
		.await
	}
}

// Ensure the retry classification is wired: the sink machinery consults it.
const _: fn() = || {
	fn assert_retryable<T: Retryable>() {}
	assert_retryable::<VectorError>();
};

// Keep `Symbol` in scope for downstream signatures that shape it into points.
const _: Option<Symbol> = None;
