//! Product composition for the durable registry acquisition effect.
//!
//! This module contains product composition only. [`AcquisitionService`]
//! owns the generic typed state machine, singleflight, negative facts,
//! breaker, leases, and immutable source receipt; the registry owner remains
//! the protocol adapter for feed journal phases and 64 KiB archive streaming.

use crate::process::{AdvisoryConfig, AdvisorySourceConfig, RegistryConfig};
use backend_engine::acquisition::{
    AcquisitionOutcome as TypedAcquisitionOutcome, AcquisitionRequest, AcquisitionService,
    CorruptReason, NegativeFactKind, RejectReason, ReleaseClaim, SourceSnapshot,
};
use backend_engine::registry::{
    AcquisitionError, AcquisitionPolicy, EcosystemAdapter, HttpRegistryTransport,
    PackageCoordinate, REGISTRY_SOURCE_ROOT_VERSION, RegistryId, RegistrySource, RegistrySourceSet,
    admit_registry_coordinate,
};
use backend_library::is_hard_ignored_path;
use backend_platform::directory::{DirectoryCapability, DirectoryEntry, EntryKind};
use flate2::read::{DeflateDecoder, GzDecoder};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

/// Local policy horizon for how long a source observation is projected as
/// fresh. It is not a guarantee that the registry has not changed; a negative
/// source fact may impose a shorter expiry.
const PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS: u64 = 60_000;
/// Bound process-local freshness evidence so long-lived owners do not retain
/// an observation for every package version they have ever fetched.
const MAX_FRESH_PACKAGE_FACT_OBSERVATIONS: usize = 4_096;
const MAX_OSV_ZIP_MEMBERS: usize = 1_001_024;
const MAX_OSV_ZIP_COMPRESSED_BYTES: usize = 4 * 1024 * 1024 * 1024;
const MAX_OSV_ZIP_EXPANDED_BYTES: usize = 64 * 1024 * 1024 * 1024;
const MAX_OSV_ZIP_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RUSTSEC_TREE_ENTRIES: usize = 1_000_000;
const MAX_RUSTSEC_TREE_DEPTH: usize = 64;

/// Durable registry state attached to one local owner loop.
pub(super) struct RegistryGateway {
    sources: RegistrySourceSet,
    slots: BTreeMap<(RegistryId, bool), RegistrySlot>,
    config: RegistryConfig,
    workspace_root: PathBuf,
    source_root: PathBuf,
    shared_objects: PathBuf,
    advisory: Arc<backend_engine::advisory::AdvisoryAuthority>,
    advisory_path: PathBuf,
    advisory_config: AdvisoryConfig,
    fresh_package_facts: FreshPackageFactMap,
    observation_generation: u64,
    projection: Option<(CatalogProjectionKey, Arc<CatalogProjection>)>,
}

/// Resident catalog rows, name index, and registry dependency facts.
///
/// Rebuilt when a source facts root, mutable-fact freshness state, or advisory
/// feed generation changes. Surface commands borrow this projection instead
/// of re-admitting every published package.
pub(super) struct CatalogProjection {
    pub(super) records: Vec<backend_engine::RegistryPackageRecord>,
    pub(super) index: Arc<super::product_state::CatalogLookupIndex>,
    /// Digest of the currently selected row overlays. This can change when
    /// freshness or mutable facts change while the reusable Tantivy index does
    /// not.
    pub(super) selected_rows_snapshot: [u8; 32],
    /// Authority generation that produced the current advisory row overlays.
    pub(super) advisory_generation: [u8; 32],
    pub(super) dependency_facts: Vec<backend_engine::PackageDependencySourceFacts>,
}

/// Exact registry row identity paired with its current advisory authority DTO.
/// The coordinate includes the package version; source identity keeps the
/// release standing facts separate when multiple registries publish it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AdvisoryOverlayKey {
    source: [u8; 32],
    coordinate: PackageCoordinate,
}

/// Current advisory results projected over immutable publication receipts.
#[derive(Clone, Debug, Eq, PartialEq)]
struct VersionedAdvisoryOverlay {
    generation: [u8; 32],
    entries: BTreeMap<AdvisoryOverlayKey, backend_engine::advisory::AdvisoryPackageDto>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogSourceProjectionKey {
    source: [u8; 32],
    facts_frontier: [u8; 32],
    observation_state: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogProjectionKey {
    sources: Vec<CatalogSourceProjectionKey>,
    advisory_generation: [u8; 32],
}

/// Completeness of one mutable package fact group, kept separate from the
/// freshness of the source observation that selected it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageFactCompleteness {
    Complete,
    Partial,
    NotRecorded,
    Unsupported,
    Unavailable,
    Unknown,
}

/// Proof that this process observed the selected mutable facts, or an explicit
/// historical marker after a cold start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageFactFreshness {
    Observed {
        at_millis: u64,
        valid_until_millis: u64,
        proof: PackageFactObservationProof,
    },
    Historical,
}

/// The source evidence that established the selected package facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageFactObservationProof {
    AcquisitionReceipt {
        receipt: [u8; 32],
        snapshot: [u8; 32],
    },
    SourceNegativeFact {
        authority: [u8; 32],
        source_proof: [u8; 32],
        cursor: [u8; 32],
        observed_at_millis: u64,
        expires_at_millis: u64,
        policy_epoch: u64,
        kind: NegativeFactKind,
    },
}

/// Versioned source authority for one selected release row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PackageFactAuthority {
    pub(super) source: [u8; 32],
    pub(super) source_facts_root: [u8; 32],
    pub(super) source_provenance: [u8; 32],
    pub(super) facts_version: [u8; 32],
    pub(super) advisory_facts_version: [u8; 32],
    pub(super) selection_version: [u8; 32],
    pub(super) standing: PackageFactCompleteness,
    pub(super) downloads: PackageFactCompleteness,
    pub(super) advisories: PackageFactCompleteness,
    /// Freshness of the selected release facts (standing and download count).
    pub(super) release_facts_freshness: PackageFactFreshness,
    /// Advisory-feed freshness remains independent from registry release facts.
    pub(super) advisory_freshness: backend_engine::advisory::FreshnessState,
}

impl PackageFactAuthority {
    fn to_surface(self) -> backend_engine::RegistryPackageFactAuthority {
        use backend_engine::{
            RegistryNegativeFactKind, RegistryPackageFactCompleteness as Completeness,
            RegistryPackageFactFreshness as Freshness, RegistryPackageFactProof as Proof,
        };

        let completeness = |value| match value {
            PackageFactCompleteness::Complete => Completeness::Complete,
            PackageFactCompleteness::Partial => Completeness::Partial,
            PackageFactCompleteness::NotRecorded => Completeness::NotRecorded,
            PackageFactCompleteness::Unsupported => Completeness::Unsupported,
            PackageFactCompleteness::Unavailable => Completeness::Unavailable,
            PackageFactCompleteness::Unknown => Completeness::Unknown,
        };
        let release_facts_freshness = match self.release_facts_freshness {
            PackageFactFreshness::Historical => Freshness::Historical,
            PackageFactFreshness::Observed {
                at_millis,
                valid_until_millis,
                proof,
            } => {
                let proof = match proof {
                    PackageFactObservationProof::AcquisitionReceipt { receipt, snapshot } => {
                        Proof::AcquisitionReceipt { receipt, snapshot }
                    }
                    PackageFactObservationProof::SourceNegativeFact {
                        authority,
                        source_proof,
                        cursor,
                        observed_at_millis,
                        expires_at_millis,
                        policy_epoch,
                        kind,
                    } => Proof::SourceNegativeFact {
                        authority,
                        source_proof,
                        cursor,
                        observed_at_millis,
                        expires_at_millis,
                        policy_epoch,
                        fact: match kind {
                            NegativeFactKind::NotFound => RegistryNegativeFactKind::NotFound,
                            NegativeFactKind::Yanked => RegistryNegativeFactKind::Yanked,
                            NegativeFactKind::AdvisoryBlocked => {
                                RegistryNegativeFactKind::AdvisoryBlocked
                            }
                            NegativeFactKind::Unsupported => RegistryNegativeFactKind::Unsupported,
                        },
                    },
                };
                Freshness::Current {
                    observed_at_millis: at_millis,
                    valid_until_millis,
                    proof,
                }
            }
        };
        backend_engine::RegistryPackageFactAuthority {
            source: self.source,
            source_facts_root: self.source_facts_root,
            source_provenance: self.source_provenance,
            facts_version: self.facts_version,
            advisory_facts_version: self.advisory_facts_version,
            selection_version: self.selection_version,
            standing: completeness(self.standing),
            downloads: completeness(self.downloads),
            advisories: completeness(self.advisories),
            release_facts_freshness,
            advisory_freshness: self.advisory_freshness,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FreshPackageFactsObservation {
    facts_version: [u8; 32],
    source_provenance: [u8; 32],
    at_millis: u64,
    proof: PackageFactObservationProof,
}

type FreshPackageFactMap = BTreeMap<([u8; 32], PackageCoordinate), FreshPackageFactsObservation>;

impl FreshPackageFactsObservation {
    fn valid_until_millis(self) -> u64 {
        let max_age = self
            .at_millis
            .saturating_add(PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS);
        match self.proof {
            PackageFactObservationProof::AcquisitionReceipt { .. } => max_age,
            PackageFactObservationProof::SourceNegativeFact {
                expires_at_millis, ..
            } => max_age.min(expires_at_millis),
        }
    }

    fn is_current_at(self, now_millis: u64) -> bool {
        self.valid_until_millis() > now_millis
    }
}

fn prune_expired_package_fact_observations(
    observations: &mut FreshPackageFactMap,
    now_millis: u64,
) -> usize {
    let previous_len = observations.len();
    observations.retain(|_, observation| observation.is_current_at(now_millis));
    previous_len - observations.len()
}

fn remember_package_fact_observation(
    observations: &mut FreshPackageFactMap,
    key: ([u8; 32], PackageCoordinate),
    observation: FreshPackageFactsObservation,
    now_millis: u64,
) -> bool {
    let changed = prune_expired_package_fact_observations(observations, now_millis) > 0;
    if !observation.is_current_at(now_millis) {
        return changed;
    }
    if observations.get(&key) == Some(&observation) {
        return changed;
    }
    while !observations.contains_key(&key)
        && observations.len() >= MAX_FRESH_PACKAGE_FACT_OBSERVATIONS
    {
        let oldest = observations
            .iter()
            .min_by_key(|(_, observation)| observation.valid_until_millis())
            .map(|(key, _)| key.clone());
        let Some(oldest) = oldest else {
            break;
        };
        observations.remove(&oldest);
    }
    observations.insert(key, observation);
    true
}

fn package_fact_observation_state(
    observations: &FreshPackageFactMap,
    generation: u64,
    now_millis: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.package-fact-observation-state.v1\0");
    hasher.update(&generation.to_le_bytes());
    for ((source, coordinate), observation) in observations {
        hasher.update(source);
        hasher.update(coordinate.as_str().as_bytes());
        hasher.update(&observation.facts_version);
        hasher.update(&observation.source_provenance);
        hasher.update(&observation.at_millis.to_be_bytes());
        hasher.update(&[u8::from(observation.is_current_at(now_millis))]);
    }
    *hasher.finalize().as_bytes()
}

struct RegistrySlot {
    service: Option<AcquisitionService>,
}

/// Typed terminal state returned while satisfying a remote package add.
#[derive(Debug)]
pub(super) enum RegistryAddError {
    /// The owner observed cancellation between bounded acquisition stages.
    Cancelled,
    /// A registry acquisition worker panicked before it could return a receipt.
    WorkerPanicked,
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
            Self::Cancelled => formatter.write_str("registry acquisition was cancelled"),
            Self::WorkerPanicked => {
                formatter.write_str("registry acquisition worker ended unexpectedly")
            }
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

    /// The advisory authority every read observes.
    pub(super) fn advisory(&self) -> &backend_engine::advisory::AdvisoryAuthority {
        &self.advisory
    }

    /// Refreshes every configured advisory source, persists the result, and
    /// makes it the authority for every later read and acquisition.
    ///
    /// A source that fails keeps its last good body and is marked
    /// unavailable, so coverage says "unavailable" rather than "clean".
    pub(super) fn refresh_advisories(
        &mut self,
    ) -> Result<Vec<backend_library::browse::AdvisorySourceState>, String> {
        let mut authority = (*self.advisory).clone();
        let mut states = Vec::new();
        let osv_snapshot_root =
            backend_engine::advisory::AdvisoryAuthority::osv_snapshot_root(&self.advisory_path);
        for source in &self.advisory_config.sources {
            let error = match refresh_authority_source(
                &authority,
                source,
                self.advisory_config.osv_scope,
                self.advisory_config.max_feed_bytes,
                &osv_snapshot_root,
            ) {
                Ok(feed) => authority
                    .apply(feed)
                    .err()
                    .map(|error| bounded_advisory_error(error.to_string())),
                Err(error) => Some(bounded_advisory_error(error)),
            };
            if error.is_some() {
                authority.mark_unavailable(source.source, advisory_now());
            }
            let frontier = authority.frontier(source.source);
            states.push(backend_library::browse::AdvisorySourceState {
                source: format!("{:?}", source.source).to_ascii_lowercase(),
                scope: (source.source == backend_engine::advisory::AdvisorySource::Osv).then(
                    || {
                        self.advisory_config.osv_scope.map_or_else(
                            || "unspecified".to_owned(),
                            |scope| scope.label().to_owned(),
                        )
                    },
                ),
                complete: frontier.is_some_and(|frontier| frontier.complete),
                advisories: frontier.map_or(0, |frontier| frontier.entries),
                observed_at: frontier.map_or(0, |frontier| frontier.observed_at),
                expires_at: frontier.and_then(|frontier| frontier.expires_at),
                error,
            });
        }
        if let Err(error) = authority.persist(&self.advisory_path) {
            if matches!(
                &error,
                backend_engine::advisory::AuthorityStorageError::CommittedButNotDurable(_)
            ) {
                // The authority pathname now refers to this candidate, but a
                // failed parent flush means cold recovery must decide whether
                // the old or new journal survived. Keep both snapshot leases
                // alive in memory, and prevent another refresh from collecting
                // the maybe-selected generation before that recovery.
                self.advisory = Arc::new(authority);
                self.slots.clear();
                self.projection = None;
            }
            return Err(error.to_string());
        }
        self.advisory = Arc::new(authority);
        // Open owners hold the previous authority as their resolver; they
        // reopen lazily with the new one.
        self.slots.clear();
        // Keep old callers' Arcs valid, but make the next catalog read project
        // the newly committed feed generation immediately.
        self.projection = None;
        Ok(states)
    }

    /// Projects the complete recovered local catalog without network I/O.
    pub(super) fn catalog(&mut self) -> Result<Vec<backend_engine::RegistryPackageRecord>, String> {
        Ok(self.catalog_projection()?.records.clone())
    }

    fn project_catalog_records(
        &mut self,
        generation: [u8; 32],
    ) -> Result<
        (
            Vec<backend_engine::RegistryPackageRecord>,
            VersionedAdvisoryOverlay,
        ),
        String,
    > {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        let mut advisory_overlay = VersionedAdvisoryOverlay {
            generation,
            entries: BTreeMap::new(),
        };
        for source in self.sources.sources().cloned().collect::<Vec<_>>() {
            let (source_id, source_facts_root, publications) = {
                let service = self
                    .service_for(&source)
                    .map_err(|error| format!("open registry source: {error}"))?;
                let publications = service
                    .published_packages()
                    .into_iter()
                    .map(|published| {
                        let forge_sources = service
                            .forge_sources_for(&published)
                            .map_err(|error| error.to_string())?;
                        let advisory = service.advisory_for_published(&published);
                        Ok((published, forge_sources, advisory))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                (service.source_id(), service.facts_frontier(), publications)
            };
            for (published, forge_sources, advisory) in publications {
                if !remember_catalog_publication(&mut seen, source_id, &published.coordinate) {
                    continue;
                }
                let advisory_key = AdvisoryOverlayKey {
                    source: source_id,
                    coordinate: published.coordinate.clone(),
                };
                advisory_overlay
                    .entries
                    .insert(advisory_key.clone(), advisory);
                let advisory = advisory_overlay
                    .entries
                    .get(&advisory_key)
                    .cloned()
                    .ok_or_else(|| "advisory overlay omitted a published release".to_owned())?;
                let admitted = admit_registry_coordinate(&published.coordinate)
                    .map_err(|_| backend_engine::ProductAdmissionError::PackageReference)
                    .map_err(|error| error.to_string())?;
                let mut record = backend_engine::RegistryPackageRecord {
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
                    authority: None,
                    native_metadata_version: published
                        .native_metadata
                        .identity()
                        .map_err(|error| error.to_string())?,
                    native_metadata: published.native_metadata.clone(),
                    forge_sources,
                    advisory,
                };
                let mut authority = package_fact_authority(
                    source_id,
                    source_facts_root,
                    published.provenance.as_bytes(),
                    published.facts.version(),
                    &record,
                    self.fresh_package_facts
                        .get(&(source_id, published.coordinate.clone())),
                )?;
                if matches!(
                    authority.release_facts_freshness,
                    PackageFactFreshness::Historical
                ) {
                    let current_advisory = record.advisory.clone();
                    mark_mutable_facts_stale(&mut record);
                    // The release observation expired across process restart,
                    // but advisory freshness comes from the separately
                    // persisted and currently selected feed generation.
                    record.advisory = current_advisory;
                }
                // Advisory feed freshness is a separate mutable overlay from
                // release facts; reflect its projected state in the authority
                // DTO carried by this row.
                authority.advisory_freshness = record.advisory.freshness;
                record.authority = Some(authority.to_surface());
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            left.coordinate.cmp(&right.coordinate).then_with(|| {
                left.authority
                    .map(|authority| authority.source)
                    .cmp(&right.authority.map(|authority| authority.source))
            })
        });
        Ok((records, advisory_overlay))
    }

    /// Identity of the opened catalog generations.
    ///
    /// The stamp changes when a source commits a package or forge link, a
    /// mutable-fact observation expires, or advisory authority advances.
    /// Callers keep a resident dependency index until it changes. Opening a
    /// resident source reads its generation.
    pub(super) fn publication_stamp(&mut self) -> Result<[u8; 32], String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.registry.publication-stamp.v1\0");
        for source in self.sources.sources().cloned().collect::<Vec<_>>() {
            let service = self
                .service_for(&source)
                .map_err(|error| format!("open registry source: {error}"))?;
            service
                .refresh_external()
                .map_err(|error| format!("refresh registry source: {error}"))?;
            hasher.update(&source.id().as_bytes());
            hasher.update(&service.catalog_generation().to_le_bytes());
        }
        hasher.update(&self.observation_freshness_state());
        hasher.update(&self.advisory_overlay_generation()?);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Returns dependency facts from the same immutable publication records as
    /// the catalog. Unknown and unavailable metadata stay typed all the way to
    /// the product surface; an empty known set is the only representation of
    /// a package that has no declared edges.
    pub(super) fn dependency_facts(&mut self) -> Vec<backend_engine::PackageDependencySourceFacts> {
        let mut facts = Vec::new();
        let mut seen = BTreeSet::new();
        for ((registry_id, _), slot) in &self.slots {
            let Some(service) = slot.service.as_ref() else {
                continue;
            };
            for published in service.published_packages() {
                if !seen.insert((*registry_id, published.coordinate.clone())) {
                    continue;
                }
                if let Ok(source) = backend_engine::PackageReference::parse(
                    published.coordinate.as_str().to_owned(),
                ) {
                    let source_authority = backend_library::PackageGraphSourceAuthority::Registry(
                        backend_library::RegistryAuthorityId::from_configured_source(
                            registry_id.as_bytes(),
                        ),
                    );
                    let dependency_facts = match &published.dependency_facts {
                        backend_library::DependencyFacts::Known(rows) => {
                            backend_library::DependencyFacts::Known(
                                rows.iter()
                                    .cloned()
                                    .map(|row| row.with_source_authority(source_authority))
                                    .collect::<Vec<_>>()
                                    .into_boxed_slice(),
                            )
                        }
                        backend_library::DependencyFacts::Unknown(reason) => {
                            backend_library::DependencyFacts::Unknown(reason.clone())
                        }
                        backend_library::DependencyFacts::Unavailable(reason) => {
                            backend_library::DependencyFacts::Unavailable(reason.clone())
                        }
                    };
                    facts.push((
                        backend_library::PackageGraphSourceKey::new(source, source_authority),
                        dependency_facts,
                    ));
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
            advisory_path,
            advisory_config: advisory_config.clone(),
            // Freshness observations are deliberately process-local. A
            // recovered catalog retains authenticated facts, but a restart
            // cannot make a mutable fact current merely by reopening cache.
            fresh_package_facts: BTreeMap::new(),
            observation_generation: 0,
            projection: None,
        }))
    }

    /// Returns the resident catalog projection, rebuilding it when a source
    /// facts root or mutable-fact freshness state changes.
    pub(super) fn catalog_projection(&mut self) -> Result<Arc<CatalogProjection>, String> {
        let key = self.projection_key()?;
        if let Some((cached_key, projection)) = &self.projection
            && cached_key == &key
        {
            return Ok(Arc::clone(projection));
        }
        // Release-fact freshness changes the row overlay without changing
        // searchable terms. Advisory generations are part of the search
        // revision because they can add or remove indexed advisory terms.
        let reusable_index = self
            .projection
            .as_ref()
            .filter(|(cached_key, _)| same_catalog_search_revision(cached_key, &key))
            .map(|(_, projection)| Arc::clone(&projection.index));
        let (records, advisory_overlay) = self.project_catalog_records(key.advisory_generation)?;
        let dependency_facts = self.dependency_facts();
        let index = reusable_index.unwrap_or_else(|| {
            Arc::new(super::product_state::CatalogLookupIndex::from_catalog(
                &records,
            ))
        });
        let projection = Arc::new(CatalogProjection {
            selected_rows_snapshot: super::product_state::CatalogLookupIndex::snapshot_for_catalog(
                &records,
            ),
            advisory_generation: advisory_overlay.generation,
            records,
            index,
            dependency_facts,
        });
        self.projection = Some((key, Arc::clone(&projection)));
        Ok(projection)
    }

    fn projection_key(&mut self) -> Result<CatalogProjectionKey, String> {
        let sources = self.sources.sources().cloned().collect::<Vec<_>>();
        let mut source_keys = Vec::with_capacity(sources.len());
        let observation_state = self.observation_freshness_state();
        for source in &sources {
            let service = self
                .service_for(source)
                .map_err(|error| format!("open registry source: {error}"))?;
            service
                .refresh_external()
                .map_err(|error| format!("refresh registry source: {error}"))?;
            source_keys.push(CatalogSourceProjectionKey {
                source: service.source_id(),
                facts_frontier: service.facts_frontier(),
                observation_state,
            });
        }
        Ok(CatalogProjectionKey {
            sources: source_keys,
            advisory_generation: self.advisory_overlay_generation()?,
        })
    }

    fn advisory_overlay_generation(&self) -> Result<[u8; 32], String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.registry.advisory-overlay-generation.v1\0");
        hasher.update(&1_u16.to_be_bytes());
        hasher.update(&self.advisory.max_age_secs.to_be_bytes());
        hasher.update(&[u8::from(self.advisory_config.offline)]);
        let now = advisory_now();
        for source in &self.advisory_config.sources {
            let source_identity = backend_engine::serde_json::to_vec(&source.source)
                .map_err(|error| error.to_string())?;
            hasher.update(&source_identity);
            match self.advisory.frontier(source.source) {
                Some(frontier) => {
                    hasher.update(&[1]);
                    hasher.update(&frontier.sequence.to_be_bytes());
                    hasher.update(&frontier.digest);
                    hasher.update(&frontier.observed_at.to_be_bytes());
                    hasher.update(&[u8::from(frontier.complete)]);
                    hasher.update(&frontier.entries.to_be_bytes());
                    hasher.update(&[u8::from(frontier.not_modified)]);
                    hasher.update(&[match frontier.availability {
                        backend_engine::advisory::AuthorityAvailability::Available => 0,
                        backend_engine::advisory::AuthorityAvailability::Unavailable => 1,
                    }]);
                    let stale =
                        now.saturating_sub(frontier.observed_at) > self.advisory.max_age_secs;
                    hasher.update(&[u8::from(stale)]);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        let osv_scope = backend_engine::serde_json::to_vec(&self.advisory_config.osv_scope)
            .map_err(|error| error.to_string())?;
        hasher.update(&(osv_scope.len() as u64).to_be_bytes());
        hasher.update(&osv_scope);
        Ok(*hasher.finalize().as_bytes())
    }

    fn observation_freshness_state(&mut self) -> [u8; 32] {
        let now = current_millis();
        self.prune_expired_package_facts(now);
        package_fact_observation_state(&self.fresh_package_facts, self.observation_generation, now)
    }

    fn prune_expired_package_facts(&mut self, now_millis: u64) {
        let removed =
            prune_expired_package_fact_observations(&mut self.fresh_package_facts, now_millis);
        if removed > 0 {
            self.observation_generation = self.observation_generation.saturating_add(1);
        }
    }

    /// Fetches, verifies, and durably publishes one exact remote coordinate.
    pub(super) fn acquire(
        &mut self,
        coordinate: &PackageCoordinate,
    ) -> Result<Vec<u8>, RegistryAddError> {
        self.acquire_with_cancellation(coordinate, None)
    }

    /// Acquires an exact coordinate while checking cancellation between
    /// configured source requests. An in-flight HTTP request is bounded by
    /// the transport's connect and read deadlines before cancellation is seen.
    pub(super) fn acquire_cancellable(
        &mut self,
        coordinate: &PackageCoordinate,
        cancellation: &AtomicBool,
    ) -> Result<Vec<u8>, RegistryAddError> {
        self.acquire_with_cancellation(coordinate, Some(cancellation))
    }

    fn acquire_with_cancellation(
        &mut self,
        coordinate: &PackageCoordinate,
        cancellation: Option<&AtomicBool>,
    ) -> Result<Vec<u8>, RegistryAddError> {
        if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
            return Err(RegistryAddError::Cancelled);
        }
        self.prune_expired_package_facts(current_millis());
        let route = self
            .sources
            .route(coordinate)
            .map_err(RegistryAddError::Acquisition)?;
        let mut last_fallback = None;
        for source in route.candidates().iter().cloned() {
            if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
                return Err(RegistryAddError::Cancelled);
            }
            let outcome = self.acquire_from_source(&source, coordinate)?;
            if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
                return Err(RegistryAddError::Cancelled);
            }
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
        let request =
            AcquisitionRequest::for_coordinate(source_id, coordinate.to_string(), 1, 0)
                .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::InvalidCoordinate))?;
        let live_observation = !matches!(source.policy(), AcquisitionPolicy::Offline);
        let outcome = if !live_observation {
            // An explicitly offline add is a cache read. Rehydrate it from
            // the durable catalog while preserving its historical freshness.
            let service = self.service_for(source)?;
            service.ensure(&request)
        } else {
            let mut transport = self.transport(source, coordinate)?;
            let service = self.service_for(source)?;
            service.acquire(&request, &mut transport)
        };
        let observation = match &outcome {
            TypedAcquisitionOutcome::Hit(result) => {
                Some(PackageFactObservationProof::AcquisitionReceipt {
                    receipt: result.receipt.id.to_bytes(),
                    snapshot: result.snapshot.id().to_bytes(),
                })
            }
            TypedAcquisitionOutcome::NegativeFact(fact)
                if matches!(
                    fact.kind,
                    NegativeFactKind::Yanked | NegativeFactKind::AdvisoryBlocked
                ) =>
            {
                Some(PackageFactObservationProof::SourceNegativeFact {
                    authority: fact.authority,
                    source_proof: fact.source_proof,
                    cursor: fact.cursor,
                    observed_at_millis: fact.observed_at_millis,
                    expires_at_millis: fact.expires_at_millis,
                    policy_epoch: fact.policy_epoch,
                    kind: fact.kind,
                })
            }
            _ => None,
        };
        let result = self.finish_acquisition(outcome);
        if live_observation && let Some(proof) = observation {
            self.remember_fresh_package_facts(source_id, coordinate, proof);
        }
        match result {
            Ok(bytes) => Ok(CandidateOutcome::Done(bytes)),
            Err(error) if should_fallback(&error) => Ok(CandidateOutcome::Fallback(error)),
            Err(error) => Ok(CandidateOutcome::Terminal(error)),
        }
    }

    fn remember_fresh_package_facts(
        &mut self,
        source_id: [u8; 32],
        coordinate: &PackageCoordinate,
        proof: PackageFactObservationProof,
    ) {
        let Some((facts_version, source_provenance, valid, receipt_observed_at)) = (|| {
            let service = self.slots.values().find_map(|slot| {
                slot.service
                    .as_ref()
                    .filter(|service| service.source_id() == source_id)
            })?;
            if service.refresh_external().is_err() {
                return None;
            }
            let package = service.published_package(coordinate)?;
            let (valid, receipt_observed_at) = match proof {
                PackageFactObservationProof::AcquisitionReceipt { receipt, snapshot } => {
                    let request = AcquisitionRequest::for_coordinate(
                        source_id,
                        coordinate.to_string(),
                        1,
                        service.policy_epoch(),
                    )
                    .ok()?;
                    let record = service.recover_product_record(&request).ok().flatten();
                    let valid = record.as_ref().is_some_and(|record| {
                        let (Some(selected_receipt), Some(selected_snapshot)) =
                            (record.receipt.as_deref(), record.target_snapshot.as_deref())
                        else {
                            return false;
                        };
                        record.terminal
                            == backend_engine::acquisition::AcquisitionProductTerminal::Published
                            && record.source == source_id
                            && record.policy_epoch == service.policy_epoch()
                            && record.facts_frontier == service.facts_frontier()
                            && record.metadata_digest == Some(package.metadata_evidence_digest())
                            && record.raw_object == Some(package.raw_object)
                            && selected_receipt.id.to_bytes() == receipt
                            && selected_snapshot.id().to_bytes() == snapshot
                            && selected_receipt.target == selected_snapshot.id()
                            && selected_receipt.owner_cursor == record.owner_cursor
                            && selected_receipt.metadata_digest
                                == package.metadata_evidence_digest()
                            && selected_receipt.raw_object == package.raw_object
                            && selected_snapshot.source() == source_id
                            && selected_snapshot.facts_frontier() == service.facts_frontier()
                            && selected_receipt.policy_epoch == service.policy_epoch()
                            && snapshot_selects_package(selected_snapshot, source_id, &package)
                    });
                    (valid, record.map(|record| record.observed_at_millis))
                }
                PackageFactObservationProof::SourceNegativeFact {
                    authority,
                    source_proof,
                    cursor,
                    observed_at_millis,
                    expires_at_millis,
                    policy_epoch,
                    kind,
                    ..
                } => {
                    let request = AcquisitionRequest::for_coordinate(
                        source_id,
                        coordinate.to_string(),
                        1,
                        service.policy_epoch(),
                    )
                    .ok()?;
                    let expected_fact = backend_engine::acquisition::NegativeFact {
                        kind,
                        authority,
                        source_proof,
                        cursor,
                        observed_at_millis,
                        expires_at_millis,
                        policy_epoch,
                    };
                    let record = service.recover_product_record(&request).ok().flatten();
                    let durable_fact_matches = record.as_ref().is_some_and(|record| {
                        let backend_engine::acquisition::AcquisitionProductTerminal::NegativeFact(
                            durable_fact,
                        ) = record.terminal
                        else {
                            return false;
                        };
                        record.source == source_id
                            && record.source_intent == request.source_intent()
                            && record.coordinate.as_ref() == coordinate.as_str()
                            && record.policy_epoch == service.policy_epoch()
                            && record.owner_cursor == cursor
                            && record.facts_frontier == service.facts_frontier()
                            && record.metadata_digest == Some(package.metadata_evidence_digest())
                            && record.raw_object == Some(package.raw_object)
                            && durable_fact == expected_fact
                    });
                    (
                        durable_fact_matches
                            && authority == source_id
                            && source_proof == cursor
                            && expires_at_millis > current_millis()
                            && policy_epoch == service.policy_epoch()
                            && match kind {
                                NegativeFactKind::Yanked => matches!(
                                    package.facts.standing(),
                                    backend_engine::registry::ReleaseStanding::Yanked
                                ),
                                NegativeFactKind::AdvisoryBlocked => matches!(
                                    package.advisory.decision,
                                    backend_engine::advisory::AcquisitionDecision::Deny(_)
                                ),
                                NegativeFactKind::NotFound | NegativeFactKind::Unsupported => false,
                            },
                        None,
                    )
                }
            };
            Some((
                package.facts.version(),
                package.provenance.as_bytes(),
                valid,
                receipt_observed_at,
            ))
        })() else {
            return;
        };
        if !valid {
            return;
        }
        let at_millis = match proof {
            PackageFactObservationProof::AcquisitionReceipt { .. } => {
                receipt_observed_at.unwrap_or_else(current_millis)
            }
            PackageFactObservationProof::SourceNegativeFact {
                observed_at_millis, ..
            } => observed_at_millis,
        };
        let key = (source_id, coordinate.clone());
        let observation = FreshPackageFactsObservation {
            facts_version,
            source_provenance,
            at_millis,
            proof,
        };
        if remember_package_fact_observation(
            &mut self.fresh_package_facts,
            key,
            observation,
            current_millis(),
        ) {
            self.observation_generation = self.observation_generation.saturating_add(1);
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
            TypedAcquisitionOutcome::Cancelled => Err(RegistryAddError::Cancelled),
        }
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

fn same_catalog_search_revision(left: &CatalogProjectionKey, right: &CatalogProjectionKey) -> bool {
    left.advisory_generation == right.advisory_generation
        && left.sources.len() == right.sources.len()
        && left
            .sources
            .iter()
            .zip(&right.sources)
            .all(|(left, right)| {
                left.source == right.source && left.facts_frontier == right.facts_frontier
            })
}

fn remember_catalog_publication(
    seen: &mut BTreeSet<([u8; 32], PackageCoordinate)>,
    source: [u8; 32],
    coordinate: &PackageCoordinate,
) -> bool {
    seen.insert((source, coordinate.clone()))
}

fn snapshot_selects_package(
    snapshot: &SourceSnapshot,
    source_id: [u8; 32],
    package: &backend_engine::registry::PublishedPackage,
) -> bool {
    let Ok(admitted) = admit_registry_coordinate(&package.coordinate) else {
        return false;
    };
    let Ok(claim) = ReleaseClaim::new_with_facts(
        source_id,
        package.coordinate.as_str(),
        admitted.version().as_str(),
        package.raw_object,
        package.facts.version(),
    ) else {
        return false;
    };
    snapshot.source() == source_id
        && snapshot
            .manifest()
            .entries()
            .binary_search_by(|entry| entry.path.as_ref().cmp(package.coordinate.as_str()))
            .is_ok_and(|index| snapshot.manifest().entries()[index].object == package.raw_object)
        && snapshot.claims().binary_search(&claim.id).is_ok()
}

fn package_fact_authority(
    source: [u8; 32],
    source_facts_root: [u8; 32],
    source_provenance: [u8; 32],
    facts_version: [u8; 32],
    record: &backend_engine::RegistryPackageRecord,
    observation: Option<&FreshPackageFactsObservation>,
) -> Result<PackageFactAuthority, String> {
    let freshness = observation
        .filter(|observation| {
            observation.facts_version == facts_version
                && observation.source_provenance == source_provenance
                && observation.is_current_at(current_millis())
        })
        .map_or(PackageFactFreshness::Historical, |observation| {
            PackageFactFreshness::Observed {
                at_millis: observation.at_millis,
                valid_until_millis: observation.valid_until_millis(),
                proof: observation.proof,
            }
        });
    let advisory_facts_version = *blake3::hash(
        &serde_json::to_vec(&record.advisory)
            .map_err(|error| format!("encode selected advisory facts: {error}"))?,
    )
    .as_bytes();
    let mut selection = blake3::Hasher::new();
    selection.update(b"backend.registry.selected-package-facts.v1\0");
    selection.update(&source);
    selection.update(&source_facts_root);
    selection.update(record.coordinate.as_str().as_bytes());
    selection.update(&source_provenance);
    selection.update(&facts_version);
    selection.update(&advisory_facts_version);
    match freshness {
        PackageFactFreshness::Historical => {
            selection.update(b"historical\0");
        }
        PackageFactFreshness::Observed {
            at_millis,
            valid_until_millis,
            proof,
        } => {
            selection.update(b"observed\0");
            selection.update(&at_millis.to_be_bytes());
            selection.update(&valid_until_millis.to_be_bytes());
            match proof {
                PackageFactObservationProof::AcquisitionReceipt { receipt, snapshot } => {
                    selection.update(b"receipt\0");
                    selection.update(&receipt);
                    selection.update(&snapshot);
                }
                PackageFactObservationProof::SourceNegativeFact {
                    authority,
                    source_proof,
                    cursor,
                    observed_at_millis,
                    expires_at_millis,
                    policy_epoch,
                    kind,
                } => {
                    selection.update(b"negative\0");
                    selection.update(&authority);
                    selection.update(&source_proof);
                    selection.update(&cursor);
                    selection.update(&observed_at_millis.to_be_bytes());
                    selection.update(&expires_at_millis.to_be_bytes());
                    selection.update(&policy_epoch.to_be_bytes());
                    let kind = match kind {
                        NegativeFactKind::NotFound => 0,
                        NegativeFactKind::Yanked => 1,
                        NegativeFactKind::AdvisoryBlocked => 2,
                        NegativeFactKind::Unsupported => 3,
                    };
                    selection.update(&[kind]);
                }
            }
        }
    }
    let availability = |value| match value {
        backend_engine::RegistryFactAvailability::NotRecorded => {
            PackageFactCompleteness::NotRecorded
        }
        backend_engine::RegistryFactAvailability::Unsupported => {
            PackageFactCompleteness::Unsupported
        }
        backend_engine::RegistryFactAvailability::Unavailable => {
            PackageFactCompleteness::Unavailable
        }
        backend_engine::RegistryFactAvailability::Stale => PackageFactCompleteness::Partial,
        backend_engine::RegistryFactAvailability::Unknown => PackageFactCompleteness::Unknown,
    };
    let downloads = match &record.downloads {
        backend_engine::RegistryDownloadCount::Exact(_) => PackageFactCompleteness::Complete,
        backend_engine::RegistryDownloadCount::Approximate(_) => PackageFactCompleteness::Partial,
        backend_engine::RegistryDownloadCount::Unavailable(state) => availability(*state),
    };
    let advisories = match record.advisory.coverage {
        backend_engine::advisory::AdvisoryCoverage::Complete => PackageFactCompleteness::Complete,
        backend_engine::advisory::AdvisoryCoverage::Partial => PackageFactCompleteness::Partial,
        backend_engine::advisory::AdvisoryCoverage::Unavailable => {
            PackageFactCompleteness::Unavailable
        }
        backend_engine::advisory::AdvisoryCoverage::Unknown => PackageFactCompleteness::Unknown,
    };
    Ok(PackageFactAuthority {
        source,
        source_facts_root,
        source_provenance,
        facts_version,
        advisory_facts_version,
        selection_version: *selection.finalize().as_bytes(),
        standing: PackageFactCompleteness::Complete,
        downloads,
        advisories,
        release_facts_freshness: freshness,
        advisory_freshness: record.advisory.freshness,
    })
}

fn mark_mutable_facts_stale(record: &mut backend_engine::RegistryPackageRecord) {
    if !matches!(
        &record.downloads,
        backend_engine::RegistryDownloadCount::Unavailable(_)
    ) {
        record.downloads = backend_engine::RegistryDownloadCount::Unavailable(
            backend_engine::RegistryFactAvailability::Stale,
        );
    }
    record.advisory.freshness = backend_engine::advisory::FreshnessState::Stale;
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
    let maximum_state_bytes = u64::try_from(config.max_feed_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(4)
        .clamp(
            64 * 1024 * 1024,
            backend_engine::advisory::MAX_ADVISORY_AUTHORITY_STATE_BYTES,
        );
    let mut authority = backend_engine::advisory::AdvisoryAuthority::open_with_limit(
        path,
        config.max_age_secs,
        maximum_state_bytes,
    )
    .map_err(|error| error.to_string())?;
    authority.set_max_age_secs(config.max_age_secs);
    authority.set_offline(config.offline);
    authority.configure_sources(config.sources.iter().map(|source| source.source));
    authority.configure_osv_scope(config.osv_scope);
    authority.configure_source_identities(config.sources.iter().map(|source| {
        (
            source.source,
            advisory_source_identity(source.source, &source.location, config.osv_scope),
        )
    }));
    // Advisory refresh is intentionally not part of process composition.
    // A daemon must be able to bind and serve local/cached reads with no
    // startup network dependency; a future explicit refresh command can use
    // the existing bounded source adapter.
    authority.persist(path).map_err(|error| error.to_string())?;
    Ok(Arc::new(authority))
}

/// A bounded, seekable compressed OSV response staged outside the authority
/// journal. The anonymous temporary file is removed when this handle closes.
struct AdvisoryZipSpool {
    file: File,
    digest: [u8; 32],
}

fn spool_advisory_zip(mut input: impl Read, maximum: usize) -> Result<AdvisoryZipSpool, String> {
    if maximum == 0 {
        return Err("OSV ZIP compressed bound is zero".to_owned());
    }
    let mut file = tempfile::tempfile()
        .map_err(|_| "could not create private bounded OSV ZIP spool".to_owned())?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0usize;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if total == maximum {
            let mut extra = [0_u8; 1];
            match input.read(&mut extra) {
                Ok(0) => {}
                Ok(_) => {
                    drop(file);
                    return Err("OSV ZIP compressed body exceeds bound".to_owned());
                }
                Err(_) => {
                    drop(file);
                    return Err("could not read bounded OSV ZIP body".to_owned());
                }
            }
            break;
        }
        let available = (maximum - total).min(buffer.len());
        let read = match input.read(&mut buffer[..available]) {
            Ok(read) => read,
            Err(_) => {
                drop(file);
                return Err("could not read bounded OSV ZIP body".to_owned());
            }
        };
        if read == 0 {
            break;
        }
        let Some(next_total) = total.checked_add(read) else {
            drop(file);
            return Err("OSV ZIP compressed length overflow".to_owned());
        };
        total = next_total;
        hasher.update(&buffer[..read]);
        if file.write_all(&buffer[..read]).is_err() {
            drop(file);
            return Err("could not write bounded OSV ZIP spool".to_owned());
        }
    }
    if file.sync_all().is_err() {
        drop(file);
        return Err("could not sync bounded OSV ZIP spool".to_owned());
    }
    Ok(AdvisoryZipSpool {
        file,
        digest: *hasher.finalize().as_bytes(),
    })
}

fn advisory_location_is_osv_zip(source: &AdvisorySourceConfig) -> bool {
    if source.source != backend_engine::advisory::AdvisorySource::Osv {
        return false;
    }
    let location = source.location.split(['?', '#']).next().unwrap_or_default();
    Path::new(location)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Keeps diagnostics useful while preventing an endpoint's userinfo or query
/// token from being copied into the durable/user-visible source status.
fn public_advisory_request_error(error: ureq::Error) -> String {
    let message = match error {
        ureq::Error::StatusCode(status) => {
            format!("advisory source returned HTTP {status}")
        }
        ureq::Error::Timeout(_) => "advisory source request timed out".to_owned(),
        ureq::Error::HostNotFound => "advisory source host could not be resolved".to_owned(),
        ureq::Error::ConnectionFailed | ureq::Error::Io(_) => {
            "advisory source connection failed".to_owned()
        }
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "advisory source redirect was rejected".to_owned()
        }
        ureq::Error::BadUri(_) | ureq::Error::InvalidProxyUrl => {
            "advisory source endpoint configuration is invalid".to_owned()
        }
        ureq::Error::Tls(_) | ureq::Error::TlsRequired => {
            "advisory source TLS validation failed".to_owned()
        }
        ureq::Error::Protocol(_) => "advisory source response was malformed".to_owned(),
        _ => "advisory source request failed".to_owned(),
    };
    bounded_advisory_error(message)
}

fn bounded_advisory_error(mut error: String) -> String {
    const MAX_ERROR_BYTES: usize = 256;
    if error.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES - 3;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        error.push_str("...");
    }
    error
}

fn advisory_http_expiry(
    cache_control: Option<&str>,
    age: Option<&str>,
    expires: Option<&str>,
    observed_at: u64,
) -> Option<u64> {
    let mut deadlines = Vec::new();
    if let Some(cache_control) = cache_control {
        for directive in cache_control.split(',').map(str::trim) {
            let (name, value) = directive
                .split_once('=')
                .map_or((directive, None), |(name, value)| {
                    (name.trim(), Some(value.trim()))
                });
            if name.eq_ignore_ascii_case("no-cache") || name.eq_ignore_ascii_case("no-store") {
                return Some(observed_at);
            }
            if name.eq_ignore_ascii_case("max-age") {
                if let Some(max_age) = value
                    .map(|value| value.trim_matches('"'))
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    let response_age = age
                        .and_then(|age| age.trim().parse::<u64>().ok())
                        .unwrap_or(0);
                    deadlines
                        .push(observed_at.saturating_add(max_age.saturating_sub(response_age)));
                }
            }
        }
    }
    if let Some(expires) = expires
        && let Ok(deadline) = httpdate::parse_http_date(expires)
    {
        let seconds = deadline
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        deadlines.push(seconds);
    }
    deadlines.into_iter().min()
}

fn parse_osv_zip(
    spool: &mut AdvisoryZipSpool,
    observed_at: u64,
    maximum_expanded_bytes: usize,
    snapshot_root: &Path,
    etag: Option<String>,
    last_modified: Option<String>,
    scope: Option<backend_engine::advisory::OsvFeedScope>,
) -> Result<backend_engine::advisory::AuthorityFeed, String> {
    let scope = scope
        .ok_or_else(|| "OSV ZIP requires an explicit --advisory-osv-scope selection".to_owned())?;
    spool
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|_| "could not rewind private OSV ZIP spool".to_owned())?;
    parse_osv_zip_reader(
        &mut spool.file,
        spool.digest,
        observed_at,
        maximum_expanded_bytes,
        snapshot_root,
        backend_engine::advisory::MAX_OSV_SNAPSHOT_STORAGE_BYTES,
        etag,
        last_modified,
        scope,
    )
}

fn parse_osv_zip_reader(
    input: impl Read + Seek,
    digest: [u8; 32],
    observed_at: u64,
    maximum_expanded_bytes: usize,
    snapshot_root: &Path,
    maximum_snapshot_bytes: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    scope: backend_engine::advisory::OsvFeedScope,
) -> Result<backend_engine::advisory::AuthorityFeed, String> {
    use backend_engine::advisory::{
        MAX_OSV_SNAPSHOT_OBJECTS, MAX_OSV_SNAPSHOT_PACKAGE_ROWS, MAX_OSV_SNAPSHOT_PACKAGES,
        OsvSnapshotBuilder, parse_osv_scoped,
    };

    let mut archive = zip::ZipArchive::new(input)
        .map_err(|_| "OSV advisory archive is not a valid ZIP container".to_owned())?;
    if archive.len() > MAX_OSV_ZIP_MEMBERS {
        return Err("OSV advisory ZIP member count is outside bounds".to_owned());
    }
    let mut builder = OsvSnapshotBuilder::create(
        snapshot_root,
        scope,
        MAX_OSV_SNAPSHOT_OBJECTS,
        MAX_OSV_SNAPSHOT_PACKAGES,
        MAX_OSV_SNAPSHOT_PACKAGE_ROWS,
        maximum_snapshot_bytes,
    )
    .map_err(|error| error.to_string())?;
    let mut documents = 0usize;
    let mut expanded = 0usize;
    for index in 0..archive.len() {
        let mut member = archive
            .by_index(index)
            .map_err(|_| "OSV advisory ZIP member is unreadable".to_owned())?;
        let name = member.name();
        if member
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("OSV advisory ZIP contains a symbolic-link member".to_owned());
        }
        let declared = usize::try_from(member.size())
            .map_err(|_| "OSV advisory ZIP member size is oversized".to_owned())?;
        expanded = expanded
            .checked_add(declared)
            .filter(|total| *total <= maximum_expanded_bytes)
            .ok_or_else(|| "OSV advisory ZIP expanded size exceeds bound".to_owned())?;
        let is_json = !member.is_dir()
            && name
                .rsplit('/')
                .next()
                .unwrap_or(name)
                .to_ascii_lowercase()
                .ends_with(".json");
        if !is_json {
            // Drain every non-JSON member too: zip's CRC validation happens
            // only when its stream reaches EOF. A corrupt ignored member must
            // never allow a complete absence snapshot to be selected.
            let read = std::io::copy(
                &mut member.by_ref().take(
                    u64::try_from(declared)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                ),
                &mut std::io::sink(),
            )
            .map_err(|_| "OSV advisory ZIP member could not be verified".to_owned())?;
            if read != u64::try_from(declared).unwrap_or(u64::MAX) {
                return Err("OSV advisory ZIP member size did not match its directory".to_owned());
            }
            continue;
        }
        if declared == 0 || declared > MAX_OSV_ZIP_DOCUMENT_BYTES {
            return Err("OSV advisory ZIP document is outside bounds".to_owned());
        }
        let mut document = Vec::with_capacity(declared);
        member
            .by_ref()
            .take(
                u64::try_from(declared)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut document)
            .map_err(|_| "OSV advisory ZIP document could not be read".to_owned())?;
        if document.len() != declared {
            return Err("OSV advisory ZIP document size did not match its directory".to_owned());
        }
        documents = documents.saturating_add(1);
        if u64::try_from(documents).unwrap_or(u64::MAX) > MAX_OSV_SNAPSHOT_OBJECTS {
            return Err("OSV advisory ZIP contains too many JSON documents".to_owned());
        }
        let mut advisory = parse_osv_scoped(&document, observed_at, scope)
            .map_err(|_| "OSV advisory ZIP contains an invalid advisory document".to_owned())?;
        advisory.evidence.snapshot = Some(format!(
            "blake3:{}",
            blake3::Hash::from_bytes(digest).to_hex()
        ));
        builder.push(&advisory).map_err(|error| error.to_string())?;
    }
    if documents == 0 {
        return Err("OSV advisory ZIP contains no advisory JSON documents".to_owned());
    }
    let snapshot = builder.finish(digest).map_err(|error| error.to_string())?;
    Ok(backend_engine::advisory::AuthorityFeed::from_osv_snapshot(
        snapshot,
        observed_at,
        etag,
        last_modified,
    ))
}

fn refresh_authority_source(
    authority: &backend_engine::advisory::AdvisoryAuthority,
    source: &AdvisorySourceConfig,
    osv_scope: Option<backend_engine::advisory::OsvFeedScope>,
    maximum: usize,
    snapshot_root: &Path,
) -> Result<backend_engine::advisory::AuthorityFeed, String> {
    let source_identity = advisory_source_identity(source.source, &source.location, osv_scope);
    let previous = authority.frontier(source.source).filter(|frontier| {
        frontier.source_identity == Some(source_identity)
            && (source.source != backend_engine::advisory::AdvisorySource::Osv
                || frontier.osv_scope == osv_scope)
    });
    let observed_at = advisory_now();
    let is_osv_zip = advisory_location_is_osv_zip(source);
    let mut zip_spool = None;
    let mut response_expires_at = None;
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
        let mut conditional_request_sent = false;
        if let Some(etag) = previous.and_then(|frontier| frontier.etag.as_deref()) {
            request = request.header("if-none-match", etag);
            conditional_request_sent = true;
        }
        if let Some(last_modified) = previous.and_then(|frontier| frontier.last_modified.as_deref())
        {
            request = request.header("if-modified-since", last_modified);
            conditional_request_sent = true;
        }
        let mut response = request.call().map_err(public_advisory_request_error)?;
        response_expires_at = advisory_http_expiry(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            response
                .headers()
                .get("age")
                .and_then(|value| value.to_str().ok()),
            response
                .headers()
                .get("expires")
                .and_then(|value| value.to_str().ok()),
            observed_at,
        );
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
            if !conditional_request_sent {
                return Err(
                    "advisory source returned 304 without a conditional validator".to_owned(),
                );
            }
            (Vec::new(), etag, last_modified, true)
        } else if (200..300).contains(&status) {
            if is_osv_zip {
                zip_spool = Some(spool_advisory_zip(
                    response.body_mut().as_reader(),
                    MAX_OSV_ZIP_COMPRESSED_BYTES,
                )?);
                (Vec::new(), etag, last_modified, false)
            } else {
                let mut bytes = Vec::new();
                response
                    .body_mut()
                    .as_reader()
                    .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|_| "advisory authority response body could not be read".to_owned())?;
                if bytes.len() > maximum {
                    return Err("advisory authority body exceeds bound".to_owned());
                }
                (bytes, etag, last_modified, false)
            }
        } else {
            return Err(format!("advisory authority returned HTTP {status}"));
        }
    } else {
        let path = Path::new(&source.location);
        if source.source == backend_engine::advisory::AdvisorySource::RustSec && path.is_dir() {
            let entries = read_rustsec_tree(path, maximum, observed_at)?;
            let mut feed = backend_engine::advisory::AuthorityFeed::from_entries(
                source.source,
                entries,
                observed_at,
                previous.and_then(|frontier| frontier.etag.clone()),
                previous.and_then(|frontier| frontier.last_modified.clone()),
            );
            feed.source_identity = Some(source_identity);
            return Ok(feed);
        }
        let etag = previous.and_then(|frontier| frontier.etag.clone());
        let last_modified = previous.and_then(|frontier| frontier.last_modified.clone());
        if is_osv_zip {
            zip_spool = Some(spool_advisory_zip(
                File::open(path)
                    .map_err(|_| "could not open OSV ZIP advisory source".to_owned())?,
                MAX_OSV_ZIP_COMPRESSED_BYTES,
            )?);
            (Vec::new(), etag, last_modified, false)
        } else {
            (
                backend_engine::advisory::read_feed(path, maximum)
                    .map_err(|error| error.to_string())?,
                etag,
                last_modified,
                false,
            )
        }
    };
    let mut feed = if not_modified {
        backend_engine::advisory::AuthorityFeed::not_modified(
            source.source,
            observed_at,
            etag,
            last_modified,
        )
    } else {
        if let Some(spool) = zip_spool.as_mut() {
            parse_osv_zip(
                spool,
                observed_at,
                MAX_OSV_ZIP_EXPANDED_BYTES,
                snapshot_root,
                etag,
                last_modified,
                osv_scope,
            )?
        } else {
            let mut feed = backend_engine::advisory::AuthorityFeed::parse(
                source.source,
                &bytes,
                observed_at,
                etag,
                last_modified,
            )
            .map_err(|error| error.to_string())?;
            if source.source == backend_engine::advisory::AdvisorySource::Osv {
                // An individual JSON document or arbitrary batch cannot prove
                // that absent advisories are clean. Only an admitted ZIP with
                // an explicit selection is a complete OSV snapshot.
                feed.complete = false;
            }
            feed
        }
    };
    feed.freshness.expires_at = response_expires_at;
    feed.source_identity = Some(source_identity);
    Ok(feed)
}

/// Hashes the configured source location and, for OSV only, selected partition without
/// storing the raw URL/path (which can contain credentials) in authority state.
fn advisory_source_identity(
    source: backend_engine::advisory::AdvisorySource,
    location: &str,
    osv_scope: Option<backend_engine::advisory::OsvFeedScope>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.advisory.source-identity.v1\0");
    hasher.update(&[match source {
        backend_engine::advisory::AdvisorySource::Osv => 1,
        backend_engine::advisory::AdvisorySource::RustSec => 2,
        backend_engine::advisory::AdvisorySource::Ghsa => 3,
    }]);
    hasher.update(location.as_bytes());
    hasher.update(&[0]);
    if source == backend_engine::advisory::AdvisorySource::Osv
        && let Some(scope) = osv_scope
    {
        hasher.update(scope.label().as_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn read_rustsec_tree(
    root: &Path,
    maximum: usize,
    observed_at: u64,
) -> Result<Vec<backend_engine::advisory::Advisory>, String> {
    #[derive(Clone)]
    struct DirectorySnapshot {
        directory: DirectoryCapability,
        count: usize,
        digest: [u8; 32],
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct FileStamp {
        len: u64,
        modified: Option<std::time::SystemTime>,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        mtime: i64,
        #[cfg(unix)]
        mtime_nsec: i64,
        #[cfg(windows)]
        volume: Option<u32>,
        #[cfg(windows)]
        index: Option<u64>,
        #[cfg(windows)]
        last_write: u64,
    }

    impl FileStamp {
        fn capture(metadata: &std::fs::Metadata) -> Self {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Self {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    mtime: metadata.mtime(),
                    mtime_nsec: metadata.mtime_nsec(),
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                Self {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                    volume: metadata.volume_serial_number(),
                    index: metadata.file_index(),
                    last_write: metadata.last_write_time(),
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                Self {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }
            }
        }
    }

    struct FileSnapshot {
        directory: DirectoryCapability,
        name: String,
        stamp: FileStamp,
        digest: [u8; 32],
    }

    fn listing_digest(entries: &[DirectoryEntry]) -> Result<[u8; 32], String> {
        let mut hasher = blake3::Hasher::new();
        for entry in entries {
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| "RustSec authority tree has a non-UTF-8 name".to_owned())?;
            hasher.update(&u64::try_from(name.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(name.as_bytes());
            hasher.update(&[match entry.kind {
                EntryKind::File => 1,
                EntryKind::Directory => 2,
                EntryKind::Link => 3,
                EntryKind::Special => 4,
            }]);
        }
        Ok(*hasher.finalize().as_bytes())
    }

    fn read_file_snapshot(
        directory: &DirectoryCapability,
        name: &str,
        maximum: usize,
    ) -> Result<(Vec<u8>, FileStamp, [u8; 32]), String> {
        let file = directory
            .open_file_read(name)
            .map_err(|error| error.to_string())?;
        let before = file.metadata().map_err(|error| error.to_string())?;
        if !before.is_file() || before.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
            return Err("RustSec authority tree exceeds bound".to_owned());
        }
        let stamp = FileStamp::capture(&before);
        let mut reader = file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1));
        let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(maximum));
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let after = reader
            .get_ref()
            .metadata()
            .map_err(|error| error.to_string())?;
        if bytes.len() > maximum
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len()
            || FileStamp::capture(&after) != stamp
        {
            return Err("RustSec authority file changed during admission".to_owned());
        }
        let digest = *blake3::hash(&bytes).as_bytes();
        Ok((bytes, stamp, digest))
    }

    fn visit(
        directory: &DirectoryCapability,
        depth: usize,
        maximum: usize,
        observed_at: u64,
        total: &mut usize,
        visited: &mut usize,
        output: &mut Vec<backend_engine::advisory::Advisory>,
        directories: &mut Vec<DirectorySnapshot>,
        files: &mut Vec<FileSnapshot>,
    ) -> Result<(), String> {
        let entries = directory
            .entries(MAX_RUSTSEC_TREE_ENTRIES.saturating_sub(*visited))
            .map_err(|error| error.to_string())?;
        directories.push(DirectorySnapshot {
            directory: directory.clone(),
            count: entries.len(),
            digest: listing_digest(&entries)?,
        });
        for entry in entries {
            *visited = (*visited)
                .checked_add(1)
                .filter(|count| *count <= MAX_RUSTSEC_TREE_ENTRIES)
                .ok_or_else(|| "RustSec authority tree contains too many entries".to_owned())?;
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| "RustSec authority tree has a non-UTF-8 name".to_owned())?
                .to_owned();
            if entry.kind == EntryKind::Directory {
                // `.git` and other dot directories hold no advisories.
                if !name.starts_with('.') {
                    if depth >= MAX_RUSTSEC_TREE_DEPTH {
                        return Err("RustSec authority tree is nested too deeply".to_owned());
                    }
                    let child = directory
                        .open_dir(&name)
                        .map_err(|error| error.to_string())?;
                    visit(
                        &child,
                        depth + 1,
                        maximum,
                        observed_at,
                        total,
                        visited,
                        output,
                        directories,
                        files,
                    )?;
                }
            } else if entry.kind == EntryKind::File
                && (name.rsplit_once('.').is_some_and(|(_, extension)| extension == "toml")
                    // advisory-db keeps each advisory as `RUSTSEC-*.md`: fenced TOML
                    // front matter, then prose. README/CONTRIBUTING are not advisories.
                    || (name.starts_with("RUSTSEC-") && name.ends_with(".md")))
            {
                let remaining = maximum.saturating_sub(*total);
                let per_document =
                    remaining.min(backend_engine::advisory::MAX_ADVISORY_DOCUMENT_BYTES);
                let (bytes, stamp, digest) = read_file_snapshot(directory, &name, per_document)?;
                *total = (*total)
                    .checked_add(bytes.len())
                    .filter(|total| *total <= maximum)
                    .ok_or_else(|| "RustSec authority tree exceeds bound".to_owned())?;
                if output.len() >= backend_engine::advisory::MAX_ADVISORY_BATCH_OBJECTS {
                    return Err("RustSec authority tree contains too many advisories".to_owned());
                }
                let mut advisory = backend_engine::advisory::parse_rustsec(&bytes, observed_at)
                    .map_err(|error| format!("{error:?}"))?;
                advisory.evidence.snapshot = Some(format!("blake3:{}", hex_digest(digest)));
                output.push(advisory);
                files.push(FileSnapshot {
                    directory: directory.clone(),
                    name,
                    stamp,
                    digest,
                });
            } else if !matches!(entry.kind, EntryKind::File | EntryKind::Directory) {
                return Err("RustSec authority tree contains a link or special file".to_owned());
            }
        }
        Ok(())
    }

    fn hex_digest(bytes: [u8; 32]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(64);
        for byte in bytes {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        text
    }

    let mut total = 0;
    let mut visited = 0;
    let mut output = Vec::new();
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let root_directory =
        DirectoryCapability::open_read_only_source(root).map_err(|error| error.to_string())?;
    visit(
        &root_directory,
        0,
        maximum,
        observed_at,
        &mut total,
        &mut visited,
        &mut output,
        &mut directories,
        &mut files,
    )?;
    for snapshot in directories {
        let current = snapshot
            .directory
            .entries(MAX_RUSTSEC_TREE_ENTRIES)
            .map_err(|error| error.to_string())?;
        if current.len() != snapshot.count || listing_digest(&current)? != snapshot.digest {
            return Err("RustSec authority tree changed during snapshot admission".to_owned());
        }
    }
    for snapshot in files {
        let file = snapshot
            .directory
            .open_file_read(&snapshot.name)
            .map_err(|error| error.to_string())?;
        if FileStamp::capture(&file.metadata().map_err(|error| error.to_string())?)
            != snapshot.stamp
        {
            return Err("RustSec authority file changed during snapshot admission".to_owned());
        }
        let mut hasher = blake3::Hasher::new();
        let mut total_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        let mut reader = file.take(
            u64::try_from(backend_engine::advisory::MAX_ADVISORY_DOCUMENT_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            total_bytes = total_bytes
                .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
                .ok_or_else(|| "RustSec authority tree exceeds bound".to_owned())?;
            hasher.update(&buffer[..read]);
        }
        let after = reader
            .get_ref()
            .metadata()
            .map_err(|error| error.to_string())?;
        if total_bytes != snapshot.stamp.len
            || FileStamp::capture(&after) != snapshot.stamp
            || *hasher.finalize().as_bytes() != snapshot.digest
        {
            return Err("RustSec authority file changed during snapshot admission".to_owned());
        }
    }
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
    /// Names the package root inside one staged archive.
    ///
    /// Registry archives wrap their sources in one conventional packaging
    /// directory: `package/` for npm, `<name>-<version>/` for crates and
    /// source distributions, `<module>@<version>/` for Go module zips. That
    /// wrapper is packaging, not source layout, so when it is the archive's
    /// only entry it is descended into. Every path a reader sees is then
    /// relative to the package (`src/lib.rs`), as the identity contract
    /// states, rather than to the archive (`package/src/lib.rs`).
    ///
    /// Only a recognized wrapper is descended into: `package`, or a name
    /// ending in the release version. A lone directory that is source layout
    /// (a Maven sources jar holding only `com/`) keeps its place, because a
    /// Java package path must stay intact.
    fn at(directory: PathBuf, version: &str) -> Result<Self, RegistryAddError> {
        let io = |error| RegistryAddError::Acquisition(AcquisitionError::Io(error));
        if let Some(member) = generated_cargo_workspace_member(&directory)? {
            return Ok(Self { path: member });
        }
        let mut entries = fs::read_dir(&directory).map_err(io)?;
        let (Some(only), None) = (entries.next(), entries.next()) else {
            return Ok(Self { path: directory });
        };
        let only = only.map_err(io)?;
        let name = only.file_name();
        let wrapper = name.to_str().is_some_and(|name| {
            name == "package"
                || name.starts_with("__nudox_registry_package_")
                || (!version.is_empty() && name.ends_with(version))
        });
        if wrapper && only.file_type().map_err(io)?.is_dir() {
            return Ok(Self { path: only.path() });
        }
        Ok(Self { path: directory })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

const GENERATED_CARGO_WORKSPACE_HEADER: &str = "# nudox-registry-workspace-v1\n";

/// Places Cargo archives behind a workspace boundary without editing package files.
///
/// Registry source trees are staged beneath the application checkout, which may
/// itself be a Cargo workspace. Cargo otherwise walks upward from a downloaded
/// package and rejects it as an unlisted child of that unrelated workspace.
/// A tiny generated workspace at the archive digest root gives Cargo the right
/// boundary while leaving the verified package manifest and sources untouched.
fn ensure_cargo_workspace_boundary(root: &Path) -> Result<(), RegistryAddError> {
    let io = |error| RegistryAddError::Acquisition(AcquisitionError::Io(error));
    if generated_cargo_workspace_member(root)?.is_some() {
        return Ok(());
    }

    let root_manifest = root.join("Cargo.toml");
    let package_root = if root_manifest.is_file() {
        // Some valid source archives have no `<name>-<version>/` wrapper. Move
        // their extracted entries under a generated member instead of replacing
        // the authentic root Cargo.toml with the workspace marker.
        let mut suffix = 0_u32;
        let member_name = loop {
            let candidate = format!("__nudox_registry_package_{suffix}");
            if !root.join(&candidate).exists() {
                break candidate;
            }
            suffix = suffix
                .checked_add(1)
                .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        };
        let member = root.join(&member_name);
        fs::create_dir(&member).map_err(io)?;
        let entries = fs::read_dir(root).map_err(io)?;
        for entry in entries {
            let entry = entry.map_err(io)?;
            if entry.file_name().to_string_lossy() == member_name {
                continue;
            }
            fs::rename(entry.path(), member.join(entry.file_name())).map_err(io)?;
        }
        (member_name, member)
    } else {
        let mut package_roots = Vec::new();
        for entry in fs::read_dir(root).map_err(io)? {
            let entry = entry.map_err(io)?;
            if entry.file_type().map_err(io)?.is_dir() && entry.path().join("Cargo.toml").is_file()
            {
                package_roots.push(entry.path());
            }
        }
        let [package_root] = package_roots.as_slice() else {
            return Err(RegistryAddError::UnsupportedArchive);
        };
        let member_name = package_root
            .strip_prefix(root)
            .map_err(|_| RegistryAddError::UnsupportedArchive)?
            .to_string_lossy()
            .into_owned();
        (member_name, package_root.clone())
    };

    let package_manifest_path = package_root.1.join("Cargo.toml");
    let package_manifest = fs::read_to_string(&package_manifest_path).map_err(io)?;
    let package_value: toml::Value = package_manifest
        .parse()
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    let package_table = package_value
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or(RegistryAddError::UnsupportedArchive)?;

    // A package that owns a workspace already provides Cargo's nearest
    // boundary, including its explicit resolver setting.
    if package_value.get("workspace").is_some() {
        return Ok(());
    }
    let edition = package_table
        .get("edition")
        .and_then(toml::Value::as_str)
        .unwrap_or("2015");
    let resolver = match package_table.get("resolver").and_then(toml::Value::as_str) {
        Some("1") => "1",
        Some("2") => "2",
        Some("3") => "3",
        Some(_) => return Err(RegistryAddError::UnsupportedArchive),
        None => match edition {
            "2015" | "2018" => "1",
            "2021" => "2",
            "2024" => "3",
            _ => return Err(RegistryAddError::UnsupportedArchive),
        },
    };
    let mut workspace_table = toml::map::Map::new();
    workspace_table.insert(
        "members".to_owned(),
        toml::Value::Array(vec![toml::Value::String(package_root.0.clone())]),
    );
    workspace_table.insert(
        "resolver".to_owned(),
        toml::Value::String(resolver.to_owned()),
    );
    let mut root_table = toml::map::Map::new();
    root_table.insert("workspace".to_owned(), toml::Value::Table(workspace_table));
    let workspace = format!(
        "{GENERATED_CARGO_WORKSPACE_HEADER}{}",
        toml::to_string(&toml::Value::Table(root_table))
            .map_err(|_| RegistryAddError::UnsupportedArchive)?
    );
    let workspace_path = root.join("Cargo.toml");
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&workspace_path)
    {
        Ok(mut file) => {
            file.write_all(workspace.as_bytes()).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if generated_cargo_workspace_member(root)?.is_none() {
                return Err(RegistryAddError::Acquisition(AcquisitionError::Io(error)));
            }
        }
        Err(error) => return Err(io(error)),
    }
    backend_platform::durability::open_directory(&package_root.1)
        .and_then(|directory| directory.sync_all())
        .map_err(io)?;
    backend_platform::durability::open_directory(root)
        .and_then(|directory| directory.sync_all())
        .map_err(io)?;
    Ok(())
}

/// Returns the sole package member declared by a generated staging workspace.
fn generated_cargo_workspace_member(root: &Path) -> Result<Option<PathBuf>, RegistryAddError> {
    let io = |error| RegistryAddError::Acquisition(AcquisitionError::Io(error));
    let path = root.join("Cargo.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let Some(generated) = contents.strip_prefix(GENERATED_CARGO_WORKSPACE_HEADER) else {
        return Ok(None);
    };
    let value: toml::Value = generated
        .parse()
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    let members = value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    let [member] = members.as_slice() else {
        return Err(RegistryAddError::UnsupportedArchive);
    };
    let member = member
        .as_str()
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    let member = confined_path(member)?;
    let member = root.join(member);
    if !member.join("Cargo.toml").is_file() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(Some(member))
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
    coordinate: &PackageCoordinate,
    archive: &[u8],
    workspace_root: impl AsRef<Path>,
) -> Result<StagedProject, RegistryAddError> {
    let admitted = admit_registry_coordinate(coordinate).map_err(RegistryAddError::Acquisition)?;
    let version = admitted.version().as_str().to_owned();
    let needs_cargo_boundary = admitted.ecosystem() == backend_library::RegistryEcosystem::Cargo;
    if archive.is_empty() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let staging_root = workspace_root.as_ref().join("registry-staging");
    fs::create_dir_all(&staging_root)
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    let digest = blake3::hash(archive);
    let directory = staging_root.join(hex(digest.as_bytes()));
    if directory.exists() {
        if needs_cargo_boundary {
            ensure_cargo_workspace_boundary(&directory)?;
        }
        return StagedProject::at(directory, &version);
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
    if needs_cargo_boundary {
        if let Err(error) = ensure_cargo_workspace_boundary(&temporary) {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
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
    StagedProject::at(directory, &version)
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
    use flate2::{Compression, write::GzEncoder};
    use std::io::{Read as IoRead, Write as IoWrite};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        for _ in 0..64 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-registry-stage-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated registry fixture directory: {error}"),
            }
        }
        panic!("registry fixture directory capacity exhausted")
    }

    fn tar_file(name: &str, bytes: &[u8]) -> Vec<u8> {
        tar_files(&[(name, bytes)])
    }

    fn tar_files(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = Vec::new();
        for (name, bytes) in files {
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
            archive.extend_from_slice(&header);
            archive.extend_from_slice(bytes);
            archive.resize(archive.len().div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES, 0);
        }
        archive.resize(archive.len() + TAR_BLOCK_BYTES * 2, 0);
        archive
    }

    fn catalog_record(
        coordinate: &str,
        ecosystem: backend_library::RegistryEcosystem,
        facts_version: [u8; 32],
    ) -> backend_engine::RegistryPackageRecord {
        let native_metadata = backend_library::RegistryNativeMetadata::unavailable(
            ecosystem,
            "catalog authority test",
        );
        let coordinate = PackageCoordinate::parse(coordinate).expect("coordinate");
        let admitted = admit_registry_coordinate(&coordinate).expect("admitted coordinate");
        backend_engine::RegistryPackageRecord {
            coordinate: backend_engine::PackageReference::Purl(coordinate),
            ecosystem,
            name: backend_engine::ProductText::new(admitted.qualified_name().as_str())
                .expect("name"),
            version: backend_engine::ProductText::new(admitted.version().as_str())
                .expect("version"),
            bytes: 4,
            standing: backend_engine::RegistryReleaseStanding::Available,
            downloads: backend_engine::RegistryDownloadCount::Exact(42),
            facts_version,
            authority: None,
            native_metadata_version: native_metadata.identity().expect("metadata identity"),
            native_metadata,
            forge_sources: Box::new([]),
            advisory: backend_engine::advisory::AdvisoryPackageDto::unknown(),
        }
    }

    #[test]
    fn advisory_refresh_errors_do_not_retain_endpoint_credentials() {
        let error = public_advisory_request_error(ureq::Error::BadUri(
            "https://operator:secret@example.test/feed.zip?token=hidden".to_owned(),
        ));
        assert_eq!(error, "advisory source endpoint configuration is invalid");
        assert!(error.len() <= 256);
        assert!(!error.contains("secret"));
        assert!(!error.contains("hidden"));
    }

    #[test]
    fn advisory_304_without_a_sent_validator_cannot_refresh_prior_facts() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local authority");
        let endpoint = format!("http://{}/advisories.json", listener.local_addr().unwrap());
        let body = br#"{"schema_version":"1.3.1","id":"OSV-NO-VALIDATOR-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"demo"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#
            .to_vec();
        let server = thread::spawn(move || {
            for response in [
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes(),
                b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            ]
            .into_iter()
            {
                let (mut stream, _) = listener.accept().expect("accept authority request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).expect("read request headers");
                    assert_ne!(count, 0, "request headers complete");
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                assert!(
                    !request.contains("if-none-match:") && !request.contains("if-modified-since:"),
                    "both requests are unconditional because the first response had no validators"
                );
                stream.write_all(&response).expect("write HTTP response");
                if response.starts_with(b"HTTP/1.1 200") {
                    stream.write_all(&body).expect("write authority body");
                }
            }
        });

        let source = AdvisorySourceConfig {
            source: backend_engine::advisory::AdvisorySource::Osv,
            location: endpoint,
        };
        let scope = Some(backend_engine::advisory::OsvFeedScope::All);
        let mut authority = backend_engine::advisory::AdvisoryAuthority::new(60_000);
        authority.configure_sources([source.source]);
        let directory = scratch();
        let first = refresh_authority_source(&authority, &source, scope, 1024 * 1024, &directory)
            .expect("initial body without validators");
        assert_eq!(first.freshness.etag, None);
        assert_eq!(first.freshness.last_modified, None);
        authority.apply(first).expect("select initial feed");
        assert_eq!(authority.frontier(source.source).unwrap().etag, None);

        let error = refresh_authority_source(&authority, &source, scope, 1024 * 1024, &directory)
            .expect_err("unconditional 304 cannot refresh old facts");
        assert_eq!(
            error,
            "advisory source returned 304 without a conditional validator"
        );
        server.join().expect("authority server thread");
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn advisory_expiry_uses_cache_age_and_absolute_expiry_conservatively() {
        assert_eq!(
            advisory_http_expiry(Some("public, max-age=120"), Some("20"), None, 1_000),
            Some(1_100)
        );
        assert_eq!(
            advisory_http_expiry(
                Some("max-age=120"),
                Some("20"),
                Some("Thu, 01 Jan 1970 00:18:00 GMT"),
                1_000,
            ),
            Some(1_080)
        );
        assert_eq!(
            advisory_http_expiry(Some("no-cache"), None, None, 1_000),
            Some(1_000)
        );
    }

    fn empty_product_view() -> backend_engine::ViewRoot {
        let root = backend_engine::view_state_root(&[]);
        let basis = backend_engine::Basis::new(
            root,
            backend_engine::object_version(b"catalog-authority-test"),
        );
        backend_engine::ViewRoot::new_incomplete(
            backend_engine::view_key(b"catalog-authority-test"),
            basis,
            backend_engine::Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty product view")
    }

    fn advisory_config(location: Option<&Path>) -> AdvisoryConfig {
        let gate = backend_engine::advisory::AcquisitionGate {
            offline: backend_engine::advisory::OfflinePolicy::Warn,
        };
        AdvisoryConfig {
            sources: location
                .map(|path| {
                    vec![AdvisorySourceConfig {
                        source: backend_engine::advisory::AdvisorySource::Osv,
                        location: path.to_string_lossy().into_owned(),
                    }]
                })
                .unwrap_or_default(),
            osv_scope: Some(backend_engine::advisory::OsvFeedScope::All),
            max_age_secs: 60 * 60,
            offline: false,
            gate,
            max_feed_bytes: 1024 * 1024,
        }
    }

    fn registry_config(endpoint: String) -> RegistryConfig {
        let endpoint = backend_engine::registry::RegistryEndpoint::new(
            backend_library::RegistryEcosystem::Cargo,
            endpoint,
        )
        .expect("loopback registry endpoint");
        let source = backend_engine::registry::RegistrySource::new(endpoint).with_native(false);
        let sources = backend_engine::registry::RegistrySourceSet::empty()
            .with_source(source)
            .expect("canonical test registry source");
        RegistryConfig {
            sources,
            endpoint: None,
            authentication: None,
            policy: backend_engine::registry::AcquisitionPolicy::Online,
            native: false,
            limits: backend_engine::registry::AcquisitionLimits::default(),
            advisory_gate: backend_engine::advisory::AcquisitionGate {
                offline: backend_engine::advisory::OfflinePolicy::Warn,
            },
        }
    }

    fn local_registry_server(
        listener: TcpListener,
        feed: Vec<u8>,
        archive: Vec<u8>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("local registry request");
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).expect("read registry request");
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.starts_with("GET /feed?") {
                    &feed
                } else if request.starts_with("GET /archive ") {
                    &archive
                } else {
                    panic!(
                        "unexpected registry request: {}",
                        request.lines().next().unwrap_or("")
                    );
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .expect("write registry response headers");
                stream
                    .write_all(body)
                    .expect("write registry response body");
            }
        })
    }

    #[test]
    fn pre_cancelled_registry_job_never_starts_acquisition() {
        let root = scratch();
        let config = registry_config("http://127.0.0.1:9".to_owned());
        let mut gateway = RegistryGateway::open(&config, &root, &advisory_config(None))
            .expect("open registry gateway")
            .expect("configured registry gateway");
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0")
            .expect("valid exact package coordinate");
        let cancelled = AtomicBool::new(true);

        assert!(matches!(
            gateway.acquire_cancellable(&coordinate, &cancelled),
            Err(RegistryAddError::Cancelled)
        ));
        drop(gateway);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn advisory_refresh_overlays_the_same_release_after_cold_reopen() {
        const ADVISORY_ID: &str = "OSV-REGRESSION-1";
        let root = scratch();
        fs::create_dir_all(&root).expect("workspace root");
        let advisory_feed = root.join("live-osv-feed.json");
        fs::write(&advisory_feed, br#"{"vulns":[]}"#).expect("initial clean feed");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("local registry listener");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let config = registry_config(endpoint);
        let archive = b"registry release whose advisory arrives later".to_vec();
        let digest =
            *backend_engine::capability::CapabilityArtifactId::from_value(&archive).as_bytes();
        let feed = format!(
            r#"{{"schema":1,"next":"{}","items":[{{"name":"demo","version":"1.0.0","blake3":"{}","provenance":"{}","archive":"/archive"}}]}}"#,
            format!("{}", "07".repeat(32)),
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "09".repeat(32),
        )
        .into_bytes();
        let server = local_registry_server(listener, feed, archive.clone());

        let mut gateway =
            RegistryGateway::open(&config, &root, &advisory_config(Some(&advisory_feed)))
                .expect("compose gateway")
                .expect("configured registry gateway");
        gateway
            .refresh_advisories()
            .expect("refresh initial clean feed");
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let acquired = gateway
            .acquire(&coordinate)
            .expect("acquire release once from local registry");
        assert_eq!(acquired, archive);
        server.join().expect("local registry server");

        let source = config
            .sources
            .sources()
            .next()
            .expect("configured source")
            .clone();
        let acquisition_time_package = gateway
            .service_for(&source)
            .expect("resident service")
            .published_packages()
            .into_iter()
            .next()
            .expect("published release");
        let clean_projection = gateway.catalog_projection().expect("clean projection");
        let clean_stamp = gateway
            .publication_stamp()
            .expect("initial published catalog stamp");
        assert_eq!(clean_projection.records.len(), 1);
        assert_eq!(
            clean_projection.records[0].advisory.coverage,
            backend_engine::advisory::AdvisoryCoverage::Partial
        );
        assert!(clean_projection.records[0].advisory.advisories.is_empty());
        assert_eq!(
            clean_projection.records[0].coordinate,
            backend_engine::PackageReference::Purl(coordinate.clone())
        );

        let vulnerable = format!(
            r#"{{"schema_version":"1.3.1","id":"{ADVISORY_ID}","modified":"2026-09-28T00:00:00Z","affected":[{{"package":{{"ecosystem":"Cargo","name":"demo"}},"ranges":[{{"type":"SEMVER","events":[{{"introduced":"0"}},{{"fixed":"2.0.0"}}]}}]}}]}}"#
        );
        fs::write(&advisory_feed, vulnerable).expect("mutate local feed with vulnerability");
        gateway
            .refresh_advisories()
            .expect("refresh changed local feed");
        let refreshed_stamp = gateway
            .publication_stamp()
            .expect("publication stamp after advisory refresh");
        assert_ne!(clean_stamp, refreshed_stamp);
        let refreshed_projection = gateway
            .catalog_projection()
            .expect("same-process projection after refresh");
        assert_ne!(
            clean_projection.advisory_generation,
            refreshed_projection.advisory_generation
        );
        assert_ne!(
            clean_projection.index.search_identity(),
            refreshed_projection.index.search_identity()
        );
        assert!(
            refreshed_projection.records[0]
                .advisory
                .advisories
                .iter()
                .any(|advisory| advisory.canonical_id == ADVISORY_ID)
        );
        assert!(matches!(
            &refreshed_projection.records[0].advisory.decision,
            backend_engine::advisory::AcquisitionDecision::Deny(_)
        ));
        let refreshed_id_hits = refreshed_projection
            .index
            .search_page(&refreshed_projection.records, ADVISORY_ID, 8)
            .expect("search advisory id immediately after refresh");
        assert_eq!(refreshed_id_hits.hits.len(), 1);
        drop(gateway);

        // A fresh gateway instance recovers the same immutable release and
        // the persisted new advisory authority before rebuilding its row/search overlay.
        let mut gateway =
            RegistryGateway::open(&config, &root, &advisory_config(Some(&advisory_feed)))
                .expect("cold reopen gateway")
                .expect("configured registry gateway");
        let vulnerable_projection = gateway
            .catalog_projection()
            .expect("vulnerable projection after cold reopen");
        assert_ne!(
            clean_projection.advisory_generation,
            vulnerable_projection.advisory_generation
        );
        let row = vulnerable_projection
            .records
            .first()
            .expect("same acquired package row");
        assert_eq!(
            row.coordinate,
            backend_engine::PackageReference::Purl(coordinate.clone())
        );
        assert!(
            row.advisory
                .advisories
                .iter()
                .any(|advisory| advisory.canonical_id == ADVISORY_ID)
        );
        assert!(matches!(
            &row.advisory.decision,
            backend_engine::advisory::AcquisitionDecision::Deny(_)
        ));
        let exact_id_hits = vulnerable_projection
            .index
            .search_page(&vulnerable_projection.records, ADVISORY_ID, 8)
            .expect("search exact advisory id");
        assert_eq!(exact_id_hits.hits.len(), 1);
        assert_eq!(
            vulnerable_projection.records[exact_id_hits.hits[0].key].coordinate,
            backend_engine::PackageReference::Purl(coordinate.clone())
        );

        let reopened_package = gateway
            .service_for(&source)
            .expect("recovered acquisition service")
            .published_packages()
            .into_iter()
            .next()
            .expect("recovered immutable publication");
        assert_eq!(
            reopened_package.coordinate,
            acquisition_time_package.coordinate
        );
        assert_eq!(
            reopened_package.raw_object,
            acquisition_time_package.raw_object
        );
        assert_eq!(reopened_package.advisory, acquisition_time_package.advisory);
        assert!(reopened_package.advisory.advisories.is_empty());

        struct NoNetwork;
        impl backend_engine::registry::RegistryTransport for NoNetwork {
            fn fetch_page(
                &mut self,
                _request: backend_engine::registry::FeedRequest,
            ) -> Result<
                backend_engine::registry::TransportResult<backend_engine::registry::FeedPage>,
                backend_engine::registry::TransportFailure,
            > {
                panic!("current advisory denial must be decided from the cached release")
            }

            fn fetch_archive(
                &mut self,
                _package: &backend_engine::registry::RemotePackage,
            ) -> Result<
                backend_engine::registry::TransportResult<
                    backend_engine::registry::ArchiveArtifact,
                >,
                backend_engine::registry::TransportFailure,
            > {
                panic!("current advisory denial must not download the archive")
            }
        }
        let service = gateway
            .service_for(&source)
            .expect("current authority service");
        let request = AcquisitionRequest::for_coordinate(
            service.source_id(),
            coordinate.as_str(),
            1,
            service.policy_epoch(),
        )
        .expect("cache-only policy request")
        .with_fact_freshness(backend_engine::acquisition::FactFreshness::max_age_millis(
            u64::MAX,
        ));
        assert!(matches!(
            service.acquire(&request, &mut NoNetwork),
            TypedAcquisitionOutcome::NegativeFact(fact)
                if fact.kind == NegativeFactKind::AdvisoryBlocked
        ));
        drop(gateway);

        let no_sources = advisory_config(None);
        let mut unknown_gateway = RegistryGateway::open(&config, &root, &no_sources)
            .expect("open with no advisory authorities")
            .expect("configured registry gateway");
        let unknown = unknown_gateway
            .catalog_projection()
            .expect("unknown coverage projection");
        assert_eq!(
            unknown.records[0].advisory.coverage,
            backend_engine::advisory::AdvisoryCoverage::Unknown
        );
        assert!(unknown.records[0].advisory.advisories.is_empty());
        drop(unknown_gateway);

        fs::write(&advisory_feed, b"not an advisory feed").expect("break live feed");
        let mut unavailable_gateway =
            RegistryGateway::open(&config, &root, &advisory_config(Some(&advisory_feed)))
                .expect("reopen configured authority")
                .expect("configured registry gateway");
        let states = unavailable_gateway
            .refresh_advisories()
            .expect("persist unavailable source state");
        assert!(states[0].error.is_some());
        drop(unavailable_gateway);
        let mut unavailable_gateway =
            RegistryGateway::open(&config, &root, &advisory_config(Some(&advisory_feed)))
                .expect("cold reopen unavailable authority")
                .expect("configured registry gateway");
        let unavailable = unavailable_gateway
            .catalog_projection()
            .expect("unavailable coverage projection");
        assert_eq!(
            unavailable.records[0].advisory.coverage,
            backend_engine::advisory::AdvisoryCoverage::Unavailable
        );
        assert!(
            unavailable.records[0]
                .advisory
                .advisories
                .iter()
                .any(|advisory| advisory.canonical_id == ADVISORY_ID)
        );

        drop(unavailable_gateway);
        fs::remove_dir_all(root).expect("remove test workspace");
    }

    #[test]
    fn freshness_only_projection_reuses_search_and_structural_revision_rebuilds_it() {
        let old_key = CatalogProjectionKey {
            sources: vec![CatalogSourceProjectionKey {
                source: [1; 32],
                facts_frontier: [2; 32],
                observation_state: [3; 32],
            }],
            advisory_generation: [5; 32],
        };
        let freshness_advanced = CatalogProjectionKey {
            sources: vec![CatalogSourceProjectionKey {
                observation_state: [4; 32],
                ..old_key.sources[0]
            }],
            ..old_key.clone()
        };
        let source_advanced = CatalogProjectionKey {
            sources: vec![CatalogSourceProjectionKey {
                facts_frontier: [9; 32],
                observation_state: [4; 32],
                ..old_key.sources[0]
            }],
            ..old_key.clone()
        };
        let source_replaced = CatalogProjectionKey {
            sources: vec![CatalogSourceProjectionKey {
                source: [8; 32],
                observation_state: [4; 32],
                ..old_key.sources[0]
            }],
            ..old_key.clone()
        };
        let advisory_advanced = CatalogProjectionKey {
            advisory_generation: [6; 32],
            ..old_key.clone()
        };
        assert!(same_catalog_search_revision(&old_key, &freshness_advanced));
        assert!(!same_catalog_search_revision(&old_key, &source_advanced));
        assert!(!same_catalog_search_revision(&old_key, &source_replaced));
        assert!(!same_catalog_search_revision(&old_key, &advisory_advanced));

        let catalog = [catalog_record(
            "pkg:cargo/shared@1.0.0",
            backend_library::RegistryEcosystem::Cargo,
            [5; 32],
        )];
        let original = std::sync::Arc::new(
            super::super::product_state::CatalogLookupIndex::from_catalog(&catalog),
        );
        let identity = original.search_identity().expect("Tantivy projection");
        let reused = if same_catalog_search_revision(&old_key, &freshness_advanced) {
            std::sync::Arc::clone(&original)
        } else {
            std::sync::Arc::new(
                super::super::product_state::CatalogLookupIndex::from_catalog(&catalog),
            )
        };
        assert!(std::sync::Arc::ptr_eq(&original, &reused));
        assert_eq!(reused.search_identity(), Some(identity));
        let rebuilt = std::sync::Arc::new(
            super::super::product_state::CatalogLookupIndex::from_catalog(&catalog),
        );
        assert_ne!(rebuilt.search_identity(), Some(identity));
    }

    #[test]
    fn same_coordinate_from_two_sources_retains_both_authorities_and_facts() {
        let coordinate = PackageCoordinate::parse("pkg:cargo/shared@1.0.0").expect("coordinate");
        let mut seen = BTreeSet::new();
        assert!(remember_catalog_publication(
            &mut seen,
            [1; 32],
            &coordinate
        ));
        assert!(!remember_catalog_publication(
            &mut seen,
            [1; 32],
            &coordinate
        ));
        assert!(remember_catalog_publication(
            &mut seen,
            [2; 32],
            &coordinate
        ));

        let mut available = catalog_record(
            coordinate.as_str(),
            backend_library::RegistryEcosystem::Cargo,
            [3; 32],
        );
        available.authority = Some(
            package_fact_authority([1; 32], [4; 32], [5; 32], [3; 32], &available, None)
                .expect("available source authority")
                .to_surface(),
        );
        let mut yanked = catalog_record(
            coordinate.as_str(),
            backend_library::RegistryEcosystem::Cargo,
            [6; 32],
        );
        yanked.standing = backend_engine::RegistryReleaseStanding::Yanked;
        yanked.authority = Some(
            package_fact_authority([2; 32], [7; 32], [8; 32], [6; 32], &yanked, None)
                .expect("yanked source authority")
                .to_surface(),
        );

        let mut catalog = vec![available, yanked];
        catalog.sort_by(|left, right| {
            left.coordinate.cmp(&right.coordinate).then_with(|| {
                left.authority
                    .map(|authority| authority.source)
                    .cmp(&right.authority.map(|authority| authority.source))
            })
        });
        let index = super::super::product_state::CatalogLookupIndex::from_catalog(&catalog);
        let reference = backend_engine::PackageReference::Purl(coordinate);
        let rows = index
            .records_for(&catalog, &reference, false)
            .expect("exact coordinate rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].standing,
            backend_engine::RegistryReleaseStanding::Available
        );
        assert_eq!(rows[0].facts_version, [3; 32]);
        assert_eq!(rows[0].authority.expect("authority").source, [1; 32]);
        assert_eq!(
            rows[1].standing,
            backend_engine::RegistryReleaseStanding::Yanked
        );
        assert_eq!(rows[1].facts_version, [6; 32]);
        assert_eq!(rows[1].authority.expect("authority").source, [2; 32]);
        assert!(
            index
                .first_coordinate(&catalog, &reference)
                .expect_err("ambiguous authority must not select a source")
                .contains("multiple registry authorities")
        );
    }

    #[test]
    fn catalog_facts_bind_provenance_and_fact_versions_with_separate_freshness() {
        let coordinate = "pkg:cargo/demo@1.10.0";
        let source = [3; 32];
        let source_root = [4; 32];
        let provenance = [5; 32];
        let facts_version = [6; 32];
        let mut record = catalog_record(
            coordinate,
            backend_library::RegistryEcosystem::Cargo,
            facts_version,
        );

        // A cold owner reopens historical facts with their source and content
        // identities intact, but no live observation proof.
        let historical = package_fact_authority(
            source,
            source_root,
            provenance,
            facts_version,
            &record,
            None,
        )
        .expect("historical authority");
        assert_eq!(historical.source, source);
        assert_eq!(historical.source_facts_root, source_root);
        assert_eq!(historical.source_provenance, provenance);
        assert_eq!(historical.facts_version, facts_version);
        assert_ne!(historical.advisory_facts_version, [0; 32]);
        assert_eq!(historical.downloads, PackageFactCompleteness::Complete);
        assert_eq!(
            historical.release_facts_freshness,
            PackageFactFreshness::Historical
        );

        let selected_record = record.clone();
        mark_mutable_facts_stale(&mut record);
        assert_eq!(
            record.downloads,
            backend_engine::RegistryDownloadCount::Unavailable(
                backend_engine::RegistryFactAvailability::Stale
            )
        );
        assert_eq!(
            record.advisory.freshness,
            backend_engine::advisory::FreshnessState::Stale
        );

        let observation = FreshPackageFactsObservation {
            facts_version,
            source_provenance: provenance,
            at_millis: current_millis(),
            proof: PackageFactObservationProof::AcquisitionReceipt {
                receipt: [7; 32],
                snapshot: [8; 32],
            },
        };
        let observed = package_fact_authority(
            source,
            source_root,
            provenance,
            facts_version,
            &selected_record,
            Some(&observation),
        )
        .expect("observed authority");
        assert!(matches!(
            observed.release_facts_freshness,
            PackageFactFreshness::Observed {
                proof: PackageFactObservationProof::AcquisitionReceipt {
                    receipt,
                    snapshot,
                },
                ..
            } if receipt == [7; 32] && snapshot == [8; 32]
        ));
        assert_ne!(observed.selection_version, historical.selection_version);

        let mut changed_advisory = selected_record.clone();
        changed_advisory.advisory.freshness = backend_engine::advisory::FreshnessState::Fresh;
        let changed_advisory_authority = package_fact_authority(
            source,
            source_root,
            provenance,
            facts_version,
            &changed_advisory,
            Some(&observation),
        )
        .expect("changed advisory authority");
        assert_ne!(
            changed_advisory_authority.advisory_facts_version,
            observed.advisory_facts_version
        );
        assert_ne!(
            changed_advisory_authority.selection_version,
            observed.selection_version
        );

        // A new yank produces a new selected facts version. The older receipt
        // cannot make that transition look current.
        let yanked_facts = [9; 32];
        let changed = catalog_record(
            coordinate,
            backend_library::RegistryEcosystem::Cargo,
            yanked_facts,
        );
        let changed_authority = package_fact_authority(
            source,
            [10; 32],
            provenance,
            yanked_facts,
            &changed,
            Some(&observation),
        )
        .expect("changed authority");
        assert_eq!(
            changed_authority.release_facts_freshness,
            PackageFactFreshness::Historical
        );
        assert_ne!(
            changed_authority.selection_version,
            observed.selection_version
        );
    }

    #[test]
    fn cold_catalog_package_and_profile_replies_expose_historical_authority() {
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.10.0").expect("coordinate");
        let mut record = catalog_record(
            coordinate.as_str(),
            backend_library::RegistryEcosystem::Cargo,
            [6; 32],
        );
        // A reopened registry owner has durable selected facts but an empty
        // process-local observation map, so the projected row stays Available
        // as a historical fact and carries that status explicitly.
        let authority = package_fact_authority(
            [3; 32],
            [4; 32],
            [5; 32],
            record.facts_version,
            &record,
            None,
        )
        .expect("historical fact authority");
        record.authority = Some(authority.to_surface());
        mark_mutable_facts_stale(&mut record);
        let catalog = [record.clone()];
        let catalog_index = super::super::product_state::CatalogLookupIndex::from_catalog(&catalog);
        let mut state = super::super::product_state::ProductState::open(
            scratch().join("cold-product-state.json"),
        )
        .expect("cold product state");
        let dependency_facts: [backend_engine::PackageDependencySourceFacts; 0] = [];
        let dependency_index = backend_library::PackageGraphIndex::from_facts(&dependency_facts);
        let view = empty_product_view();

        let package_reply = state
            .execute(
                backend_engine::SurfaceCommand::Package {
                    package: backend_engine::PackageReference::Purl(coordinate.clone()),
                },
                &view,
                &catalog,
                &catalog_index,
                &dependency_facts,
                &dependency_index,
                None,
            )
            .expect("package query");
        let backend_engine::SurfaceReply::Package(package_rows) = package_reply else {
            panic!("package reply shape");
        };
        assert_eq!(package_rows.len(), 1);
        assert_eq!(
            package_rows[0].standing,
            backend_engine::RegistryReleaseStanding::Available
        );
        assert_eq!(package_rows[0].downloads, record.downloads);
        assert_eq!(
            package_rows[0]
                .authority
                .expect("package authority")
                .release_facts_freshness,
            backend_engine::RegistryPackageFactFreshness::Historical
        );

        let versions_reply = state
            .execute(
                backend_engine::SurfaceCommand::PackageVersions {
                    package: backend_engine::PackageReference::Purl(coordinate.clone()),
                },
                &view,
                &catalog,
                &catalog_index,
                &dependency_facts,
                &dependency_index,
                None,
            )
            .expect("versions query");
        let backend_engine::SurfaceReply::PackageVersions(version_rows) = versions_reply else {
            panic!("package versions reply shape");
        };
        assert_eq!(version_rows.len(), 1);
        assert_eq!(
            version_rows[0]
                .authority
                .expect("version authority")
                .release_facts_freshness,
            backend_engine::RegistryPackageFactFreshness::Historical
        );

        let profile_reply = state
            .execute(
                backend_engine::SurfaceCommand::PackageProfile {
                    package: backend_engine::PackageReference::Purl(coordinate),
                },
                &view,
                &catalog,
                &catalog_index,
                &dependency_facts,
                &dependency_index,
                None,
            )
            .expect("profile query");
        let backend_engine::SurfaceReply::PackageProfile {
            latest,
            versions,
            candidate_authority,
        } = profile_reply
        else {
            panic!("profile reply shape");
        };
        assert_eq!(versions, 1);
        assert!(latest.is_none());
        assert_eq!(
            candidate_authority
                .expect("profile authority for withheld latest")
                .release_facts_freshness,
            backend_engine::RegistryPackageFactFreshness::Historical
        );
    }

    #[test]
    fn catalog_package_fact_observations_expire_and_keep_source_identity() {
        let now = 200_000;
        let receipt_observation = FreshPackageFactsObservation {
            facts_version: [1; 32],
            source_provenance: [2; 32],
            at_millis: now,
            proof: PackageFactObservationProof::AcquisitionReceipt {
                receipt: [3; 32],
                snapshot: [4; 32],
            },
        };
        assert!(receipt_observation.is_current_at(now));
        assert!(!receipt_observation.is_current_at(now + PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS));
        assert_eq!(
            receipt_observation.valid_until_millis(),
            now + PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS
        );

        let negative_observation = FreshPackageFactsObservation {
            facts_version: [1; 32],
            source_provenance: [2; 32],
            at_millis: now,
            proof: PackageFactObservationProof::SourceNegativeFact {
                authority: [5; 32],
                source_proof: [6; 32],
                cursor: [6; 32],
                observed_at_millis: now,
                expires_at_millis: now + 500,
                policy_epoch: 7,
                kind: NegativeFactKind::Yanked,
            },
        };
        assert!(negative_observation.is_current_at(now + 499));
        assert!(!negative_observation.is_current_at(now + 500));

        let coordinate = "pkg:cargo/shared@1.0.0";
        let record = catalog_record(
            coordinate,
            backend_library::RegistryEcosystem::Cargo,
            [8; 32],
        );
        let left = package_fact_authority([9; 32], [10; 32], [11; 32], [8; 32], &record, None)
            .expect("left source authority");
        let right = package_fact_authority([12; 32], [13; 32], [14; 32], [8; 32], &record, None)
            .expect("right source authority");
        assert_ne!(left.source, right.source);
        assert_ne!(left.selection_version, right.selection_version);
    }

    fn receipt_observation(
        at_millis: u64,
        facts_version: [u8; 32],
    ) -> FreshPackageFactsObservation {
        FreshPackageFactsObservation {
            facts_version,
            source_provenance: [2; 32],
            at_millis,
            proof: PackageFactObservationProof::AcquisitionReceipt {
                receipt: [3; 32],
                snapshot: [4; 32],
            },
        }
    }

    #[test]
    fn package_fact_observations_remain_bounded_across_many_versions() {
        let source = [21; 32];
        let first_at = 300_000;
        let version_count = MAX_FRESH_PACKAGE_FACT_OBSERVATIONS + 16;
        let mut observations = FreshPackageFactMap::new();
        let mut newest = None;

        for index in 0..version_count {
            let at_millis = first_at + u64::try_from(index).expect("index fits");
            let coordinate = PackageCoordinate::parse(format!("pkg:cargo/demo@1.0.{index}"))
                .expect("version coordinate");
            let mut facts_version = [0; 32];
            facts_version[..8]
                .copy_from_slice(&u64::try_from(index).expect("index fits").to_le_bytes());
            assert!(remember_package_fact_observation(
                &mut observations,
                (source, coordinate.clone()),
                receipt_observation(at_millis, facts_version),
                at_millis,
            ));
            newest = Some(coordinate);
        }

        assert_eq!(observations.len(), MAX_FRESH_PACKAGE_FACT_OBSERVATIONS);
        let newest = newest.expect("at least one package version");
        assert!(observations.contains_key(&(source, newest)));
        assert!(observations.values().all(|observation| {
            observation.is_current_at(first_at + u64::try_from(version_count).expect("count fits"))
        }));
    }

    #[test]
    fn expired_observations_are_pruned_and_cold_reopen_starts_historical() {
        let source = [31; 32];
        let expired_coordinate =
            PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("expired coordinate");
        let live_coordinate =
            PackageCoordinate::parse("pkg:cargo/demo@1.0.1").expect("live coordinate");
        let expired_at = 100_000;
        let live_at = expired_at + PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS - 1;
        let expires_at = expired_at + PACKAGE_FACTS_OBSERVATION_HORIZON_MILLIS;
        let mut observations = FreshPackageFactMap::new();
        assert!(remember_package_fact_observation(
            &mut observations,
            (source, expired_coordinate.clone()),
            receipt_observation(expired_at, [1; 32]),
            expired_at,
        ));
        assert!(remember_package_fact_observation(
            &mut observations,
            (source, live_coordinate.clone()),
            receipt_observation(live_at, [2; 32]),
            live_at,
        ));

        let before_expiry = package_fact_observation_state(&observations, 2, expires_at - 1);
        let removed = prune_expired_package_fact_observations(&mut observations, expires_at);
        assert_eq!(removed, 1);
        assert!(!observations.contains_key(&(source, expired_coordinate)));
        assert!(observations.contains_key(&(source, live_coordinate)));
        let after_expiry = package_fact_observation_state(
            &observations,
            2 + u64::try_from(removed).expect("removed count fits"),
            expires_at,
        );
        assert_ne!(before_expiry, after_expiry);

        // RegistryGateway::open intentionally initializes an empty process-local
        // observation map, so durable facts after restart are historical.
        let reopened_observations = FreshPackageFactMap::new();
        assert!(reopened_observations.is_empty());
        assert_ne!(
            after_expiry,
            package_fact_observation_state(&reopened_observations, 0, expires_at)
        );
        assert!(!receipt_observation(expired_at, [1; 32]).is_current_at(expires_at));
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
        let archive = tar_files(&[
            (
                "package/Cargo.toml",
                b"[package]\nname = \"demo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
            ),
            ("package/src/lib.rs", b"pub fn from_registry() {}"),
        ]);
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage archive");
        let source = fs::read(staged.path().join("src/lib.rs")).expect("read source");
        assert!(
            staged.path().ends_with("package"),
            "the npm-style wrapper is the package root"
        );
        assert_eq!(source, b"pub fn from_registry() {}");
        let path = staged.path().to_path_buf();
        drop(staged);
        assert!(path.exists());
        let reused = stage_archive(&coordinate, &archive, &root).expect("reuse archive");
        assert_eq!(reused.path(), path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_versioned_wrapper_is_the_package_root_but_source_layout_is_kept() {
        let root = scratch();
        let crate_coordinate =
            PackageCoordinate::parse("pkg:cargo/demo@1.4.0").expect("coordinate");
        let crate_archive = tar_files(&[
            (
                "demo-1.4.0/Cargo.toml",
                b"[package]\nname = \"demo\"\nversion = \"1.4.0\"\nedition = \"2021\"\n",
            ),
            ("demo-1.4.0/src/lib.rs", b"pub fn wrapped() {}"),
        ]);
        let staged = stage_archive(&crate_coordinate, &crate_archive, &root).expect("stage crate");
        assert_eq!(
            fs::read(staged.path().join("src/lib.rs")).expect("crate source at package root"),
            b"pub fn wrapped() {}"
        );
        // A Maven sources jar whose only entry is the `com/` package tree:
        // that directory is Java source layout, not a packaging wrapper.
        let jar_coordinate =
            PackageCoordinate::parse("pkg:maven/com.demo/demo@2.0.0").expect("coordinate");
        let jar_archive = tar_file("com/demo/Demo.java", b"package com.demo; class Demo {}");
        let jar = stage_archive(&jar_coordinate, &jar_archive, &root).expect("stage jar");
        assert!(
            jar.path().join("com/demo/Demo.java").is_file(),
            "the Java package path must stay intact under {}",
            jar.path().display()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_archive_gets_a_resolver_preserving_workspace_boundary() {
        const SERDE_BUILD_SCRIPT: &[u8] = include_bytes!(
            "../../../../frontends/rust/tests/fixtures/serde-1.0.228-build-script.txt"
        );
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/serde@1.0.228").expect("coordinate");
        let manifest = b"[package]\nname = \"serde\"\nversion = \"1.0.228\"\nedition = \"2021\"\nbuild = \"build.rs\"\n";
        let archive = tar_files(&[
            ("serde-1.0.228/Cargo.toml", manifest),
            ("serde-1.0.228/build.rs", SERDE_BUILD_SCRIPT),
            ("serde-1.0.228/src/lib.rs", b"pub fn fixture() {}\n"),
        ]);
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage serde archive");

        assert_eq!(
            staged.path().file_name().unwrap().to_string_lossy(),
            "serde-1.0.228"
        );
        assert_eq!(
            fs::read(staged.path().join("Cargo.toml")).expect("package manifest"),
            manifest
        );
        assert_eq!(
            fs::read(staged.path().join("build.rs")).expect("serde build script"),
            SERDE_BUILD_SCRIPT
        );
        let workspace_manifest =
            fs::read_to_string(staged.path().parent().unwrap().join("Cargo.toml"))
                .expect("generated workspace manifest");
        assert!(workspace_manifest.starts_with(GENERATED_CARGO_WORKSPACE_HEADER));
        let workspace: toml::Value = workspace_manifest
            .strip_prefix(GENERATED_CARGO_WORKSPACE_HEADER)
            .expect("generated marker")
            .parse()
            .expect("workspace TOML");
        assert_eq!(
            workspace["workspace"]["members"].as_array().unwrap()[0].as_str(),
            Some("serde-1.0.228")
        );
        assert_eq!(workspace["workspace"]["resolver"].as_str(), Some("2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flat_cargo_archive_is_wrapped_without_overwriting_its_manifest() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/flat-demo@1.0.0").expect("coordinate");
        let manifest =
            b"[package]\nname = \"flat-demo\"\nversion = \"1.0.0\"\nedition = \"2018\"\n";
        let archive = tar_files(&[
            ("Cargo.toml", manifest),
            ("build.rs", b"fn main() {}\n"),
            ("src/lib.rs", b"pub fn fixture() {}\n"),
        ]);
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage flat crate");
        assert_eq!(
            fs::read(staged.path().join("Cargo.toml")).expect("package manifest"),
            manifest
        );
        let member = staged.path().file_name().unwrap().to_string_lossy();
        assert!(member.starts_with("__nudox_registry_package_"));
        let workspace_manifest =
            fs::read_to_string(staged.path().parent().unwrap().join("Cargo.toml"))
                .expect("generated workspace manifest");
        assert!(workspace_manifest.contains("resolver = \"1\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_archive_without_a_manifest_is_rejected_as_unsupported() {
        let root = scratch();
        let coordinate =
            PackageCoordinate::parse("pkg:cargo/no-manifest@1.0.0").expect("coordinate");
        let archive = tar_file("no-manifest-1.0.0/src/lib.rs", b"pub fn fixture() {}\n");
        assert!(matches!(
            stage_archive(&coordinate, &archive, &root),
            Err(RegistryAddError::UnsupportedArchive)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_package_resolver_overrides_edition_default() {
        let root = scratch();
        let coordinate =
            PackageCoordinate::parse("pkg:cargo/resolver-demo@1.0.0").expect("coordinate");
        let archive = tar_files(&[
            (
                "Cargo.toml",
                b"[package]\nname = \"resolver-demo\"\nversion = \"1.0.0\"\nedition = \"2018\"\nresolver = \"2\"\n",
            ),
            ("src/lib.rs", b"pub fn fixture() {}\n"),
        ]);
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage explicit resolver");
        let workspace_manifest =
            fs::read_to_string(staged.path().parent().unwrap().join("Cargo.toml"))
                .expect("generated workspace manifest");
        assert!(workspace_manifest.contains("resolver = \"2\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_path_traversal_is_rejected_before_writing_outside_the_jail() {
        let root = scratch();
        let escaped_staging_path = root.join("registry-staging/outside.rs");
        let escaped_workspace_path = root.join("outside.rs");
        assert!(!escaped_staging_path.exists());
        assert!(!escaped_workspace_path.exists());
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let archive = tar_file("../outside.rs", b"must not escape");
        assert!(matches!(
            stage_archive(&coordinate, &archive, &root),
            Err(RegistryAddError::UnsupportedArchive)
        ));
        assert!(!escaped_staging_path.exists());
        assert!(!escaped_workspace_path.exists());
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
            fs::read(staged.path().join("src/index.mts")).expect("source"),
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
    fn osv_zip_is_a_complete_bounded_snapshot_with_content_provenance() {
        use zip::write::SimpleFileOptions;

        let root = scratch();
        let snapshot_root = root.join("advisory-snapshots");
        let authority_path = root.join("advisory-authority.json");
        let document = br#"{"id":"OSV-TEST-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"demo"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#;
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        writer
            .start_file("osv/CARGO/OSV-TEST-1.json", SimpleFileOptions::default())
            .expect("start OSV JSON member");
        writer.write_all(document).expect("write OSV JSON member");
        let bytes = writer.finish().expect("finish OSV ZIP").into_inner();
        let digest = *blake3::hash(&bytes).as_bytes();
        let expected_snapshot = format!("blake3:{}", blake3::Hash::from_bytes(digest).to_hex());
        let feed = parse_osv_zip_reader(
            Cursor::new(bytes),
            digest,
            17,
            1024,
            &snapshot_root,
            128 * 1024 * 1024,
            None,
            None,
            backend_engine::advisory::OsvFeedScope::Ecosystem(
                backend_engine::advisory::OsvEcosystem::Cargo,
            ),
        )
        .expect("complete OSV ZIP snapshot");
        assert!(feed.complete);
        assert_eq!(feed.freshness.observed_at, 17);
        assert!(feed.entries.is_empty());
        assert_eq!(
            feed.osv_snapshot.as_ref().map(|snapshot| format!(
                "blake3:{}",
                blake3::Hash::from_bytes(snapshot.source_digest()).to_hex()
            )),
            Some(expected_snapshot)
        );

        let mut authority = backend_engine::advisory::AdvisoryAuthority::new(100);
        let scope = backend_engine::advisory::OsvFeedScope::Ecosystem(
            backend_engine::advisory::OsvEcosystem::Cargo,
        );
        authority.configure_sources([backend_engine::advisory::AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(scope));
        authority.apply(feed).expect("admit OSV snapshot");
        let package =
            backend_engine::advisory::normalize_package("cargo", "demo").expect("Cargo identity");
        let observation = authority.observe(&package, "1.0.0", false, false, 17, false);
        assert_eq!(
            observation.coverage,
            backend_engine::advisory::AdvisoryCoverage::Complete
        );
        assert_eq!(observation.advisories.len(), 1);
        assert_eq!(
            authority
                .frontier(backend_engine::advisory::AdvisorySource::Osv)
                .expect("selected OSV frontier")
                .entries,
            1
        );
        authority
            .persist(&authority_path)
            .expect("persist snapshot authority");
        let reopened = backend_engine::advisory::AdvisoryAuthority::open(&authority_path, 100)
            .expect("cold-open snapshot authority");
        let reopened = reopened.observe(&package, "1.0.0", false, false, 17, false);
        assert_eq!(
            reopened.coverage,
            backend_engine::advisory::AdvisoryCoverage::Complete
        );
        assert_eq!(reopened.advisories.len(), 1);
        let generation = fs::read_dir(&snapshot_root)
            .expect("snapshot root")
            .map(|entry| entry.expect("snapshot root entry"))
            .find(|entry| entry.file_type().expect("snapshot entry type").is_dir())
            .map(|entry| entry.path())
            .expect("published generation directory");
        let index_path = generation.join("package-index.bin");
        let mut index = fs::read(&index_path).expect("package index");
        index[0] ^= 1;
        fs::write(index_path, index).expect("tamper package index");
        let corrupted = backend_engine::advisory::AdvisoryAuthority::open(&authority_path, 100)
            .expect("authority state remains parseable");
        let corrupted = corrupted.observe(&package, "1.0.0", false, false, 17, false);
        assert_eq!(
            corrupted.coverage,
            backend_engine::advisory::AdvisoryCoverage::Unavailable
        );
        assert_eq!(corrupted.advisories.len(), 0);

        let cursor = Cursor::new(Vec::new());
        let empty = zip::ZipWriter::new(cursor)
            .finish()
            .expect("finish empty ZIP")
            .into_inner();
        let empty = parse_osv_zip_reader(
            Cursor::new(empty),
            [0; 32],
            17,
            1024,
            root.join("empty-snapshot").as_path(),
            128 * 1024 * 1024,
            None,
            None,
            backend_engine::advisory::OsvFeedScope::Ecosystem(
                backend_engine::advisory::OsvEcosystem::Cargo,
            ),
        );
        assert!(empty.is_err(), "empty ZIP cannot assert a clean feed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn osv_zip_rejects_malformed_declared_overrun_and_crc_corruption() {
        let root = scratch();
        let snapshot_root = root.join("advisory-snapshots");
        let scope = backend_engine::advisory::OsvFeedScope::Ecosystem(
            backend_engine::advisory::OsvEcosystem::Cargo,
        );
        assert!(
            parse_osv_zip_reader(
                Cursor::new(b"not a ZIP"),
                [0; 32],
                17,
                1024,
                &snapshot_root,
                128 * 1024 * 1024,
                None,
                None,
                scope,
            )
            .is_err()
        );

        let document = br#"{"id":"OSV-TEST-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"demo"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "osv/CARGO/OSV-TEST-1.json",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("start OSV JSON member");
        writer.write_all(document).expect("write OSV JSON member");
        let valid = writer.finish().expect("finish OSV ZIP").into_inner();
        assert!(
            parse_osv_zip_reader(
                Cursor::new(valid.clone()),
                *blake3::hash(&valid).as_bytes(),
                17,
                document.len() - 1,
                &snapshot_root,
                128 * 1024 * 1024,
                None,
                None,
                scope,
            )
            .is_err(),
            "declared expansion beyond the configured bound must fail before allocation"
        );

        let mut corrupt = valid;
        let local_header = corrupt
            .windows(4)
            .position(|bytes| bytes == b"PK\x03\x04")
            .expect("local ZIP header");
        let central_header = corrupt
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .expect("central ZIP header");
        corrupt[local_header + 14] ^= 1;
        corrupt[central_header + 16] ^= 1;
        assert!(
            parse_osv_zip_reader(
                Cursor::new(corrupt),
                [0; 32],
                17,
                1024,
                &snapshot_root,
                128 * 1024 * 1024,
                None,
                None,
                scope,
            )
            .is_err(),
            "archive checksum corruption must not be admitted"
        );
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("README.txt", zip::write::SimpleFileOptions::default())
            .expect("start ignored archive member");
        writer
            .write_all(b"ignored archive metadata")
            .expect("write ignored archive member");
        let mut ignored_member = writer.finish().expect("finish ignored ZIP").into_inner();
        let local_header = ignored_member
            .windows(4)
            .position(|bytes| bytes == b"PK\x03\x04")
            .expect("ignored local ZIP header");
        let central_header = ignored_member
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .expect("ignored central ZIP header");
        ignored_member[local_header + 14] ^= 1;
        ignored_member[central_header + 16] ^= 1;
        assert!(
            parse_osv_zip_reader(
                Cursor::new(ignored_member),
                [0; 32],
                17,
                1024,
                &snapshot_root,
                128 * 1024 * 1024,
                None,
                None,
                scope,
            )
            .is_err(),
            "corrupt ignored ZIP members are still source corruption"
        );
        for entry in fs::read_dir(&snapshot_root).expect("staging root") {
            let entry = entry.expect("snapshot root entry");
            assert!(
                !entry.file_type().expect("snapshot entry type").is_dir(),
                "failed feeds leave no staging or published generation directories: {:?}",
                entry.file_name()
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn osv_zip_spool_hashes_a_bounded_stream_and_rejects_overrun() {
        let bytes = b"bounded OSV archive";
        let mut spool = spool_advisory_zip(Cursor::new(bytes), bytes.len()).expect("exact bound");
        assert_eq!(spool.digest, *blake3::hash(bytes).as_bytes());
        spool.file.seek(SeekFrom::Start(0)).expect("rewind spool");
        let mut contents = Vec::new();
        spool.file.read_to_end(&mut contents).expect("read spool");
        assert_eq!(contents, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                spool
                    .file
                    .metadata()
                    .expect("spool metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(spool_advisory_zip(Cursor::new(bytes), bytes.len() - 1).is_err());
    }

    #[test]
    fn rustsec_tree_rejects_symlinks_and_bounds_each_file_before_reading() {
        let oversized_root = scratch();
        fs::write(oversized_root.join("RUSTSEC-TEST-1.toml"), b"12345")
            .expect("write oversized RustSec file");
        assert!(read_rustsec_tree(&oversized_root, 4, 17).is_err());
        let _ = fs::remove_dir_all(&oversized_root);

        #[cfg(unix)]
        {
            let symlink_root = scratch();
            fs::write(
                symlink_root.join("RUSTSEC-2025-0141.md"),
                include_bytes!("../../../../crates/advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md"),
            )
            .expect("write RustSec fixture");
            std::os::unix::fs::symlink(".", symlink_root.join("loop"))
                .expect("create recursive RustSec symlink");
            assert!(read_rustsec_tree(&symlink_root, 1024 * 1024, 17).is_err());
            let _ = fs::remove_dir_all(&symlink_root);
        }
    }

    #[test]
    fn advisory_source_identity_only_binds_osv_to_its_selected_scope() {
        use backend_engine::advisory::{
            AdvisoryAuthority, AdvisorySource, AuthorityFeed, OsvEcosystem, OsvFeedScope,
            PackageIdentity,
        };

        let all = Some(OsvFeedScope::All);
        let cargo = Some(OsvFeedScope::Ecosystem(OsvEcosystem::Cargo));
        assert_ne!(
            advisory_source_identity(AdvisorySource::Osv, "https://osv.example/feed", all),
            advisory_source_identity(AdvisorySource::Osv, "https://osv.example/feed", cargo),
            "OSV cache validators are partition-specific"
        );
        for source in [AdvisorySource::RustSec, AdvisorySource::Ghsa] {
            assert_eq!(
                advisory_source_identity(source, "https://advisories.example/feed", all),
                advisory_source_identity(source, "https://advisories.example/feed", cargo),
                "an OSV-only scope change cannot invalidate another authority"
            );
        }

        let ghsa_endpoint = "https://advisories.example/feed";
        let initial_identity = advisory_source_identity(AdvisorySource::Ghsa, ghsa_endpoint, all);
        let mut feed = AuthorityFeed::parse(
            AdvisorySource::Ghsa,
            br#"{"ghsa_id":"GHSA-test","malware_coverage":false,"vulnerabilities":[{"package":{"ecosystem":"npm","name":"demo"},"vulnerable_version_range":">= 1.0.0, < 2.0.0"}]}"#,
            10,
            None,
            None,
        )
        .expect("valid GHSA source object");
        feed.source_identity = Some(initial_identity);
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Ghsa]);
        authority.configure_source_identities([(AdvisorySource::Ghsa, initial_identity)]);
        authority.apply(feed).expect("admit GHSA source");

        authority.configure_osv_scope(cargo);
        let selected_identity =
            advisory_source_identity(AdvisorySource::Ghsa, ghsa_endpoint, cargo);
        authority.configure_source_identities([(AdvisorySource::Ghsa, selected_identity)]);
        let observation = authority.observe(
            &PackageIdentity {
                ecosystem: "npm".to_owned(),
                name: "demo".to_owned(),
                canonical_purl: None,
            },
            "1.1.0",
            false,
            false,
            10,
            false,
        );
        assert_eq!(
            observation.coverage,
            backend_engine::advisory::AdvisoryCoverage::Complete
        );
        assert_eq!(observation.advisories.len(), 1);
        assert_eq!(observation.advisories[0].key.native.id, "GHSA-test");
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
