//! Persisting / restoring the terminus graph so the structural/API-surface graph
//! can be rebuilt from the source of truth.
//!
//! Terminus is a *derived* store under the outbox architecture: its "snapshot"
//! is the recorded [`heart::ContentHash`] generation of the blobs it was
//! materialized from, and its "restore" is a fan-out re-enqueue for the
//! [`DerivedStore::Graph`] sink — the graph consumer re-derives the `Symbol`
//! and `Relation` documents from the package's IR/reference sections. The
//! outbox dedupe on `(package, generation, sink)` keeps every re-emit
//! idempotent, so restoring is always safe to repeat.
//!
//! KNOWN GAP: per-package content verification (compare the graph's `Symbol`
//! documents against the blob-derived expectation) is one visibility change
//! away: [`runtime::graph::Graph`] already walks exactly the needed documents
//! in `symbols_in_package` (`workspace/runtime/graph/mod.rs`), but that method
//! is `pub(crate)` to runtime. Exposing it — or a count, the port of the old
//! `NudoxStore::symbol_count` (`source/store/src/lib.rs`) — plus the
//! symbol-projection read API `crate::poll::materialize` names as its own gap
//! would let [`Server::verify_graph_current`] diff actual documents instead of
//! stopping at watermark-level currency.

use heart::{ContentHash, PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

use super::DerivedStore;
use super::blobs::SinkDrainState;

/// A point-in-time view of the graph store's rebuild machinery. The graph
/// handle exposes no public endpoint/organization metadata (deliberately —
/// credentials ride along with it), so the status is its drain state alone.
#[derive(Debug, Clone)]
pub struct GraphStoreStatus {
	/// The Graph sink's position against the outbox.
	pub drain: SinkDrainState,
}

impl<M: EmbeddingModel> Server<M> {
	/// Rebuild the graph's view of one package from its blob: the store-scoped
	/// form of [`Server::rebuild`], targeting [`DerivedStore::Graph`]. The
	/// graph consumer re-materializes the package's symbol/relation documents
	/// when it drains the intent.
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn rebuild_graph_store(
		&self,
		package: PackageId,
		snapshot: ContentHash,
	) -> ServerResult<()> {
		self.authorize("save.rebuild_graph_store")?;
		self.rebuild(package, DerivedStore::Graph, snapshot).await
	}

	/// The graph store's current rebuild status. A non-zero
	/// [`SinkDrainState::pending`] means relationship queries are answering
	/// from before the newest emitted generations.
	#[tracing::instrument(skip(self))]
	pub async fn graph_store_status(&self) -> ServerResult<GraphStoreStatus> {
		self.authorize("save.graph_store_status")?;
		Ok(GraphStoreStatus { drain: self.sink_drain_state(DerivedStore::Graph).await? })
	}

	/// Whether the graph is current for `package`: the blobs still hold the
	/// recorded snapshot and the Graph sink has drained every intent emitted
	/// for the package. `false` is the re-run-[`rebuild_graph_store`] (or just
	/// wait-for-the-poller) signal.
	///
	/// Watermark-level only — see the module docs for the missing runtime API
	/// that would upgrade this to a document-by-document comparison.
	///
	/// [`rebuild_graph_store`]: Server::rebuild_graph_store
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn verify_graph_current(&self, package: PackageId) -> ServerResult<bool> {
		self.authorize("save.verify_graph_current")?;
		self.derived_store_current(package, DerivedStore::Graph).await
	}
}
