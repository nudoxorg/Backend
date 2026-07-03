//! The indexing flow: drive one dequeued job through the pipeline phases and
//! record every transition durably.
//!
//! The flow lives on [`Indexer`] — a struct that owns a handle to the assembled
//! server — rather than as free `impl Server` methods, so the pipeline's moving
//! parts (job loop, phase transitions, freshness) sit in one place with the
//! state they operate on.
//!
//! Runs on a background worker (not a request path). Each phase updates the
//! job's [`heart::ResolutionState`]; a failure is classified into a
//! [`heart::FailureKind`] and handed to the queue's retry policy, which decides
//! retry-with-backoff vs dead-letter — so a poison pill can neither livelock nor
//! vanish.

use std::sync::Arc;

use heart::{ContentHash, JobProgress, PackageId, Progressive};
use runtime::vector::EmbeddingModel;

use crate::Server;
use crate::error::ServerResult;

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
	server: Arc<Server<M>>,
}

impl<M: EmbeddingModel> Indexer<M> {
	/// Build an indexer over the assembled server.
	pub fn new(server: Arc<Server<M>>) -> Self { Self { server } }

	/// The server this indexer drives.
	pub fn server(&self) -> &Arc<Server<M>> { &self.server }

	/// Process a single indexing job end to end:
	/// 1. **Acquire** the source archive (via the resolved origin);
	/// 2. **Extract** it through the sanitizing extractor into a content-addressed
	///    blob (bounded memory, spawn_blocking);
	/// 3. **Compile** source → IR (spawn_blocking; drives the compiler);
	/// 4. **Emit** the manifest to the object store and append fan-out intents to
	///    the outbox — all transactional with the `Stored { hash }` transition.
	///
	/// Returns the resulting snapshot [`ContentHash`]. Never fans out to derived
	/// stores directly; that is the pollers' job.
	pub async fn run_indexing_job(&self, package: PackageId) -> ServerResult<ContentHash> {
		let _ = (&self.server, package);
		todo!("acquire -> extract -> compile -> emit+outbox, updating ResolutionState per phase")
	}

	/// The canonical package-content hash used for the freshness check and
	/// recorded as the `Stored { hash }` snapshot — the sorted fold of per-file
	/// digests.
	pub async fn content_hash(&self, package: PackageId) -> ServerResult<ContentHash> {
		let _ = (&self.server, package);
		todo!("stream files through ContentHasher in sorted order")
	}

	/// The live [`JobProgress`] for a package, derived from its persisted
	/// [`heart::ResolutionState`] plus the in-flight phase fraction. This is the
	/// [`Progressive`] view the health/status surface and any progress stream
	/// report through.
	pub async fn job_progress(&self, package: PackageId) -> ServerResult<JobProgress> {
		let _ = (&self.server, package);
		todo!("load ResolutionState + current phase fraction into a JobProgress")
	}

	/// Advance a job's persisted phase and re-derive its progress in one place,
	/// so the state machine and the [`Progressive`] view can never drift.
	async fn advance(&self, package: PackageId, progress: &JobProgress) -> ServerResult<()> {
		// Persist `progress.state`, then surface `progress.overall()` to metrics.
		let _ = (&self.server, package, progress.overall(), progress.current_phase());
		todo!("persist state transition + emit overall()/phase_progress() to metrics")
	}

	/// Run the lease → process → complete/fail loop until shutdown, honouring
	/// `max_inflight_jobs`. (Replaces the old stateless `IndexingWorker`; the
	/// server handle now lives on the indexer itself.)
	pub async fn run_worker(&self) -> ServerResult<()> {
		let _ = &self.server;
		todo!("dequeue_batch(lease) with SKIP LOCKED, drive jobs, reclaim expired leases")
	}
}
