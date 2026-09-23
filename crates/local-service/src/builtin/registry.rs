//! Product composition for the durable registry acquisition effect.
//!
//! This module contains product composition only. [`AcquisitionService`]
//! owns the generic typed state machine, singleflight, negative facts,
//! breaker, leases, and immutable source receipt; the registry owner remains
//! the protocol adapter for feed journal phases and 64 KiB archive streaming.

use crate::process::{AdvisoryConfig, AdvisorySourceConfig, RegistryConfig};
use backend_engine::acquisition::{
    AcquisitionOutcome as TypedAcquisitionOutcome, AcquisitionRequest, AcquisitionService,
    CorruptReason, RejectReason,
};
use backend_engine::registry::{
    admit_registry_coordinate, AcquisitionError, AcquisitionPolicy, EcosystemAdapter,
    HttpRegistryTransport, PackageCoordinate, RegistryId, RegistrySource, RegistrySourceSet,
    REGISTRY_SOURCE_ROOT_VERSION,
};
use backend_library::is_hard_ignored_path;
use flate2::read::{DeflateDecoder, GzDecoder};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

/// Durable registry state attached to one local owner loop.
pub(super) struct RegistryGateway {
    sources: RegistrySourceSet,
    slots: BTreeMap<(RegistryId, bool), RegistrySlot>,
    config: RegistryConfig,
    workspace_root: PathBuf,
    source_root: PathBuf,
    shared_objects: PathBuf,
    advisory: Arc<backend_engine::advisory::AdvisoryAuthority>,
    last_receipt: Option<Arc<backend_engine::acquisition::AcquisitionReceipt>>,
    last_snapshot: Option<Arc<backend_engine::acquisition::SourceSnapshot>>,
}

struct RegistrySlot {
    service: Option<AcquisitionService>,
}

/// Typed terminal state returned while satisfying a remote package add.
#[derive(Debug)]
pub(super) enum RegistryAddError {
    /// The endpoint policy explicitly forbids network effects.
    Offline,
    /// The endpoint could not be reached within its configured deadline.
    Unavailable,
    /// The endpoint asked the caller to retry later.
    RetryAfter(Duration),
    /// The feed completed without publishing the requested coordinate.
    NotFound,
    /// The verified bytes are not a supported bounded source archive.
    UnsupportedArchive,
    /// Release exists but registry policy excludes it from a new add.
    ReleasePolicy(backend_engine::registry::ReleaseStanding),
    /// Active advisory policy excludes this exact release from a new add.
    SecurityPolicy,
    /// A typed registry acquisition or persistence failure.
    Acquisition(AcquisitionError),
}

impl fmt::Display for RegistryAddError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => formatter.write_str("registry acquisition is offline"),
            Self::Unavailable => formatter.write_str("registry is unavailable"),
            Self::RetryAfter(delay) => {
                write!(formatter, "registry requested retry after {delay:?}")
            }
            Self::NotFound => formatter.write_str("registry package was not found"),
            Self::UnsupportedArchive => {
                formatter.write_str("registry archive is unsupported or contains no source files")
            }
            Self::ReleasePolicy(standing) => {
                write!(
                    formatter,
                    "registry release is {standing:?} and cannot be newly added"
                )
            }
            Self::SecurityPolicy => {
                formatter.write_str("registry release is excluded by advisory policy")
            }
            Self::Acquisition(error) => error.fmt(formatter),
        }
    }
}

impl RegistryGateway {
    /// Projects the complete recovered local catalog without network I/O.
    pub(super) fn catalog(&mut self) -> Result<Vec<backend_engine::RegistryPackageRecord>, String> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        for source in self.sources.sources().cloned().collect::<Vec<_>>() {
            let service = self
                .service_for(&source)
                .map_err(|error| format!("open registry source: {error}"))?;
            for published in service.published_packages() {
                if !seen.insert(published.coordinate.clone()) {
                    continue;
                }
                let admitted = admit_registry_coordinate(&published.coordinate)
                    .map_err(|_| backend_engine::ProductAdmissionError::PackageReference)
                    .map_err(|error| error.to_string())?;
                records.push(backend_engine::RegistryPackageRecord {
                    ecosystem: admitted.ecosystem(),
                    coordinate: backend_engine::PackageReference::Purl(
                        published.coordinate.clone(),
                    ),
                    name: backend_engine::ProductText::new(admitted.qualified_name().as_str())
                        .map_err(|error| error.to_string())?,
                    version: backend_engine::ProductText::new(admitted.version().as_str())
                        .map_err(|error| error.to_string())?,
                    bytes: published.bytes,
                    standing: match published.facts.standing() {
                        backend_engine::registry::ReleaseStanding::Available => {
                            backend_engine::RegistryReleaseStanding::Available
                        }
                        backend_engine::registry::ReleaseStanding::Yanked => {
                            backend_engine::RegistryReleaseStanding::Yanked
                        }
                        backend_engine::registry::ReleaseStanding::Deprecated => {
                            backend_engine::RegistryReleaseStanding::Deprecated
                        }
                        backend_engine::registry::ReleaseStanding::Unlisted => {
                            backend_engine::RegistryReleaseStanding::Unlisted
                        }
                        backend_engine::registry::ReleaseStanding::Retracted => {
                            backend_engine::RegistryReleaseStanding::Retracted
                        }
                        backend_engine::registry::ReleaseStanding::Removed => {
                            backend_engine::RegistryReleaseStanding::Removed
                        }
                    },
                    downloads: match published.facts.downloads() {
                        backend_engine::registry::DownloadCount::Exact(value) => {
                            backend_engine::RegistryDownloadCount::Exact(value)
                        }
                        backend_engine::registry::DownloadCount::Approximate(value) => {
                            backend_engine::RegistryDownloadCount::Approximate(value)
                        }
                        backend_engine::registry::DownloadCount::NotReported(reason) => {
                            backend_engine::RegistryDownloadCount::Unavailable(match reason {
                                backend_engine::registry::DownloadCountGap::Unsupported => {
                                    backend_engine::RegistryFactAvailability::Unsupported
                                }
                                backend_engine::registry::DownloadCountGap::Privileged
                                | backend_engine::registry::DownloadCountGap::Unavailable => {
                                    backend_engine::RegistryFactAvailability::Unavailable
                                }
                                backend_engine::registry::DownloadCountGap::Withheld => {
                                    backend_engine::RegistryFactAvailability::NotRecorded
                                }
                            })
                        }
                    },
                    facts_version: published.facts.version(),
                    native_metadata_version: published
                        .native_metadata
                        .identity()
                        .map_err(|error| error.to_string())?,
                    native_metadata: published.native_metadata.clone(),
                    forge_sources: service
                        .forge_sources_for(&published)
                        .map_err(|error| error.to_string())?,
                    advisory: published.advisory.clone(),
                });
            }
        }
        records.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
        Ok(records)
    }

    /// Returns dependency facts from the same immutable publication records as
    /// the catalog. Unknown and unavailable metadata stay typed all the way to
    /// the product surface; an empty known set is the only representation of
    /// a package that has no declared edges.
    pub(super) fn dependency_facts(&mut self) -> Vec<backend_engine::PackageDependencySourceFacts> {
        let mut facts = Vec::new();
        let mut seen = BTreeSet::new();
        for slot in self.slots.values() {
            let Some(service) = slot.service.as_ref() else {
                continue;
            };
            for published in service.published_packages() {
                if !seen.insert(published.coordinate.clone()) {
                    continue;
                }
                if let Ok(source) = backend_engine::PackageReference::parse(
                    published.coordinate.as_str().to_owned(),
                ) {
                    facts.push((source, published.dependency_facts.clone()));
                }
            }
        }
        facts
    }

    /// Composes the source set without opening a network connection or source
    /// owner. Each owner is opened on the first catalog read or acquisition.
    pub(super) fn open(
        config: &RegistryConfig,
        root: impl AsRef<Path>,
        advisory_config: &AdvisoryConfig,
    ) -> Result<Option<Self>, AcquisitionError> {
        let workspace_root = root.as_ref().to_path_buf();
        let advisory_path = workspace_root.join("advisory-authority.json");
        let advisory = open_advisory_authority(&advisory_path, advisory_config)
            .map_err(|error| AcquisitionError::Io(std::io::Error::other(error)))?;
        // Every source, including a legacy endpoint override, is composed
        // below the versioned router root. This keeps cache migration and
        // owner identity independent of the process adapter that selected it.
        let source_root = workspace_root.join(REGISTRY_SOURCE_ROOT_VERSION);
        let shared_objects = source_root.join("cas").join("objects");
        Ok(Some(Self {
            sources: config.sources.clone(),
            slots: BTreeMap::new(),
            config: config.clone(),
            workspace_root,
            source_root,
            shared_objects,
            advisory,
            last_receipt: None,
            last_snapshot: None,
        }))
    }

    /// Fetches, verifies, and durably publishes one exact remote coordinate.
    pub(super) fn acquire(
        &mut self,
        coordinate: &PackageCoordinate,
    ) -> Result<Vec<u8>, RegistryAddError> {
        let route = self
            .sources
            .route(coordinate)
            .map_err(RegistryAddError::Acquisition)?;
        let mut last_fallback = None;
        for source in route.candidates().iter().cloned() {
            let outcome = self.acquire_from_source(&source, coordinate)?;
            match outcome {
                CandidateOutcome::Done(bytes) => return Ok(bytes),
                CandidateOutcome::Fallback(error) => last_fallback = Some(error),
                CandidateOutcome::Terminal(error) => return Err(error),
            }
        }
        Err(last_fallback.unwrap_or(RegistryAddError::Unavailable))
    }

    fn acquire_from_source(
        &mut self,
        source: &RegistrySource,
        coordinate: &PackageCoordinate,
    ) -> Result<CandidateOutcome, RegistryAddError> {
        let source_id = self.service_for(source)?.source_id();
        let slot_key = (
            source.id(),
            matches!(source.policy(), AcquisitionPolicy::Offline),
        );
        let contains = self
            .slots
            .get(&slot_key)
            .and_then(|slot| slot.service.as_ref())
            .is_some_and(|service| service.contains(coordinate));
        let request =
            AcquisitionRequest::for_coordinate(source_id, coordinate.to_string(), 1, 0)
                .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::InvalidCoordinate))?;
        let outcome = if contains || matches!(source.policy(), AcquisitionPolicy::Offline) {
            // A repeated or explicitly offline add is a cache read. Rehydrate
            // it from this source's durable catalog without a network effect.
            let service = self.service_for(source)?;
            service.ensure(&request)
        } else {
            let mut transport = self.transport(source, coordinate)?;
            let service = self.service_for(source)?;
            service.acquire(&request, &mut transport)
        };
        match self.finish_acquisition(outcome) {
            Ok(bytes) => Ok(CandidateOutcome::Done(bytes)),
            Err(error) if should_fallback(&error) => Ok(CandidateOutcome::Fallback(error)),
            Err(error) => Ok(CandidateOutcome::Terminal(error)),
        }
    }

    fn finish_acquisition(
        &mut self,
        outcome: TypedAcquisitionOutcome<
            Arc<backend_engine::acquisition::RegistryAcquisitionResult>,
        >,
    ) -> Result<Vec<u8>, RegistryAddError> {
        match outcome {
            TypedAcquisitionOutcome::Hit(result) => {
                // The local ingester consumes the immutable target root and
                // receipt alongside the bytes, so a successful Add cannot
                // discard its source snapshot evidence.
                self.last_receipt = Some(Arc::clone(&result.receipt));
                self.last_snapshot = Some(Arc::clone(&result.snapshot));
                Ok(result.artifact.bytes().to_vec())
            }
            TypedAcquisitionOutcome::NegativeFact(fact) => match fact.kind {
                backend_engine::acquisition::NegativeFactKind::Yanked => {
                    Err(RegistryAddError::ReleasePolicy(
                        backend_engine::registry::ReleaseStanding::Yanked,
                    ))
                }
                backend_engine::acquisition::NegativeFactKind::AdvisoryBlocked => {
                    Err(RegistryAddError::SecurityPolicy)
                }
                backend_engine::acquisition::NegativeFactKind::Unsupported => {
                    Err(RegistryAddError::UnsupportedArchive)
                }
                backend_engine::acquisition::NegativeFactKind::NotFound => {
                    Err(RegistryAddError::NotFound)
                }
            },
            TypedAcquisitionOutcome::RetryAt(retry) => Err(RegistryAddError::RetryAfter(
                Duration::from_millis(retry.at_millis.saturating_sub(current_millis())),
            )),
            TypedAcquisitionOutcome::CircuitOpen(open) => Err(RegistryAddError::RetryAfter(
                Duration::from_millis(open.until_millis.saturating_sub(current_millis())),
            )),
            TypedAcquisitionOutcome::Unavailable(_) => Err(RegistryAddError::Unavailable),
            TypedAcquisitionOutcome::Offline(_) => Err(RegistryAddError::Offline),
            TypedAcquisitionOutcome::Rejected(reason) => {
                let error = match reason {
                    RejectReason::Bounds => AcquisitionError::Bounds,
                    RejectReason::Policy => AcquisitionError::Transport(
                        backend_engine::registry::TransportFailure::Rejected(403),
                    ),
                    RejectReason::Protocol => AcquisitionError::Transport(
                        backend_engine::registry::TransportFailure::Protocol,
                    ),
                };
                Err(RegistryAddError::Acquisition(error))
            }
            TypedAcquisitionOutcome::Corrupt(reason) => {
                Err(RegistryAddError::Acquisition(match reason {
                    CorruptReason::Integrity => AcquisitionError::Transport(
                        backend_engine::registry::TransportFailure::Integrity,
                    ),
                    CorruptReason::Journal => AcquisitionError::CorruptJournal,
                }))
            }
            TypedAcquisitionOutcome::Cancelled => Err(RegistryAddError::Unavailable),
        }
    }

    /// Returns the last immutable receipt consumed by a local Add.
    pub(super) fn last_receipt(&self) -> Option<&backend_engine::acquisition::AcquisitionReceipt> {
        self.last_receipt.as_deref()
    }

    /// Returns the target source snapshot consumed by a local Add.
    pub(super) fn last_snapshot(&self) -> Option<&backend_engine::acquisition::SourceSnapshot> {
        self.last_snapshot.as_deref()
    }

    pub(super) fn stage_archive(
        &self,
        coordinate: &PackageCoordinate,
        archive: &[u8],
    ) -> Result<StagedProject, RegistryAddError> {
        stage_archive(coordinate, archive, &self.workspace_root)
    }

    fn service_for(
        &mut self,
        source: &RegistrySource,
    ) -> Result<&AcquisitionService, RegistryAddError> {
        let source_id = source.id();
        let slot_key = (
            source_id,
            matches!(source.policy(), AcquisitionPolicy::Offline),
        );
        let needs_open = self
            .slots
            .get(&slot_key)
            .is_none_or(|slot| slot.service.is_none());
        if needs_open {
            let endpoint = source.endpoint_for_owner();
            let (owner, _) = backend_engine::registry::RegistryOwner::open_with_shared_objects(
                &self.source_root,
                endpoint,
                source.policy(),
                self.config.limits,
                &self.shared_objects,
            )
            .map_err(RegistryAddError::Acquisition)?;
            let owner = owner
                .with_advisory_gate(self.config.advisory_gate)
                .with_advisory_resolver(Arc::clone(&self.advisory)
                    as Arc<dyn backend_engine::advisory::AdvisoryResolver>);
            let service = AcquisitionService::from_owner(
                owner,
                self.workspace_root.join("registry-acquisition"),
            )
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
            self.slots
                .entry(slot_key)
                .or_insert_with(|| RegistrySlot { service: None })
                .service = Some(service);
        }
        self.slots
            .get(&slot_key)
            .and_then(|slot| slot.service.as_ref())
            .ok_or_else(|| RegistryAddError::Acquisition(AcquisitionError::InvalidConfiguration))
    }

    fn transport(
        &self,
        source: &RegistrySource,
        coordinate: &PackageCoordinate,
    ) -> Result<HttpRegistryTransport, RegistryAddError> {
        let endpoint = source.endpoint().clone();
        let admitted =
            admit_registry_coordinate(coordinate).map_err(RegistryAddError::Acquisition)?;
        if endpoint.ecosystem() != admitted.ecosystem() {
            return Err(RegistryAddError::Acquisition(
                AcquisitionError::InvalidConfiguration,
            ));
        }
        if source.native() {
            let adapter = native_adapter(endpoint, coordinate)?;
            HttpRegistryTransport::for_native(adapter, source.authentication(), self.config.limits)
                .map_err(RegistryAddError::Acquisition)
        } else {
            HttpRegistryTransport::new(endpoint, source.authentication(), self.config.limits)
                .map_err(RegistryAddError::Acquisition)
        }
    }
}

enum CandidateOutcome {
    Done(Vec<u8>),
    Fallback(RegistryAddError),
    Terminal(RegistryAddError),
}

fn should_fallback(error: &RegistryAddError) -> bool {
    matches!(
        error,
        RegistryAddError::NotFound
            | RegistryAddError::Unavailable
            | RegistryAddError::Offline
            | RegistryAddError::RetryAfter(_)
    )
}

fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn native_adapter(
    endpoint: backend_engine::registry::RegistryEndpoint,
    coordinate: &PackageCoordinate,
) -> Result<EcosystemAdapter, RegistryAddError> {
    let admitted = admit_registry_coordinate(coordinate).map_err(RegistryAddError::Acquisition)?;
    EcosystemAdapter::new_with_version(
        endpoint,
        admitted.name().clone(),
        admitted.namespace().cloned(),
        admitted.version().clone(),
    )
    .map_err(RegistryAddError::Acquisition)
}

fn open_advisory_authority(
    path: &Path,
    config: &AdvisoryConfig,
) -> Result<Arc<backend_engine::advisory::AdvisoryAuthority>, String> {
    let mut authority =
        backend_engine::advisory::AdvisoryAuthority::open(path, config.max_age_secs)
            .map_err(|error| error.to_string())?;
    authority.set_max_age_secs(config.max_age_secs);
    authority.set_offline(config.offline);
    authority.configure_sources(config.sources.iter().map(|source| source.source));
    // Advisory refresh is intentionally not part of process composition.
    // A daemon must be able to bind and serve local/cached reads with no
    // startup network dependency; a future explicit refresh command can use
    // the existing bounded source adapter.
    authority.persist(path).map_err(|error| error.to_string())?;
    Ok(Arc::new(authority))
}

fn refresh_authority_source(
    authority: &backend_engine::advisory::AdvisoryAuthority,
    source: &AdvisorySourceConfig,
    maximum: usize,
) -> Result<backend_engine::advisory::AuthorityFeed, String> {
    let previous = authority.frontier(source.source);
    let observed_at = advisory_now();
    let (bytes, etag, last_modified, not_modified) = if source.location.starts_with("https://")
        || source.location.starts_with("http://localhost")
        || source.location.starts_with("http://127.0.0.1")
        || source.location.starts_with("http://[::1]")
    {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut request = agent
            .get(&source.location)
            .header("accept-encoding", "identity");
        if let Some(etag) = previous.and_then(|frontier| frontier.etag.as_deref()) {
            request = request.header("if-none-match", etag);
        }
        if let Some(last_modified) = previous.and_then(|frontier| frontier.last_modified.as_deref())
        {
            request = request.header("if-modified-since", last_modified);
        }
        let mut response = request.call().map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| previous.and_then(|frontier| frontier.etag.clone()));
        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .or_else(|| previous.and_then(|frontier| frontier.last_modified.clone()));
        if status == 304 {
            (Vec::new(), etag, last_modified, true)
        } else if (200..300).contains(&status) {
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > maximum {
                return Err("advisory authority body exceeds bound".to_owned());
            }
            (bytes, etag, last_modified, false)
        } else {
            return Err(format!("advisory authority returned HTTP {status}"));
        }
    } else {
        let path = Path::new(&source.location);
        if source.source == backend_engine::advisory::AdvisorySource::RustSec && path.is_dir() {
            let entries = read_rustsec_tree(path, maximum, observed_at)?;
            return Ok(backend_engine::advisory::AuthorityFeed::from_entries(
                source.source,
                entries,
                observed_at,
                previous.and_then(|frontier| frontier.etag.clone()),
                previous.and_then(|frontier| frontier.last_modified.clone()),
            ));
        }
        (
            backend_engine::advisory::read_feed(path, maximum)
                .map_err(|error| error.to_string())?,
            previous.and_then(|frontier| frontier.etag.clone()),
            previous.and_then(|frontier| frontier.last_modified.clone()),
            false,
        )
    };
    if not_modified {
        Ok(backend_engine::advisory::AuthorityFeed::not_modified(
            source.source,
            observed_at,
            etag,
            last_modified,
        ))
    } else {
        backend_engine::advisory::AuthorityFeed::parse(
            source.source,
            &bytes,
            observed_at,
            etag,
            last_modified,
        )
        .map_err(|error| error.to_string())
    }
}

fn read_rustsec_tree(
    root: &Path,
    maximum: usize,
    observed_at: u64,
) -> Result<Vec<backend_engine::advisory::Advisory>, String> {
    fn visit(
        path: &Path,
        maximum: usize,
        observed_at: u64,
        total: &mut usize,
        output: &mut Vec<backend_engine::advisory::Advisory>,
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(path).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                visit(&path, maximum, observed_at, total, output)?;
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("toml") {
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                *total = total.saturating_add(bytes.len());
                if *total > maximum {
                    return Err("RustSec authority tree exceeds bound".to_owned());
                }
                output.push(
                    backend_engine::advisory::parse_rustsec(&bytes, observed_at)
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
        }
        Ok(())
    }
    let mut total = 0;
    let mut output = Vec::new();
    visit(root, maximum, observed_at, &mut total, &mut output)?;
    Ok(output)
}

fn advisory_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A temporary, sanitized source tree used by the normal product ingester.
pub(super) struct StagedProject {
    path: PathBuf,
}

impl StagedProject {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

const MAX_EXTRACTED_FILES: usize = 100_000;
// Preserve real declaration maps and generated sources during staging; the
// compiler applies its own per-source limits after the archive is confined.
const MAX_EXTRACTED_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXTRACTED_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXPANSION_RATIO: usize = 200;
const EXPANSION_SLACK_BYTES: usize = 1024 * 1024;
const TAR_BLOCK_BYTES: usize = 512;
static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

/// Materializes a verified archive beneath a private workspace staging root.
///
/// Only regular files with normalized relative paths are written. Tar/gzip and
/// zip/deflate are accepted because those are the formats emitted by the
/// supported ecosystem registries. Every other byte shape reaches the typed
/// unsupported terminal instead of being published as a label-only package.
pub(super) fn stage_archive(
    _coordinate: &PackageCoordinate,
    archive: &[u8],
    workspace_root: impl AsRef<Path>,
) -> Result<StagedProject, RegistryAddError> {
    if archive.is_empty() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let staging_root = workspace_root.as_ref().join("registry-staging");
    fs::create_dir_all(&staging_root)
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    let digest = blake3::hash(archive);
    let directory = staging_root.join(hex(digest.as_bytes()));
    if directory.exists() {
        return Ok(StagedProject { path: directory });
    }
    let temporary = staging_root.join(format!(
        ".{}.{}.{}.tmp",
        hex(digest.as_bytes()),
        std::process::id(),
        STAGING_NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&temporary)
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    let mut writer = StageWriter::new(temporary.clone());
    let result = extract_archive(archive, &mut writer);
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result?;
    if writer.source_files == 0 {
        let _ = fs::remove_dir_all(&temporary);
        return Err(RegistryAddError::UnsupportedArchive);
    }
    backend_platform::durability::open_directory(&temporary)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    if let Err(error) = fs::rename(&temporary, &directory) {
        if directory.is_dir() {
            let _ = fs::remove_dir_all(&temporary);
        } else {
            let _ = fs::remove_dir_all(&temporary);
            return Err(RegistryAddError::Acquisition(AcquisitionError::Io(error)));
        }
    }
    backend_platform::durability::open_directory(&staging_root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    Ok(StagedProject { path: directory })
}

struct StageWriter {
    root: PathBuf,
    paths: BTreeSet<String>,
    files: usize,
    source_files: usize,
    bytes: usize,
}

impl StageWriter {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            paths: BTreeSet::new(),
            files: 0,
            source_files: 0,
            bytes: 0,
        }
    }

    fn directory(raw: &str) -> Result<(), RegistryAddError> {
        let _ = confined_path(raw)?;
        Ok(())
    }

    fn file(&mut self, raw: &str, bytes: &[u8]) -> Result<(), RegistryAddError> {
        let relative = confined_path(raw)?;
        // Registry archives frequently carry dependency trees and framework
        // output.  Apply the same hard product-owned directory policy as
        // local source discovery before accounting or materialising bytes.
        // This keeps archive staging and checkout discovery on one boundary;
        // a later source scan still applies its smaller per-source limit.
        if is_hard_ignored_path(&relative) {
            return Ok(());
        }
        if bytes.len() > MAX_EXTRACTED_FILE_BYTES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        self.files = self
            .files
            .checked_add(1)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if self.files > MAX_EXTRACTED_FILES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if self.bytes > MAX_EXTRACTED_TOTAL_BYTES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        let key = relative.to_string_lossy().into_owned();
        if !self.paths.insert(key.clone()) {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let path = self.root.join(&relative);
        let parent = path.parent().ok_or(RegistryAddError::UnsupportedArchive)?;
        create_confined_directories(&self.root, parent)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        file.write_all(bytes)
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        file.sync_all()
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        if supported_source_name(&key) {
            self.source_files = self.source_files.saturating_add(1);
        }
        Ok(())
    }
}

fn extract_archive(archive: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    if archive.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(Cursor::new(archive));
        let decompressed = read_bounded(&mut decoder, MAX_EXTRACTED_TOTAL_BYTES)?;
        admit_expansion(archive.len(), decompressed.len())?;
        return extract_tar(&decompressed, writer);
    }
    if archive.starts_with(b"PK\x03\x04") || archive.starts_with(b"PK\x05\x06") {
        return extract_zip(archive, writer);
    }
    extract_tar(archive, writer)
}

fn extract_tar(bytes: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    let mut offset = 0usize;
    let mut terminated = false;
    let mut pending_path = None;
    let mut global_path = None;
    while offset
        .checked_add(TAR_BLOCK_BYTES)
        .is_some_and(|end| end <= bytes.len())
    {
        let header = &bytes[offset..offset + TAR_BLOCK_BYTES];
        if header.iter().all(|byte| *byte == 0) {
            terminated = true;
            break;
        }
        validate_tar_checksum(header)?;
        let name = tar_name(header)?;
        let size = tar_octal(&header[124..136])?;
        let data_start = offset
            .checked_add(TAR_BLOCK_BYTES)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let data_end = data_start
            .checked_add(size)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if data_end > bytes.len() {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let mut path = global_path
            .clone()
            .or(pending_path.take())
            .unwrap_or_else(|| name.clone());
        match header[156] {
            0 | b'0' => writer.file(&path, &bytes[data_start..data_end])?,
            b'5' => StageWriter::directory(&path)?,
            b'x' => pending_path = pax_path(&bytes[data_start..data_end])?,
            b'g' => global_path = pax_path(&bytes[data_start..data_end])?,
            b'L' => {
                let end = bytes[data_start..data_end]
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(data_end - data_start);
                path = std::str::from_utf8(&bytes[data_start..data_start + end])
                    .map_err(|_| RegistryAddError::UnsupportedArchive)?
                    .to_owned();
                pending_path = Some(path);
            }
            // Links and special nodes are never materialized.
            _ => return Err(RegistryAddError::UnsupportedArchive),
        }
        let padded = size
            .checked_add(TAR_BLOCK_BYTES - 1)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?
            / TAR_BLOCK_BYTES
            * TAR_BLOCK_BYTES;
        offset = data_start
            .checked_add(padded)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    }
    if !terminated {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(())
}

fn pax_path(bytes: &[u8]) -> Result<Option<String>, RegistryAddError> {
    let mut offset = 0usize;
    let mut path = None;
    while offset < bytes.len() {
        let Some(space) = bytes[offset..].iter().position(|byte| *byte == b' ') else {
            return Err(RegistryAddError::UnsupportedArchive);
        };
        let length = std::str::from_utf8(&bytes[offset..offset + space])
            .map_err(|_| RegistryAddError::UnsupportedArchive)?
            .parse::<usize>()
            .map_err(|_| RegistryAddError::UnsupportedArchive)?;
        let end = offset
            .checked_add(length)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if length == 0 || end > bytes.len() || bytes[end - 1] != b'\n' {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let record = &bytes[offset + space + 1..end - 1];
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(
                std::str::from_utf8(value)
                    .map_err(|_| RegistryAddError::UnsupportedArchive)?
                    .to_owned(),
            );
        }
        offset = end;
    }
    Ok(path)
}

fn tar_name(header: &[u8]) -> Result<String, RegistryAddError> {
    let name = trim_nul(&header[..100]);
    let prefix = trim_nul(&header[345..500]);
    let name = if prefix.is_empty() {
        name.to_owned()
    } else if name.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}/{name}")
    };
    (!name.is_empty())
        .then_some(name)
        .ok_or(RegistryAddError::UnsupportedArchive)
}

fn tar_octal(field: &[u8]) -> Result<usize, RegistryAddError> {
    let value = trim_nul(field).trim();
    if value.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(value, 8).map_err(|_| RegistryAddError::UnsupportedArchive)
}

fn validate_tar_checksum(header: &[u8]) -> Result<(), RegistryAddError> {
    let expected = tar_octal(&header[148..156])?;
    let measured = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                usize::from(b' ')
            } else {
                usize::from(*byte)
            }
        })
        .sum::<usize>();
    (expected == measured)
        .then_some(())
        .ok_or(RegistryAddError::UnsupportedArchive)
}

fn admit_expansion(compressed: usize, expanded: usize) -> Result<(), RegistryAddError> {
    let maximum = compressed
        .checked_mul(MAX_EXPANSION_RATIO)
        .and_then(|value| value.checked_add(EXPANSION_SLACK_BYTES))
        .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    if expanded > maximum {
        Err(RegistryAddError::Acquisition(AcquisitionError::Bounds))
    } else {
        Ok(())
    }
}

fn trim_nul(bytes: &[u8]) -> &str {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap_or("")
}

fn extract_zip(bytes: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    // Go module proxies emit ZIP local headers with a data descriptor. Read
    // the authenticated central directory first so those entries retain
    // bounded sizes and checksums without trusting the local zero fields.
    let central = match zip_central_entries(bytes) {
        Ok(entries) => entries,
        Err(RegistryAddError::UnsupportedArchive) => BTreeMap::new(),
        Err(error) => return Err(error),
    };
    let mut offset = 0usize;
    let mut entries = 0usize;
    while offset.checked_add(4).is_some_and(|end| end <= bytes.len()) {
        let signature = le_u32(bytes, offset)?;
        if signature == 0x0201_4b50 || signature == 0x0605_4b50 {
            break;
        }
        if signature != 0x0403_4b50 {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        if offset.checked_add(30).is_none_or(|end| end > bytes.len()) {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let flags = le_u16(bytes, offset + 6)?;
        let method = le_u16(bytes, offset + 8)?;
        let local_crc = le_u32(bytes, offset + 14)?;
        let local_compressed = usize::try_from(le_u32(bytes, offset + 18)?)
            .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let local_declared = usize::try_from(le_u32(bytes, offset + 22)?)
            .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let (expected_crc, compressed, declared) = if flags & 0x0008 != 0 {
            let entry = central
                .get(&offset)
                .ok_or(RegistryAddError::UnsupportedArchive)?;
            (entry.crc, entry.compressed, entry.declared)
        } else {
            (local_crc, local_compressed, local_declared)
        };
        admit_expansion(compressed, declared)?;
        let name_len = usize::from(le_u16(bytes, offset + 26)?);
        let extra_len = usize::from(le_u16(bytes, offset + 28)?);
        let name_start = offset + 30;
        let data_start = name_start
            .checked_add(name_len)
            .and_then(|value| value.checked_add(extra_len))
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let data_end = data_start
            .checked_add(compressed)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if data_end > bytes.len() || flags & 0x0001 != 0 {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let name = std::str::from_utf8(
            bytes
                .get(name_start..name_start.saturating_add(name_len))
                .ok_or(RegistryAddError::UnsupportedArchive)?,
        )
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
        if name.ends_with('/') {
            StageWriter::directory(name)?;
        } else {
            if declared > MAX_EXTRACTED_FILE_BYTES {
                return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
            }
            let content = match method {
                0 => bytes[data_start..data_end].to_vec(),
                8 => {
                    let mut decoder =
                        DeflateDecoder::new(Cursor::new(&bytes[data_start..data_end]));
                    read_bounded(&mut decoder, MAX_EXTRACTED_FILE_BYTES)?
                }
                _ => return Err(RegistryAddError::UnsupportedArchive),
            };
            if content.len() != declared {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            if crc32(&content) != expected_crc {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            writer.file(name, &content)?;
            entries = entries.saturating_add(1);
        }
        offset = data_end;
        if flags & 0x0008 != 0 {
            let has_signature = le_u32(bytes, offset)? == 0x0807_4b50;
            let descriptor = offset
                .checked_add(if has_signature { 16 } else { 12 })
                .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
            if descriptor > bytes.len() {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            let start = offset + usize::from(has_signature) * 4;
            let tuple = (
                le_u32(bytes, start)?,
                usize::try_from(le_u32(bytes, start + 4)?)
                    .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?,
                usize::try_from(le_u32(bytes, start + 8)?)
                    .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?,
            );
            if tuple != (expected_crc, compressed, declared) {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            offset = descriptor;
        }
    }
    (entries > 0)
        .then_some(())
        .ok_or(RegistryAddError::UnsupportedArchive)
}

struct ZipCentralEntry {
    crc: u32,
    compressed: usize,
    declared: usize,
}

fn zip_central_entries(bytes: &[u8]) -> Result<BTreeMap<usize, ZipCentralEntry>, RegistryAddError> {
    let eocd = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    let size = usize::try_from(le_u32(bytes, eocd + 12)?)
        .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    let start = usize::try_from(le_u32(bytes, eocd + 16)?)
        .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    let end = start
        .checked_add(size)
        .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    if end > bytes.len() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let mut entries = BTreeMap::new();
    let mut offset = start;
    while offset < end {
        if le_u32(bytes, offset)? != 0x0201_4b50 || offset + 46 > end {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let name_len = usize::from(le_u16(bytes, offset + 28)?);
        let extra_len = usize::from(le_u16(bytes, offset + 30)?);
        let comment_len = usize::from(le_u16(bytes, offset + 32)?);
        let next = offset
            .checked_add(46)
            .and_then(|value| value.checked_add(name_len))
            .and_then(|value| value.checked_add(extra_len))
            .and_then(|value| value.checked_add(comment_len))
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if next > end || le_u16(bytes, offset + 8)? & 0x0001 != 0 {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let local = usize::try_from(le_u32(bytes, offset + 42)?)
            .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        entries.insert(
            local,
            ZipCentralEntry {
                crc: le_u32(bytes, offset + 16)?,
                compressed: usize::try_from(le_u32(bytes, offset + 20)?)
                    .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?,
                declared: usize::try_from(le_u32(bytes, offset + 24)?)
                    .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?,
            },
        );
        offset = next;
    }
    (offset == end)
        .then_some(entries)
        .ok_or(RegistryAddError::UnsupportedArchive)
}

fn read_bounded(reader: &mut impl Read, maximum: usize) -> Result<Vec<u8>, RegistryAddError> {
    let capacity = maximum.saturating_add(1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve(capacity)
        .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    reader
        .take(u64::try_from(capacity).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    if bytes.len() > maximum {
        return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
    }
    Ok(bytes)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16, RegistryAddError> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, RegistryAddError> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn confined_path(raw: &str) -> Result<PathBuf, RegistryAddError> {
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.contains(['\\', '\0'])
        || (raw.len() >= 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes()[1] == b':')
    {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let mut path = PathBuf::new();
    for component in raw.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(RegistryAddError::UnsupportedArchive),
            value => path.push(value),
        }
    }
    if path.as_os_str().is_empty() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(path)
}

fn create_confined_directories(root: &Path, parent: &Path) -> Result<(), RegistryAddError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(RegistryAddError::UnsupportedArchive);
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
            }
            Err(_) => return Err(RegistryAddError::UnsupportedArchive),
        }
    }
    Ok(())
}

fn supported_source_name(path: &str) -> bool {
    let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "rs" | "py"
            | "pyw"
            | "pyi"
            | "ts"
            | "tsx"
            | "mts"
            | "cts"
            | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "go"
            | "java"
            | "cs"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "c++"
            | "hh"
            | "hpp"
            | "hxx"
            | "h++"
            | "ipp"
            | "m"
            | "mm"
    )
}

fn hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in value {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-registry-stage-{}-{id}",
            std::process::id()
        ))
    }

    fn tar_file(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut header = [0_u8; TAR_BLOCK_BYTES];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}\0", bytes.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[148..156].fill(b' ');
        let checksum: usize = header.iter().map(|byte| usize::from(*byte)).sum();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        let mut archive = header.to_vec();
        archive.extend_from_slice(bytes);
        archive.resize(archive.len().div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES, 0);
        archive.resize(archive.len() + TAR_BLOCK_BYTES * 2, 0);
        archive
    }

    fn stored_zip_file(name: &str, bytes: &[u8], checksum: u32) -> Vec<u8> {
        let mut archive = Vec::new();
        archive.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&20_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&checksum.to_le_bytes());
        archive.extend_from_slice(&u32::try_from(bytes.len()).expect("zip size").to_le_bytes());
        archive.extend_from_slice(&u32::try_from(bytes.len()).expect("zip size").to_le_bytes());
        archive.extend_from_slice(&u16::try_from(name.len()).expect("zip name").to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(name.as_bytes());
        archive.extend_from_slice(bytes);
        archive
    }

    #[test]
    fn tar_archive_materializes_once_and_reuses_the_immutable_tree() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let archive = tar_file("package/src/lib.rs", b"pub fn from_registry() {}");
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage archive");
        let source = fs::read(staged.path().join("package/src/lib.rs")).expect("read source");
        assert_eq!(source, b"pub fn from_registry() {}");
        let path = staged.path().to_path_buf();
        drop(staged);
        assert!(path.exists());
        let reused = stage_archive(&coordinate, &archive, &root).expect("reuse archive");
        assert_eq!(reused.path(), path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_path_traversal_is_rejected_before_writing_outside_the_jail() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let archive = tar_file("../outside.rs", b"must not escape");
        assert!(matches!(
            stage_archive(&coordinate, &archive, &root),
            Err(RegistryAddError::UnsupportedArchive)
        ));
        assert!(!root.parent().unwrap_or(&root).join("outside.rs").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zip_entries_require_their_declared_crc() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:npm/demo@1.0.0").expect("coordinate");
        let source = b"export const verified = true;";
        let valid = stored_zip_file("package/src/index.mts", source, crc32(source));
        let staged = stage_archive(&coordinate, &valid, &root).expect("valid zip");
        assert_eq!(
            fs::read(staged.path().join("package/src/index.mts")).expect("source"),
            source
        );

        let corrupt_coordinate =
            PackageCoordinate::parse("pkg:npm/corrupt@1.0.0").expect("coordinate");
        let corrupt = stored_zip_file("package/src/index.ts", source, crc32(source) ^ 1);
        assert!(matches!(
            stage_archive(&corrupt_coordinate, &corrupt, &root),
            Err(RegistryAddError::UnsupportedArchive)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_hard_generated_paths_are_ignored_before_accounting() {
        let root = scratch();
        fs::create_dir_all(&root).expect("stage root");
        let mut writer = StageWriter::new(root.clone());
        writer
            .file("package/node_modules/dependency/index.js", b"generated")
            .expect("generated archive entries are ignored");
        writer
            .file("package/src/index.js", b"export const source = true;")
            .expect("source archive entry");
        assert_eq!(writer.files, 1);
        assert_eq!(writer.source_files, 1);
        assert!(!root.join("package/node_modules").exists());
        assert!(root.join("package/src/index.js").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gzip_expansion_ratio_is_bounded_before_staging() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/bomb@1.0.0").expect("coordinate");
        let tar = tar_file("package/src/large.rs", &vec![0_u8; 2 * 1024 * 1024]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&tar).expect("compress archive");
        let archive = encoder.finish().expect("finish archive");
        assert!(
            archive.len() < tar.len() / 200,
            "fixture must exercise the ratio cap"
        );
        assert!(matches!(
            stage_archive(&coordinate, &archive, &root),
            Err(RegistryAddError::Acquisition(AcquisitionError::Bounds))
        ));
        assert_eq!(
            fs::read_dir(root.join("registry-staging"))
                .expect("staging root")
                .count(),
            0,
            "rejected archive left a temporary staging directory"
        );
        let _ = fs::remove_dir_all(root);
    }
}
