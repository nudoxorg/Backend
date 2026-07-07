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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use heart::{
	ContentHash, FailureKind, JobProgress, PackageId, Percent, Phase, Progressive,
	ResolutionState, Retryable,
};
use registry::identity::PackageCoordinates;
use registry::ingest::{ArchiveFormat, EntryAllowlist, ExtractionLimits, ingest_archive};
use registry::queue::Job;
use registry::{RegistryError, error::ResolveError};
use runtime::vector::EmbeddingModel;

use crate::error::{ServerError, ServerResult};
use crate::{Server, SourceStores};

/// How long a claimed job's lease lasts — comfortably above the job deadline so
/// a live worker never loses a race with the reclaimer.
const JOB_LEASE: Duration = Duration::from_secs(15 * 60);

/// The hard deadline for one job, end to end.
const JOB_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
	server: Arc<Server<M>>,

	/// The archive-acquisition client.
	acquisition: reqwest::Client,

	/// In-flight per-package phase fractions, feeding the [`Progressive`] view.
	fractions: Mutex<HashMap<PackageId, Percent>>,
}

impl<M: EmbeddingModel> Indexer<M> {
	/// Build an indexer over the assembled server.
	pub fn new(server: Arc<Server<M>>) -> Self {
		Self { server, acquisition: reqwest::Client::new(), fractions: Mutex::new(HashMap::new()) }
	}

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
		self.run_indexing_job_on(self.server.base(), package).await
	}

	/// The per-source pipeline behind [`Self::run_indexing_job`].
	async fn run_indexing_job_on(
		&self,
		stores: &SourceStores<M>,
		package: PackageId,
	) -> ServerResult<ContentHash> {
		// ── Acquire ────────────────────────────────────────────────────────────
		self.advance(stores, package, &progressing(Phase::Acquiring)).await?;
		let record = stores.global_store.get(package).await.map_err(RegistryError::from)?;
		let coordinates = record.package.coordinates.clone();
		let archive = self.fetch_archive(&coordinates).await?;
		tracing::info!(%package, bytes = archive.len(), "source archive acquired");

		// ── Extract ────────────────────────────────────────────────────────────
		self.advance(stores, package, &progressing(Phase::Extracting)).await?;
		let mut builder = ingest_archive(
			package,
			record.package.toolchain.clone(),
			std::io::Cursor::new(archive),
			ArchiveFormat::TarGz,
			ExtractionLimits::DEFAULT,
			EntryAllowlist::SAFE,
		)
		.await
		.map_err(RegistryError::from)?;

		// ── Compile ────────────────────────────────────────────────────────────
		self.advance(stores, package, &progressing(Phase::Compiling)).await?;
		// KNOWN GAP: the compiler crate is not a server dependency yet, so the IR
		// and cross-reference sections are structurally-valid empties. The blob,
		// hashing, fan-out, and lifecycle machinery around them is complete.
		let intermediate = empty_intermediate_representation()?;
		builder.set_ir(intermediate).map_err(RegistryError::from)?;
		builder
			.set_references(&registry::blob::ReferenceSet { by_file: Vec::new() })
			.map_err(RegistryError::from)?;

		// ── Emit ───────────────────────────────────────────────────────────────
		self.advance(stores, package, &progressing(Phase::Emitting)).await?;
		let (manifest, sections) = builder.finalize().map_err(RegistryError::from)?;
		let snapshot = ContentHash::of_bytes(&manifest.identity_bytes());
		let emitted = registry::blob::emit::emit(&stores.blobs, &stores.outbox, manifest, sections)
			.await
			.map_err(RegistryError::from)?;

		// The transactional boundary: `Stored { snapshot }` and the per-sink
		// fan-out intents commit together (emit's own appends dedupe against it).
		stores
			.outbox
			.record_stored(&stores.global_store, package, snapshot)
			.await
			.map_err(RegistryError::from)?;

		if let Ok(mut fractions) = self.fractions.lock() {
			fractions.remove(&package);
		}
		tracing::info!(
			%package,
			written = emitted.written,
			deduped = emitted.deduped,
			snapshot = ?snapshot,
			"package stored"
		);
		Ok(snapshot)
	}

	/// The canonical package-content hash used for the freshness check and
	/// recorded as the `Stored { hash }` snapshot — the sorted fold of per-file
	/// digests.
	pub async fn content_hash(&self, package: PackageId) -> ServerResult<ContentHash> {
		let stores = self.server.base();
		let record = stores.global_store.get(package).await.map_err(RegistryError::from)?;
		let manifest = stores
			.blobs
			.get_manifest(&record.package.coordinates)
			.await
			.map_err(RegistryError::from)?;
		// `identity_bytes` is the registry's canonical "same snapshot" encoding —
		// the exact fold the emit path recorded.
		Ok(ContentHash::of_bytes(&manifest.identity_bytes()))
	}

	/// The live [`JobProgress`] for a package, derived from its persisted
	/// [`heart::ResolutionState`] plus the in-flight phase fraction. This is the
	/// [`Progressive`] view the health/status surface and any progress stream
	/// report through.
	pub async fn job_progress(&self, package: PackageId) -> ServerResult<JobProgress> {
		let state = self
			.server
			.base()
			.global_store
			.get_state(package)
			.await
			.map_err(RegistryError::from)?;
		let phase_fraction = self
			.fractions
			.lock()
			.ok()
			.and_then(|fractions| fractions.get(&package).copied())
			.unwrap_or_else(zero_percent);
		Ok(JobProgress { state, phase_fraction })
	}

	/// Advance a job's persisted phase and re-derive its progress in one place,
	/// so the state machine and the [`Progressive`] view can never drift.
	async fn advance(
		&self,
		stores: &SourceStores<M>,
		package: PackageId,
		progress: &JobProgress,
	) -> ServerResult<()> {
		stores
			.global_store
			.set_state(package, &progress.state)
			.await
			.map_err(RegistryError::from)?;
		if let Ok(mut fractions) = self.fractions.lock() {
			fractions.insert(package, progress.phase_fraction);
		}
		metrics::gauge!("indexing_progress_overall", "package" => package.to_string())
			.set(progress.overall().into_inner() as f64);
		if let Some(phase) = progress.current_phase() {
			tracing::debug!(%package, %phase, overall = progress.overall().into_inner(), "phase advanced");
		}
		Ok(())
	}

	/// Run the lease → process → complete/fail loop until shutdown, honouring
	/// `max_inflight_jobs`. (Replaces the old stateless `IndexingWorker`; the
	/// server handle now lives on the indexer itself.)
	pub async fn run_worker(&self) -> ServerResult<()> {
		let limits = self.server.config().limits;
		loop {
			for sourced in self.server.federation().in_precedence() {
				let stores = sourced.value;

				// Return crashed workers' jobs to the runnable set first.
				let reclaimed = stores
					.queue
					.reclaim_expired_leases()
					.await
					.map_err(RegistryError::from)?;
				if reclaimed > 0 {
					tracing::warn!(source = %sourced.source, reclaimed, "reclaimed expired job leases");
				}

				let jobs = stores
					.queue
					.dequeue_batch(limits.max_inflight_jobs, JOB_LEASE)
					.await
					.map_err(RegistryError::from)?;
				futures::stream::iter(jobs)
					.for_each_concurrent(limits.max_inflight_jobs, |job| {
						self.drive_job(stores, job)
					})
					.await;
			}
			tokio::time::sleep(limits.poll_interval).await;
		}
	}

	/// One claimed job: run the pipeline under the deadline, then settle the
	/// queue row — completion on success, retry-or-dead-letter on failure. Job
	/// failures are contained here; only queue/store faults escape to the
	/// supervisor.
	async fn drive_job(&self, stores: &SourceStores<M>, job: Job) {
		let package = job.package;
		let outcome = match tokio::time::timeout(
			JOB_DEADLINE,
			self.run_indexing_job_on(stores, package),
		)
		.await
		{
			Ok(outcome) => outcome,
			Err(_) => Err(ServerError::Internal(format!(
				"indexing exceeded the {JOB_DEADLINE:?} deadline"
			))),
		};

		match outcome {
			Ok(snapshot) => {
				let stored = ResolutionState::Stored { hash: snapshot };
				if let Err(error) = stores.queue.complete(job.id, &stored).await {
					tracing::warn!(%package, error = %error, "job completed but settling raced");
				}
			}
			Err(error) => {
				let kind = classify_failure(&error);
				let message = error.to_string();
				tracing::warn!(%package, %kind, error = %message, "indexing job failed");

				// Record the failure durably before consulting the retry policy.
				let phase = match stores.global_store.get_state(package).await {
					Ok(ResolutionState::Progressing(phase)) => phase,
					_ => Phase::Acquiring,
				};
				let failure = heart::Failure {
					attempts: job.attempts,
					phase,
					error: message.clone(),
					at: chrono::Utc::now(),
				};
				let failed = ResolutionState::Failed(failure);
				if let Err(state_error) = stores.global_store.set_state(package, &failed).await {
					tracing::error!(%package, error = %state_error, "failed to persist failure state");
				}
				match stores.queue.fail(job.id, kind, message).await {
					Ok(decision) => {
						tracing::info!(%package, ?decision, "retry policy applied");
					}
					Err(queue_error) => {
						tracing::error!(%package, error = %queue_error, "failed to settle failed job");
					}
				}
			}
		}
	}

	/// Download one package's source archive from its registry of origin,
	/// bounded by the extraction policy's total-byte ceiling.
	async fn fetch_archive(&self, coordinates: &PackageCoordinates) -> ServerResult<bytes::Bytes> {
		let url = self.archive_url(coordinates).await?;
		let response = self
			.acquisition
			.get(url)
			.timeout(JOB_DEADLINE / 2)
			.send()
			.await
			.map_err(lookup_failure)?;
		if response.status() == reqwest::StatusCode::NOT_FOUND {
			return Err(not_found(coordinates));
		}
		let response = response.error_for_status().map_err(lookup_failure)?;

		if let Some(length) = response.content_length() {
			if length > ExtractionLimits::DEFAULT.max_total_bytes {
				return Err(ServerError::BadRequest(format!(
					"archive is {length} bytes; the ceiling is {}",
					ExtractionLimits::DEFAULT.max_total_bytes
				)));
			}
		}
		let archive = response.bytes().await.map_err(lookup_failure)?;
		if archive.len() as u64 > ExtractionLimits::DEFAULT.max_total_bytes {
			return Err(ServerError::BadRequest("archive exceeded the download ceiling".into()));
		}
		Ok(archive)
	}

	/// The download URL for a package's source archive under its origin's
	/// conventions. PyPI needs one metadata round-trip to find the sdist.
	async fn archive_url(&self, coordinates: &PackageCoordinates) -> ServerResult<url::Url> {
		use heart::RegistryOrigin;

		let name = coordinates.name.canonical();
		let version = coordinates.version.canonical();
		let raw = match &coordinates.origin {
			RegistryOrigin::CratesIo => {
				format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")
			}
			RegistryOrigin::NpmPublic => {
				// Scoped names keep the scope in the path but not in the tarball leaf.
				let leaf = name.rsplit('/').next().unwrap_or(name);
				format!("https://registry.npmjs.org/{name}/-/{leaf}-{version}.tgz")
			}
			RegistryOrigin::PyPi => return self.pypi_sdist_url(name, &version).await,
			RegistryOrigin::Custom { url, .. } => {
				// Convention for self-hosted origins: a flat archives/ namespace.
				format!("{}archives/{name}/{name}-{version}.tar.gz", ensure_trailing_slash(url))
			}
		};
		url::Url::parse(&raw)
			.map_err(|error| ServerError::Internal(format!("malformed archive url {raw:?}: {error}")))
	}

	/// Resolve a PyPI release to its sdist URL via the JSON metadata API (sdist
	/// filenames are not predictable from coordinates alone).
	async fn pypi_sdist_url(&self, name: &str, version: &str) -> ServerResult<url::Url> {
		#[derive(serde::Deserialize)]
		struct Release {
			urls: Vec<ReleaseFile>,
		}
		#[derive(serde::Deserialize)]
		struct ReleaseFile {
			packagetype: String,
			url: String,
		}

		let metadata = format!("https://pypi.org/pypi/{name}/{version}/json");
		let response = self.acquisition.get(&metadata).send().await.map_err(lookup_failure)?;
		if response.status() == reqwest::StatusCode::NOT_FOUND {
			return Err(ServerError::Registry(
				ResolveError::NotFound(name.to_owned()).into(),
			));
		}
		let bytes = response
			.error_for_status()
			.map_err(lookup_failure)?
			.bytes()
			.await
			.map_err(lookup_failure)?;
		let release: Release = serde_json::from_slice(&bytes).map_err(|error| {
			ServerError::Internal(format!("malformed pypi metadata for {name} {version}: {error}"))
		})?;
		let sdist = release
			.urls
			.into_iter()
			.find(|file| file.packagetype == "sdist")
			.ok_or_else(|| {
				ServerError::Registry(
					ResolveError::NoMatch { name: name.to_owned(), request: "an sdist".into() }
						.into(),
				)
			})?;
		url::Url::parse(&sdist.url)
			.map_err(|error| ServerError::Internal(format!("malformed sdist url: {error}")))
	}
}

/// Classify a pipeline failure for the queue's retry policy: ingest failures
/// carry their own class, transport faults are transient, malformed requests
/// are terminal, everything else is an internal fault.
pub fn classify_failure(error: &ServerError) -> FailureKind {
	match error {
		ServerError::Registry(RegistryError::Ingest(ingest)) => ingest.failure_kind(),
		ServerError::Registry(RegistryError::Resolve(ResolveError::NotFound(_))) => {
			FailureKind::SourceUnavailable
		}
		ServerError::BadRequest(_) => FailureKind::Malformed,
		ServerError::Internal(message) if message.contains("deadline") => FailureKind::Timeout,
		_ if error.is_retryable() => FailureKind::Transient,
		_ => FailureKind::Internal,
	}
}

/// A fresh `Progressing` view for a phase, at zero fraction.
fn progressing(phase: Phase) -> JobProgress {
	JobProgress { state: ResolutionState::Progressing(phase), phase_fraction: zero_percent() }
}

/// Zero percent (provably valid).
fn zero_percent() -> Percent {
	Percent::try_new(Percent::ZERO).expect("zero is a valid percentage")
}

/// The serialized empty `ir::entry::Index` the compile phase emits until the
/// compiler is wired in.
fn empty_intermediate_representation() -> ServerResult<bytes::Bytes> {
	let index = ir::entry::Index {
		root_ids: Vec::new(),
		entries_by_path: Default::default(),
	};
	serde_json::to_vec(&index)
		.map(bytes::Bytes::from)
		.map_err(|error| ServerError::Internal(format!("could not serialize empty IR: {error}")))
}

/// A registry lookup/transport fault, carried with its typed source.
fn lookup_failure(error: reqwest::Error) -> ServerError {
	ServerError::Registry(ResolveError::Lookup(error).into())
}

/// The package's origin does not know it.
fn not_found(coordinates: &PackageCoordinates) -> ServerError {
	ServerError::Registry(
		ResolveError::NotFound(coordinates.name.canonical().to_owned()).into(),
	)
}

/// Render a base URL with exactly one trailing slash.
fn ensure_trailing_slash(url: &url::Url) -> String {
	let mut rendered = url.to_string();
	if !rendered.ends_with('/') {
		rendered.push('/');
	}
	rendered
}
