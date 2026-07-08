//! Persisting / restoring the qdrant vector collection so semantic search can be
//! rebuilt deterministically from the blobs.
//!
//! Under the outbox architecture there is no bespoke dump/load pair: the
//! collection is a *derived* store, so its "snapshot" is the recorded
//! [`heart::ContentHash`] generation and its "restore" is a fan-out re-enqueue
//! for the [`DerivedStore::Vector`] sink — the vector consumer re-materializes
//! from the blobs, and the outbox dedupe on `(package, generation, sink)` keeps
//! the whole operation idempotent. (Changing the *embedding model* is a
//! different operation entirely — a cutover to a second collection; see the
//! migration playbook in `runtime::vector`'s module docs.)
//!
//! KNOWN GAP: content-level verification (do the collection's points match the
//! blob-derived expectation?) needs two APIs that do not exist yet:
//! - a count/enumeration surface on [`runtime::vector::Semantic`] — the old
//!   client had exactly this (`count_points` / the dimension assertion in
//!   `source/search/src/integration/qdrant.rs`), but only the dimension check
//!   was ported (into `Connect`); a public point count belongs in
//!   `workspace/runtime/vector/mod.rs`;
//! - the symbol-projection read API `crate::poll::materialize` names as its own
//!   known gap, to know which symbols a package *should* hold points for.
//!
//! Until those land, [`Server::verify_vector_current`] proves the weaker
//! watermark-level property: the blobs still hold the recorded generation and
//! the Vector sink has acknowledged every intent emitted for the package.

use heart::{ContentHash, PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

use super::DerivedStore;
use super::blobs::SinkDrainState;

/// A point-in-time view of the vector store's rebuild machinery: which
/// collection it serves from and how far its consumer lags the emission log.
#[derive(Debug, Clone)]
pub struct VectorStoreStatus {
	/// The qdrant collection every symbol vector lives in (the model-branded
	/// collection the read path is currently cut over to).
	pub collection: String,

	/// The Vector sink's position against the outbox.
	pub drain: SinkDrainState,
}

impl<M: EmbeddingModel> Server<M> {
	/// Rebuild the vector collection's view of one package from its blob: the
	/// store-scoped form of [`Server::rebuild`], targeting
	/// [`DerivedStore::Vector`]. The vector consumer re-embeds and re-upserts
	/// the package's points when it drains the intent.
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn rebuild_vector_store(
		&self,
		package: PackageId,
		snapshot: ContentHash,
	) -> ServerResult<()> {
		self.authorize("save.rebuild_vector_store")?;
		self.rebuild(package, DerivedStore::Vector, snapshot).await
	}

	/// The vector store's current rebuild status: its collection plus the
	/// Vector sink's drain state. A non-zero [`SinkDrainState::pending`] means
	/// semantic search is serving from before the newest emitted generations.
	#[tracing::instrument(skip(self))]
	pub async fn vector_store_status(&self) -> ServerResult<VectorStoreStatus> {
		self.authorize("save.vector_store_status")?;
		Ok(VectorStoreStatus {
			collection: self.base().semantics.collection().as_str().to_owned(),
			drain: self.sink_drain_state(DerivedStore::Vector).await?,
		})
	}

	/// Whether the vector store is current for `package`: the blobs still hold
	/// the recorded snapshot and the Vector sink has drained every intent
	/// emitted for the package. `false` is the re-run-[`rebuild_vector_store`]
	/// (or just wait-for-the-poller) signal.
	///
	/// Watermark-level only — see the module docs for the two missing APIs that
	/// would upgrade this to a content-level point-by-point comparison.
	///
	/// [`rebuild_vector_store`]: Server::rebuild_vector_store
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn verify_vector_current(&self, package: PackageId) -> ServerResult<bool> {
		self.authorize("save.verify_vector_current")?;
		self.derived_store_current(package, DerivedStore::Vector).await
	}
}
