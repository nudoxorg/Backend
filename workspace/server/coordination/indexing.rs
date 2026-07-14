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
use crate::registry::identity::PackageCoordinates;
use crate::registry::ingest::{ArchiveFormat, EntryAllowlist, ExtractionLimits, ingest_archive};
use crate::registry::queue::LeasedJob;
use crate::registry::{RegistryError, error::ResolveError};
use registry::runtime::vector::EmbeddingModel;

use crate::compiler_client::CompilerClient;
use crate::error::{BadRequestReason, InternalError, ServerError, ServerResult};
use crate::{Server, SourceStores};

/// The floor on the lease heartbeat interval, so a pathologically small
/// configured `job_lease` cannot spin the renew loop.
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
	server: Arc<Server<M>>,

	/// The HTTP client for the compiler daemon.
	compiler: CompilerClient,

	/// The archive-acquisition client.
	acquisition: reqwest::Client,

	/// In-flight per-package phase fractions, feeding the [`Progressive`] view.
	fractions: Mutex<HashMap<PackageId, Percent>>,
}

impl<M: EmbeddingModel> Indexer<M> {
	/// Build an indexer over the assembled server and compiler client.
	pub fn new(server: Arc<Server<M>>, compiler: CompilerClient) -> Self {
		Self {
			server,
			compiler,
			acquisition: reqwest::Client::new(),
			fractions: Mutex::new(HashMap::new()),
		}
	}

	/// The server this indexer drives.
	pub fn server(&self) -> &Arc<Server<M>> { &self.server }

	/// Process a single indexing job end to end:
	/// 1. **Acquire** the source archive (via the resolved origin);
	/// 2. **Extract** it through the sanitizing extractor into a content-addressed
	///    blob (bounded memory, spawn_blocking);
	/// 3. **Compile** source → IR (HTTP call to the compiler daemon);
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
		let files: Vec<registry::protocol::FileBytes> = builder
			.source_files()
			.map(|(path, bytes)| registry::protocol::FileBytes {
				path: path.to_string(),
				bytes: bytes.to_vec(),
			})
			.collect();
		let req = registry::protocol::CompileRequest {
			coordinates: coordinates.clone(),
			toolchain: builder.toolchain().clone(),
			files,
		};
		let compile_resp = self
			.compiler
			.compile(req)
			.await
			.map_err(ServerError::Compile)?;

		let (ir_bytes, references, identifiers) = match compile_resp {
			registry::protocol::CompileResponse::Ok { surface, references, identifiers } => {
				let ir_bytes = bytes::Bytes::from(surface);
				let ref_set = wire_references_to_reference_set(references)?;
				(ir_bytes, ref_set, identifiers)
			}
			registry::protocol::CompileResponse::Err { kind, message } => {
				return Err(ServerError::Compile(
					crate::compiler_client::CompilerClientError::RemoteError { kind, message },
				));
			}
		};
		builder.set_ir(ir_bytes).map_err(crate::registry::RegistryError::from)?;
		builder
			.set_references(&references)
			.map_err(crate::registry::RegistryError::from)?;

		// ── Emit ───────────────────────────────────────────────────────────────
		self.advance(stores, package, &progressing(Phase::Emitting)).await?;
		let (manifest, sections) = builder.finalize().map_err(RegistryError::from)?;
		let snapshot = ContentHash::of_bytes(&manifest.identity_bytes());

		// Derive the search facets *before* `emit` consumes the manifest+sections:
		// the `sections` still hold every file's bytes in memory, so we read
		// `Cargo.toml` + README straight from them (no post-emit blob round-trip).
		// Non-fatal by construction: a missing/unparseable manifest yields `None`
		// and ingest proceeds — search metadata must never fail a store.
		let facets =
			extract_facets(&coordinates, &manifest, &sections, &identifiers, self.server.heuristics());

		let emitted = crate::registry::blob::emit::emit(&stores.blobs, &stores.outbox, manifest, sections)
			.await
			.map_err(RegistryError::from)?;

		// The transactional boundary: `Stored { snapshot }`, the derived facets,
		// and the per-sink fan-out intents commit together (emit's own appends
		// dedupe against it).
		stores
			.outbox
			.record_stored(&stores.global_store, package, snapshot, facets.as_ref())
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
		self.run_worker_until(&tokio_util::sync::CancellationToken::new()).await
	}

	/// The worker loop with an explicit drain signal. While `drain` is
	/// un-cancelled it dequeues and drives jobs as normal; once `drain` fires it
	/// stops pulling *new* jobs and returns cleanly, letting the caller wait for
	/// the in-flight `drive_job` futures (already awaited each tick) to settle.
	/// [`run_worker`](Self::run_worker) is this with a never-cancelled token.
	pub async fn run_worker_until(&self, drain: &tokio_util::sync::CancellationToken) -> ServerResult<()> {
		let lease = self.job_lease();
		loop {
			if drain.is_cancelled() {
				tracing::info!("queue worker draining: no longer dequeuing new jobs");
				return Ok(());
			}
			let max_inflight = self.server.config().limits.max_inflight_jobs;
			let poll_interval = self.server.config().limits.poll_interval;
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

				// Stop pulling new work the moment a drain is requested; jobs
				// already dequeued below still run to completion.
				if drain.is_cancelled() {
					return Ok(());
				}

				let jobs = stores
					.queue
					.dequeue_batch(max_inflight, lease)
					.await
					.map_err(RegistryError::from)?;
				futures::stream::iter(jobs)
					.for_each_concurrent(max_inflight, |job| {
						self.drive_job(stores, job)
					})
					.await;
			}
			tokio::time::sleep(poll_interval).await;
		}
	}

	/// The configured claim lease (shortened, heartbeat-kept — DAEMON-PLAN §2.5).
	fn job_lease(&self) -> Duration { self.server.config().limits.job_lease }

	/// The configured end-to-end job deadline.
	fn job_deadline(&self) -> Duration { self.server.config().limits.job_deadline }

	/// The lease-heartbeat interval: a third of the lease (so two heartbeats can
	/// be missed before a lapse), floored at [`MIN_HEARTBEAT`].
	fn heartbeat_interval(&self) -> Duration { (self.job_lease() / 3).max(MIN_HEARTBEAT) }

	/// Beat this job's lease every [`heartbeat_interval`](Self::heartbeat_interval)
	/// for as long as it runs. Returns only when the lease can no longer be
	/// renewed — i.e. it lapsed and was reclaimed ([`crate::registry::QueueError::LeaseLost`])
	/// — signaling the driver to abandon the orphaned run. Transient renew faults
	/// (a blip against postgres) are logged and retried on the next beat rather
	/// than abandoning a still-valid claim. Never returns while the lease holds,
	/// so [`drive_job`](Self::drive_job) can `select!` it against the job future.
	///
	/// Takes a `&LeasedJob` witness: the heartbeat can only keep alive a claim
	/// that THIS worker holds (enforced by [`crate::registry::queue::Queue::renew_lease`]).
	async fn beat_lease(&self, stores: &SourceStores<M>, leased: &LeasedJob) {
		let interval = self.heartbeat_interval();
		let lease = self.job_lease();
		let package = leased.package();
		loop {
			tokio::time::sleep(interval).await;
			match stores.queue.renew_lease(leased, lease).await {
				Ok(()) => {
					tracing::trace!(%package, "job lease renewed");
				}
				Err(crate::registry::QueueError::LeaseLost { .. }) => {
					tracing::warn!(%package, "job lease lost to reclaimer; abandoning run");
					return;
				}
				Err(error) => {
					// Transient DB fault: keep the run going and try again next beat.
					tracing::warn!(%package, error = %error, "lease heartbeat failed; will retry");
				}
			}
		}
	}

	/// One claimed job: run the pipeline under the deadline, then settle the
	/// queue row — completion on success, retry-or-dead-letter on failure. Job
	/// failures are contained here; only queue/store faults escape to the
	/// supervisor.
	///
	/// Takes a [`LeasedJob`] witness: the terminal operations (`complete` and
	/// `fail`) require and consume this witness, making "settle a job you never
	/// dequeued" unrepresentable at the call site.
	async fn drive_job(&self, stores: &SourceStores<M>, leased: LeasedJob) {
		let package = leased.package();
		let attempts = leased.attempts();

		// Race the deadline-bounded pipeline against a lease heartbeat. The
		// shortened lease (config `job_lease`, ~2 min) is kept alive by renewing
		// every `heartbeat_interval` while the job runs, so a live worker never
		// loses its claim to the reclaimer even though the lease is far shorter
		// than the job deadline. `select!` biases to the job branch; the heartbeat
		// loop never resolves on its own (it either keeps beating or the job
		// finishes first and cancels it).
		//
		// `beat_lease` borrows `&leased`; the select arms run sequentially (only
		// one wins), so after the select `leased` is no longer borrowed and we
		// can move it into the terminal operation.
		let job_fut = tokio::time::timeout(self.job_deadline(), self.run_indexing_job_on(stores, package));
		let heartbeat = self.beat_lease(stores, &leased);
		let outcome = tokio::select! {
			biased;
			result = job_fut => match result {
				Ok(outcome) => outcome,
				Err(_) => Err(ServerError::Internal(InternalError::IndexingDeadlineExceeded)),
			},
			// The heartbeat only completes if the lease was lost (reclaimed out
			// from under us): abandon the run rather than commit a result we can
			// no longer own.
			() = heartbeat => Err(ServerError::Internal(InternalError::IndexingDeadlineExceeded)),
		};

		match outcome {
			Ok(snapshot) => {
				let stored = ResolutionState::Stored { hash: snapshot };
				// Consume the witness — the job is terminal after this point.
				if let Err(error) = stores.queue.complete(leased, &stored).await {
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
					attempts,
					phase,
					message: message.clone(),
					cause: Some(heart::ErrorDetails::Message(message.clone())),
					at: chrono::Utc::now(),
				};
				let failed = ResolutionState::Failed(failure);
				if let Err(state_error) = stores.global_store.set_state(package, &failed).await {
					tracing::error!(%package, error = %state_error, "failed to persist failure state");
				}
				// Consume the witness — fail is terminal (retry or dead-letter,
				// either way this worker's claim is done).
				match stores.queue.fail(leased, kind, message).await {
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
			.timeout(self.job_deadline() / 2)
			.send()
			.await
			.map_err(lookup_failure)?;
		if response.status() == reqwest::StatusCode::NOT_FOUND {
			return Err(not_found(coordinates));
		}
		let response = response.error_for_status().map_err(lookup_failure)?;

		if let Some(length) = response.content_length()
			&& length > ExtractionLimits::DEFAULT.max_total_bytes {
				return Err(ServerError::BadRequest(BadRequestReason::ArchiveTooLarge {
					actual: length,
					limit: ExtractionLimits::DEFAULT.max_total_bytes,
				}));
			}
		let archive = response.bytes().await.map_err(lookup_failure)?;
		if archive.len() as u64 > ExtractionLimits::DEFAULT.max_total_bytes {
			return Err(ServerError::BadRequest(BadRequestReason::ArchiveExceedsLimit));
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
			RegistryOrigin::NuGet => {
				// Flat-container nupkg (a zip): ids + versions are lowercased.
				let id = name.to_ascii_lowercase();
				let ver = version.to_ascii_lowercase();
				format!("https://api.nuget.org/v3-flatcontainer/{id}/{ver}/{id}.{ver}.nupkg")
			}
			RegistryOrigin::FlakeHub => {
				// FlakeHub's tarball endpoint 307-redirects to a pinned,
				// CloudFront-signed URL; the Nix producer's `traversal` module
				// follows the chain. `name` is the `org/project` slug.
				format!("https://api.flakehub.com/f/{name}/{version}.tar.gz")
			}
			RegistryOrigin::Custom { url, .. } => {
				// Convention for self-hosted origins: a flat archives/ namespace.
				format!("{}archives/{name}/{name}-{version}.tar.gz", ensure_trailing_slash(url))
			}
		};
		url::Url::parse(&raw)
			.map_err(|_| ServerError::Internal(InternalError::MalformedArchiveUrl { raw }))
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
				ResolveError::NotFound { name: name.to_owned() }.into(),
			));
		}
		let bytes = response
			.error_for_status()
			.map_err(lookup_failure)?
			.bytes()
			.await
			.map_err(lookup_failure)?;
		let release: Release = serde_json::from_slice(&bytes).map_err(|_| {
			ServerError::Internal(InternalError::MalformedPypiMetadata {
				name: name.to_owned(),
				version: version.to_owned(),
			})
		})?;
		let sdist = release
			.urls
			.into_iter()
			.find(|file| file.packagetype == "sdist")
			.ok_or_else(|| {
				ServerError::Registry(
					ResolveError::NoMatchRange { name: name.to_owned(), spec: "an sdist".into() }
						.into(),
				)
			})?;
		url::Url::parse(&sdist.url)
			.map_err(|_| ServerError::Internal(InternalError::MalformedSdistUrl))
	}
}

/// Classify a pipeline failure for the queue's retry policy: ingest failures
/// carry their own class, transport faults are transient, malformed requests
/// are terminal, everything else is an internal fault.
pub fn classify_failure(error: &ServerError) -> FailureKind {
	match error {
		ServerError::Registry(RegistryError::Ingest(ingest)) => ingest.failure_kind(),
		ServerError::Registry(RegistryError::Resolve(ResolveError::NotFound { .. })) => {
			FailureKind::SourceUnavailable
		}
		ServerError::BadRequest(_) => FailureKind::Malformed,
		ServerError::Internal(InternalError::IndexingDeadlineExceeded) => FailureKind::Timeout,
		// Compile failures that aren't I/O-transient are source/malformed — a
		// retry will just re-fail on the same tree.
		ServerError::Compile(compile) if compile.is_retryable() => FailureKind::Transient,
		ServerError::Compile(_) => FailureKind::Malformed,
		_ if error.is_retryable() => FailureKind::Transient,
		_ => FailureKind::Internal,
	}
}

/// Convert a list of [`registry::protocol::WireFile`] references into the registry's
/// [`crate::registry::blob::ReferenceSet`].
fn wire_references_to_reference_set(
	wire_files: Vec<registry::protocol::WireFile>,
) -> ServerResult<crate::registry::blob::ReferenceSet> {
	use crate::registry::blob::{FileReferences, ReferenceSet};
	let by_file = wire_files
		.into_iter()
		.map(|wf| {
			let references = wf
				.references
				.into_iter()
				.map(|wr| {
					wr.into_reference().map_err(|_e| {
						ServerError::Internal(crate::error::InternalError::Other {
							message: "wire reference decode failed".to_owned(),
						})
					})
				})
				.collect::<Result<Vec<_>, _>>()?;
			Ok(FileReferences {
				path: smol_str::SmolStr::from(wf.path),
				references,
			})
		})
		.collect::<ServerResult<Vec<_>>>()?;
	Ok(ReferenceSet { by_file })
}

/// A fresh `Progressing` view for a phase, at zero fraction.
fn progressing(phase: Phase) -> JobProgress {
	JobProgress { state: ResolutionState::Progressing(phase), phase_fraction: zero_percent() }
}

/// Zero percent (provably valid).
fn zero_percent() -> Percent {
	Percent::try_new(Percent::ZERO).expect("zero is a valid percentage")
}

/// A registry lookup/transport fault, carried with its typed source.
fn lookup_failure(error: reqwest::Error) -> ServerError {
	ServerError::Registry(ResolveError::Lookup(error).into())
}

/// The package's origin does not know it.
fn not_found(coordinates: &PackageCoordinates) -> ServerError {
	ServerError::Registry(
		ResolveError::NotFound { name: coordinates.name.canonical().to_owned() }.into(),
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

// ─────────────────────────────────────────────────────────────────────────────
// Facet extraction — turn the emitted snapshot's manifest + README into the
// searchable [`crate::registry::metadata::SearchFacets`] that ride the stored record.
// ─────────────────────────────────────────────────────────────────────────────

use crate::registry::blob::{BlobManifest, FileEntry};
use crate::registry::blob::creation::PendingSection;
use crate::registry::metadata::SearchFacets;
use crate::registry::metadata::rich::{self, ExtractionInput};

/// The owned inputs a `Cargo.toml` yields for [`rich::extract`]. Separated from
/// the borrowing [`ExtractionInput`] so the parse is a *pure*, unit-testable
/// function over a `&str` (the `ExtractionInput` is stitched together from these
/// owned fields + the README at the call site, where the borrows are valid).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CargoManifestFacts {
	/// `[package] description`.
	pub description: Option<String>,
	/// `[package] keywords` (Cargo caps at 5).
	pub keywords: Vec<String>,
	/// `[package] categories`.
	pub categories: Vec<String>,
	/// `[package] repository` is present.
	pub has_repository: bool,
	/// `[package] documentation` is present.
	pub has_documentation: bool,
	/// `[package] license` or `license-file` is present.
	pub has_license: bool,
}

/// Parse a `Cargo.toml` string into the [`CargoManifestFacts`] rich-metadata
/// extraction cares about. Pure and total — a malformed or minimal manifest
/// simply yields empty/`false` facts (never an error), so the caller's facet
/// extraction stays non-fatal. Kept `pub` so the isolated verification harness
/// exercises exactly this parse.
pub fn parse_cargo_toml(text: &str) -> CargoManifestFacts {
	let Ok(value) = text.parse::<toml::Value>() else {
		return CargoManifestFacts::default();
	};
	let Some(package) = value.get("package").and_then(toml::Value::as_table) else {
		return CargoManifestFacts::default();
	};

	let string_field = |key: &str| {
		package.get(key).and_then(toml::Value::as_str).map(str::to_owned)
	};
	let string_array = |key: &str| {
		package
			.get(key)
			.and_then(toml::Value::as_array)
			.map(|arr| {
				arr.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect::<Vec<_>>()
			})
			.unwrap_or_default()
	};

	CargoManifestFacts {
		description: string_field("description"),
		keywords: string_array("keywords"),
		categories: string_array("categories"),
		has_repository: package.contains_key("repository"),
		has_documentation: package.contains_key("documentation"),
		has_license: package.contains_key("license") || package.contains_key("license-file"),
	}
}

/// Build the [`SearchFacets`] for a freshly-emitted snapshot from its manifest
/// files and their still-in-memory `sections` bytes.
///
/// Non-fatal by design: any missing/unparseable input degrades to a minimal
/// name-only facet set (or `None`), so ingest never fails because search
/// metadata could not be derived. Only Rust/`Cargo.toml` is parsed today; other
/// ecosystems fall back to a name-only [`SearchFacets`].
fn extract_facets(
	coordinates: &PackageCoordinates,
	manifest: &BlobManifest,
	sections: &[PendingSection],
	identifiers: &[String],
	heuristics: Option<&crate::Heuristics>,
) -> Option<SearchFacets> {
	let (synonyms, specifics) = match heuristics {
		Some(h) => (Some(h.synonyms()), Some(h.specifics())),
		None => (None, None),
	};
	// Bytes of a manifest file are fetched from `sections` by content hash — the
	// same hash the `FileEntry` records — so no post-emit blob round-trip is
	// needed.
	let file_text = |entry: &FileEntry| -> Option<String> {
		let section = sections.iter().find(|s| s.hash == entry.hash)?;
		String::from_utf8(section.bytes.to_vec()).ok()
	};

	// Also match manifests under a single tarball wrapper dir (`pkg-1.0/Cargo.toml`).
	let is_root_manifest = |path: &str| {
		path == "Cargo.toml"
			|| path
				.strip_suffix("/Cargo.toml")
				.is_some_and(|prefix| !prefix.contains('/'))
	};
	let is_root_readme = |path: &str| {
		let leaf = path.rsplit('/').next().unwrap_or(path);
		leaf.to_ascii_lowercase().starts_with("readme")
			&& path.matches('/').count() <= 1
	};

	let name = coordinates.name.canonical().to_owned();

	// Only Rust is parsed for rich Cargo.toml facets today; other ecosystems
	// still get the compiler-harvested identifiers + a name-only base.
	if coordinates.ecosystem() != heart::ecosystem::Language::Rust {
		let input = ExtractionInput {
			name: &name,
			identifiers,
			..Default::default()
		};
		let rich = rich::extract(&input, synonyms, specifics);
		let facets = SearchFacets::from_rich(&rich);
		tracing::debug!(package = %manifest.package, "non-Rust ecosystem: name + identifier facets");
		return Some(facets);
	}

	let manifest_text = manifest
		.files
		.iter()
		.find(|e| is_root_manifest(e.path.as_str()))
		.and_then(file_text);
	let readme_text = manifest
		.files
		.iter()
		.find(|e| is_root_readme(e.path.as_str()))
		.and_then(file_text);

	let facts = match &manifest_text {
		Some(text) => parse_cargo_toml(text),
		None => {
			// No manifest: fall back to a name-only facet set rather than failing.
			tracing::debug!(package = %manifest.package, "no Cargo.toml in snapshot; name-only facets");
			CargoManifestFacts::default()
		}
	};

	// `loc` ≈ total README + source-file line count would be costly to compute
	// precisely; leave 0 for now (a cheap, honest under-count).
	let loc = 0u32;
	let dependencies: Vec<String> = Vec::new();

	let input = ExtractionInput {
		name: &name,
		description: facts.description.as_deref(),
		manifest_keywords: &facts.keywords,
		manifest_categories: &facts.categories,
		readme: readme_text.as_deref(),
		identifiers,
		dependencies: &dependencies,
		has_repository: facts.has_repository,
		has_documentation: facts.has_documentation,
		has_license: facts.has_license,
		loc,
	};

	let rich = rich::extract(&input, synonyms, specifics);
	Some(SearchFacets::from_rich(&rich))
}
