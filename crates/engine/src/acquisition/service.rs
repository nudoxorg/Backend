use crate::registry::{
    AcquisitionError, AcquisitionError as RegistryAcquisitionError, CanonicalFeedV1, FeedSchema,
    PackageCoordinate, RegistryOwner, RegistryTransport, TransportFailure,
    admit_registry_coordinate,
};
use backend_execution::{
    Cancellation, OutputAdmission, OutputValidationError, ResultCoverage, UntrustedOutputClaim,
    WorkInterner, WorkKey,
};
use std::{
    collections::BTreeMap,
    fmt, io,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::delta::{AcquisitionDelta, DeltaChange, snapshot_changes};
use super::freshness::{
    FactFreshness, FactObservation, observed_facts_at, remember_fact_observation,
};
use super::identity::{
    ID_BYTES, IdentityError, ManifestEntry, RawArchiveObjectId, ReleaseClaim, SourceSnapshot,
    TreeManifest, now_millis,
};
use super::lease::LeaseStore;
use super::outcome::{
    AcquisitionOutcome, CorruptReason, NegativeCache, NegativeFact, NegativeFactKind, Offline,
    RejectReason, RetryAt, Unavailable, promote_bytes_outcome, promote_void_outcome,
};
use super::phase::{
    AcquisitionReceipt, AcquisitionRequest, Metadata, MetadataRecord, Policy, PublishedDelta,
    Resolve,
};
use super::retry::{CircuitBreaker, RetryPolicy};
use super::telemetry::{AcquisitionTelemetry, Telemetry};

/// Result consumed by product services after a registry effect completes.
///
/// The archive bytes are an immutable, reference-counted handoff.  The
/// snapshot, delta, and receipt are all derived from the same owner cursor and
/// are therefore deterministic for every coalesced caller.
#[derive(Clone, Debug)]
pub struct RegistryAcquisitionResult {
    /// Verified immutable object admitted by the registry owner.
    pub artifact: Arc<backend_store::TypedObject>,
    /// Source snapshot selected by the receipt's target root. Registry
    /// adapters use a catalog manifest here (coordinate paths to archive
    /// objects); archive extraction remains a separate local-service step.
    pub snapshot: Arc<SourceSnapshot>,
    /// Root-bound versioned change from the pre-effect source.
    pub delta: Arc<AcquisitionDelta>,
    /// Immutable receipt pairing the delta and publication root.
    pub receipt: Arc<AcquisitionReceipt>,
}

/// Production registry coordinator.
///
/// `RegistryOwner` remains responsible for decoding protocol pages, durable
/// journal phases, and 64 KiB archive streaming.  This service owns the
/// generic acquisition state machine around that adapter: process-local
/// singleflight, negative facts, retry/breaker decisions, leases, and the
/// canonical source snapshot/delta receipt consumed by local services.
pub struct AcquisitionService {
    owner: Arc<Mutex<RegistryOwner>>,
    coordinator: AcquisitionCoordinator,
    catalog_snapshots: Arc<Mutex<BTreeMap<CatalogSnapshotKey, Arc<SourceSnapshot>>>>,
    negative: NegativeCache,
    breaker: CircuitBreaker,
    leases: LeaseStore,
    /// Process-local observations; restart forces mutable facts to refresh.
    fact_observations: Arc<Mutex<BTreeMap<Arc<str>, FactObservation>>>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct CatalogSnapshotKey {
    source: [u8; ID_BYTES],
    cursor: [u8; ID_BYTES],
    policy_epoch: u64,
    facts_frontier: [u8; ID_BYTES],
}

pub(super) struct ExistingRegistryTransport;

impl RegistryTransport for ExistingRegistryTransport {
    fn fetch_page(
        &mut self,
        _request: crate::registry::FeedRequest,
    ) -> Result<crate::registry::TransportResult<crate::registry::FeedPage>, TransportFailure> {
        Err(TransportFailure::Configuration)
    }

    fn fetch_archive(
        &mut self,
        _package: &crate::registry::RemotePackage,
    ) -> Result<crate::registry::TransportResult<crate::registry::ArchiveArtifact>, TransportFailure>
    {
        Err(TransportFailure::Configuration)
    }
}

impl fmt::Debug for AcquisitionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquisitionService")
            .field("policy_epoch", &self.policy_epoch())
            .finish_non_exhaustive()
    }
}

impl AcquisitionService {
    /// Attaches generic acquisition ownership to one already-opened registry
    /// protocol owner.  The lease and breaker files are durable; the
    /// `WorkInterner` itself is deliberately process-local.
    pub fn from_owner(owner: RegistryOwner, root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        let leases = LeaseStore::open(root.join("coordination"))?;
        let breaker =
            CircuitBreaker::open_persisted(root.join("circuit.state"), 3, Duration::from_secs(5))?;
        Ok(Self {
            owner: Arc::new(Mutex::new(owner)),
            coordinator: AcquisitionCoordinator::new(64, 256),
            catalog_snapshots: Arc::new(Mutex::new(BTreeMap::new())),
            negative: NegativeCache::new(256),
            breaker,
            leases,
            fact_observations: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// Returns the stable nonzero policy/advisory frontier digest.
    #[must_use]
    pub fn policy_epoch(&self) -> u64 {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .policy_epoch()
    }

    /// Returns the authenticated registry facts root used by snapshot and
    /// GUI projections. Recovery must reproduce this exact root from the
    /// versioned journal without reopening archive bytes.
    #[must_use]
    pub fn facts_frontier(&self) -> [u8; ID_BYTES] {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .facts_frontier()
    }

    /// Stable source identity for request construction.
    #[must_use]
    pub fn source_id(&self) -> [u8; ID_BYTES] {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .source_id()
            .as_bytes()
    }

    /// Returns whether a coordinate is already in the durable owner catalog.
    #[must_use]
    pub fn contains(&self, coordinate: &PackageCoordinate) -> bool {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .published(coordinate)
            .is_some()
    }

    /// Returns a bounded clone of the durable catalog for read-only product
    /// projections.
    #[must_use]
    pub fn published_packages(&self) -> Vec<crate::registry::PublishedPackage> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .published_packages()
            .cloned()
            .collect()
    }

    /// Joins the normalized forge lineage facts for one borrowed publication.
    /// The association table remains owned by the registry journal; this
    /// method only clones the bounded surface projection.
    pub fn forge_sources_for(
        &self,
        package: &crate::registry::PublishedPackage,
    ) -> Result<Box<[backend_library::RegistryForgeAssociation]>, RegistryAcquisitionError> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forge_sources_for(package)
    }

    /// Appends a validated registry-to-forge association delta and advances
    /// the owner's authenticated facts frontier in bounded path work.
    pub fn link_forge_associations(
        &self,
        coordinate: &crate::registry::PackageCoordinate,
        associations: Box<[backend_library::RegistryForgeAssociation]>,
    ) -> Result<(), RegistryAcquisitionError> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .link_forge_associations(coordinate, associations)
    }

    /// Converts an admitted forge acquisition receipt into the normalized
    /// registry lineage event used by the same owner journal.
    pub fn link_forge_receipt(
        &self,
        coordinate: &crate::registry::PackageCoordinate,
        result: &crate::forge::ForgeAcquisitionResult,
    ) -> Result<(), RegistryAcquisitionError> {
        let registry = backend_library::PackageReference::Purl(coordinate.clone());
        let association = result
            .registry_association(registry)
            .map_err(|_| RegistryAcquisitionError::Transport(TransportFailure::Protocol))?;
        self.link_forge_associations(coordinate, vec![association].into_boxed_slice())
    }

    /// Reads one owner-admitted archive without changing acquisition state.
    pub fn read_artifact(
        &self,
        coordinate: &PackageCoordinate,
    ) -> Result<Option<Arc<backend_store::TypedObject>>, RegistryAcquisitionError> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_artifact(coordinate)
    }

    /// Exposes bounded process-local coordination telemetry.
    #[must_use]
    pub fn telemetry(&self) -> AcquisitionTelemetry {
        self.coordinator.telemetry().snapshot()
    }

    /// Coordinates one registry acquisition through the typed state machine.
    ///
    /// The caller supplies a transport adapter; it is never retained by the
    /// service.  The returned outcome is closed over every terminal class,
    /// including negative facts, breaker admission, corruption, and caller
    /// cancellation.
    pub fn acquire<T: RegistryTransport>(
        &self,
        request: &AcquisitionRequest,
        transport: &mut T,
    ) -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>> {
        let owner_source = self.source_id();
        if request.source != owner_source || request.schema != CanonicalFeedV1::VERSION {
            return AcquisitionOutcome::Rejected(RejectReason::Protocol);
        }
        let policy_epoch = self.policy_epoch();
        let request = request.clone().with_policy_epoch(policy_epoch);
        let key = request.work_key();
        let now = now_millis();
        if let Some(fact) = self.negative.get(*key.as_bytes(), now, policy_epoch) {
            return AcquisitionOutcome::NegativeFact(fact);
        }
        let owner = Arc::clone(&self.owner);
        let leases = self.leases.clone();
        let breaker = self.breaker.clone();
        let negative = self.negative.clone();
        let snapshot_cache = Arc::clone(&self.catalog_snapshots);
        let fact_observations = Arc::clone(&self.fact_observations);
        self.coordinator.coordinate_registry(&request, || {
            let _circuit = match breaker.allow(now_millis()) {
                Ok(permit) => permit,
                Err(open) => return AcquisitionOutcome::CircuitOpen(open),
            };
            let Some(_lease) = leases.acquire(key, Duration::from_secs(30)).ok().flatten() else {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            };
            let requested = request.coordinate.as_ref();
            // A request may lose a reservation to another coordinate's
            // deterministic commit. Retry from the newly visible cursor; the
            // winning archive writes remain content-addressed and are reused.
            for _ in 0..256 {
                let mut owner_guard = owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let base =
                    match registry_catalog_snapshot(&owner_guard, policy_epoch, &snapshot_cache) {
                        Ok(base) => base,
                        Err(outcome) => return promote_bytes_outcome(outcome),
                    };
                let offline = owner_guard.is_offline();
                let observed_at = observed_facts_at(
                    &fact_observations
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    request.coordinate.as_ref(),
                    policy_epoch,
                );
                let present = owner_guard
                    .published_packages()
                    .find(|package| package.coordinate.as_str() == requested)
                    .cloned();
                let up_to_date = present.is_some()
                    && (offline
                        || request.facts.max_age_millis == u64::MAX
                        || !request.facts.due(observed_at, now_millis()));
                if up_to_date {
                    let package = present.expect("up-to-date acquisition has a package");
                    breaker.success();
                    let current_epoch = owner_guard.policy_epoch();
                    remember_fact_observation(
                        &mut fact_observations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        requested,
                        now_millis(),
                        current_epoch,
                    );
                    if matches!(
                        package.facts.standing(),
                        crate::registry::ReleaseStanding::Yanked
                    ) {
                        let cursor = owner_guard.cursor().token();
                        let fact = NegativeFact {
                            kind: NegativeFactKind::Yanked,
                            authority: request.source,
                            source_proof: cursor,
                            cursor,
                            observed_at_millis: now_millis(),
                            expires_at_millis: now_millis().saturating_add(60_000),
                            policy_epoch: current_epoch,
                        };
                        negative.record(*key.as_bytes(), fact);
                        return AcquisitionOutcome::NegativeFact(fact);
                    }
                    if !owner_guard.cached_policy_allows(&package) {
                        let cursor = owner_guard.cursor().token();
                        let fact = NegativeFact {
                            kind: NegativeFactKind::AdvisoryBlocked,
                            authority: request.source,
                            source_proof: cursor,
                            cursor,
                            observed_at_millis: now_millis(),
                            expires_at_millis: now_millis().saturating_add(60_000),
                            policy_epoch: current_epoch,
                        };
                        negative.record(*key.as_bytes(), fact);
                        return AcquisitionOutcome::NegativeFact(fact);
                    }
                    let target = match registry_catalog_snapshot(
                        &owner_guard,
                        current_epoch,
                        &snapshot_cache,
                    ) {
                        Ok(target) => target,
                        Err(outcome) => return promote_bytes_outcome(outcome),
                    };
                    let result = match registry_result(
                        &owner_guard,
                        &base,
                        target,
                        package,
                        &request,
                        current_epoch,
                    ) {
                        Ok(result) => Arc::new(result),
                        Err(outcome) => return promote_bytes_outcome(outcome),
                    };
                    return AcquisitionOutcome::Hit(result);
                }
                if offline {
                    return AcquisitionOutcome::Offline(Offline {
                        source: request.source,
                    });
                }
                let intent = match owner_guard.begin_intent() {
                    Ok(intent) => intent,
                    Err(error) => {
                        return promote_bytes_outcome(registry_error_outcome(
                            error,
                            request.source,
                            &breaker,
                        ));
                    }
                };
                let feed_request = owner_guard.request_for_intent(intent);
                let stage = owner_guard.archive_stage();
                drop(owner_guard);

                let page = match transport
                    .fetch_page(feed_request)
                    .map_err(AcquisitionError::Transport)
                {
                    Ok(crate::registry::TransportResult::Available(page)) => page,
                    Ok(crate::registry::TransportResult::Unavailable) => {
                        breaker.failure(now_millis());
                        return AcquisitionOutcome::Unavailable(Unavailable {
                            source: request.source,
                        });
                    }
                    Ok(crate::registry::TransportResult::RetryAfter(delay)) => {
                        breaker.failure(now_millis());
                        let retry = RetryPolicy::default().delay(0, Some(delay));
                        return AcquisitionOutcome::RetryAt(RetryAt {
                            at_millis: now_millis()
                                .saturating_add(retry.as_millis().min(u128::from(u64::MAX)) as u64),
                            attempt: 0,
                        });
                    }
                    Ok(crate::registry::TransportResult::NotModified) => {
                        let mut owner_guard = owner
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let package = owner_guard
                            .published_packages()
                            .find(|package| package.coordinate.as_str() == requested)
                            .cloned();
                        match owner_guard.settle_reserved(intent) {
                            Ok(()) => {}
                            Err(AcquisitionError::StaleReservation) => continue,
                            Err(error) => {
                                return promote_bytes_outcome(registry_error_outcome(
                                    error,
                                    request.source,
                                    &breaker,
                                ));
                            }
                        }
                        let Some(package) = package else {
                            // A 304 confirms that the previously observed
                            // metadata representation is unchanged, but this
                            // owner does not retain enough page evidence to
                            // turn that validator response into an exact
                            // absence claim for an unseen coordinate.
                            return AcquisitionOutcome::Unavailable(Unavailable {
                                source: request.source,
                            });
                        };
                        breaker.success();
                        let current_epoch = owner_guard.policy_epoch();
                        remember_fact_observation(
                            &mut fact_observations
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                            requested,
                            now_millis(),
                            current_epoch,
                        );
                        if matches!(
                            package.facts.standing(),
                            crate::registry::ReleaseStanding::Yanked
                        ) {
                            let cursor = owner_guard.cursor().token();
                            let fact = NegativeFact {
                                kind: NegativeFactKind::Yanked,
                                authority: request.source,
                                source_proof: cursor,
                                cursor,
                                observed_at_millis: now_millis(),
                                expires_at_millis: now_millis().saturating_add(60_000),
                                policy_epoch: current_epoch,
                            };
                            negative.record(*key.as_bytes(), fact);
                            return AcquisitionOutcome::NegativeFact(fact);
                        }
                        if !owner_guard.cached_policy_allows(&package) {
                            let cursor = owner_guard.cursor().token();
                            let fact = NegativeFact {
                                kind: NegativeFactKind::AdvisoryBlocked,
                                authority: request.source,
                                source_proof: cursor,
                                cursor,
                                observed_at_millis: now_millis(),
                                expires_at_millis: now_millis().saturating_add(60_000),
                                policy_epoch: current_epoch,
                            };
                            negative.record(*key.as_bytes(), fact);
                            return AcquisitionOutcome::NegativeFact(fact);
                        }
                        let target = match registry_catalog_snapshot(
                            &owner_guard,
                            current_epoch,
                            &snapshot_cache,
                        ) {
                            Ok(target) => target,
                            Err(outcome) => return promote_bytes_outcome(outcome),
                        };
                        let result = match registry_result(
                            &owner_guard,
                            &base,
                            target,
                            package,
                            &request,
                            current_epoch,
                        ) {
                            Ok(result) => Arc::new(result),
                            Err(outcome) => return promote_bytes_outcome(outcome),
                        };
                        return AcquisitionOutcome::Hit(result);
                    }
                    Err(AcquisitionError::Transport(TransportFailure::Rejected(404))) => {
                        // A metadata 404 is a source-attested absence. Settle
                        // the prepared intent before retaining the negative
                        // fact so a restart cannot replay an already resolved
                        // miss as an unfinished network effect.
                        let mut owner_guard = owner
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        match owner_guard.settle_reserved(intent) {
                            Ok(()) => {}
                            Err(AcquisitionError::StaleReservation) => continue,
                            Err(error) => {
                                return promote_bytes_outcome(registry_error_outcome(
                                    error,
                                    request.source,
                                    &breaker,
                                ));
                            }
                        }
                        breaker.success();
                        let cursor = owner_guard.cursor().token();
                        let current_epoch = owner_guard.policy_epoch();
                        let fact = NegativeFact {
                            kind: NegativeFactKind::NotFound,
                            authority: request.source,
                            source_proof: cursor,
                            cursor,
                            observed_at_millis: now_millis(),
                            expires_at_millis: now_millis().saturating_add(60_000),
                            policy_epoch: current_epoch,
                        };
                        negative.record(*key.as_bytes(), fact);
                        return AcquisitionOutcome::NegativeFact(fact);
                    }
                    Err(error) => {
                        return promote_bytes_outcome(registry_error_outcome(
                            error,
                            request.source,
                            &breaker,
                        ));
                    }
                };
                if page.base != intent.cursor || page.packages.len() > feed_request.max_items {
                    return AcquisitionOutcome::Rejected(RejectReason::Protocol);
                }
                {
                    let owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Err(error) = owner_guard.validate_page_catalog_capacity(&page.packages) {
                        return promote_bytes_outcome(registry_error_outcome(
                            error,
                            request.source,
                            &breaker,
                        ));
                    }
                }
                if page.packages.is_empty() && page.next_token == intent.cursor.token() {
                    let mut owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match owner_guard.settle_reserved(intent) {
                        Ok(()) => {
                            let cursor = owner_guard.cursor().token();
                            let current_epoch = owner_guard.policy_epoch();
                            let fact = NegativeFact {
                                kind: NegativeFactKind::NotFound,
                                authority: request.source,
                                source_proof: cursor,
                                cursor,
                                observed_at_millis: now_millis(),
                                expires_at_millis: now_millis().saturating_add(60_000),
                                policy_epoch: current_epoch,
                            };
                            negative.record(*key.as_bytes(), fact);
                            return AcquisitionOutcome::NegativeFact(fact);
                        }
                        Err(AcquisitionError::StaleReservation) => continue,
                        Err(error) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                    }
                }
                let mut publications = Vec::with_capacity(page.packages.len());
                let mut downloads = Vec::new();
                {
                    let owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for package in &page.packages {
                        let advisory = owner_guard.advisory_for(package);
                        if matches!(
                            advisory.decision,
                            backend_advisory::AcquisitionDecision::Deny(_)
                        ) {
                            return AcquisitionOutcome::Rejected(RejectReason::Policy);
                        }
                        if let Some(existing) = owner_guard.published(&package.coordinate) {
                            if existing.upstream_integrity == package.integrity_version() {
                                publications.push(crate::registry::PublishedPackage {
                                    coordinate: existing.coordinate.clone(),
                                    registry: existing.registry.clone(),
                                    artifact: existing.artifact,
                                    raw_object: existing.raw_object,
                                    bytes: existing.bytes,
                                    provenance: package.provenance,
                                    upstream_integrity: package.integrity_version(),
                                    facts: package.facts,
                                    native_metadata: package.native_metadata.clone(),
                                    forge_source_ids: existing.forge_source_ids.clone(),
                                    advisory,
                                    dependency_facts: package.dependency_facts.clone(),
                                });
                                continue;
                            }
                        }
                        downloads.push((package.clone(), advisory));
                    }
                }
                let mut page_bytes = 0usize;
                for (package, advisory) in downloads {
                    let transfer = match stage.open_transfer(&package) {
                        Ok(transfer) => transfer,
                        Err(error) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                    };
                    let checkpoint = transfer.checkpoint();
                    let artifact = match transport
                        .fetch_archive_resumable(&package, checkpoint)
                        .map_err(AcquisitionError::Transport)
                    {
                        Ok(crate::registry::TransportResult::Available(artifact)) => artifact,
                        Ok(crate::registry::TransportResult::Unavailable) => {
                            breaker.failure(now_millis());
                            return AcquisitionOutcome::Unavailable(Unavailable {
                                source: request.source,
                            });
                        }
                        Ok(crate::registry::TransportResult::RetryAfter(delay)) => {
                            breaker.failure(now_millis());
                            let retry = RetryPolicy::default().delay(0, Some(delay));
                            return AcquisitionOutcome::RetryAt(RetryAt {
                                at_millis: now_millis().saturating_add(
                                    retry.as_millis().min(u128::from(u64::MAX)) as u64,
                                ),
                                attempt: 0,
                            });
                        }
                        Ok(crate::registry::TransportResult::NotModified) => {
                            return AcquisitionOutcome::Rejected(RejectReason::Protocol);
                        }
                        Err(error) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                    };
                    let artifact_bytes = match usize::try_from(artifact.length()) {
                        Ok(bytes) => bytes,
                        Err(_) => return AcquisitionOutcome::Rejected(RejectReason::Bounds),
                    };
                    page_bytes = page_bytes.saturating_add(artifact_bytes);
                    let publication = match stage.verify_and_store_with_transfer(
                        &package, artifact, page_bytes, advisory, transfer,
                    ) {
                        Ok(publication) => publication,
                        Err(error) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                    };
                    publications.push(publication);
                }
                publications.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
                let mut owner_guard = owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match owner_guard.commit_reserved_page(intent, &page, publications) {
                    Ok(_) => {}
                    Err(AcquisitionError::StaleReservation) => continue,
                    Err(error) => {
                        return promote_bytes_outcome(registry_error_outcome(
                            error,
                            request.source,
                            &breaker,
                        ));
                    }
                }
                let Some(package) = owner_guard
                    .published_packages()
                    .find(|package| package.coordinate.as_str() == requested)
                    .cloned()
                else {
                    let cursor = owner_guard.cursor().token();
                    let current_epoch = owner_guard.policy_epoch();
                    let fact = NegativeFact {
                        kind: NegativeFactKind::NotFound,
                        authority: request.source,
                        source_proof: cursor,
                        cursor,
                        observed_at_millis: now_millis(),
                        expires_at_millis: now_millis().saturating_add(60_000),
                        policy_epoch: current_epoch,
                    };
                    negative.record(*key.as_bytes(), fact);
                    return AcquisitionOutcome::NegativeFact(fact);
                };
                breaker.success();
                let current_epoch = owner_guard.policy_epoch();
                remember_fact_observation(
                    &mut fact_observations
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    requested,
                    now_millis(),
                    current_epoch,
                );
                if matches!(
                    package.facts.standing(),
                    crate::registry::ReleaseStanding::Yanked
                ) {
                    let cursor = owner_guard.cursor().token();
                    let fact = NegativeFact {
                        kind: NegativeFactKind::Yanked,
                        authority: request.source,
                        source_proof: cursor,
                        cursor,
                        observed_at_millis: now_millis(),
                        expires_at_millis: now_millis().saturating_add(60_000),
                        policy_epoch: current_epoch,
                    };
                    negative.record(*key.as_bytes(), fact);
                    return AcquisitionOutcome::NegativeFact(fact);
                }
                if !owner_guard.cached_policy_allows(&package) {
                    let cursor = owner_guard.cursor().token();
                    let fact = NegativeFact {
                        kind: NegativeFactKind::AdvisoryBlocked,
                        authority: request.source,
                        source_proof: cursor,
                        cursor,
                        observed_at_millis: now_millis(),
                        expires_at_millis: now_millis().saturating_add(60_000),
                        policy_epoch: current_epoch,
                    };
                    negative.record(*key.as_bytes(), fact);
                    return AcquisitionOutcome::NegativeFact(fact);
                }
                let target =
                    match registry_catalog_snapshot(&owner_guard, current_epoch, &snapshot_cache) {
                        Ok(target) => target,
                        Err(outcome) => return promote_bytes_outcome(outcome),
                    };
                let result = match registry_result(
                    &owner_guard,
                    &base,
                    target,
                    package,
                    &request,
                    current_epoch,
                ) {
                    Ok(result) => Arc::new(result),
                    Err(outcome) => return promote_bytes_outcome(outcome),
                };
                return AcquisitionOutcome::Hit(result);
            }
            AcquisitionOutcome::Rejected(RejectReason::Bounds)
        })
    }

    /// Coordinates a no-network hit for an object already admitted by the
    /// durable owner. This still emits the canonical source receipt/delta so
    /// local services consume the same evidence for a no-op reuse.
    pub fn ensure(
        &self,
        request: &AcquisitionRequest,
    ) -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>> {
        let request = request
            .clone()
            .with_fact_freshness(FactFreshness::max_age_millis(u64::MAX));
        self.acquire(&request, &mut ExistingRegistryTransport)
    }
}
pub(super) fn registry_catalog_snapshot(
    owner: &RegistryOwner,
    policy_epoch: u64,
    cache: &Arc<Mutex<BTreeMap<CatalogSnapshotKey, Arc<SourceSnapshot>>>>,
) -> Result<Arc<SourceSnapshot>, AcquisitionOutcome<Arc<[u8]>>> {
    let source = owner.source_id().as_bytes();
    let key = CatalogSnapshotKey {
        source,
        cursor: owner.cursor().token(),
        policy_epoch,
        facts_frontier: owner.facts_frontier(),
    };
    if let Some(snapshot) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
    {
        return Ok(snapshot);
    }
    let mut entries = Vec::with_capacity(owner.published_packages().len());
    let mut claims = Vec::with_capacity(owner.published_packages().len());
    for package in owner.published_packages() {
        // The owner receipt contains a verified archive claim and exact byte
        // extent. Rehydrate the object identity from those durable facts; a
        // warm snapshot must never reopen or hash every archive in the
        // catalog.
        let object = package.raw_object;
        let coordinate = Arc::from(package.coordinate.as_str());
        entries.push(ManifestEntry {
            path: coordinate,
            object,
            mode: 0,
        });
        let admitted = admit_registry_coordinate(&package.coordinate)
            .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
        let claim = ReleaseClaim::new_with_facts(
            source,
            package.coordinate.as_str(),
            admitted.version().as_str(),
            object,
            package.facts.version(),
        )
        .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
        claims.push(claim.id);
    }
    let manifest = Arc::new(
        TreeManifest::from_sorted(entries)
            .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?,
    );
    let snapshot = SourceSnapshot::new_with_frontier(
        source,
        owner.cursor().token(),
        policy_epoch,
        owner.facts_frontier(),
        manifest,
        claims,
    )
    .map(Arc::new)
    .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol));
    if let Ok(snapshot) = &snapshot {
        let mut cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(key, Arc::clone(snapshot));
        // The source cursor is monotonic; retaining a tiny tail keeps the
        // prior root available for delta consumers while bounding memory.
        while cache.len() > 8 {
            let Some(oldest) = cache.keys().next().copied() else {
                break;
            };
            cache.remove(&oldest);
        }
    }
    snapshot
}

pub(super) fn registry_result(
    owner: &RegistryOwner,
    base: &SourceSnapshot,
    target: Arc<SourceSnapshot>,
    package: crate::registry::PublishedPackage,
    request: &AcquisitionRequest,
    policy_epoch: u64,
) -> Result<RegistryAcquisitionResult, AcquisitionOutcome<Arc<[u8]>>> {
    let artifact = owner
        .read_artifact(&package.coordinate)
        .map_err(|_| AcquisitionOutcome::Corrupt(CorruptReason::Journal))?
        .ok_or(AcquisitionOutcome::Corrupt(CorruptReason::Journal))?;
    let object = package.raw_object;
    if request
        .artifact
        .is_some_and(|expected| expected != RawArchiveObjectId::from_bytes(artifact.bytes()))
    {
        return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
    }
    let admitted = admit_registry_coordinate(&package.coordinate)
        .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
    let effective = AcquisitionRequest::new(
        request.source,
        package.coordinate.as_str(),
        object,
        request.schema,
        policy_epoch,
    )
    .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
    let claim = ReleaseClaim::new(
        request.source,
        package.coordinate.as_str(),
        admitted.version().as_str(),
        object,
    )
    .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
    let metadata = Resolve::new(effective)
        .metadata(MetadataRecord {
            claim,
            length: package.bytes,
            source_proof: owner.cursor().token(),
        })
        .map_err(promote_void_outcome)?;
    let verified = metadata
        .object(object)
        .and_then(|object_phase| object_phase.verified(object))
        .map_err(promote_void_outcome)?;
    let policy_allowed = matches!(
        package.facts.standing(),
        crate::registry::ReleaseStanding::Available
    ) && !matches!(
        package.facts.security(),
        crate::registry::SecurityStanding::Affected { .. }
    );
    let published = verified
        .policy(policy_allowed)
        .map_err(promote_void_outcome)?;
    let changes = snapshot_changes(base, &target);
    let published = match published.publish(base, target.clone(), changes) {
        AcquisitionOutcome::Hit(published) => published,
        AcquisitionOutcome::Rejected(reason) => return Err(AcquisitionOutcome::Rejected(reason)),
        _ => return Err(AcquisitionOutcome::Corrupt(CorruptReason::Journal)),
    };
    Ok(RegistryAcquisitionResult {
        artifact,
        snapshot: target,
        delta: published.delta,
        receipt: published.receipt,
    })
}

pub(super) fn registry_error_outcome(
    error: RegistryAcquisitionError,
    source: [u8; ID_BYTES],
    breaker: &CircuitBreaker,
) -> AcquisitionOutcome<Arc<[u8]>> {
    match error {
        RegistryAcquisitionError::Transport(failure) => match failure {
            TransportFailure::Integrity => AcquisitionOutcome::Corrupt(CorruptReason::Integrity),
            TransportFailure::Overrun { .. } | TransportFailure::Bounds => {
                AcquisitionOutcome::Rejected(RejectReason::Bounds)
            }
            TransportFailure::Configuration
            | TransportFailure::Protocol
            | TransportFailure::Rejected(_)
            | TransportFailure::DownloadUnavailable => {
                AcquisitionOutcome::Rejected(RejectReason::Protocol)
            }
        },
        RegistryAcquisitionError::Io(_)
        | RegistryAcquisitionError::Journal(_)
        | RegistryAcquisitionError::CorruptJournal => {
            AcquisitionOutcome::Corrupt(CorruptReason::Journal)
        }
        RegistryAcquisitionError::Injected(_) => {
            breaker.failure(now_millis());
            AcquisitionOutcome::Unavailable(Unavailable { source })
        }
        RegistryAcquisitionError::AdvisoryDenied(_) => {
            AcquisitionOutcome::Rejected(RejectReason::Policy)
        }
        RegistryAcquisitionError::StaleReservation => {
            AcquisitionOutcome::Unavailable(Unavailable { source })
        }
        RegistryAcquisitionError::InvalidConfiguration
        | RegistryAcquisitionError::InvalidCoordinate
        | RegistryAcquisitionError::Bounds
        | RegistryAcquisitionError::Overrun { .. } => {
            AcquisitionOutcome::Rejected(RejectReason::Bounds)
        }
    }
}
/// One bounded shared effect slot used by the acquisition coordinator.
#[derive(Clone)]
pub(super) struct SharedSlotValue {
    outcome: AcquisitionOutcome<Arc<[u8]>>,
    registry: Option<Arc<RegistryAcquisitionResult>>,
}

pub(super) struct SharedSlot {
    result: Mutex<Option<SharedSlotValue>>,
    wake: Condvar,
}

impl SharedSlot {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            wake: Condvar::new(),
        }
    }
}

/// Process-local acquisition coordinator using the execution WorkInterner.
#[derive(Clone)]
pub struct AcquisitionCoordinator {
    interner: Arc<WorkInterner>,
    slots: Arc<Mutex<BTreeMap<WorkKey, Arc<SharedSlot>>>>,
    max_slots: usize,
    telemetry: Telemetry,
}

impl AcquisitionCoordinator {
    /// Creates a coordinator with bounded live work and follower demand.
    #[must_use]
    pub fn new(live: usize, followers: usize) -> Self {
        Self {
            interner: WorkInterner::new(live.max(1), followers),
            slots: Arc::new(Mutex::new(BTreeMap::new())),
            max_slots: live.max(1),
            telemetry: Telemetry::default(),
        }
    }

    /// Returns coordinator telemetry.
    #[must_use]
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry.clone()
    }

    /// Coalesces one metadata/object effect. Exactly one leader invokes
    /// `effect`; followers receive its immutable bytes or cancel their own
    /// demand without interrupting the leader.
    pub fn coordinate<F>(
        &self,
        request: &AcquisitionRequest,
        effect: F,
    ) -> AcquisitionOutcome<Arc<[u8]>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<[u8]>>,
    {
        let (cancellation, _) = Cancellation::new();
        self.coordinate_with_cancellation(request, &cancellation, effect)
    }

    /// Coalesces one demand while observing caller-owned cancellation. A
    /// cancelled follower drops only its own demand; the leader and remaining
    /// followers continue to share the same effect and receipt.
    pub fn coordinate_with_cancellation<F>(
        &self,
        request: &AcquisitionRequest,
        cancellation: &Cancellation,
        effect: F,
    ) -> AcquisitionOutcome<Arc<[u8]>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<[u8]>>,
    {
        self.coordinate_inner(
            request,
            cancellation,
            effect,
            |outcome| SharedSlotValue {
                outcome: outcome.clone(),
                registry: None,
            },
            |shared| shared.outcome.clone(),
        )
    }

    /// Coalesces a registry effect while retaining its immutable receipt and
    /// source snapshot in the same slot as the WorkInterner output. Followers
    /// therefore cannot observe a byte result from a different receipt map.
    pub fn coordinate_registry<F>(
        &self,
        request: &AcquisitionRequest,
        effect: F,
    ) -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>>,
    {
        let (cancellation, _) = Cancellation::new();
        self.coordinate_registry_with_cancellation(request, &cancellation, effect)
    }

    fn coordinate_registry_with_cancellation<F>(
        &self,
        request: &AcquisitionRequest,
        cancellation: &Cancellation,
        effect: F,
    ) -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<RegistryAcquisitionResult>>,
    {
        self.coordinate_inner(
            request,
            cancellation,
            effect,
            |outcome| match outcome {
                AcquisitionOutcome::Hit(result) => SharedSlotValue {
                    outcome: AcquisitionOutcome::Hit(result.artifact.bytes_shared()),
                    registry: Some(Arc::clone(result)),
                },
                AcquisitionOutcome::NegativeFact(fact) => SharedSlotValue {
                    outcome: AcquisitionOutcome::NegativeFact(*fact),
                    registry: None,
                },
                AcquisitionOutcome::RetryAt(retry) => SharedSlotValue {
                    outcome: AcquisitionOutcome::RetryAt(*retry),
                    registry: None,
                },
                AcquisitionOutcome::CircuitOpen(open) => SharedSlotValue {
                    outcome: AcquisitionOutcome::CircuitOpen(*open),
                    registry: None,
                },
                AcquisitionOutcome::Unavailable(unavailable) => SharedSlotValue {
                    outcome: AcquisitionOutcome::Unavailable(*unavailable),
                    registry: None,
                },
                AcquisitionOutcome::Offline(offline) => SharedSlotValue {
                    outcome: AcquisitionOutcome::Offline(*offline),
                    registry: None,
                },
                AcquisitionOutcome::Rejected(reason) => SharedSlotValue {
                    outcome: AcquisitionOutcome::Rejected(*reason),
                    registry: None,
                },
                AcquisitionOutcome::Corrupt(reason) => SharedSlotValue {
                    outcome: AcquisitionOutcome::Corrupt(*reason),
                    registry: None,
                },
                AcquisitionOutcome::Cancelled => SharedSlotValue {
                    outcome: AcquisitionOutcome::Cancelled,
                    registry: None,
                },
            },
            |shared| match (&shared.registry, &shared.outcome) {
                (Some(result), AcquisitionOutcome::Hit(_)) => {
                    AcquisitionOutcome::Hit(Arc::clone(result))
                }
                (_, AcquisitionOutcome::NegativeFact(fact)) => {
                    AcquisitionOutcome::NegativeFact(*fact)
                }
                (_, AcquisitionOutcome::RetryAt(retry)) => AcquisitionOutcome::RetryAt(*retry),
                (_, AcquisitionOutcome::CircuitOpen(open)) => {
                    AcquisitionOutcome::CircuitOpen(*open)
                }
                (_, AcquisitionOutcome::Unavailable(unavailable)) => {
                    AcquisitionOutcome::Unavailable(*unavailable)
                }
                (_, AcquisitionOutcome::Offline(offline)) => AcquisitionOutcome::Offline(*offline),
                (_, AcquisitionOutcome::Rejected(reason)) => AcquisitionOutcome::Rejected(*reason),
                (_, AcquisitionOutcome::Corrupt(reason)) => AcquisitionOutcome::Corrupt(*reason),
                (_, AcquisitionOutcome::Cancelled) => AcquisitionOutcome::Cancelled,
                (None, AcquisitionOutcome::Hit(_)) => {
                    AcquisitionOutcome::Corrupt(CorruptReason::Journal)
                }
            },
        )
    }

    fn coordinate_inner<T, F, ToShared, FromShared>(
        &self,
        request: &AcquisitionRequest,
        cancellation: &Cancellation,
        effect: F,
        to_shared: ToShared,
        from_shared: FromShared,
    ) -> AcquisitionOutcome<T>
    where
        T: Clone,
        F: FnOnce() -> AcquisitionOutcome<T>,
        ToShared: Fn(&AcquisitionOutcome<T>) -> SharedSlotValue,
        FromShared: Fn(&SharedSlotValue) -> AcquisitionOutcome<T>,
    {
        let key = request.work_key();
        let (slot, created_slot) = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slots.retain(|_, slot| {
                slot.result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                    || Arc::strong_count(slot) > 1
            });
            if slots.len() >= self.max_slots && !slots.contains_key(&key) {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            }
            match slots.entry(key) {
                std::collections::btree_map::Entry::Occupied(entry) => {
                    (Arc::clone(entry.get()), false)
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let slot = Arc::new(SharedSlot::new());
                    entry.insert(Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        let interned = match self.interner.intern(key) {
            Ok(interned) => interned,
            Err(_) => {
                if created_slot {
                    let mut slots = self
                        .slots
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if slots
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &slot))
                        && slot
                            .result
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .is_none()
                    {
                        slots.remove(&key);
                    }
                }
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            }
        };
        if !interned.is_leader() {
            self.telemetry.record_follower();
            let mut result = slot
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if cancellation.is_cancelled() || interned.is_cancelled() {
                    interned.cancel();
                    return AcquisitionOutcome::Cancelled;
                }
                if let Some(result) = result.as_ref() {
                    return from_shared(result);
                }
                let (next, _) = slot
                    .wake
                    .wait_timeout(result, Duration::from_millis(5))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                result = next;
            }
        }
        self.telemetry.record_leader();
        let outcome = if cancellation.is_cancelled() {
            AcquisitionOutcome::Cancelled
        } else {
            effect()
        };
        let shared = to_shared(&outcome);
        {
            let mut result = slot
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *result = Some(shared.clone());
            slot.wake.notify_all();
        }
        if let AcquisitionOutcome::Hit(bytes) = &shared.outcome {
            let output = backend_execution::OutputVersion::from_value(bytes.as_ref());
            let claim = UntrustedOutputClaim {
                output: output.to_bytes(),
                canonical_bytes: Arc::new(bytes.to_vec()),
                coverage: ResultCoverage::Complete,
            };
            let validator = |_: backend_execution::OutputVersion,
                             _: &[u8],
                             _: ResultCoverage|
             -> Result<(), OutputValidationError> { Ok(()) };
            if let Ok(admission) = OutputAdmission::admit(claim, &validator) {
                let _ = interned.complete(admission);
            } else {
                let _ = interned.terminate();
            }
        } else {
            let _ = interned.terminate();
        }
        outcome
    }
}
