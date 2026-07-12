//! Persisting / restoring the tantivy indexes (symbol + registry search) so
//! precise search can be rebuilt from the source of truth.
//!
//! Both tantivy indexes are **replica-local projections**, which changes what
//! "persist" and "restore" mean here: the local directories need no durable
//! backup at all, because a fresh replica rebuilds them from postgres — that is
//! the point of the poller design. The two indexes catch up along different
//! spines, so restore is two different levers:
//! - the *symbol* index is fed by the Text sink's outbox watermark — restoring
//!   a package is a fan-out re-enqueue for [`DerivedStore::Text`]
//!   ([`Server::rebuild_text_index`]);
//! - the *package* (registry-search) index is fed by the postgres sync
//!   watermark — restoring it is forcing a synchronize tick
//!   ([`Server::resync_package_index`]).
//!
//! The old index's persistence property (upsert-by-id over a reopened
//! directory, `source/search/src/integration/tantivy.rs`) is carried by
//! [`runtime::text::TextIndex`] itself; this module is the rebuild/verify
//! surface over it.
//!
//! KNOWN GAP: per-package verification of the *symbol* index needs a
//! package-scoped query — the tantivy schema stores a `package` field
//! (`workspace/runtime/text/index.rs`) but [`runtime::text::TextQuery`]
//! (`workspace/runtime/text/query.rs`) cannot filter on it — plus the
//! symbol-projection read API `crate::poll::materialize` names as its own gap.
//! Until those land, [`Server::verify_text_current`] proves watermark-level
//! currency, and [`Server::text_index_status`] exposes the snapshot hashes a
//! caller can compare across replicas.

use heart::{ContentHash, PackageId};
use registry::search::tantivy::SyncWatermark;

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::{ServerError, ServerResult};

use super::DerivedStore;
use super::blobs::SinkDrainState;

/// A point-in-time view of both replica-local tantivy indexes: their content
/// snapshots (the hashes search cursors anchor to) and the symbol index's
/// position against the emission log.
#[derive(Debug, Clone)]
pub struct TextIndexStatus {
	/// The symbol index's searchable-state hash (segment ids + live/deleted doc
	/// counts) — any commit that changes visible documents changes it.
	pub symbol_snapshot: ContentHash,

	/// The package-search index's snapshot, derived from its sync watermark.
	pub package_snapshot: ContentHash,

	/// The Text sink's position against the outbox.
	pub drain: SinkDrainState,
}

impl<M: EmbeddingModel> Server<M> {
	/// Rebuild the symbol index's view of one package from its blob: the
	/// store-scoped form of [`Server::rebuild`], targeting
	/// [`DerivedStore::Text`]. The text poller re-upserts the package's symbol
	/// documents (upsert-by-id, so replay never duplicates) when it drains the
	/// intent.
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn rebuild_text_index(
		&self,
		package: PackageId,
		snapshot: ContentHash,
	) -> ServerResult<()> {
		self.authorize("save.rebuild_text_index")?;
		self.rebuild(package, DerivedStore::Text, snapshot).await
	}

	/// Force one synchronize tick of the replica-local package-search index —
	/// the restore lever for the registry-search half, which follows the
	/// postgres sync watermark rather than the outbox. Idempotent and
	/// resumable; returns the watermark the index caught up to.
	#[tracing::instrument(skip(self))]
	pub async fn resync_package_index(&self) -> ServerResult<SyncWatermark> {
		self.authorize("save.resync_package_index")?;
		self.base().packages.synchronize().await
	}

	/// Both tantivy indexes' current rebuild status. A non-zero
	/// [`SinkDrainState::pending`] means precise symbol search is serving from
	/// before the newest emitted generations; diverging snapshot hashes across
	/// replicas mean their local projections have not converged yet.
	#[tracing::instrument(skip(self))]
	pub async fn text_index_status(&self) -> ServerResult<TextIndexStatus> {
		self.authorize("save.text_index_status")?;
		let stores = self.base();

		// `TextIndex::snapshot` reloads the reader and hashes segment *metadata*
		// — cheap enough to stay on the async worker (unlike indexing, which the
		// runtime dispatches to spawn_blocking).
		let symbol_snapshot =
			stores.text.snapshot().map_err(|error| ServerError::Runtime(error.into()))?;
		let package_snapshot = stores.packages.snapshot().await;
		Ok(TextIndexStatus {
			symbol_snapshot,
			package_snapshot,
			drain: self.sink_drain_state(DerivedStore::Text).await?,
		})
	}

	/// Whether the symbol index is current for `package`: the blobs still hold
	/// the recorded snapshot and the Text sink has drained every intent emitted
	/// for the package. `false` is the re-run-[`rebuild_text_index`] (or just
	/// wait-for-the-poller) signal.
	///
	/// Watermark-level only — see the module docs for the package-scoped query
	/// that would upgrade this to a document-by-document comparison.
	///
	/// [`rebuild_text_index`]: Server::rebuild_text_index
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn verify_text_current(&self, package: PackageId) -> ServerResult<bool> {
		self.authorize("save.verify_text_current")?;
		self.derived_store_current(package, DerivedStore::Text).await
	}
}
