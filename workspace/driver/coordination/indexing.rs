//! The indexing flow: drive one dequeued job through the pipeline phases and
//! record every transition durably.
//!
//! The flow lives on [`Indexer`] — a struct that owns a handle to the assembled
//! server — rather than as free `impl Server` methods, so the pipeline's moving
//! parts (job loop, phase transitions, freshness) sit in one place with the
//! state they operate on.

#[allow(unused_imports)]
use crate::{registry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::registry::identity::PackageCoordinates;
use crate::registry::blob::creation::BlobBuilder;
use crate::registry::ingest::{
    EntryAllowlist, ExtractionLimits, ingest_archive,
};
use crate::registry::metadata::rich::{self, ExtractionInput};
use ecosystem::{self, DynSpec, LanguageExt};
use heart::Retryable;
use crate::registry::queue::LeasedJob;
use crate::registry::{RegistryError, error::ResolveError};
use futures::StreamExt;
use heart::{
    ContentHash, FailureKind, JobProgress, PackageId, Percent, Phase, ResolutionState,
};
use registry::vector::EmbeddingModel;

use crate::error::{BadRequestReason, InternalError, ServerError, ServerResult};
use crate::registry::blob::creation::PendingSection;
use crate::registry::blob::{BlobManifest, FileEntry};
use crate::registry::metadata::SearchFacets;
use crate::{Server, SourceStores};

/// The floor on the lease heartbeat interval, so a pathologically small
/// configured job_lease cannot spin the renew loop.
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
    server: Arc<Server<M>>,
    acquisition: registry::upstream::UpstreamClient,
    fractions: Mutex<HashMap<PackageId, Percent>>,
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
        }
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

        let archive_format = ecosystem::spec(record.package.coordinates.ecosystem()).archive();
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

    // TODO(driver): rewire the compile phase onto the ephemeral SmolvmCage
    // (SMOLVM-PLAN) — the long-lived compiler daemon this used to POST to was
    // deleted, so there is no `CompileRequest`/`CompileResponse` wire protocol
    // any more. The extract/emit phases around it are intact; only the actual
    // IR production is stubbed. A forge node that reaches `Phase::Compiling`
    // fails the job loudly (idempotent retry) rather than silently emitting an
    // IR-less blob.
    async fn execute_compile_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        _coordinates: &PackageCoordinates,
        _builder: &mut BlobBuilder,
    ) -> ServerResult<Vec<String>> {
        self.advance(stores, package, &progressing(Phase::Compiling))
            .await?;
        Err(ServerError::Internal(InternalError::Other {
            message: "compile phase unimplemented: the compiler daemon was removed \
                      (cage is ephemeral, SMOLVM-PLAN); rewire onto SmolvmCage"
                .to_owned(),
        }))
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
            f.squat_suspect = crate::registry::search::squat::is_squat_suspect(
                crate::registry::search::squat::SquatInput {
                    name: &coordinates.name.canonical(),
                    quality: f.quality(),
                    downloads: f.downloads,
                    release_count: f.release_count,
                    description: f.description.as_deref(),
                    has_repository: f.repo_slug.is_some(),
                },
            );
        }

        let emitted = crate::registry::blob::emit::emit(
            &stores.blobs,
            &stores.outbox,
            manifest,
            sections,
        )
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
                other => ServerError::Internal(InternalError::UpstreamFetch { reason: other.to_string() }),
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
                let escaped = ecosystem::escape_module_path(name);
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
            .get(ecosystem::Language::Python, &metadata_url)
            .await
            .map_err(|e| match e {
                UpstreamError::NotFound => ServerError::Registry(
                    ResolveError::NotFound { name: name.to_owned() }.into(),
                ),
                other => ServerError::Internal(InternalError::UpstreamFetch { reason: other.to_string() }),
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
                Err(crate::registry::QueueError::LeaseLost { .. }) => return,
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
    use crate::registry::search::listing_signals::listing_signals_from_body;
    use ecosystem::LanguageExt;

    let spec = coordinates.ecosystem().spec();
    let name = coordinates.name.canonical();
    let version = coordinates.version.canonical();

    // Prefer reusing the absolute download/listing URL when available (crates.io,
    // npm packument patterns). Relative templates alone need an origin we don't
    // always have at this call site.
    let full_url = if let Some(dl) = spec.download_source() {
        let url = dl.url.replace("{name}", &name);
        // crates.io download source *is* the listing JSON — use it for signals.
        if coordinates.ecosystem() == ecosystem::Language::Rust {
            url
        } else {
            // npm downloads API is not the packument; use known packument/JSON URLs.
            match coordinates.ecosystem() {
                ecosystem::Language::Typescript => {
                    format!("https://registry.npmjs.org/{name}")
                }
                ecosystem::Language::Python => {
                    format!("https://pypi.org/pypi/{name}/json")
                }
                _ => url,
            }
        }
    } else {
        match coordinates.ecosystem() {
            ecosystem::Language::Python => format!("https://pypi.org/pypi/{name}/json"),
            ecosystem::Language::Typescript => format!("https://registry.npmjs.org/{name}"),
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
/// [`DownloadEndpoint`](ecosystem::upstream::DownloadEndpoint) (if any) and
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
        Ok(body) => {
            match spec.parse_download_count(&body) {
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
            }
        }
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
    heuristics: Option<&crate::Heuristics>,
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
                    && let Some(bytes) = file_bytes(entry) {
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
                ecosystem::manifest::ExtractedFacts::default()
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
        let entry = manifest.files.iter().find(|e| readme_leaf_matches(e.path.as_str()));
        let entry = entry.or_else(|| {
            facts.readme_hint.as_deref().and_then(|hint| {
                manifest.files.iter().find(|e| {
                    e.path.as_str().eq_ignore_ascii_case(hint)
                })
            })
        });
        entry.and_then(file_bytes).and_then(|b| String::from_utf8(b.to_vec()).ok())
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
        .and_then(ecosystem::repo::normalize_repo_url)
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
    facets.verified_repo = crate::registry::search::gates::verified_repo(
        &name,
        facets.repo_slug.as_deref(),
    );

    // Automatic squat / land-grab heuristic (quality-gated; never flags mature pkgs).
    facets.squat_suspect = crate::registry::search::squat::is_squat_suspect(
        crate::registry::search::squat::SquatInput {
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
