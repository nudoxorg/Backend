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
    AcquisitionError, AcquisitionPolicy, EcosystemAdapter, HttpRegistryTransport,
    PackageCoordinate, REGISTRY_SOURCE_ROOT_VERSION, RegistryId, RegistrySource, RegistrySourceSet,
    admit_registry_coordinate,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod stage;

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
    /// Returns the daemon workspace root that owns registry staging.
    pub(super) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

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
    ) -> Result<stage::StagedProject, RegistryAddError> {
        stage::stage_archive(coordinate, archive, &self.workspace_root)
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
        // The owner is opened with `source.endpoint_for_owner()`, whose id is
        // salted by adapter/namespace (see `source_identity`). The transport
        // must carry the same salted id so its cursor-registry check against
        // the owner-issued `FeedRequest` agrees with the owner that reserved
        // it, rather than the source's raw, unsalted endpoint identity.
        let endpoint = source.endpoint_for_owner();
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
