//! The indexing flow: drive one dequeued job through the pipeline phases and
//! record every transition durably.
//!
//! The flow lives on [`Indexer`] — a struct that owns a handle to the assembled
//! server — rather than as free `impl Server` methods, so the pipeline's moving
//! parts (job loop, phase transitions, freshness) sit in one place with the
//! state they operate on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::registry::identity::PackageCoordinates;
use crate::registry::blob::creation::BlobBuilder;
use crate::registry::ingest::{
    ArchiveFormat, EntryAllowlist, ExtractionLimits, ingest_archive,
};
use crate::registry::metadata::rich::{self, ExtractionInput};
use heart::Retryable;
use crate::registry::queue::LeasedJob;
use crate::registry::{RegistryError, error::ResolveError};
use futures::StreamExt;
use heart::{
    ContentHash, FailureKind, JobProgress, PackageId, Percent, Phase, ResolutionState,
};
use registry::runtime::vector::EmbeddingModel;

use crate::compiler_client::CompilerClient;
use crate::error::{BadRequestReason, InternalError, ServerError, ServerResult};
use crate::registry::blob::creation::PendingSection;
use crate::registry::blob::{BlobManifest, FileEntry, FileReferences, ReferenceSet};
use crate::registry::metadata::SearchFacets;
use crate::{Server, SourceStores};

/// The floor on the lease heartbeat interval, so a pathologically small
/// configured job_lease cannot spin the renew loop.
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);

/// The indexing service: owns a handle to the assembled [`Server`] and drives
/// packages through the pipeline.
pub struct Indexer<M: EmbeddingModel> {
    server: Arc<Server<M>>,
    compiler: CompilerClient,
    acquisition: reqwest::Client,
    fractions: Mutex<HashMap<PackageId, Percent>>,
}

impl<M: EmbeddingModel> Indexer<M> {
    pub fn new(server: Arc<Server<M>>, compiler: CompilerClient) -> Self {
        Self {
            server,
            compiler,
            acquisition: reqwest::Client::new(),
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
        let mut builder = self
            .execute_extract_phase(stores, package, &coordinates)
            .await?;
        self.execute_compile_phase(stores, package, &coordinates, &mut builder)
            .await?;
        self.execute_emit_phase(stores, package, &coordinates, builder)
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

        Ok(ingest_archive(
            package,
            record.package.toolchain,
            std::io::Cursor::new(archive_bytes),
            ArchiveFormat::TarGz,
            ExtractionLimits::DEFAULT,
            EntryAllowlist::SAFE,
        )
        .await
        .map_err(RegistryError::from)?)
    }

    async fn execute_compile_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        coordinates: &PackageCoordinates,
        builder: &mut BlobBuilder,
    ) -> ServerResult<()> {
        self.advance(stores, package, &progressing(Phase::Compiling))
            .await?;

        let files: Vec<registry::protocol::FileBytes> = builder
            .source_files()
            .map(|(path, bytes)| registry::protocol::FileBytes {
                path: path.to_string(),
                bytes: bytes.to_vec(),
            })
            .collect();

        let request = registry::protocol::CompileRequest {
            coordinates: coordinates.clone(),
            toolchain: builder.toolchain().clone(),
            files,
        };

        let compile_response = self
            .compiler
            .compile(request)
            .await
            .map_err(ServerError::Compile)?;

        let (ir_bytes, references, _identifiers) = match compile_response {
            registry::protocol::CompileResponse::Ok {
                surface,
                references,
                identifiers,
            } => {
                let reference_set = convert_wire_references_to_reference_set(references)?;
                (bytes::Bytes::from(surface), reference_set, identifiers)
            }
            registry::protocol::CompileResponse::Err { kind, message } => {
                return Err(ServerError::Compile(
                    crate::compiler_client::CompilerClientError::RemoteError { kind, message },
                ));
            }
        };

        builder.set_ir(ir_bytes).map_err(RegistryError::from)?;
        builder
            .set_references(&references)
            .map_err(RegistryError::from)?;
        Ok(())
    }

    async fn execute_emit_phase(
        &self,
        stores: &SourceStores<M>,
        package: PackageId,
        coordinates: &PackageCoordinates,
        builder: BlobBuilder,
    ) -> ServerResult<ContentHash> {
        self.advance(stores, package, &progressing(Phase::Emitting))
            .await?;

        let (manifest, sections) = builder.finalize().map_err(RegistryError::from)?;
        let snapshot = ContentHash::of_bytes(&manifest.identity_bytes());

        let facets = extract_facets(
            coordinates,
            &manifest,
            &sections,
            &[],
            self.server.heuristics(),
        );

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
            && length > ExtractionLimits::DEFAULT.max_total_bytes
        {
            return Err(ServerError::BadRequest(BadRequestReason::ArchiveTooLarge {
                actual: length,
                limit: ExtractionLimits::DEFAULT.max_total_bytes,
            }));
        }

        let archive = response.bytes().await.map_err(lookup_failure)?;
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
            RegistryOrigin::Custom { url, .. } => {
                format!(
                    "{}archives/{name}/{name}-{version}.tar.gz",
                    ensure_trailing_slash(url)
                )
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

        let metadata_url = format!("https://pypi.org/pypi/{name}/{version}/json");
        let response = self
            .acquisition
            .get(&metadata_url)
            .send()
            .await
            .map_err(lookup_failure)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ServerError::Registry(
                ResolveError::NotFound {
                    name: name.to_owned(),
                }
                .into(),
            ));
        }

        let bytes = response
            .error_for_status()
            .map_err(lookup_failure)?
            .bytes()
            .await
            .map_err(lookup_failure)?;

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
        ServerError::Compile(compile) if compile.is_retryable() => FailureKind::Transient,
        ServerError::Compile(_) => FailureKind::Malformed,
        _ if error.is_retryable() => FailureKind::Transient,
        _ => FailureKind::Internal,
    }
}

fn convert_wire_references_to_reference_set(
    wire_files: Vec<registry::protocol::WireFile>,
) -> ServerResult<ReferenceSet> {
    let by_file = wire_files
        .into_iter()
        .map(|wire_file| {
            let parsed_references = wire_file
                .references
                .into_iter()
                .map(|wire_reference| {
                    wire_reference.into_reference().map_err(|_| {
                        ServerError::Internal(InternalError::Other {
                            message: "wire reference decode failed".to_owned(),
                        })
                    })
                })
                .collect::<ServerResult<Vec<_>>>()?;

            Ok(FileReferences {
                path: smol_str::SmolStr::from(wire_file.path),
                references: parsed_references,
            })
        })
        .collect::<ServerResult<Vec<_>>>()?;

    Ok(ReferenceSet { by_file })
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

fn lookup_failure(error: reqwest::Error) -> ServerError {
    ServerError::Registry(ResolveError::Lookup(error).into())
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

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CargoManifestFacts {
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub has_repository: bool,
    pub has_documentation: bool,
    pub has_license: bool,
}

pub fn parse_cargo_toml(text: &str) -> CargoManifestFacts {
    let Ok(value) = text.parse::<toml::Value>() else {
        return CargoManifestFacts::default();
    };
    let Some(package) = value.get("package").and_then(toml::Value::as_table) else {
        return CargoManifestFacts::default();
    };

    let string_field = |key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
    };
    let string_array = |key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
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
        leaf.to_ascii_lowercase().starts_with("readme") && path.matches('/').count() <= 1
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
