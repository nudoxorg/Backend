//! The indexing flow: drive one dequeued job through the pipeline phases and
//! record every transition durably.
//!
//! Runs on a background worker (not a request path). Each phase updates the
//! job's [`heart::ResolutionState`]; a failure is classified into a
//! [`heart::FailureKind`] and handed to the queue's retry policy, which decides
//! retry-with-backoff vs dead-letter — so a poison pill can neither livelock nor
//! vanish.

use heart::{ContentHash, Generation, JobProgress, Progressive, package::PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

impl<M: EmbeddingModel> Server<M> {
	/// Process a single indexing job end to end:
	/// 1. **Acquire** the source archive (via the resolved origin);
	/// 2. **Extract** it through the sanitizing extractor into a content-addressed
	///    blob (bounded memory, spawn_blocking);
	/// 3. **Compile** source → IR (spawn_blocking; drives the compiler);
	/// 4. **Emit** the manifest to the object store and append fan-out intents to
	///    the outbox — all transactional with the `Stored { hash }` transition.
	///
	/// Returns the resulting [`Generation`]. Never fans out to derived stores
	/// directly; that is the pollers' job.
	pub async fn run_indexing_job(&self, package: PackageId) -> ServerResult<Generation> {
		let _ = package;
		todo!("acquire -> extract -> compile -> emit+outbox, updating ResolutionState per phase")
	}

	/// The canonical package-content hash used for the freshness check and the
	/// generation stamp — the sorted fold of per-file digests.
	pub async fn compute_generation(&self, package: PackageId) -> ServerResult<ContentHash> {
		let _ = package;
		todo!("stream files through ContentHasher in sorted order")
	}

	/// The live [`JobProgress`] for a package, derived from its persisted
	/// [`heart::ResolutionState`] plus the in-flight phase fraction. This is the
	/// [`Progressive`] view the health/status surface and any progress stream
	/// report through.
	pub async fn job_progress(&self, package: PackageId) -> ServerResult<JobProgress> {
		let _ = package;
		todo!("load ResolutionState + current phase fraction into a JobProgress")
	}

	/// Advance a job's persisted phase and re-derive its progress in one place,
	/// so the state machine and the [`Progressive`] view can never drift.
	async fn advance(&self, package: PackageId, progress: &JobProgress) -> ServerResult<()> {
		// Persist `progress.state`, then surface `progress.overall()` to metrics.
		let _ = (package, progress.overall(), progress.current_phase());
		todo!("persist state transition + emit overall()/phase_progress() to metrics")
	}
}

/// A background worker that leases jobs from the registry queue and drives each
/// through [`Server::run_indexing_job`], honouring `max_inflight_jobs`.
pub struct IndexingWorker;

impl IndexingWorker {
	/// Run the lease → process → complete/fail loop until shutdown.
	pub async fn run<M: EmbeddingModel>(server: std::sync::Arc<Server<M>>) -> ServerResult<()> {
		let _ = server;
		todo!("dequeue_batch(lease) with SKIP LOCKED, drive jobs, reclaim expired leases")
	}
}
