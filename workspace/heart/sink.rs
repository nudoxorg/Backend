//! The generic remote-sink upload trait shared by every module that ships data
//! out to an external store (qdrant, terminus, object store, tantivy).
//!
//! The default [`Sink::deliver`] wraps [`Sink::upload`] in the crate-wide
//! backoff policy and consults [`crate::error::Retryable`] on the error, so
//! retry behaviour is uniform and defined once. `BatchSink` layers a
//! backend-declared maximum batch size for stores that ingest in bulk.

use backon::{BackoffBuilder, ExponentialBuilder, Retryable as _};
use serde::{Deserialize, Serialize};

use crate::error::{Retryable, StoreError};

/// The kinds of *derived* read-model store fed from the durable spine: the
/// vector store, the graph store, and the text index. This is the single shared
/// enumeration used for outbox fan-out intents (registry) and rebuild targets
/// (server) — neither re-defines it.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Serialize,
	Deserialize,
	strum::Display,
	strum::EnumString,
	strum::EnumIter,
)]
pub enum DerivedStore {
	/// The semantic vector store (qdrant).
	Vector,
	/// The relationship graph store (terminus).
	Graph,
	/// The full-text search index (tantivy).
	Text,
}

impl DerivedStore {
	/// All derived stores, for iterating fan-out targets.
	pub const ALL: [DerivedStore; 3] = [DerivedStore::Vector, DerivedStore::Graph, DerivedStore::Text];
}

/// A remote sink: an external store we ship produced records to. Implementors
/// need only provide [`upload`](Sink::upload) and (optionally) tune the backoff;
/// delivery-with-retry is provided.
pub trait Sink: Send + Sync {
	/// The record type this sink ingests.
	type Item: Send + Sync;

	/// The failure mode of an upload. Must classify itself as [`Retryable`] so
	/// [`deliver`](Sink::deliver) can decide retries without backend-specific
	/// logic.
	type Error: StoreError + Retryable;

	/// Upload a single record to the store.
	async fn upload(&self, item: Self::Item) -> Result<(), Self::Error>;

	/// The backoff schedule for retries. Exponential with jitter by default.
	fn backoff(&self) -> impl BackoffBuilder { ExponentialBuilder::default().with_jitter() }

	/// Deliver a record, retrying transient failures per the backoff schedule.
	async fn deliver(&self, item: Self::Item) -> Result<(), Self::Error>
	where
		Self::Item: Clone,
	{
		(|| self.upload(item.clone()))
			.retry(self.backoff())
			.when(|e: &Self::Error| e.is_retryable())
			.await
	}
}

/// A sink that ingests records in bulk. Batching cuts round-trips to stores
/// (qdrant, terminus) that expose a maximum batch size.
pub trait BatchSink: Sink {
	/// The largest batch the backend accepts in one call. Callers must chunk to
	/// this size.
	const MAX_BATCH: usize;

	/// Upload a batch (must be `<= MAX_BATCH`).
	async fn upload_batch(&self, items: Vec<Self::Item>) -> Result<(), Self::Error>;

	/// Deliver a batch, retrying transient failures per the backoff schedule.
	async fn deliver_batch(&self, items: Vec<Self::Item>) -> Result<(), Self::Error>
	where
		Vec<Self::Item>: Clone,
	{
		(|| self.upload_batch(items.clone()))
			.retry(self.backoff())
			.when(|e: &Self::Error| e.is_retryable())
			.await
	}
}
