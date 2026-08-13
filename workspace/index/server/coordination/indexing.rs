//! The indexing flow: drive one dequeued job through the pipeline phases and
//! record every transition durably.
//!
//! The flow lives on [`Indexer`] — a struct that owns a handle to the assembled
//! server — rather than as free `impl Server` methods, so the pipeline's moving
//! parts (job loop, phase transitions, freshness) sit in one place with the
//! state they operate on.

#[allow(unused_imports)]
use crate::server::registry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::server::registry::blob::creation::BlobBuilder;
use crate::server::registry::blob::{BlobManifest, FileEntry, creation::PendingSection};
use crate::server::registry::identity::PackageCoordinates;
use crate::server::registry::metadata::rich::{self, ExtractionInput};
use crate::server::registry::queue::LeasedJob;
use crate::server::registry::{RegistryError, error::ResolveError};
use futures::StreamExt;
use heart::Retryable;
use heart::{ContentHash, FailureKind, JobProgress, PackageId, Percent, Phase, ResolutionState};
use crate::ecosystem::{DynSpec, LanguageExt};
use crate::ingest::archive::{EntryAllowlist, ExtractionLimits, ingest_archive};
use registry::vector::EmbeddingModel;

use crate::server::error::{BadRequestReason, InternalError, ServerError, ServerResult};
use crate::server::registry::metadata::SearchFacets;
use crate::server::{Server, SourceStores};

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
        Ok(ingest_archive(
            package,
            record.package.toolchain,
            std::io::Cursor::new(archive_bytes),
            archive_format,
            ExtractionLimits::DEFAULT,
            EntryAllowlist::SAFE,
        )
        .await
        .map_err(RegistryError::from)?)
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
        materialize_sources(builder, &source_root, &scratch_root).map_err(|source| {
            ServerError::Internal(InternalError::MaterializeForCompile { source })
        })?;

        // ── 2. Resolve toolchain golden + drive the cage (blocking) ───────────
        // The cage is a synchronous, CPU/VM-bound boundary; run it off the async
        // reactor via `spawn_blocking` so heartbeats/other jobs keep flowing.
        let profile = producer_profile(language);
        let image = self
            .toolchain_images
            .as_deref()
            .and_then(|store| store.lookup(profile))
            .map(|img| img.config_digest)
            .unwrap_or_else(|| toolchain_image_digest_placeholder(language));
        let cage_name = name.clone();
        let ir_bytes = tokio::task::spawn_blocking(move || {
            run_producer_in_cage(
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

        // ── 3. Hand the produced IR bytes to the emit path ────────────────────
        // `ingest_ir_bytes` decodes the producer's NdIrF1 stream via
        // `ir_vcs::protocol::StreamReceiver`: Symbols frames become the IR blob
        // section + the returned symbol identifier list, and Bodies frames are
        // lowered into the blob `ReferenceSet` (oracle calls / type mentions).
        // The producer binary is provisioned in the golden toolchain image; the
        // on-wire framing contract is honored here.
        let identifiers = ingest_ir_bytes(builder, &ir_bytes);

        tracing::info!(
            %package,
            language = language.as_token(),
            ir_bytes = ir_bytes.len(),
            identifiers = identifiers.len(),
            "cage compile produced IR"
        );

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
        let listing = fetch_listing_signals(coordinates, &self.acquisition).await;

        let mut facets = extract_facets(
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
            fetch_and_set_downloads(coordinates, f, &self.acquisition).await;
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

        let emitted =
            crate::server::registry::blob::emit::emit(&stores.blobs, &stores.outbox, manifest, sections)
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
        let archive = self
            .acquisition
            .get(lang, url.as_str())
            .await
            .map_err(|e| match e {
                UpstreamError::NotFound => not_found(coordinates),
                other => ServerError::Internal(InternalError::UpstreamFetch {
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

        let name = coordinates.name.canonical();
        let version = coordinates.version.canonical();

        let raw_url_string = match &coordinates.origin {
            RegistryOrigin::CratesIo => {
                format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")
            }
            RegistryOrigin::NpmPublic => {
                let leaf = name.rsplit('/').next().unwrap_or(name);
                format!("https://registry.npmjs.org/{name}/-/{leaf}-{version}.tgz")
            }
            RegistryOrigin::PyPi => return self.pypi_sdist_url(name, &version).await,
            RegistryOrigin::NuGet => {
                let id = name.to_ascii_lowercase();
                let ver = version.to_ascii_lowercase();
                format!("https://api.nuget.org/v3-flatcontainer/{id}/{ver}/{id}.{ver}.nupkg")
            }
            RegistryOrigin::FlakeHub => {
                format!("https://api.flakehub.com/f/{name}/{version}.tar.gz")
            }
            RegistryOrigin::GoProxy => {
                // goproxy capital-escaping on module path.
                let escaped = crate::ecosystem::escape_module_path(name);
                format!("https://proxy.golang.org/{escaped}/@v/{version}.zip")
            }
            RegistryOrigin::MavenCentral => {
                // canonical is `group:artifact`; sources jar preferred.
                if let Some((group, artifact)) = name.split_once(':') {
                    let group_path = group.replace('.', "/");
                    format!(
                        "https://repo1.maven.org/maven2/{group_path}/{artifact}/{version}/{artifact}-{version}-sources.jar"
                    )
                } else {
                    format!(
                        "https://repo1.maven.org/maven2/{name}/{version}/{name}-{version}-sources.jar"
                    )
                }
            }
            RegistryOrigin::Custom { url, .. } => {
                format!(
                    "{}archives/{name}/{name}-{version}.tar.gz",
                    ensure_trailing_slash(url)
                )
            }
            // The registry-less `cpp` plane acquires source by git checkout
            // (RL-14, §7.4), not by archive download — there is no URL to build.
            RegistryOrigin::Git => {
                return Err(ServerError::Internal(
                    InternalError::GitOriginHasNoArchiveUrl,
                ));
            }
        };

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
        // (compile-daemon failure kinds removed with the daemon; the stubbed
        // compile phase surfaces as `Internal`, handled by the fallback below)
        _ if error.is_retryable() => FailureKind::Transient,
        _ => FailureKind::Internal,
    }
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

// ── Compile phase: cage lifecycle helpers ─────────────────────────────────

/// Guest mount point (via the virtiofs tag `ro0`) for the materialized source
/// tree. The cage projects `FsGrant.read_only[0]` to `/mnt/ro0` inside the VM
/// (see `sandbox::smolvm_backend::translate_mounts`).
const GUEST_SOURCE_MOUNT: &str = "/mnt/ro0";

/// The per-language producer invocation contract.
///
/// This is the **code-level** specification of HOW each producer is invoked.
/// The golden toolchain OCI images (built by the Buck2 compiler tree) fulfill
/// this contract by shipping the producer binary at `entrypoint` and honouring
/// the `args` convention. The cage on the host side merely execs these bytes;
/// the content of the image is a deployment concern, not a code concern.
///
/// # Convention
///
/// Every producer binary follows the same interface:
/// - `--source <path>`  : the RO-mounted source tree inside the guest VM
///                        (always [`GUEST_SOURCE_MOUNT`]).
/// - `--emit ndirf1`    : emit IR on stdout in the NdIrF1 / `ir-stream`
///                        postcard-framed protocol (SMOLVM-PLAN §6.1).
///
/// Language-specific additional flags (e.g. `--edition 2021` for Rust,
/// `--target-jdk 21` for Java) follow after `--emit ndirf1`; they are
/// listed per-language below.
///
/// # Guest paths
///
/// All binary paths are absolute guest paths within the language toolchain
/// image (e.g. `/opt/nudox/rust/bin/nudox-rust-producer`). The host never
/// resolves these paths; the image's `PATH` is irrelevant because the cage
/// execs the binary directly.
pub struct ProducerInvocation {
    /// Absolute guest path to the producer binary.
    pub entrypoint: &'static str,
    /// Argv (excluding argv0). Passed verbatim to the cage exec.
    pub args: &'static [&'static str],
}

/// Return the producer invocation contract for `language`.
///
/// Each arm names the guest entrypoint + the fixed argv the cage passes.
/// The golden toolchain image for that language must ship the binary at this
/// path and must accept this argv without modification.
pub fn producer_command(language: crate::ecosystem::Language) -> ProducerInvocation {
    use crate::ecosystem::Language;
    match language {
        // Rust: nudox-rust-producer (ra_ap-backed) — reads Cargo sources at
        // GUEST_SOURCE_MOUNT, emits NdIrF1 IR on stdout. `--edition 2021` is
        // the default; the producer overrides it per-crate from Cargo.toml.
        Language::Rust => ProducerInvocation {
            entrypoint: "/opt/nudox/rust/bin/nudox-rust-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--edition",
                "2021",
            ],
        },

        // Java: nudox-java-producer (Doclet/javac-oracle-backed) — reads
        // Maven/Gradle sources at GUEST_SOURCE_MOUNT, emits NdIrF1.
        // `--target-jdk 21` is the baseline; the producer auto-detects from
        // pom.xml/build.gradle when available.
        Language::Java => ProducerInvocation {
            entrypoint: "/opt/nudox/java/bin/nudox-java-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--target-jdk",
                "21",
            ],
        },

        // Go: nudox-go-producer (go/types-backed) — reads Go module sources.
        // `--module-root` instructs the producer to treat GUEST_SOURCE_MOUNT
        // as the module root (go.mod must be present at that path).
        Language::Go => ProducerInvocation {
            entrypoint: "/opt/nudox/go/bin/nudox-go-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--module-root",
                GUEST_SOURCE_MOUNT,
            ],
        },

        // C#: nudox-csharp-producer (Roslyn-backed) — reads .NET sources.
        // `--target-tfm net9.0` is the baseline target framework moniker; the
        // producer overrides it from the .csproj when available.
        Language::CSharp => ProducerInvocation {
            entrypoint: "/opt/nudox/dotnet/bin/nudox-csharp-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--target-tfm",
                "net9.0",
            ],
        },

        // Nix: nudox-nix-producer (snix-eval-backed) — evaluates the Nix
        // flake at GUEST_SOURCE_MOUNT and emits IR for derivation symbols.
        // `--flake` instructs the producer to look for a `flake.nix` at the
        // source root and evaluate the default package set.
        Language::Nix => ProducerInvocation {
            entrypoint: "/opt/nudox/nix/bin/nudox-nix-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--flake",
            ],
        },

        // TypeScript: nudox-ts-producer (OXC-backed static parser, LOW tier).
        // OXC operates on the source tree directly; no separate oracle binary.
        Language::Typescript => ProducerInvocation {
            entrypoint: "/opt/nudox/ts/bin/nudox-ts-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },

        // Python: nudox-python-producer (pyrefly/static-parse-backed, LOW tier).
        Language::Python => ProducerInvocation {
            entrypoint: "/opt/nudox/python/bin/nudox-python-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },

        // C/C++: nudox-cpp-producer (tree-sitter static parse, LOW tier).
        // No compiler oracle yet; source-only, git-checkout plane (RL-14 §7.4).
        Language::Cpp => ProducerInvocation {
            entrypoint: "/opt/nudox/cpp/bin/nudox-cpp-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },
    }
}

/// Map a package language onto the sandbox [`ProducerProfile`] whose resource
/// ceilings + threat tier the compile runs under.
fn producer_profile(language: crate::ecosystem::Language) -> sandbox::ProducerProfile {
    use crate::ecosystem::Language;
    use sandbox::ProducerProfile;
    match language {
        Language::Rust => ProducerProfile::Rust,
        Language::Java => ProducerProfile::Java,
        Language::Go => ProducerProfile::Go,
        Language::CSharp => ProducerProfile::CSharp,
        Language::Nix => ProducerProfile::Nix,
        // deno_doc (TS) / pyrefly (Python) are LOW static parsers; C/C++ has no
        // dedicated profile yet and reads as a static parse over source.
        Language::Typescript | Language::Python | Language::Cpp => ProducerProfile::StaticParser,
    }
}

/// Fallback toolchain-image digest used when no [`sandbox::ToolchainImageStore`]
/// is wired into the [`Indexer`] (e.g. when images are not yet provisioned on
/// this forge node, or in development without a full OCI image store).
///
/// Returns a deterministic, collision-free-per-language placeholder so
/// `prepare_golden`/`fork_golden` are exercised for real (distinct languages
/// get distinct goldens) without inventing image content. Each byte 0 is the
/// first byte of the language token, rest zero.
///
/// The `Indexer::with_toolchain_images` call wires in the real
/// `ToolchainImageStore`; once images are provisioned the store lookup in
/// `execute_compile_phase` supersedes this function for every language whose
/// image is registered. This fallback fires only for the not-found case.
fn toolchain_image_digest_placeholder(
    language: crate::ecosystem::Language,
) -> sandbox::ImageDigest {
    let mut bytes = [0u8; 32];
    if let Some(&b0) = language.as_token().as_bytes().first() {
        bytes[0] = b0;
    }
    sandbox::ImageDigest::from_bytes(bytes)
}

/// Write the builder's staged source files onto `source_root` (creating parent
/// dirs) and create an empty `scratch_root`. Path traversal is already excluded
/// by ingest sanitization; we defensively skip any absolute / `..` component.
fn materialize_sources(
    builder: &BlobBuilder,
    source_root: &std::path::Path,
    scratch_root: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(source_root)?;
    std::fs::create_dir_all(scratch_root)?;
    for (rel, bytes) in builder.source_files() {
        let rel_path = std::path::Path::new(rel.as_str());
        // Defensive: never escape the source root.
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let dest = source_root.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, bytes)?;
    }
    Ok(())
}

/// Fork a warm cage from the language golden (falling back to a fresh cage) and
/// run the sealed producer command inside it, returning the produced IR bytes
/// (the producer's stdout). Synchronous: intended for `spawn_blocking`.
///
/// This is the real cage lifecycle: `prepare_golden` (idempotent) → `fork_golden`
/// → `Cage::run` → teardown (the clone's `kill` runs inside `run`). The producer
/// argv is resolved by [`producer_command`], which specifies the per-language
/// invocation contract that the golden toolchain images fulfil.
fn run_producer_in_cage(
    package: &str,
    language: crate::ecosystem::Language,
    profile: sandbox::ProducerProfile,
    image: sandbox::ImageDigest,
    source_root: &std::path::Path,
    scratch_root: &std::path::Path,
) -> ServerResult<Vec<u8>> {
    use sandbox::vm::{VmError, VmHandle, VmRuntime};
    use sandbox::{
        Cage, CancelToken, CapabilityBudget, Env, FsGrant, NetGrant, RootfsStore, SealedCommand,
        SmolvmCage, SmolvmRuntime,
    };

    let cage_err = |reason: String| {
        ServerError::Internal(InternalError::CageCompile {
            package: package.to_owned(),
            reason,
        })
    };

    // Bootstrap the rootfs store + runtime (NUDOX_GUEST_ROOTFS on a forge node).
    let store = RootfsStore::from_env().map_err(|e| cage_err(e.to_string()))?;
    let runtime = SmolvmRuntime::new(store);

    // ── Golden: prepare (idempotent) then fork a warm clone ───────────────────
    // Falls back to a fresh cold-boot cage when live fork is unavailable on this
    // host (non-Linux/macOS, or the golden could not be parked) — the no-silent-
    // degrade rule still holds: only *unsupported fork* falls back; a hard cage
    // failure propagates.
    let handle = match runtime
        .prepare_golden(&image)
        .and_then(|golden| runtime.fork_golden(&golden))
    {
        Ok(handle) => Some(handle),
        Err(VmError::Unsupported { reason }) => {
            tracing::info!(
                %package,
                reason,
                "golden fork unavailable on this host; cold-booting a fresh cage"
            );
            None
        }
        Err(other) => return Err(cage_err(format!("golden prepare/fork: {other}"))),
    };

    // ── Budget: RO source root, ephemeral scratch overlay, network OFF ────────
    // By design (sealed-image plane): the toolchain roots (rustup/cargo/GOROOT/…)
    // live INSIDE the golden guest image, so there are NO host-side RO toolchain
    // binds — only the package source is bound RO. A host-bind toolchain plane
    // would extend this `FsGrant`; the golden-image plane does not need it.
    let fs = FsGrant::scratch(scratch_root).ro(source_root);
    let budget = CapabilityBudget::new(fs, NetGrant::Off, Env::empty(), profile.limits());

    // ── The producer invocation ───────────────────────────────────────────────
    // `producer_command` specifies the per-language invocation contract (guest
    // binary path + argv) that the golden toolchain OCI image fulfils. The cage
    // execs this verbatim; the binary inside the image reads the RO source tree
    // at GUEST_SOURCE_MOUNT and streams NdIrF1 IR frames on its stdout.
    let inv = producer_command(language);
    let command = SealedCommand::new(inv.entrypoint, inv.args.iter().copied(), budget);

    let cancel = CancelToken::never();
    let output = match handle {
        // Warm fork clone: run directly against the live handle, then tear down.
        Some(mut handle) => {
            let spec = sandbox::project_run_spec(&command);
            let out = handle.exec(&spec);
            handle.kill(); // one-VM-per-job: the clone dies at end of job
            out.map_err(|e| cage_err(format!("producer exec (forked clone): {e}")))?
        }
        // Cold path: the cage's own `run` boots a fresh VM, execs, and tears
        // it down (ephemeral overlay dies with it).
        None => {
            let cage = SmolvmCage::with_runtime(runtime);
            Cage::run(&cage, command, &cancel)
                .map_err(|e| cage_err(format!("cage run (cold boot): {e}")))?
        }
    };

    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ServerError::Internal(InternalError::ProducerFailed {
            package: package.to_owned(),
            reason: format!(
                "exit {:?}; stderr: {}",
                output.status(),
                stderr.chars().take(2000).collect::<String>()
            ),
        }));
    }

    Ok(output.stdout)
}

/// Decode the cage producer's IR stream bytes, stage them onto `builder`, and
/// return the symbol identifier list for the emit/facets path.
///
/// # Wire format
///
/// The producer writes a postcard-framed `ir-stream` protocol (SMOLVM-PLAN
/// §6.1/§6.2) onto its stdout. Each frame is `[u32 LE length][postcard bytes]`.
/// [`ir_vcs::protocol::StreamReceiver`] handles framing, ordering enforcement,
/// and emitted-count validation.
///
/// # Sections staged onto `builder`
///
/// - **IR section** (`builder.set_ir`): postcard-serialized
///   `Vec<ir_vcs::wire::OwnedEntryPayload>` — one entry per `Symbols` batch entry,
///   in stream order. This is what the IR-plane apply/checkout machinery reads.
/// - **References section** (`builder.set_references`): a [`ReferenceSet`]
///   derived from the `Bodies` frames in the stream (INDEX-PLAN §5.1).
///
/// # References derivation (INDEX-PLAN §5.1)
///
/// The `Occurrences` frames carry opaque producer-specific bytes with no
/// public decoder in `ir` or `ir-vcs` (the format is intentionally producer-
/// private and the host records them opaquely — see `stream::StreamedRecording`).
/// The `Bodies` frames, however, carry fully decoded [`ir::body::BodyEmbed`] values
/// with oracle-resolved call targets ([`ir::OracleCall`]) and type mentions
/// ([`ir::OracleTypeMention`]) — both carry a [`ir::change::StableRef`] that
/// identifies the target symbol across packages.
///
/// Policy (documented here, not a §5.1 blocker):
/// - We extract *oracle-resolved* references only (`Confidence >= Index`),
///   since tree-sitter call-name facts have no stable cross-package target.
/// - The `Reference::span_start`/`span_end` are the `RelSpan` bounds relative
///   to the owning entry's declaration span start (not absolute file offsets).
///   This is correct for the use case: the reference store holds relative
///   spans and the caller reconstructs absolute ones from the symbol's
///   `span_start` when needed.
/// - `RefTarget`: a cross-package target (`StableRef.package` ≠ owning entry's
///   package name) maps to `RefTarget::External { path: intro_hex, dependency:
///   "ecosystem:name" }`; a same-package target maps to
///   `RefTarget::Local(intro_hex)`. Using `intro_hex` as the `path` is
///   intentional: we don't have source file paths for target symbols (only for
///   source symbols from `SymbolWire::source_path`); the `intro_hex` is the
///   durable, wire-stable identity that the IR-plane apply/checkout machinery
///   resolves to a symbol path on demand.
///
/// # Aborted / empty streams
///
/// An `Abort` frame is treated as a non-fatal producer failure: the builder
/// sections are still attached with whatever was received so far (possibly
/// empty) and an empty identifier list is returned. A failed stream decode
/// (framing error, version mismatch) logs a warning and degrades the same way.
fn ingest_ir_bytes(builder: &mut BlobBuilder, ir_bytes: &[u8]) -> Vec<String> {
    use ir::change::IntroId;
    use ir_vcs::protocol::{BodyWire, Received, StreamReceiver};

    let mut rx = StreamReceiver::new(std::io::Cursor::new(ir_bytes));

    // Handshake: the Hello frame carries the job key and producer id.
    // A missing or malformed Hello means the producer wrote nothing usable.
    match rx.accept() {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(error = %err, "IR stream has no Hello frame; attaching empty IR section");
            attach_empty_ir_sections(builder);
            return Vec::new();
        }
    }

    let mut payloads: Vec<ir_vcs::wire::OwnedEntryPayload> = Vec::new();
    let mut identifiers: Vec<String> = Vec::new();
    // IntroId → source_path from the Symbols batches: used to group
    // oracle-derived references by their owning entry's source file.
    let mut intro_to_path: HashMap<IntroId, String> = HashMap::new();
    // Owning package name (ecosystem:name) from the first Symbol — needed to
    // distinguish Local vs External RefTarget. We derive it lazily from the
    // StableRef on the first WireEntry; if no symbols arrive it stays None
    // and all targets default to External (the conservative choice).
    let mut owning_pkg_key: Option<String> = None;
    // Bodies frames: accumulated across all batches for post-loop reference
    // extraction (they can arrive interleaved with Symbols).
    let mut bodies: Vec<BodyWire> = Vec::new();

    loop {
        match rx.recv() {
            Ok(Some(Received::Symbols(batch))) => {
                for entry in batch {
                    // Record source_path for this intro for reference grouping.
                    let intro = entry.stable.intro;
                    let src_path = entry.payload.symbol.source_path.clone();
                    intro_to_path.entry(intro).or_insert_with(|| src_path);

                    // Capture the owning package key (ecosystem:name) once.
                    if owning_pkg_key.is_none() {
                        let pkg = &entry.stable.package;
                        owning_pkg_key =
                            Some(format!("{}:{}", pkg.ecosystem.as_str(), pkg.name.as_str()));
                    }

                    identifiers.push(entry.payload.symbol.name.clone());
                    payloads.push(entry.payload);
                }
            }
            Ok(Some(Received::Bodies(batch))) => {
                bodies.extend(batch);
            }
            Ok(Some(Received::Links { .. }))
            | Ok(Some(Received::SourceDigest { .. }))
            | Ok(Some(Received::Progress { .. }))
            | Ok(Some(Received::Occurrences(_))) => {
                // Occurrences bytes are opaque (producer-private format, no public
                // decoder in ir/ir-vcs) — accumulated in stream.rs as raw bytes
                // for provenance; not decoded here. SourceDigest provenance is
                // similarly out of scope for the reference slot.
            }
            Ok(Some(Received::Finish { .. })) | Ok(None) => {
                break;
            }
            Ok(Some(Received::Abort { failure, message })) => {
                tracing::warn!(
                    ?failure,
                    message,
                    "IR stream producer aborted; identifiers recovered so far: {}",
                    identifiers.len()
                );
                break;
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    identifiers = identifiers.len(),
                    "IR stream decode error; using identifiers recovered so far"
                );
                break;
            }
        }
    }

    // Serialize the collected payloads as the IR blob section.
    match postcard::to_allocvec(&payloads) {
        Ok(ir_blob) => {
            if let Err(err) = builder.set_ir(bytes::Bytes::from(ir_blob)) {
                tracing::warn!(error = %err, "set_ir failed; attaching empty IR section");
                attach_empty_ir_sections(builder);
                return Vec::new();
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "IR payload serialization failed; attaching empty IR section");
            attach_empty_ir_sections(builder);
            return Vec::new();
        }
    }

    // ── Derive the ReferenceSet from the Bodies frames (INDEX-PLAN §5.1) ──────
    //
    // For each BodyWire, look up the owning entry's source file via
    // `intro_to_path`, then extract oracle-resolved references from
    // `BodyEmbed::Present(BodyFacts)`. Oracle calls and type mentions both
    // carry a StableRef target with Confidence >= Index.
    //
    // Policy: only emit references with a resolved `target` (OracleCall.target
    // = Some) and at confidence >= Index (already implied by the oracle tier,
    // but explicit for clarity). Tree-sitter call-name facts on the
    // `TreesitterBody` side have no cross-package stable target and are
    // skipped (they contribute call-site counts, not cross-reference graph
    // edges, per §5.1 merge rule 3 / Confidence::GRAPH_FLOOR).
    let ref_set =
        build_reference_set_from_bodies(&bodies, &intro_to_path, owning_pkg_key.as_deref());
    tracing::debug!(
        by_file = ref_set.by_file.len(),
        total_refs = ref_set
            .by_file
            .iter()
            .map(|f| f.references.len())
            .sum::<usize>(),
        "reference set derived from Bodies frames"
    );

    if let Err(err) = builder.set_references(&ref_set) {
        tracing::warn!(error = %err, "set_references failed; attaching empty reference set");
        let empty_refs = crate::server::registry::blob::ReferenceSet {
            by_file: Vec::new(),
        };
        let _ = builder.set_references(&empty_refs);
    }

    identifiers
}

/// Derive a [`ReferenceSet`] from the accumulated `Bodies` frames.
///
/// See `ingest_ir_bytes` for the policy rationale. This is a pure function
/// (no I/O) so it can be unit-tested independently.
///
/// # Policy
/// - `OracleCall` with `target: Some(stable_ref)` → `Reference` with
///   `kind = stable_ref.kind as u8`, span from `rel_span`.
/// - `OracleTypeMention` → `Reference` with kind discriminant for
///   `ReferenceKind::TypeReference` (2).
/// - `RefTarget::Local(intro_hex)` when `stable_ref.package` matches
///   `owning_pkg` (same-package reference). Otherwise
///   `RefTarget::External { path: intro_hex, dependency: "eco:name" }`.
/// - References are grouped by the owning entry's `source_path`; entries
///   whose `intro` is not in `intro_to_path` are grouped under the sentinel
///   path `"<unknown>"` (body arrived without a matching Symbol frame).
fn build_reference_set_from_bodies(
    bodies: &[ir_vcs::protocol::BodyWire],
    intro_to_path: &HashMap<ir::change::IntroId, String>,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, Reference, ReferenceSet};
    use ir::body::BodyEmbed;
    use ir::vocab::ReferenceKind;
    use smol_str::SmolStr;

    // Per-file accumulator: file_path → Vec<Reference>.
    let mut by_file: HashMap<SmolStr, Vec<Reference>> = HashMap::new();

    for bw in bodies {
        let src_path = intro_to_path
            .get(&bw.intro)
            .map(|s| SmolStr::from(s.as_str()))
            .unwrap_or_else(|| SmolStr::new("<unknown>"));

        let refs = by_file.entry(src_path).or_default();

        if let BodyEmbed::Present(facts) = &bw.body {
            // Oracle calls: only those with a resolved target.
            for call in &facts.oracle.calls {
                let Some(ref stable) = call.target else {
                    continue;
                };
                // Policy: emit only graph-worthy references (Confidence >= Index).
                if !call.confidence.is_graph_worthy() {
                    continue;
                }
                let target = make_ref_target(stable, owning_pkg);
                refs.push(Reference {
                    target,
                    span_start: call.rel_span.start as u64,
                    span_end: call.rel_span.end as u64,
                    kind: call.kind as u8,
                });
            }

            // Oracle type mentions: all resolved (no target-less form).
            for mention in &facts.oracle.type_mentions {
                let target = make_ref_target(&mention.ty, owning_pkg);
                refs.push(Reference {
                    target,
                    span_start: mention.rel_span.start as u64,
                    span_end: mention.rel_span.end as u64,
                    kind: ReferenceKind::TypeReference as u8,
                });
            }
        }
    }

    // Discard empty file buckets (entries with Absent bodies contribute nothing).
    let file_refs: Vec<FileReferences> = by_file
        .into_iter()
        .filter(|(_, refs)| !refs.is_empty())
        .map(|(path, references)| FileReferences { path, references })
        .collect();

    ReferenceSet { by_file: file_refs }
}

/// Map a [`ir::change::StableRef`] to a [`crate::server::registry::blob::RefTarget`].
///
/// The `path` component is the target symbol's `IntroId` in hex — the durable,
/// wire-stable identity the IR plane uses. Source file paths for target symbols
/// are not available at this stage (the host would need to look them up from the
/// IR store, which is an async operation not available in the blocking decode
/// path). The `intro_hex` is sufficient for cross-reference graph edges; the
/// IR graph adapter resolves it to a file path on demand.
fn make_ref_target(
    stable: &ir::change::StableRef,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::RefTarget {
    use crate::server::registry::blob::RefTarget;

    let intro_hex = stable.intro.to_hex();
    let target_pkg_key = format!(
        "{}:{}",
        stable.package.ecosystem.as_str(),
        stable.package.name.as_str()
    );

    if owning_pkg == Some(target_pkg_key.as_str()) {
        RefTarget::Local(intro_hex)
    } else {
        RefTarget::External {
            path: intro_hex,
            dependency: target_pkg_key,
        }
    }
}

/// Attach an empty IR section and an empty reference section to `builder` so
/// that `finalize()` does not fail with `MissingIrSection` or
/// `MissingReferencesSection`. Used when the stream is empty or undecodable.
fn attach_empty_ir_sections(builder: &mut BlobBuilder) {
    let empty_payloads: Vec<ir_vcs::wire::OwnedEntryPayload> = Vec::new();
    let ir_blob = postcard::to_allocvec(&empty_payloads).unwrap_or_default();
    let _ = builder.set_ir(bytes::Bytes::from(ir_blob));
    let empty_refs = crate::server::registry::blob::ReferenceSet {
        by_file: Vec::new(),
    };
    let _ = builder.set_references(&empty_refs);
}

fn ensure_trailing_slash(url: &url::Url) -> String {
    let mut rendered = url.to_string();
    if !rendered.ends_with('/') {
        rendered.push('/');
    }
    rendered
}

/// Fetch the registry listing body and parse release/freshness signals.
///
/// Non-fatal: returns `None` on any failure so emit still succeeds.
/// Tuple: `(total, withdrawn, this_version_withdrawn, last_release_days_ago)`.
async fn fetch_listing_signals(
    coordinates: &PackageCoordinates,
    client: &registry::upstream::UpstreamClient,
) -> Option<(u32, u32, bool, Option<u32>)> {
    use crate::server::registry::search::listing_signals::listing_signals_from_body;
    use crate::ecosystem::LanguageExt;

    let spec = coordinates.ecosystem().spec();
    let name = coordinates.name.canonical();
    let version = coordinates.version.canonical();

    // Prefer reusing the absolute download/listing URL when available (crates.io,
    // npm packument patterns). Relative templates alone need an origin we don't
    // always have at this call site.
    let full_url = if let Some(dl) = spec.download_source() {
        let url = dl.url.replace("{name}", &name);
        // crates.io download source *is* the listing JSON — use it for signals.
        if coordinates.ecosystem() == crate::ecosystem::Language::Rust {
            url
        } else {
            // npm downloads API is not the packument; use known packument/JSON URLs.
            match coordinates.ecosystem() {
                crate::ecosystem::Language::Typescript => {
                    format!("https://registry.npmjs.org/{name}")
                }
                crate::ecosystem::Language::Python => {
                    format!("https://pypi.org/pypi/{name}/json")
                }
                _ => url,
            }
        }
    } else {
        match coordinates.ecosystem() {
            crate::ecosystem::Language::Python => format!("https://pypi.org/pypi/{name}/json"),
            crate::ecosystem::Language::Typescript => format!("https://registry.npmjs.org/{name}"),
            _ => return None,
        }
    };

    let body = match client.get(coordinates.ecosystem(), &full_url).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(
                package = %name,
                ecosystem = ?coordinates.ecosystem(),
                error = %e,
                "listing signals fetch failed"
            );
            return None;
        }
    };
    let signals = listing_signals_from_body(
        coordinates.ecosystem(),
        &body,
        version.as_str(),
        chrono::Utc::now(),
    )?;
    tracing::debug!(
        package = %name,
        total = signals.total,
        withdrawn = signals.withdrawn,
        days = ?signals.last_release_days_ago,
        "listing signals fetched"
    );
    Some((
        signals.total,
        signals.withdrawn,
        signals.this_version_withdrawn,
        signals.last_release_days_ago,
    ))
}

/// Fetch the download count for `coordinates` from the ecosystem's
/// [`DownloadEndpoint`](crate::ecosystem::upstream::DownloadEndpoint) (if any) and
/// store it in `facets.downloads`.
///
/// Ecosystems with a source today: TypeScript (npm downloads API), C# (NuGet
/// search `totalDownloads`), Rust (crates.io listing `crate.recent_downloads`).
/// Others return `download_source() = None` and leave `downloads` unset so
/// ranking uses the fairness floor (and/or corpus `dependents`).
///
/// Non-fatal: any network/parse failure is logged at debug level and the
/// `downloads` field is left as `None`. Never fails ingest. Explicit zero
/// counts from a successful parse are stored as `Some(0)`.
async fn fetch_and_set_downloads(
    coordinates: &PackageCoordinates,
    facets: &mut SearchFacets,
    client: &registry::upstream::UpstreamClient,
) {
    let spec = coordinates.ecosystem().spec();
    let Some(endpoint) = spec.download_source() else {
        return; // ecosystem has no download-count API — fairness floor handles it
    };
    let name = coordinates.name.canonical();
    let url = endpoint.url.replace("{name}", name);
    match client.get(coordinates.ecosystem(), &url).await {
        Ok(body) => match spec.parse_download_count(&body) {
            Some(count) => {
                facets.downloads = Some(count);
                tracing::debug!(
                    package = %coordinates.name.canonical(),
                    ecosystem = ?coordinates.ecosystem(),
                    downloads = count,
                    "download count fetched"
                );
            }
            None => {
                tracing::debug!(
                    package = %coordinates.name.canonical(),
                    ecosystem = ?coordinates.ecosystem(),
                    "download count endpoint returned unparseable body"
                );
            }
        },
        Err(e) => {
            tracing::debug!(
                package = %coordinates.name.canonical(),
                ecosystem = ?coordinates.ecosystem(),
                error = %e,
                "download count fetch failed (non-fatal)"
            );
        }
    }
}

/// Build the [`SearchFacets`] for a freshly-emitted snapshot from its manifest
/// files and their still-in-memory `sections` bytes.
///
/// Ecosystem-generic: manifest discovery iterates `spec.manifest_candidates()`
/// in priority order, case-insensitively matched against snapshot entries,
/// preferring root or one-wrapper-dir paths. Parsing is delegated to
/// `spec.extract_facts()`. README discovery is root-first across all ecosystems.
/// `loc` is a cheap honest newline count of source-file sections. Category
/// mapping (`spec.search_norms().map_category`) is applied inside
/// `spec.extract_facts()` for ecosystems that do so at parse time (Python trove
/// classifiers); for ecosystems whose native categories ARE the shared taxonomy
/// (Rust), `extract_facts` passes them through via `map_internal_category`.
/// Either way the facts arrive already mapped into `ExtractedFacts::categories`.
///
/// Non-fatal by design: any missing/unparseable input degrades to a minimal
/// name-only facet set (or `None`), so ingest never fails because search
/// metadata could not be derived.
///
/// `listing` is release/freshness signals observed from the registry listing
/// body (or resolve): `(total, withdrawn, this_withdrawn, last_release_days_ago)`.
/// Pass `None` when no listing fetch is available.
fn extract_facets(
    coordinates: &PackageCoordinates,
    manifest: &BlobManifest,
    sections: &[PendingSection],
    identifiers: &[String],
    heuristics: Option<&crate::server::Heuristics>,
    listing: Option<(u32, u32, bool, Option<u32>)>,
) -> Option<SearchFacets> {
    let (synonyms, specifics) = match heuristics {
        Some(h) => (Some(h.synonyms()), Some(h.specifics())),
        None => (None, None),
    };

    let spec: &'static dyn DynSpec = coordinates.ecosystem().spec();
    let norms = spec.search_norms();
    let name = coordinates.name.canonical().to_owned();

    // ── Manifest discovery ────────────────────────────────────────────────────
    // Bytes of a manifest file are fetched from `sections` by content hash — the
    // same hash the `FileEntry` records — so no post-emit blob round-trip is needed.
    let file_bytes = |entry: &FileEntry| -> Option<bytes::Bytes> {
        let section = sections.iter().find(|s| s.hash == entry.hash)?;
        Some(section.bytes.clone())
    };

    // Path matches `suffix` case-insensitively, at root or under a single
    // wrapper dir (e.g. `pkg-1.0/Cargo.toml` is depth-1, which is fine).
    let matches_candidate = |path: &str, suffix: &str| -> bool {
        let path_lower = path.to_ascii_lowercase();
        let suffix_lower = suffix.to_ascii_lowercase();
        // Exact match (root).
        if path_lower == suffix_lower {
            return true;
        }
        // One wrapper dir: path ends with `/<suffix>` and has no further `/`.
        if let Some(stripped) = path_lower.strip_suffix(&format!("/{suffix_lower}")) {
            return !stripped.contains('/');
        }
        false
    };

    let facts = {
        let candidates = spec.manifest_candidates();
        let mut found = None;
        'outer: for candidate in candidates {
            for entry in &manifest.files {
                if matches_candidate(entry.path.as_str(), candidate.path_suffix)
                    && let Some(bytes) = file_bytes(entry)
                {
                    found = spec.extract_facts(candidate, &bytes);
                    if found.is_some() {
                        tracing::debug!(
                            package = %manifest.package,
                            manifest = entry.path.as_str(),
                            "manifest parsed"
                        );
                        break 'outer;
                    }
                }
            }
        }
        match found {
            Some(f) => f,
            None => {
                tracing::debug!(
                    package = %manifest.package,
                    ecosystem = ?coordinates.ecosystem(),
                    "no manifest found; name-only facets"
                );
                crate::ecosystem::manifest::ExtractedFacts::default()
            }
        }
    };

    // ── README discovery ──────────────────────────────────────────────────────
    // Root-first: README.md / README.rst / README.txt / README (case-insensitive),
    // else use the manifest-declared readme_hint path.
    let readme_text = {
        let readme_leaf_matches = |path: &str| -> bool {
            let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
            (leaf == "readme.md"
                || leaf == "readme.rst"
                || leaf == "readme.txt"
                || leaf == "readme")
                && path.matches('/').count() <= 1
        };
        let entry = manifest
            .files
            .iter()
            .find(|e| readme_leaf_matches(e.path.as_str()));
        let entry = entry.or_else(|| {
            facts.readme_hint.as_deref().and_then(|hint| {
                manifest
                    .files
                    .iter()
                    .find(|e| e.path.as_str().eq_ignore_ascii_case(hint))
            })
        });
        entry
            .and_then(file_bytes)
            .and_then(|b| String::from_utf8(b.to_vec()).ok())
    };

    // ── LOC count ─────────────────────────────────────────────────────────────
    // Cheap honest count: newlines across every source-file section already in
    // `sections`. Capped at u32::MAX.
    let loc: u32 = {
        let total: u64 = sections
            .iter()
            .map(|s| s.bytes.iter().filter(|&&b| b == b'\n').count() as u64)
            .sum();
        total.min(u32::MAX as u64) as u32
    };

    // ── Build ExtractionInput + run rich extraction ───────────────────────────
    let (release_count, withdrawn_count, last_release_days_ago) = match listing {
        Some((total, withdrawn, _, days)) => (Some(total), Some(withdrawn), days),
        None => (None, None, None),
    };

    let input = ExtractionInput {
        name: &name,
        description: facts.description.as_deref(),
        manifest_keywords: &facts.keywords,
        manifest_categories: &facts.categories,
        readme: readme_text.as_deref(),
        identifiers,
        dependencies: &facts.dependencies,
        has_repository: facts.repository.is_some(),
        has_documentation: facts.documentation,
        has_license: facts.license.is_some() || facts.has_license_file,
        loc,
        release_count,
        withdrawn_count,
        last_release_days_ago,
    };

    let rich = rich::extract(&input, norms, synonyms, specifics);
    let mut facets = SearchFacets::from_rich(&rich);

    // Propagate the manifest description into the facet row (S2).
    facets.description = facts.description.map(smol_str::SmolStr::from);

    // Propagate repository slug, license, and release stats into facets.
    facets.repo_slug = facts
        .repository
        .as_deref()
        .and_then(crate::ecosystem::repo::normalize_repo_url)
        .map(|slug| smol_str::SmolStr::from(slug.as_str()));

    facets.license = facts
        .license
        .as_deref()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| smol_str::SmolStr::from(l.to_ascii_lowercase()));

    // `dependencies` already flows via `SearchFacets::from_rich` — no duplication.

    if let Some((total, withdrawn, this_withdrawn, _)) = listing {
        facets.release_count = Some(total);
        facets.withdrawn_count = Some(withdrawn);
        facets.withdrawn = this_withdrawn;
    }

    // verified_repo soft signal from name + repo slug.
    facets.verified_repo =
        crate::server::registry::search::gates::verified_repo(&name, facets.repo_slug.as_deref());

    // Automatic squat / land-grab heuristic (quality-gated; never flags mature pkgs).
    facets.squat_suspect = crate::server::registry::search::squat::is_squat_suspect(
        crate::server::registry::search::squat::SquatInput {
            name: &name,
            quality: facets.quality(),
            downloads: facets.downloads,
            release_count: facets.release_count,
            description: facets.description.as_deref(),
            has_repository: facets.repo_slug.is_some(),
        },
    );

    Some(facets)
}
