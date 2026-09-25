//! The indexing pipeline: drive one dequeued job through acquire/extract/
//! compile/emit and record every transition durably.
//!
//! The flow lives on [`Indexer`] — a struct that owns a handle to the assembled
//! server — rather than as free `impl Server` methods, so the pipeline's moving
//! parts (job loop, phase transitions, freshness) sit in one place with the
//! state they operate on. The cage compile lifecycle, IR-stream decode, and
//! facet extraction live in the sibling modules ([`cage`], [`ir_stream`],
//! [`facets`]).

#[allow(unused_imports)]
use crate::server::registry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ingest::archive::{EntryAllowlist, ExtractionLimits, ingest_archive};
use crate::server::registry::blob::creation::BlobBuilder;
use crate::server::registry::identity::PackageCoordinates;
use crate::server::registry::queue::LeasedJob;
use crate::server::registry::{RegistryError, error::ResolveError};
use futures::StreamExt;
use heart::Retryable;
use heart::{ContentHash, FailureKind, JobProgress, PackageId, Percent, Phase, ResolutionState};
use registry::vector::EmbeddingModel;

use crate::server::error::{BadRequestReason, InternalError, ServerError, ServerResult};
use crate::server::{Server, SourceStores};

pub(super) mod cage;
pub(super) mod facets;
pub(super) mod ir_stream;

pub(super) use ir_stream::{
    attach_empty_ir_sections, build_reference_set_from_table, set_empty_ir_section,
};

/// The floor on the lease heartbeat interval, so a pathologically small
/// configured job_lease cannot spin the renew loop.
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
    server: Arc<Server<M>>,
    acquisition: registry::upstream::UpstreamClient,
    fractions: Mutex<HashMap<PackageId, Percent>>,
    /// OCI toolchain image metadata store. When `Some`, `execute_compile_phase`
    /// looks up the real `ImageDigest` for each language's toolchain before
    /// calling `prepare_golden`/`fork_golden` — keying the golden pool by the
    /// content-addressed image rather than a per-language placeholder.
    /// `None` (the default) keeps the deterministic placeholder so the code
    /// path is exercised without provisioned images.
    toolchain_images: Option<Arc<sandbox::ToolchainImageStore>>,
}

impl<M: EmbeddingModel> Indexer<M> {
    // NOTE(driver): the compiler daemon is gone (the cage is ephemeral,
    // SMOLVM-PLAN); the `Indexer` no longer holds a compiler client. The compile
    // phase is stubbed — see `execute_compile_phase`.
    pub fn new(server: Arc<Server<M>>) -> Self {
        Self {
            server,
            acquisition: registry::upstream::UpstreamClient::new(),
            fractions: Mutex::new(HashMap::new()),
            toolchain_images: None,
        }
    }

    /// Attach a [`sandbox::ToolchainImageStore`] so the compile phase can look
    /// up real OCI image digests instead of per-language placeholders.
    ///
    /// Call this once at server assembly time on a forge node. Non-forge
    /// (gateway-only) nodes never reach the compile phase, so the store stays
    /// `None` there — that is correct and intentional.
    pub fn with_toolchain_images(mut self, store: sandbox::ToolchainImageStore) -> Self {
        self.toolchain_images = Some(Arc::new(store));
        self
    }

    pub fn server(&self) -> &Arc<Server<M>> {
        &self.server
    }

    /// Process a single indexing job end to end through isolated phases.
    pub async fn run_indexing_job(&self, package: PackageId) -> ServerResult<ContentHash> {
        self.run_indexing_job_on(self.server.base(), package).await
    }

    /// The per-source pipeline executing distinct phases.
    async fn run_indexing_job_on(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
    ) -> ServerResult<ContentHash> {
        let coordinates = self.execute_acquire_phase(stores, package).await?;

        // P3 listing-persistence seam: the acquire path fetches archives
        // directly and never runs `registry::resolve::resolve`, so no
        // `observed_listing` snapshot exists here yet. When the resolve path is
        // threaded into this pipeline, call `stores.global_store.set_listing`
        // with the observed status (and `Outbox::emit_withdraw_intents_for_version`
        // for freshly-withdrawn versions). Deliberately NOT calling
        // `set_listing(_, None)` in the meantime — that would clear listing
        // state the catalog-follower path may have persisted.

        let mut builder = self
            .execute_extract_phase(stores, package, &coordinates)
            .await?;
        let identifiers = self
            .execute_compile_phase(stores, package, &coordinates, &mut builder)
            .await?;
        self.execute_emit_phase(stores, package, &coordinates, builder, identifiers)
            .await
    }

    // ── Pipeline Phases ──────────────────────────────────────────────────────

    async fn execute_acquire_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
    ) -> ServerResult<PackageCoordinates> {
        self.advance(stores, package, &progressing(Phase::Acquiring))
            .await?;
        let record = stores
            .global_store
            .get(package)
            .await
            .map_err(RegistryError::from)?;
        Ok(record.package.coordinates.clone())
    }

    async fn execute_extract_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        coordinates: &PackageCoordinates,
    ) -> ServerResult<BlobBuilder> {
        let archive_bytes = self.fetch_archive(coordinates).await?;
        tracing::info!(%package, bytes = archive_bytes.len(), "source archive acquired");

        self.advance(stores, package, &progressing(Phase::Extracting))
            .await?;
        let record = stores
            .global_store
            .get(package)
            .await
            .map_err(RegistryError::from)?;

        let archive_format =
            crate::ecosystem::spec(record.package.coordinates.ecosystem()).archive();
        let toolchain = identity_toolchain(record.package.coordinates.ecosystem());
        let builder = ingest_archive(
            package,
            toolchain.clone(),
            std::io::Cursor::new(archive_bytes),
            archive_format,
            ExtractionLimits::DEFAULT,
            EntryAllowlist::SAFE,
        )
        .await
        .map_err(RegistryError::from)?;
        strip_archive_root(package, toolchain, builder)
            .map_err(|error| RegistryError::from(error).into())
    }

    /// Produce IR for the package inside an **ephemeral** SmolvmCage
    /// (SMOLVM-PLAN §3). The long-lived compiler daemon this used to POST to is
    /// gone (there is no `CompileRequest`/`CompileResponse` wire protocol any
    /// more); IR is produced by one microVM per job, forked-from-golden for a
    /// warm ~250 ms start and torn down after (one-VM-per-job ephemerality).
    ///
    /// Lifecycle: materialize the staged sources onto a scratch tree → resolve
    /// the toolchain-image golden for the package's language and
    /// `prepare_golden` (idempotent) → `fork_golden` a warm clone (falling back
    /// to a fresh cage when live fork is unavailable on this host — the cage
    /// handles that) → build the producer invocation as a [`sandbox::SealedCommand`]
    /// under a [`sandbox::CapabilityBudget`] (RO toolchain roots + RO source,
    /// scratch overlay, network OFF) → run it inside the clone → collect the
    /// produced IR bytes → the clone is killed (the ephemeral overlay dies with
    /// it). A forge node that cannot run the cage fails the job loudly
    /// (idempotent retry) rather than silently emitting an IR-less blob.
    async fn execute_compile_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        coordinates: &PackageCoordinates,
        builder: &mut BlobBuilder,
    ) -> ServerResult<Vec<String>> {
        self.advance(stores, package, &progressing(Phase::Compiling))
            .await?;

        let language = coordinates.ecosystem();
        let name = coordinates.name.canonical().to_owned();

        // ── 1. Materialize the staged sources onto a scratch tree ─────────────
        // `execute_extract_phase` already staged the sanitized source files in
        // the builder; write them out once so the cage can mount the tree RO (no
        // second archive pass). Both dirs live under one TempDir that is dropped
        // (deleted) when this scope ends — nothing survives the job on the host.
        let workspace = tempfile::Builder::new()
            .prefix("nudox-compile-")
            .tempdir()
            .map_err(|source| {
                ServerError::Internal(InternalError::MaterializeForCompile { source })
            })?;
        let source_root = workspace.path().join("src");
        let scratch_root = workspace.path().join("scratch");
        cage::materialize_sources(builder, &source_root, &scratch_root).map_err(|source| {
            ServerError::Internal(InternalError::MaterializeForCompile { source })
        })?;

        // ── 2/3. Produce IR for this package and stage it ─────────────────────
        // reconcile: in-process (macOS) vs cage (linux)
        //
        // Only a Linux forge node can actually prepare/fork the ephemeral
        // SmolvmCage golden (libkrun); every other host has no golden rootfs to
        // fork and no toolchain image reachable from here, so
        // `run_producer_in_cage` would only ever hit its cold-boot fallback and
        // then fail outright. On such hosts `compile_inprocess` runs the
        // matching `nudox-languages` producer directly against the materialized
        // source tree instead — see that module for what it does and does not
        // reconstruct.
        #[cfg(target_os = "linux")]
        let identifiers = {
            // Resolve toolchain golden + drive the cage (blocking). The cage is
            // a synchronous, CPU/VM-bound boundary; run it off the async
            // reactor via `spawn_blocking` so heartbeats/other jobs keep flowing.
            let profile = cage::producer_profile(language);
            let image = self
                .toolchain_images
                .as_deref()
                .and_then(|store| store.lookup(profile))
                .map(|img| img.config_digest)
                .unwrap_or_else(|| cage::toolchain_image_digest_placeholder(language));
            let cage_name = name.clone();
            let ir_bytes = tokio::task::spawn_blocking(move || {
                cage::run_producer_in_cage(
                    &cage_name,
                    language,
                    profile,
                    image,
                    &source_root,
                    &scratch_root,
                )
            })
            .await
            .map_err(|join| {
                ServerError::Internal(InternalError::CageCompile {
                    package: name.clone(),
                    reason: format!("cage task panicked or was cancelled: {join}"),
                })
            })??;

            // `ingest_ir_bytes` decodes the producer's NdIrF1 stream via
            // `ir_vcs::protocol::StreamReceiver`: Symbols frames become the IR
            // blob section + the returned symbol identifier list, and Bodies
            // frames are lowered into the blob `ReferenceSet` (oracle calls /
            // type mentions). The producer binary is provisioned in the golden
            // toolchain image; the on-wire framing contract is honored here.
            //
            // W1: a broken producer stream (missing Hello, an `Abort` frame,
            // a truncated stream that never reaches `Finish`, or a host-side
            // (de)serialization failure while staging what was recovered)
            // must not complete as an empty-but-"Stored" snapshot — that is
            // indistinguishable from a genuinely empty package and silently
            // corrupts the catalog. `degraded_reason` carries that signal;
            // when set, fail the job loudly (idempotent retry) instead.
            let outcome = ir_stream::ingest_ir_bytes(builder, &ir_bytes, &[]);
            if let Some(reason) = outcome.degraded_reason {
                return Err(ServerError::Internal(InternalError::IrStreamDegraded {
                    package: name.clone(),
                    reason,
                }));
            }
            let identifiers = outcome.identifiers;

            tracing::info!(
                %package,
                language = language.as_token(),
                ir_bytes = ir_bytes.len(),
                identifiers = identifiers.len(),
                "cage compile produced IR"
            );

            // Usage-query scope: load the reverse-position index (see
            // `load_usage_scope`'s doc comment — this call site is
            // `#[cfg(target_os = "linux")]`-only, so it is only exercised by a
            // Linux `cargo test -p index --features server` run; the helper
            // itself is not cfg-gated, so its body is still type-checked on
            // every host).
            load_usage_scope(
                stores,
                outcome.owning_package,
                outcome.occurrences,
                *builder.provisional_generation().as_bytes(),
            )
            .await;

            identifiers
        };

        #[cfg(not(target_os = "linux"))]
        let identifiers = super::compile_inprocess::compile_in_process(
            stores,
            package,
            coordinates,
            builder,
            &source_root,
        )
        .await?;

        Ok(identifiers)
    }

    async fn execute_emit_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        coordinates: &PackageCoordinates,
        builder: BlobBuilder,
        identifiers: Vec<String>,
    ) -> ServerResult<ContentHash> {
        self.advance(stores, package, &progressing(Phase::Emitting))
            .await?;

        let (manifest, sections) = builder.finalize().map_err(RegistryError::from)?;
        let snapshot = ContentHash::of_bytes(&manifest.identity_bytes());

        // Listing signals (release counts + freshness) from the registry listing
        // body — same non-fatal pattern as S4 downloads. Enables temporal quality.
        let listing = facets::fetch_listing_signals(coordinates, &self.acquisition).await;

        let mut facets = facets::extract_facets(
            coordinates,
            &manifest,
            &sections,
            &identifiers,
            self.server.heuristics(),
            listing,
        );

        // S4: fetch download counts from the ecosystem's DownloadEndpoint (if any).
        // Failure is non-fatal: log debug and continue. The `downloads` field stays
        // `None` for ecosystems with no endpoint; the fairness floor handles them.
        // Re-run squat after downloads so dead-stub detection sees download volume.
        if let Some(ref mut f) = facets {
            facets::fetch_and_set_downloads(coordinates, f, &self.acquisition).await;
            f.squat_suspect = crate::server::registry::search::squat::is_squat_suspect(
                crate::server::registry::search::squat::SquatInput {
                    name: &coordinates.name.canonical(),
                    quality: f.quality(),
                    downloads: f.downloads,
                    release_count: f.release_count,
                    description: f.description.as_deref(),
                    has_repository: f.repo_slug.is_some(),
                },
            );
        }

        let emitted = crate::server::registry::blob::emit::emit(&stores.blobs, manifest, sections)
            .await
            .map_err(RegistryError::from)?;

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

    /// Recompute the content hash of a stored package from its persisted
    /// manifest — the dual of the snapshot computed in [`execute_emit_phase`].
    /// Used by the freshness-check path in the sync handler.
    pub async fn content_hash(&self, package: PackageId) -> ServerResult<ContentHash> {
        let stores = self.server.base();
        let record = stores
            .global_store
            .get(package)
            .await
            .map_err(RegistryError::from)?;
        let manifest = stores
            .blobs
            .get_manifest(&record.package.coordinates)
            .await
            .map_err(RegistryError::from)?;
        Ok(ContentHash::of_bytes(&manifest.identity_bytes()))
    }

    // ── Networking & Archive Acquisition ──────────────────────────────────

    async fn fetch_archive(&self, coordinates: &PackageCoordinates) -> ServerResult<bytes::Bytes> {
        use registry::upstream::UpstreamError;
        let url = self.archive_url(coordinates).await?;
        let lang = coordinates.ecosystem();

        // W4: `UpstreamClient::get` already retries a handful of times
        // internally with its own short backoff+jitter (see
        // `workspace/index/upstream/mod.rs`, out of this pass's edit
        // boundary), but it only honors `Retry-After` on 429 and only the
        // delta-seconds form — and a caller that exhausts *that* budget gets
        // a single terminal `UpstreamError`. `retry_upstream_get` adds an
        // outer, longer-horizon layer of politeness around the whole call
        // (bounded attempts, bounded total wait, exponential backoff +
        // jitter via the same `embedrs::BackoffConfig` the embedder already
        // uses) — genuine extra patience for a source that is transiently
        // rate-limiting or flaking, without retrying anything non-idempotent
        // (this wraps only the archive GET).
        let archive = retry_upstream_get(|| self.acquisition.get(lang, url.as_str()))
            .await
            .map_err(|e| match e {
                UpstreamError::NotFound => not_found(coordinates),
                // A response body that failed to parse as expected will not
                // start parsing on a retry — permanent, not transient.
                UpstreamError::Parse(reason) => {
                    ServerError::Internal(InternalError::UpstreamFetch { reason })
                }
                // Transport/ServerError/RateLimited/RetriesExhausted: the
                // outer retry loop already gave this every reasonable
                // chance. Classify as Transient (not a bare UpstreamFetch)
                // so `classify_failure` routes the *job* back onto the
                // queue instead of dead-lettering a package purely because
                // its registry was briefly unavailable.
                other => ServerError::Internal(InternalError::UpstreamTransient {
                    reason: other.to_string(),
                }),
            })?;

        if archive.len() as u64 > ExtractionLimits::DEFAULT.max_total_bytes {
            return Err(ServerError::BadRequest(
                BadRequestReason::ArchiveExceedsLimit,
            ));
        }
        Ok(archive)
    }

    async fn archive_url(&self, coordinates: &PackageCoordinates) -> ServerResult<url::Url> {
        use heart::RegistryOrigin;

        // PyPI needs a live metadata fetch (sdist filenames are not a
        // mechanical function of the project name — see `pypi_sdist_url`);
        // the git-native `cpp` plane has no archive URL at all. Both are
        // handled here, outside the pure builder below, precisely so that
        // builder can stay free of `&self`/IO and be unit-tested directly.
        if matches!(coordinates.origin, RegistryOrigin::PyPi) {
            let name = coordinates.name.canonical();
            let version = coordinates.version.canonical();
            return self.pypi_sdist_url(name, &version).await;
        }
        if matches!(coordinates.origin, RegistryOrigin::Git) {
            return Err(ServerError::Internal(
                InternalError::GitOriginHasNoArchiveUrl,
            ));
        }

        let raw_url_string = static_archive_url_string(coordinates).unwrap_or_else(|| {
            unreachable!("PyPi and Git are handled above; every other origin builds a static url")
        });

        url::Url::parse(&raw_url_string).map_err(|_| {
            ServerError::Internal(InternalError::MalformedArchiveUrl {
                raw: raw_url_string,
            })
        })
    }

    async fn pypi_sdist_url(&self, name: &str, version: &str) -> ServerResult<url::Url> {
        #[derive(serde::Deserialize)]
        struct PyPiRelease {
            urls: Vec<PyPiReleaseFile>,
        }
        #[derive(serde::Deserialize)]
        struct PyPiReleaseFile {
            packagetype: String,
            url: String,
        }

        use registry::upstream::UpstreamError;
        let metadata_url = format!("https://pypi.org/pypi/{name}/{version}/json");
        let bytes = self
            .acquisition
            .get(crate::ecosystem::Language::Python, &metadata_url)
            .await
            .map_err(|e| match e {
                UpstreamError::NotFound => ServerError::Registry(
                    ResolveError::NotFound {
                        name: name.to_owned(),
                    }
                    .into(),
                ),
                other => ServerError::Internal(InternalError::UpstreamFetch {
                    reason: other.to_string(),
                }),
            })?;

        let release: PyPiRelease = serde_json::from_slice(&bytes).map_err(|_| {
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
                    ResolveError::NoMatchRange {
                        name: name.to_owned(),
                        spec: "an sdist".into(),
                    }
                    .into(),
                )
            })?;

        url::Url::parse(&sdist.url)
            .map_err(|_| ServerError::Internal(InternalError::MalformedSdistUrl))
    }

    // ── Worker Loop & Queue Management ──────────────────────────────────

    pub async fn run_worker_until(
        &self,
        drain: &tokio_util::sync::CancellationToken,
    ) -> ServerResult<()> {
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
                let reclaimed = stores
                    .queue
                    .reclaim_expired_leases()
                    .await
                    .map_err(RegistryError::from)?;

                if reclaimed > 0 {
                    tracing::warn!(source = %sourced.source, reclaimed, "reclaimed expired job leases");
                }

                if drain.is_cancelled() {
                    return Ok(());
                }

                let jobs = stores
                    .queue
                    .dequeue_batch(max_inflight, lease)
                    .await
                    .map_err(RegistryError::from)?;
                futures::stream::iter(jobs)
                    .for_each_concurrent(max_inflight, |job| self.drive_job(stores, job))
                    .await;
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn drive_job(&self, stores: &SourceStores<M>, leased: LeasedJob) {
        let package = leased.package();
        let attempts = leased.attempts();

        let job_future = tokio::time::timeout(
            self.job_deadline(),
            self.run_indexing_job_on(stores, package),
        );
        let heartbeat_future = self.beat_lease(stores, &leased);

        let outcome = tokio::select! {
            biased;
            result = job_future => result.unwrap_or(Err(ServerError::Internal(InternalError::IndexingDeadlineExceeded))),
            () = heartbeat_future => Err(ServerError::Internal(InternalError::IndexingDeadlineExceeded)),
        };

        match outcome {
            Ok(snapshot) => {
                let stored_state = ResolutionState::Stored { hash: snapshot };
                if let Err(error) = stores.queue.complete(leased, &stored_state).await {
                    tracing::warn!(%package, error = %error, "job completed but settling raced");
                }
            }
            Err(error) => {
                self.handle_job_failure(stores, leased, package, attempts, error)
                    .await
            }
        }
    }

    async fn handle_job_failure(
        &self,
        stores: &SourceStores<M>,
        leased: LeasedJob,
        package: PackageId,
        attempts: u32,
        error: ServerError,
    ) {
        let kind = classify_failure(&error);
        let message = error.to_string();
        tracing::warn!(%package, %kind, error = %message, "indexing job failed");

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

        if let Err(state_error) = stores
            .global_store
            .set_state(package, &ResolutionState::Failed(failure))
            .await
        {
            tracing::error!(%package, error = %state_error, "failed to persist failure state");
        }

        if let Err(queue_error) = stores.queue.fail(leased, kind, message).await {
            tracing::error!(%package, error = %queue_error, "failed to settle failed job");
        }
    }

    async fn beat_lease(&self, stores: &SourceStores<M>, leased: &LeasedJob) {
        let interval = self.heartbeat_interval();
        let lease_duration = self.job_lease();
        let package = leased.package();

        loop {
            tokio::time::sleep(interval).await;
            match stores.queue.renew_lease(leased, lease_duration).await {
                Ok(()) => tracing::trace!(%package, "job lease renewed"),
                Err(crate::server::registry::QueueError::LeaseLost { .. }) => return,
                Err(error) => tracing::warn!(%package, error = %error, "lease heartbeat failed"),
            }
        }
    }

    fn job_lease(&self) -> Duration {
        self.server.config().limits.job_lease
    }
    fn job_deadline(&self) -> Duration {
        self.server.config().limits.job_deadline
    }
    fn heartbeat_interval(&self) -> Duration {
        (self.job_lease() / 3).max(MIN_HEARTBEAT)
    }

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
        Ok(())
    }
}

// ── Helpers & Utilities ──────────────────────────────────────────────────

pub fn classify_failure(error: &ServerError) -> FailureKind {
    match error {
        ServerError::Registry(RegistryError::Ingest(ingest)) => ingest.failure_kind(),
        ServerError::Registry(RegistryError::Resolve(ResolveError::NotFound { .. })) => {
            FailureKind::SourceUnavailable
        }
        ServerError::BadRequest(_) => FailureKind::Malformed,
        ServerError::Internal(InternalError::IndexingDeadlineExceeded) => FailureKind::Timeout,
        // W4: an upstream archive fetch that exhausted its polite retry
        // budget against a rate-limit/5xx/transport condition should come
        // back around on the queue, not dead-letter — `InternalError` is not
        // in `ServerError::is_retryable()`'s set, so this needs its own arm
        // rather than falling through to the generic fallback below.
        ServerError::Internal(InternalError::UpstreamTransient { .. }) => FailureKind::Transient,
        // (compile-daemon failure kinds removed with the daemon; the stubbed
        // compile phase surfaces as `Internal`, handled by the fallback below)
        _ if error.is_retryable() => FailureKind::Transient,
        _ => FailureKind::Internal,
    }
}

/// Bounded, polite outer retry around one upstream GET (W4).
///
/// `attempt_get` is retried with exponential backoff + jitter
/// ([`embedrs::BackoffConfig`] — the same crate/pattern
/// `search::semantic::embedder::HttpEmbedder` already uses for its own
/// retryable HTTP calls) for any [`registry::upstream::UpstreamError`] except
/// [`registry::upstream::UpstreamError::NotFound`] and
/// [`registry::upstream::UpstreamError::Parse`], both of which are permanent
/// (retrying the same URL will not make a 404 appear or a malformed body
/// parse) and are returned immediately without consuming a retry.
///
/// Takes a closure (rather than depending on a trait `UpstreamClient` would
/// need to grow) so it is unit-testable without any real network I/O: a test
/// can hand it a closure returning a scripted sequence of `Err`s then `Ok`.
async fn retry_upstream_get<F, Fut>(
    mut attempt_get: F,
) -> Result<bytes::Bytes, registry::upstream::UpstreamError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bytes::Bytes, registry::upstream::UpstreamError>>,
{
    use registry::upstream::UpstreamError;

    // Modest outer bound: `UpstreamClient::get` already burns up to 4
    // attempts internally per call, so this multiplies worst-case attempts
    // rather than adding an unbounded tail. `max_delay` keeps any single
    // outer wait well under a typical job deadline.
    const OUTER_ATTEMPTS: u32 = 3; // 1 initial + 2 outer retries
    let backoff = embedrs::BackoffConfig {
        base_delay: std::time::Duration::from_secs(1),
        max_delay: std::time::Duration::from_secs(20),
        jitter: true,
        max_http_retries: OUTER_ATTEMPTS,
    };

    let mut last_err = None;
    for attempt in 0..OUTER_ATTEMPTS {
        match attempt_get().await {
            Ok(bytes) => return Ok(bytes),
            Err(UpstreamError::NotFound) => return Err(UpstreamError::NotFound),
            Err(UpstreamError::Parse(msg)) => return Err(UpstreamError::Parse(msg)),
            Err(retryable) => {
                if attempt + 1 < OUTER_ATTEMPTS {
                    let wait = backoff.delay_for(attempt);
                    tracing::warn!(
                        attempt,
                        error = %retryable,
                        wait_ms = wait.as_millis() as u64,
                        "upstream archive fetch attempt failed; retrying after backoff"
                    );
                    tokio::time::sleep(wait).await;
                }
                last_err = Some(retryable);
            }
        }
    }
    Err(last_err.unwrap_or(UpstreamError::RetriesExhausted))
}

/// Load a compile step's occurrence facts into `stores`'s usage-query backend
/// (`SourceStores::usage_backend`), if the step captured an owning package.
///
/// Shared by the cage path (this module's `#[cfg(target_os = "linux")]`
/// compile branch, which decodes `owning_package`/`occurrences` from
/// [`ir_stream::IrIngestOutcome`]) — factored out to a free function, rather
/// than inlined at that call site, specifically so its body is type-checked
/// on every host: the call site itself only compiles on Linux, but this
/// function does not carry that `cfg`, so `cargo check`/`cargo test -p index`
/// on any host still exercises it.
///
/// `owning_package` is `None` only when the stream broke before any `Symbols`
/// batch arrived, or the producer genuinely emitted zero symbols — in either
/// case there is no package identity to bind a scope to, so whatever scope
/// this source had loaded before (if any) is left untouched rather than
/// cleared.
async fn load_usage_scope<M: EmbeddingModel>(
    stores: &SourceStores<M>,
    owning_package: Option<ir::change::PackageLineageId>,
    occurrences: Vec<(ir::change::IntroId, ir::vocab::Occurrence)>,
    channel_tip: [u8; 32],
) {
    let Some(owning_package) = owning_package else {
        return;
    };
    let (view, reverse) = crate::search::usages::build_usage_scope(
        owning_package,
        ir::apply::PristineIntroTable::new(),
        occurrences,
        channel_tip,
    );
    stores.usage_backend.store(view, reverse).await;
}

fn progressing(phase: Phase) -> JobProgress {
    JobProgress {
        state: ResolutionState::Progressing(phase),
        phase_fraction: zero_percent(),
    }
}

fn zero_percent() -> Percent {
    Percent::try_new(Percent::ZERO).expect("zero is a valid percentage")
}

fn not_found(coordinates: &PackageCoordinates) -> ServerError {
    ServerError::Registry(
        ResolveError::NotFound {
            name: coordinates.name.canonical().to_owned(),
        }
        .into(),
    )
}

/// Strip a shared archive-wrapper directory from every staged file's path,
/// if one is present.
///
/// Registry archives conventionally nest every entry under one top-level
/// directory that is a property of the *archive format*, not the package's
/// own source layout: crates.io and PyPI sdists wrap in `{name}-{version}/`,
/// npm tarballs wrap in `package/`. `ingest_archive` (the untrusted-input
/// sanitizer) intentionally does not interpret archive semantics — it only
/// enforces path-jail safety — so that wrapper directory survives into the
/// staged [`BlobBuilder`] verbatim.
///
/// `GET /packages/:id/files/*path` (`Server::source_file`) is answered by an
/// exact match against the manifest's stored [`FileEntry`] paths using the
/// caller-supplied, package-relative path (e.g. `src/lib.rs`) — a caller has
/// no way to know an internal archive-format convention, so a manifest that
/// still carries the wrapper directory can never be found by path. This
/// normalizes it away right after extraction, once, so every downstream
/// consumer (the compile phase's materialized source tree, the emitted
/// manifest, the download surface) sees package-relative paths.
///
/// A builder with no single shared leading directory (i.e. files are already
/// package-relative, or genuinely have no common wrapper) is returned
/// unchanged.
fn strip_archive_root(
    package: PackageId,
    toolchain: heart::Toolchain,
    builder: BlobBuilder,
) -> Result<BlobBuilder, crate::server::registry::BlobError> {
    let entries: Vec<(smol_str::SmolStr, bytes::Bytes)> = builder
        .source_files()
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect();

    let Some(prefix) = shared_leading_segment(&entries) else {
        return Ok(builder);
    };

    tracing::debug!(
        %package,
        %prefix,
        files = entries.len(),
        "stripping shared archive-wrapper directory from staged file paths"
    );
    let mut stripped = BlobBuilder::new(package, toolchain);
    for (path, bytes) in entries {
        let rest = path.strip_prefix(prefix.as_str()).unwrap_or(path.as_str());
        stripped.push_file(smol_str::SmolStr::new(rest), bytes)?;
    }
    Ok(stripped)
}

/// The archive-wrapper directory every staged file shares (`"{dir}/"`), or
/// `None` when there is no such single shared leading segment — either the
/// files are already package-relative, or they genuinely span more than one
/// top-level directory (no wrapper to strip).
fn shared_leading_segment(entries: &[(smol_str::SmolStr, bytes::Bytes)]) -> Option<String> {
    let (first, _) = entries.first()?;
    let head = first.split('/').next()?;
    if head.len() == first.len() {
        // The candidate "prefix" is the whole path: a bare top-level file,
        // not something nested under a wrapper directory.
        return None;
    }
    let prefix = format!("{head}/");
    entries
        .iter()
        .all(|(path, _)| path.as_str().starts_with(prefix.as_str()))
        .then_some(prefix)
}

/// The [`heart::Toolchain`] value fed into [`BlobBuilder::new`] (and, via the
/// manifest it produces, into [`crate::blob::BlobManifest::identity_bytes`])
/// for a package's snapshot identity (W9).
///
/// # Why not `record.package.toolchain`
///
/// `GlobalPackage::toolchain` (what `execute_acquire_phase` reads back off
/// the store) is *provenance*, not identity: today it is always the
/// deterministic per-ecosystem placeholder `initialization::provisional_toolchain`
/// writes at `ensure_initialized` time (see that doc comment), but nothing
/// prevents a future per-node toolchain-detection feature (e.g. reading the
/// real compiler version out of the golden OCI image on a Linux forge node,
/// `toolchain_images` at ~line 222) from starting to overwrite it with a real
/// value — and only on nodes that *can* detect one. A macOS node running
/// `compile_inprocess` never could. If the snapshot hash folded in whatever
/// happened to be on the package record at extract time, two nodes compiling
/// byte-identical source would produce two different snapshot hashes the
/// moment that landed, silently defeating cross-node dedupe/freshness.
///
/// This function always returns the pure, ecosystem-only placeholder instead,
/// so identity is provably invariant to *which node* (or which point in that
/// future feature's rollout) produced the snapshot. The real/observed
/// toolchain remains available as metadata via `GlobalPackage::toolchain` for
/// display/provenance — it is simply never an input to content identity.
///
/// This does give up "re-index automatically when the real toolchain
/// changes" for Linux, but that semantic does not exist in the codebase
/// today either (no freshness check compares toolchains) — so nothing
/// observable regresses, and the door stays open to reintroduce it later as
/// an explicit freshness signal (comparing `GlobalPackage::toolchain` against
/// a freshly-detected value) rather than as a silent identity perturbation.
pub(super) fn identity_toolchain(ecosystem: crate::ecosystem::Language) -> heart::Toolchain {
    super::initialization::provisional_toolchain(ecosystem)
}

fn ensure_trailing_slash(url: &url::Url) -> String {
    let mut rendered = url.to_string();
    if !rendered.ends_with('/') {
        rendered.push('/');
    }
    rendered
}

/// Build the raw archive-download URL string for every [`heart::RegistryOrigin`]
/// whose fetch path is a pure function of `coordinates` — every origin except
/// PyPI (needs a live metadata fetch; sdist filenames are not a mechanical
/// function of the project name, see [`Indexer::pypi_sdist_url`]) and the
/// git-native `cpp` plane (no archive URL exists; see `RegistryOrigin::Git`),
/// both handled by [`Indexer::archive_url`] before falling here.
///
/// `PackageName::canonical()` is the identity/dedup form — lowercased, and for
/// Rust `_`→`-` collapsed, for Maven both group and artifact lowercased.
/// That's correct for `PackageId` hashing (crates.io/Maven both treat those
/// variants as the same package for uniqueness) but **wrong** for a fetch URL
/// against a registry that serves the publisher's exact spelling.
/// `PackageName::original()` is the literal string the package was registered
/// under, which is what those origins' real archive paths are keyed on. Each
/// arm below picks whichever field actually matches what's on the wire; see
/// the per-arm comments for why (crates.io/Maven need `original`; npm/Go/
/// NuGet either preserve case in canonical already or the registry is itself
/// case-insensitive, so `canonical` — or an explicit lowercase, for NuGet —
/// is correct there). Unit-tested directly in `tests` below, real names only
/// (`once_cell`, `parking_lot`, `org.antlr:ST4`).
fn static_archive_url_string(coordinates: &PackageCoordinates) -> Option<String> {
    use heart::RegistryOrigin;

    let name = coordinates.name.canonical();
    let original_name = coordinates.name.original();
    let version = coordinates.version.canonical();

    Some(match &coordinates.origin {
        RegistryOrigin::CratesIo => {
            // crates.io's static CDN keys the path on the exact registered
            // crate name, `_`/`-` included (`once_cell` is served at
            // `.../once_cell/once_cell-1.19.0.crate`, NOT `.../once-cell/...`
            // — that 403s). `canonical()` collapses `_`→`-` for identity/
            // dedup (crates.io treats them as the same crate), which is
            // right for `PackageId` hashing but wrong here.
            format!(
                "https://static.crates.io/crates/{original_name}/{original_name}-{version}.crate"
            )
        }
        RegistryOrigin::NpmPublic => {
            // npm requires lowercase names at publish time (parse_name
            // already lowercases both scope and leaf), so canonical ==
            // what's on the wire; only the scope needs dropping for the
            // tarball basename.
            let leaf = name.rsplit('/').next().unwrap_or(name);
            format!("https://registry.npmjs.org/{name}/-/{leaf}-{version}.tgz")
        }
        RegistryOrigin::NuGet => {
            // NuGet's flat-container feed *requires* lowercase in both the
            // path and filename regardless of the registered display casing,
            // so lowercasing here is correct independent of canonical vs.
            // original.
            let id = name.to_ascii_lowercase();
            let ver = version.to_ascii_lowercase();
            format!("https://api.nuget.org/v3-flatcontainer/{id}/{ver}/{id}.{ver}.nupkg")
        }
        RegistryOrigin::GoProxy => {
            // Go canonical IS the original (module paths are
            // identity-preserving — see `ecosystem::go::Go::render_canonical`),
            // so no case is lost; goproxy needs its own `!`-escaping on top,
            // applied below.
            let escaped = crate::ecosystem::escape_module_path(name);
            format!("https://proxy.golang.org/{escaped}/@v/{version}.zip")
        }
        RegistryOrigin::MavenCentral => {
            // Maven Central's repository path is the literal groupId/
            // artifactId with dots-as-slashes, case-sensitive (e.g.
            // `org.antlr:ST4` lives at `.../org/antlr/ST4/...`, not
            // `.../org/antlr/st4/...`). `canonical()` lowercases both
            // (`render_canonical` in `ecosystem::java`) for identity, same
            // class of bug as crates.io above — use `original()`'s
            // `group:artifact` spelling for the fetch path instead.
            if let Some((group, artifact)) = original_name.split_once(':') {
                let group_path = group.replace('.', "/");
                format!(
                    "https://repo1.maven.org/maven2/{group_path}/{artifact}/{version}/{artifact}-{version}-sources.jar"
                )
            } else {
                format!(
                    "https://repo1.maven.org/maven2/{original_name}/{version}/{original_name}-{version}-sources.jar"
                )
            }
        }
        RegistryOrigin::Custom { url, .. } => {
            format!(
                "{}archives/{name}/{name}-{version}.tar.gz",
                ensure_trailing_slash(url)
            )
        }
        RegistryOrigin::PyPi | RegistryOrigin::Git => return None,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::registry::blob::BlobManifest;
    use crate::server::registry::upstream::UpstreamError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── W4: bounded, polite outer retry around the archive GET ────────────

    #[tokio::test]
    async fn w4_retry_succeeds_after_transient_failures_without_over_calling() {
        let calls = AtomicUsize::new(0);
        let result = retry_upstream_get(|| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(UpstreamError::ServerError(503))
                } else {
                    Ok(bytes::Bytes::from_static(b"archive-bytes"))
                }
            }
        })
        .await;
        assert_eq!(result.expect("eventual success").as_ref(), b"archive-bytes");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "must stop calling as soon as it succeeds"
        );
    }

    #[tokio::test]
    async fn w4_retry_does_not_retry_a_permanent_not_found() {
        let calls = AtomicUsize::new(0);
        let result = retry_upstream_get(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(UpstreamError::NotFound) }
        })
        .await;
        assert!(matches!(result, Err(UpstreamError::NotFound)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "NotFound must not consume a retry"
        );
    }

    #[tokio::test]
    async fn w4_retry_does_not_retry_a_permanent_parse_error() {
        let calls = AtomicUsize::new(0);
        let result = retry_upstream_get(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(UpstreamError::Parse("bad body".to_owned())) }
        })
        .await;
        assert!(matches!(result, Err(UpstreamError::Parse(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn w4_retry_is_bounded_and_surfaces_the_last_error() {
        let calls = AtomicUsize::new(0);
        let result = retry_upstream_get(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(UpstreamError::RateLimited) }
        })
        .await;
        assert!(matches!(result, Err(UpstreamError::RateLimited)));
        let made = calls.load(Ordering::SeqCst);
        assert!(made <= 3, "must be bounded: made {made} calls");
        assert!(made >= 1);
    }

    #[test]
    fn w4_classify_failure_routes_exhausted_upstream_retries_to_transient() {
        let error = ServerError::Internal(InternalError::UpstreamTransient {
            reason: "503 x3".to_owned(),
        });
        assert_eq!(classify_failure(&error), FailureKind::Transient);
    }

    // ── W9: snapshot identity must not depend on which node compiled it ───

    fn manifest_with_toolchain(toolchain: heart::Toolchain) -> BlobManifest {
        let mut builder = BlobBuilder::new(test_package(), toolchain);
        builder
            .push_file(
                smol_str::SmolStr::new("src/lib.rs"),
                bytes::Bytes::from_static(b"fn main() {}"),
            )
            .expect("push file");
        attach_empty_ir_sections(&mut builder);
        builder.finalize().expect("finalize").0
    }

    fn test_package() -> heart::PackageId {
        heart::PackageId::from_uuid(uuid::Uuid::from_bytes([7u8; 16]))
    }

    #[test]
    fn w9_identity_toolchain_is_deterministic_across_simulated_nodes() {
        // Two independent calls (standing in for two different nodes — one
        // "macOS", one "Linux" — both hitting `execute_extract_phase`) must
        // agree, since the function takes no host/environment input.
        let node_a = identity_toolchain(crate::ecosystem::Language::Rust);
        let node_b = identity_toolchain(crate::ecosystem::Language::Rust);
        assert_eq!(node_a, node_b);
    }

    #[test]
    fn w9_same_source_bytes_hash_the_same_regardless_of_simulated_node() {
        // What every node now actually feeds into the manifest, post-fix.
        let toolchain_used_by_every_node = identity_toolchain(crate::ecosystem::Language::Rust);
        let manifest_node_a = manifest_with_toolchain(toolchain_used_by_every_node.clone());
        let manifest_node_b = manifest_with_toolchain(toolchain_used_by_every_node);
        assert_eq!(
            manifest_node_a.identity_bytes(),
            manifest_node_b.identity_bytes(),
            "same source bytes must hash the same regardless of which node computed it"
        );
    }

    #[test]
    fn w9_a_mutable_per_node_toolchain_would_have_perturbed_identity_this_demonstrates_why_the_fix_is_needed()
     {
        // This is NOT the code path in use after the fix (execute_extract_phase
        // no longer reads a mutable per-node toolchain at all) — it documents
        // *why* routing through `identity_toolchain` matters: the underlying
        // `BlobManifest::identity_bytes` still folds the toolchain field in,
        // so a caller that went back to trusting a mutable per-node value
        // would immediately reintroduce W9.
        let macos_style = identity_toolchain(crate::ecosystem::Language::Rust);
        let would_be_real_linux_toolchain = heart::Toolchain::Rust {
            compiler: semver::Version::new(1, 92, 0),
            edition: heart::Edition::E2024,
        };
        assert_ne!(
            macos_style, would_be_real_linux_toolchain,
            "fixture must simulate two genuinely different toolchains"
        );

        let manifest_macos = manifest_with_toolchain(macos_style);
        let manifest_linux_style = manifest_with_toolchain(would_be_real_linux_toolchain);
        assert_ne!(
            manifest_macos.identity_bytes(),
            manifest_linux_style.identity_bytes(),
            "blob identity is toolchain-sensitive by construction — which is exactly \
             why execute_extract_phase must never feed it a mutable per-node value"
        );
    }

    // ── archive-wrapper stripping: `GET /packages/:id/files/*path` must serve
    // by package-relative path, so a registry archive's own `{name}-{version}/`
    // (crates.io/pypi) or `package/` (npm) wrapper directory must not leak
    // into the manifest's stored `FileEntry` paths. ──────────────────────────

    fn builder_with_files(files: &[(&str, &'static [u8])]) -> BlobBuilder {
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        for (path, bytes) in files {
            builder
                .push_file(
                    smol_str::SmolStr::new(*path),
                    bytes::Bytes::from_static(bytes),
                )
                .expect("push file");
        }
        builder
    }

    #[test]
    fn strip_archive_root_removes_a_real_crates_io_style_wrapper() {
        // Mirrors what `curl -s https://static.crates.io/crates/either/either-1.15.0.crate
        // | tar -tzf -` actually lists: every entry nested under `either-1.15.0/`.
        let builder = builder_with_files(&[
            ("either-1.15.0/Cargo.toml", b"[package]"),
            ("either-1.15.0/src/lib.rs", b"pub enum Either {}"),
            ("either-1.15.0/LICENSE-MIT", b"MIT"),
        ]);
        let stripped = strip_archive_root(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
            builder,
        )
        .expect("stripping succeeds");

        let mut paths: Vec<&str> = stripped
            .source_files()
            .map(|(path, _)| path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec!["Cargo.toml", "LICENSE-MIT", "src/lib.rs"],
            "every file must be package-relative after stripping the shared wrapper directory"
        );
    }

    #[test]
    fn strip_archive_root_is_a_no_op_when_files_are_already_package_relative() {
        let builder =
            builder_with_files(&[("Cargo.toml", b"[package]"), ("src/lib.rs", b"fn f() {}")]);
        let stripped = strip_archive_root(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
            builder,
        )
        .expect("stripping succeeds");

        let mut paths: Vec<&str> = stripped
            .source_files()
            .map(|(path, _)| path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["Cargo.toml", "src/lib.rs"]);
    }

    #[test]
    fn strip_archive_root_is_a_no_op_when_there_is_no_single_shared_directory() {
        // Two genuine top-level directories — nothing to strip; stripping the
        // first path's leading segment here would silently corrupt `src/`.
        let builder =
            builder_with_files(&[("src/lib.rs", b"fn f() {}"), ("tests/it.rs", b"fn t() {}")]);
        let stripped = strip_archive_root(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
            builder,
        )
        .expect("stripping succeeds");

        let mut paths: Vec<&str> = stripped
            .source_files()
            .map(|(path, _)| path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["src/lib.rs", "tests/it.rs"]);
    }

    #[test]
    fn strip_archive_root_is_a_no_op_for_a_single_bare_top_level_file() {
        // A one-file package whose only entry has no `/` at all: the "prefix"
        // would be the whole filename, not a wrapper directory — never strip.
        let builder = builder_with_files(&[("lib.rs", b"fn f() {}")]);
        let stripped = strip_archive_root(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
            builder,
        )
        .expect("stripping succeeds");

        let paths: Vec<&str> = stripped
            .source_files()
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(paths, vec!["lib.rs"]);
    }

    // ── Defect: identity canonicalization reused for archive-fetch URLs ────
    //
    // `static_archive_url_string` is a pure function of `PackageCoordinates`,
    // so these are plain unit tests — no network, no server, no live-stack
    // dependency. Real package names throughout (repo doctrine §4): every
    // crate/module/artifact/package below is a real, published identifier.

    use crate::ecosystem::{Language, PackageNameExt};
    use heart::RegistryOrigin;
    use heart::package::PackageName;

    fn coordinates(
        origin: RegistryOrigin,
        ecosystem: Language,
        raw_name: &str,
        raw_version: &str,
    ) -> PackageCoordinates {
        PackageCoordinates {
            origin,
            name: PackageName::new(ecosystem, raw_name).expect("valid fixture name"),
            version: heart::PackageVersion::try_from((ecosystem, raw_version))
                .expect("valid fixture version"),
        }
    }

    #[test]
    fn crates_io_url_uses_the_original_underscored_name_not_the_hyphenated_canonical() {
        // Regression for the defect: `once_cell`'s canonical form is
        // `once-cell` (crates.io dedups `_`/`-` for identity), but the real
        // crates.io CDN only serves the exact registered spelling. Fetching
        // the canonical form 403s (verified against the live registry).
        let coords = coordinates(
            RegistryOrigin::CratesIo,
            Language::Rust,
            "once_cell",
            "1.19.0",
        );
        assert_eq!(
            coords.name.canonical(),
            "once-cell",
            "sanity: canonical really does collapse the underscore"
        );
        let url = static_archive_url_string(&coords).expect("crates.io has a static url");
        assert_eq!(
            url, "https://static.crates.io/crates/once_cell/once_cell-1.19.0.crate",
            "must fetch the underscored name crates.io actually serves, not the canonical hyphenated one"
        );
    }

    #[test]
    fn crates_io_url_handles_a_second_real_underscored_crate() {
        // A second real, independent underscore crate, so the fix is not
        // accidentally special-cased to `once_cell`.
        let coords = coordinates(
            RegistryOrigin::CratesIo,
            Language::Rust,
            "parking_lot",
            "0.12.1",
        );
        let url = static_archive_url_string(&coords).expect("crates.io has a static url");
        assert_eq!(
            url,
            "https://static.crates.io/crates/parking_lot/parking_lot-0.12.1.crate"
        );
    }

    #[test]
    fn crates_io_url_is_unaffected_for_a_name_with_no_underscore() {
        // Non-regression: a plain hyphenated/lowercase name's canonical and
        // original forms already coincide, so the fix must not change its URL.
        let coords = coordinates(RegistryOrigin::CratesIo, Language::Rust, "serde", "1.0.203");
        let url = static_archive_url_string(&coords).expect("crates.io has a static url");
        assert_eq!(
            url,
            "https://static.crates.io/crates/serde/serde-1.0.203.crate"
        );
    }

    #[test]
    fn maven_url_uses_the_original_case_sensitive_group_and_artifact() {
        // Real Maven Central coordinate: ANTLR's StringTemplate 4 publishes
        // its artifactId as `ST4` (uppercase) — `org/antlr/ST4/...` on the
        // real repository. `render_canonical` for Java lowercases both
        // group and artifact for identity, which would 404 if used for the
        // fetch path.
        let coords = coordinates(
            RegistryOrigin::MavenCentral,
            Language::Java,
            "org.antlr:ST4",
            "4.3",
        );
        assert_eq!(
            coords.name.canonical(),
            "org.antlr:st4",
            "sanity: canonical really does lowercase the artifact"
        );
        let url = static_archive_url_string(&coords).expect("maven has a static url");
        assert_eq!(
            url, "https://repo1.maven.org/maven2/org/antlr/ST4/4.3/ST4-4.3-sources.jar",
            "must fetch the real case-sensitive `ST4` path, not the lowercased canonical `st4`"
        );
    }

    #[test]
    fn npm_url_drops_the_scope_in_the_tarball_basename_but_keeps_it_in_the_path() {
        // Real scoped npm package. npm requires lowercase at publish time, so
        // there is no canonical/original divergence here (unlike Rust/Maven) —
        // this pins the already-correct scope-handling behavior so a future
        // change cannot regress it silently.
        let coords = coordinates(
            RegistryOrigin::NpmPublic,
            Language::Typescript,
            "@types/node",
            "20.11.5",
        );
        let url = static_archive_url_string(&coords).expect("npm has a static url");
        assert_eq!(
            url,
            "https://registry.npmjs.org/@types/node/-/node-20.11.5.tgz"
        );
    }

    #[test]
    fn nuget_url_lowercases_regardless_of_the_registered_display_casing() {
        // Real NuGet package with mixed-case display name. NuGet's
        // flat-container feed requires lowercase in both path and filename
        // unconditionally, so this must lowercase even though `original()`
        // preserves the display casing.
        let coords = coordinates(
            RegistryOrigin::NuGet,
            Language::CSharp,
            "Newtonsoft.Json",
            "13.0.3",
        );
        let url = static_archive_url_string(&coords).expect("nuget has a static url");
        assert_eq!(
            url,
            "https://api.nuget.org/v3-flatcontainer/newtonsoft.json/13.0.3/newtonsoft.json.13.0.3.nupkg"
        );
    }

    #[test]
    fn go_proxy_url_preserves_case_via_bang_escaping() {
        // Real Go module (BurntSushi/toml) whose path contains uppercase
        // letters. Go's canonical form IS the original (case-preserving), so
        // this is not the canonical/original defect — it pins the goproxy
        // `!`-escaping the real proxy protocol requires for uppercase letters.
        let coords = coordinates(
            RegistryOrigin::GoProxy,
            Language::Go,
            "github.com/BurntSushi/toml",
            "v1.3.2",
        );
        let url = static_archive_url_string(&coords).expect("goproxy has a static url");
        assert_eq!(
            url,
            "https://proxy.golang.org/github.com/!burnt!sushi/toml/@v/v1.3.2.zip"
        );
    }

    #[test]
    fn pypi_and_git_have_no_static_archive_url() {
        // Both are handled outside this pure builder by `Indexer::archive_url`
        // (PyPI needs a live metadata fetch; `cpp`/Git has no archive URL at
        // all) — asserting `None` here, rather than driving them through a
        // live network call, is exactly the "URL-construction unit test"
        // that proves the split without hitting the network.
        let pypi = coordinates(
            RegistryOrigin::PyPi,
            Language::Python,
            "sqlalchemy",
            "2.0.29",
        );
        assert_eq!(static_archive_url_string(&pypi), None);

        let git_coords = PackageCoordinates {
            origin: RegistryOrigin::Git,
            name: PackageName::new(Language::Cpp, "fmtlib/fmt").expect("valid fixture name"),
            version: heart::PackageVersion::try_from((Language::Cpp, "10.2.1"))
                .expect("valid fixture version"),
        };
        assert_eq!(static_archive_url_string(&git_coords), None);
    }
}
