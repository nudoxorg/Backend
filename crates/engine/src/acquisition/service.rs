use crate::registry::{
    AcquisitionError, AcquisitionError as RegistryAcquisitionError, CanonicalFeedV1, FeedSchema,
    PackageCoordinate, RegistryOwner, RegistryTransport, TransportFailure,
    admit_registry_coordinate,
};
use backend_execution::{
    Cancellation, OutputAdmission, OutputValidationError, ResultCoverage, UntrustedOutputClaim,
    WorkInterner, WorkKey, acquisition_work_key,
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
    AcquisitionRecordId, ID_BYTES, IdentityError, ManifestEntry, RawArchiveObjectId, ReleaseClaim,
    SourceSnapshot, TreeManifest, now_millis,
};
use super::lease::{AcquisitionLease, LeaseGuard, LeaseStore};
use super::outcome::{
    AcquisitionOutcome, CorruptReason, NegativeFact, NegativeFactKind, Offline, RejectReason,
    RetryAt, Unavailable, promote_bytes_outcome, promote_void_outcome,
};
use super::phase::{
    AcquisitionReceipt, AcquisitionRequest, Metadata, MetadataRecord, Policy, PublishedDelta,
    Resolve,
};
use super::receipt_store::{
    AcquisitionProductTerminal, AcquisitionReceiptStore, AcquisitionRecoveryRecord,
};
use super::retry::{CircuitBreaker, RetryPolicy};
use super::telemetry::{AcquisitionTelemetry, Telemetry};

const ACQUISITION_LEASE_TTL: Duration = Duration::from_secs(30);

/// Compile-time permission for the low-level registry owner journal writers.
/// The service module alone can construct this in production.
pub(crate) struct AcquisitionPublicationCapability<'fence>(
    std::marker::PhantomData<(&'fence AcquisitionLease, &'fence AcquisitionLease)>,
);

fn publication_capability<'fence>(
    _endpoint_fence: &'fence AcquisitionLease,
    _product_fence: &'fence AcquisitionLease,
) -> AcquisitionPublicationCapability<'fence> {
    AcquisitionPublicationCapability(std::marker::PhantomData)
}

#[cfg(test)]
pub(crate) fn test_publication_capability<'fence>() -> AcquisitionPublicationCapability<'fence> {
    AcquisitionPublicationCapability(std::marker::PhantomData)
}

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
    /// Source root immediately before the product intent was admitted.
    pub base_snapshot: Arc<SourceSnapshot>,
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
    breaker: CircuitBreaker,
    leases: LeaseStore,
    product_receipts: AcquisitionReceiptStore,
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

fn registry_journal_work_key(source: [u8; ID_BYTES]) -> WorkKey {
    acquisition_work_key(
        source,
        b"backend.registry.owner-journal.v1",
        [0; ID_BYTES],
        CanonicalFeedV1::VERSION,
        0,
    )
}

fn acquisition_error_to_io(error: AcquisitionError) -> io::Error {
    match error {
        AcquisitionError::Io(error) => error,
        AcquisitionError::Journal(crate::journal::JournalError::Io(error)) => error,
        AcquisitionError::Journal(error) => {
            io::Error::new(io::ErrorKind::InvalidData, error.to_string())
        }
        error => io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    }
}

fn with_registry_journal_fence<T>(
    leases: &LeaseStore,
    owner: &Arc<Mutex<RegistryOwner>>,
    source: [u8; ID_BYTES],
    publish: impl for<'fence> FnOnce(
        &mut RegistryOwner,
        AcquisitionPublicationCapability<'fence>,
    ) -> Result<T, AcquisitionError>,
) -> io::Result<Option<Result<T, AcquisitionError>>> {
    let mut gate = None;
    for _ in 0..32 {
        gate = leases.acquire(registry_journal_work_key(source), ACQUISITION_LEASE_TTL)?;
        if gate.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let Some(mut gate) = gate else {
        return Ok(None);
    };
    let mut owner = owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.publish_if_current(ACQUISITION_LEASE_TTL, |fence| {
        owner
            .refresh_from_external()
            .map_err(acquisition_error_to_io)?;
        // The capability is only a visibility boundary. Runtime exclusion is
        // supplied by this exact-token callback (or by the paired callback
        // below), and the borrow prevents retaining it beyond that callback.
        let capability = publication_capability(&fence, &fence);
        Ok(publish(&mut owner, capability))
    })
}

/// Publishes one request-owned journal mutation under both the shared
/// endpoint journal fence and the exact product-intent fence. The lease API
/// acquires both coordination stripes in sorted, deduplicated order, so
/// distinct coordinates are serialized without introducing lock-order races.
fn with_product_registry_journal_fence<T>(
    leases: &LeaseStore,
    owner: &Arc<Mutex<RegistryOwner>>,
    source: [u8; ID_BYTES],
    product_lease: &mut LeaseGuard,
    publish: impl for<'fence> FnOnce(
        &mut RegistryOwner,
        AcquisitionPublicationCapability<'fence>,
        &'fence AcquisitionLease,
        &'fence AcquisitionLease,
    ) -> Result<T, AcquisitionError>,
) -> io::Result<Option<Result<T, AcquisitionError>>> {
    let mut gate = None;
    for _ in 0..32 {
        gate = leases.acquire(registry_journal_work_key(source), ACQUISITION_LEASE_TTL)?;
        if gate.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let Some(mut gate) = gate else {
        return Ok(None);
    };
    let mut owner = owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.publish_if_both_current(
        product_lease,
        ACQUISITION_LEASE_TTL,
        |endpoint_fence, product_fence| {
            owner
                .refresh_from_external()
                .map_err(acquisition_error_to_io)?;
            // This scoped token controls crate-level mutation access only;
            // the paired guard callback performs the actual runtime fencing.
            let capability = publication_capability(&endpoint_fence, &product_fence);
            Ok(publish(
                &mut owner,
                capability,
                &endpoint_fence,
                &product_fence,
            ))
        },
    )
}

fn journal_fence_outcome<T>(source: [u8; ID_BYTES], error: io::Error) -> AcquisitionOutcome<T> {
    if error.kind() == io::ErrorKind::InvalidData {
        AcquisitionOutcome::Corrupt(CorruptReason::Journal)
    } else {
        AcquisitionOutcome::Unavailable(Unavailable { source })
    }
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
        let product_receipts = AcquisitionReceiptStore::open(root.join("product-receipts"))?;
        let breaker =
            CircuitBreaker::open_persisted(root.join("circuit.state"), 3, Duration::from_secs(5))?;
        Ok(Self {
            owner: Arc::new(Mutex::new(owner)),
            coordinator: AcquisitionCoordinator::new(64, 256),
            catalog_snapshots: Arc::new(Mutex::new(BTreeMap::new())),
            breaker,
            leases,
            product_receipts,
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

    /// Refreshes the endpoint journal suffix under its interprocess fence.
    /// Read projections call this before consulting the process-local owner,
    /// so commits from another process become visible without reopening or
    /// rescanning the complete catalog.
    pub fn refresh_external(&self) -> Result<(), RegistryAcquisitionError> {
        match with_registry_journal_fence(
            &self.leases,
            &self.owner,
            self.source_id(),
            |_, _capability| Ok(()),
        ) {
            Ok(Some(Ok(()))) => Ok(()),
            Ok(Some(Err(error))) => Err(error),
            Ok(None) => Err(RegistryAcquisitionError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "registry journal refresh fence is busy",
            ))),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                Err(RegistryAcquisitionError::CorruptJournal)
            }
            Err(error) => Err(RegistryAcquisitionError::Io(error)),
        }
    }

    /// Recovers the latest exact product receipt for one request intent.
    ///
    /// Recovery reads only the request head, receipt, snapshot, and sparse
    /// delta objects. It does not reopen archive payloads or scan the catalog.
    /// A cursor, facts root, or policy-epoch mismatch is a cache miss.
    pub fn recover_product_record(
        &self,
        request: &AcquisitionRequest,
    ) -> Result<Option<AcquisitionRecoveryRecord>, AcquisitionOutcome<()>> {
        let source = self.source_id();
        if request.source != source || request.schema != CanonicalFeedV1::VERSION {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Protocol));
        }
        let (cursor, facts_frontier, policy_epoch) =
            match with_registry_journal_fence(&self.leases, &self.owner, source, |owner, _cap| {
                Ok((
                    owner.cursor().token(),
                    owner.facts_frontier(),
                    owner.policy_epoch(),
                ))
            }) {
                Ok(Some(Ok(state))) => state,
                Ok(Some(Err(_))) => {
                    return Err(AcquisitionOutcome::Corrupt(CorruptReason::Journal));
                }
                Ok(None) => {
                    return Err(AcquisitionOutcome::Unavailable(Unavailable { source }));
                }
                Err(error) => return Err(journal_fence_outcome(source, error)),
            };
        let request = request.clone().with_policy_epoch(policy_epoch);
        self.product_receipts
            .recover(&request, cursor, facts_frontier, policy_epoch)
            .map_err(|_| AcquisitionOutcome::Corrupt(CorruptReason::Journal))
    }

    /// Catalog generation of the durable owner.
    ///
    /// This is a counter, not a clone of the catalog. Callers reuse a
    /// dependency index while the value is unchanged.
    #[must_use]
    pub fn catalog_generation(&self) -> u64 {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog_generation()
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

    /// Returns one durable catalog row without cloning or scanning the catalog.
    #[must_use]
    pub fn published_package(
        &self,
        coordinate: &PackageCoordinate,
    ) -> Option<crate::registry::PublishedPackage> {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .published(coordinate)
            .cloned()
    }

    /// Resolves the current advisory authority for one immutable publication.
    /// This read does not rewrite the acquisition receipt or archive object.
    #[must_use]
    pub fn advisory_for_published(
        &self,
        package: &crate::registry::PublishedPackage,
    ) -> backend_advisory::AdvisoryPackageDto {
        self.owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .advisory_for_published(package)
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
        match with_registry_journal_fence(
            &self.leases,
            &self.owner,
            self.source_id(),
            |owner, capability| {
                owner.link_forge_associations(&capability, coordinate, associations)
            },
        ) {
            Ok(Some(Ok(()))) => Ok(()),
            Ok(Some(Err(error))) => Err(error),
            Ok(None) => Err(RegistryAcquisitionError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "registry journal publication fence is busy",
            ))),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                Err(RegistryAcquisitionError::CorruptJournal)
            }
            Err(error) => Err(RegistryAcquisitionError::Io(error)),
        }
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
        match with_registry_journal_fence(&self.leases, &self.owner, request.source, |_, _cap| {
            Ok(())
        }) {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(_))) => return AcquisitionOutcome::Corrupt(CorruptReason::Journal),
            Ok(None) => {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            }
            Err(error) => return journal_fence_outcome(request.source, error),
        }
        let policy_epoch = self.policy_epoch();
        let request = request.clone().with_policy_epoch(policy_epoch);
        let product_key = request.receipt_work_key();
        let now = now_millis();
        let (owner_cursor, facts_frontier) = {
            let owner = self
                .owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (owner.cursor().token(), owner.facts_frontier())
        };
        match self.product_receipts.recover_negative(
            &request,
            owner_cursor,
            facts_frontier,
            policy_epoch,
        ) {
            Ok(Some(record)) => {
                let AcquisitionProductTerminal::NegativeFact(fact) = record.terminal else {
                    return AcquisitionOutcome::Corrupt(CorruptReason::Journal);
                };
                if fact.valid_at(now, policy_epoch) {
                    let owner = self
                        .owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !negative_record_matches_owner(&record, &fact, &owner, &request) {
                        return AcquisitionOutcome::Corrupt(CorruptReason::Journal);
                    }
                    return AcquisitionOutcome::NegativeFact(fact);
                }
            }
            Ok(_) => {}
            Err(_) => return AcquisitionOutcome::Corrupt(CorruptReason::Journal),
        }
        let owner = Arc::clone(&self.owner);
        let leases = self.leases.clone();
        let breaker = self.breaker.clone();
        let snapshot_cache = Arc::clone(&self.catalog_snapshots);
        let fact_observations = Arc::clone(&self.fact_observations);
        self.coordinator.coordinate_registry(&request, || {
            let _circuit = match breaker.allow(now_millis()) {
                Ok(permit) => permit,
                Err(open) => return AcquisitionOutcome::CircuitOpen(open),
            };
            let Some(mut lease) = leases
                .acquire(product_key, ACQUISITION_LEASE_TTL)
                .ok()
                .flatten()
            else {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            };
            let outcome = (|| {
                let requested = request.coordinate.as_ref();
                // A request may lose a reservation to another coordinate's
                // deterministic commit. Retry from the newly visible cursor; the
                // winning archive writes remain content-addressed and are reused.
                for _ in 0..256 {
                    match with_registry_journal_fence(&leases, &owner, request.source, |_, _cap| {
                        Ok(())
                    }) {
                        Ok(Some(Ok(()))) => {}
                        Ok(Some(Err(_))) => {
                            return AcquisitionOutcome::Corrupt(CorruptReason::Journal);
                        }
                        Ok(None) => {
                            return AcquisitionOutcome::Unavailable(Unavailable {
                                source: request.source,
                            });
                        }
                        Err(error) => return journal_fence_outcome(request.source, error),
                    }
                    let mut owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let offline = owner_guard.is_offline();
                    let observed_at = observed_facts_at(
                        &fact_observations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        request.coordinate.as_ref(),
                        policy_epoch,
                    );
                    let present_coordinate = PackageCoordinate::parse(requested).ok();
                    let present = present_coordinate
                        .as_ref()
                        .and_then(|coordinate| owner_guard.published(coordinate))
                        .cloned();
                    if present.as_ref().is_some_and(|package| {
                        request
                            .artifact
                            .is_some_and(|expected| expected != package.raw_object)
                    }) {
                        return AcquisitionOutcome::Corrupt(CorruptReason::Integrity);
                    }
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
                            return AcquisitionOutcome::NegativeFact(fact);
                        }
                        match self.product_receipts.recover(
                            &request,
                            owner_guard.cursor().token(),
                            owner_guard.facts_frontier(),
                            current_epoch,
                        ) {
                            Ok(Some(record)) => {
                                match registry_result_from_record(
                                    &owner_guard,
                                    &record,
                                    &package,
                                    &request,
                                ) {
                                    Ok(result) => {
                                        return AcquisitionOutcome::Hit(Arc::new(result));
                                    }
                                    Err(outcome) => return outcome,
                                }
                            }
                            Ok(None) => {}
                            Err(_) => return AcquisitionOutcome::Corrupt(CorruptReason::Journal),
                        }
                        let base = match registry_catalog_snapshot(
                            &owner_guard,
                            current_epoch,
                            &snapshot_cache,
                        ) {
                            Ok(base) => base,
                            Err(outcome) => return promote_bytes_outcome(outcome),
                        };
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
                    let base = match registry_catalog_snapshot(
                        &owner_guard,
                        policy_epoch,
                        &snapshot_cache,
                    ) {
                        Ok(base) => base,
                        Err(outcome) => return promote_bytes_outcome(outcome),
                    };
                    let base_cursor = owner_guard.cursor().token();
                    drop(owner_guard);
                    let intent = match with_product_registry_journal_fence(
                        &leases,
                        &owner,
                        request.source,
                        &mut lease,
                        |owner_guard, capability, _, _| owner_guard.begin_intent(&capability),
                    ) {
                        Ok(Some(Ok(intent))) => intent,
                        Ok(Some(Err(error))) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                        Ok(None) => {
                            return AcquisitionOutcome::Unavailable(Unavailable {
                                source: request.source,
                            });
                        }
                        Err(error) => return journal_fence_outcome(request.source, error),
                    };
                    if intent.cursor.token() != base_cursor {
                        continue;
                    }
                    let owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                                at_millis: now_millis().saturating_add(
                                    retry.as_millis().min(u128::from(u64::MAX)) as u64,
                                ),
                                attempt: 0,
                            });
                        }
                        Ok(crate::registry::TransportResult::NotModified) => {
                            let package = match with_product_registry_journal_fence(
                                &leases,
                                &owner,
                                request.source,
                                &mut lease,
                                |owner_guard, capability, _, _| {
                                    let package = present_coordinate
                                        .as_ref()
                                        .and_then(|coordinate| owner_guard.published(coordinate))
                                        .cloned();
                                    owner_guard.settle_reserved(&capability, intent)?;
                                    Ok(package)
                                },
                            ) {
                                Ok(Some(Ok(package))) => package,
                                Ok(Some(Err(AcquisitionError::StaleReservation))) => continue,
                                Ok(Some(Err(error))) => {
                                    return promote_bytes_outcome(registry_error_outcome(
                                        error,
                                        request.source,
                                        &breaker,
                                    ));
                                }
                                Ok(None) => {
                                    return AcquisitionOutcome::Unavailable(Unavailable {
                                        source: request.source,
                                    });
                                }
                                Err(error) => {
                                    return journal_fence_outcome(request.source, error);
                                }
                            };
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
                            let owner_guard = owner
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                            match with_product_registry_journal_fence(
                                &leases,
                                &owner,
                                request.source,
                                &mut lease,
                                |owner_guard, capability, _, _| {
                                    owner_guard.settle_reserved(&capability, intent)
                                },
                            ) {
                                Ok(Some(Ok(()))) => {}
                                Ok(Some(Err(AcquisitionError::StaleReservation))) => continue,
                                Ok(Some(Err(error))) => {
                                    return promote_bytes_outcome(registry_error_outcome(
                                        error,
                                        request.source,
                                        &breaker,
                                    ));
                                }
                                Ok(None) => {
                                    return AcquisitionOutcome::Unavailable(Unavailable {
                                        source: request.source,
                                    });
                                }
                                Err(error) => {
                                    return journal_fence_outcome(request.source, error);
                                }
                            }
                            let owner_guard = owner
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                        if let Err(error) =
                            owner_guard.validate_page_catalog_capacity(&page.packages)
                        {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                    }
                    if page.packages.is_empty() && page.next_token == intent.cursor.token() {
                        match with_product_registry_journal_fence(
                            &leases,
                            &owner,
                            request.source,
                            &mut lease,
                            |owner_guard, capability, _, _| {
                                owner_guard.settle_reserved(&capability, intent)
                            },
                        ) {
                            Ok(Some(Ok(()))) => {
                                let owner_guard = owner
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                                return AcquisitionOutcome::NegativeFact(fact);
                            }
                            Ok(Some(Err(AcquisitionError::StaleReservation))) => continue,
                            Ok(Some(Err(error))) => {
                                return promote_bytes_outcome(registry_error_outcome(
                                    error,
                                    request.source,
                                    &breaker,
                                ));
                            }
                            Ok(None) => {
                                return AcquisitionOutcome::Unavailable(Unavailable {
                                    source: request.source,
                                });
                            }
                            Err(error) => {
                                return journal_fence_outcome(request.source, error);
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
                    match with_product_registry_journal_fence(
                        &leases,
                        &owner,
                        request.source,
                        &mut lease,
                        |owner_guard, capability, _, _| {
                            owner_guard.commit_reserved_page(
                                &capability,
                                intent,
                                &page,
                                publications,
                            )
                        },
                    ) {
                        Ok(Some(Ok(_))) => {}
                        Ok(Some(Err(AcquisitionError::StaleReservation))) => continue,
                        Ok(Some(Err(error))) => {
                            return promote_bytes_outcome(registry_error_outcome(
                                error,
                                request.source,
                                &breaker,
                            ));
                        }
                        Ok(None) => {
                            return AcquisitionOutcome::Unavailable(Unavailable {
                                source: request.source,
                            });
                        }
                        Err(error) => return journal_fence_outcome(request.source, error),
                    }
                    let owner_guard = owner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let Some(package) = present_coordinate
                        .as_ref()
                        .and_then(|coordinate| owner_guard.published(coordinate))
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
                        return AcquisitionOutcome::NegativeFact(fact);
                    };
                    if request
                        .artifact
                        .is_some_and(|expected| expected != package.raw_object)
                    {
                        return AcquisitionOutcome::Corrupt(CorruptReason::Integrity);
                    }
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
                AcquisitionOutcome::Rejected(RejectReason::Bounds)
            })();
            let Some(record) = product_record_for_outcome(&request, &outcome, &owner) else {
                return match outcome {
                    AcquisitionOutcome::Hit(_) | AcquisitionOutcome::NegativeFact(_) => {
                        AcquisitionOutcome::Unavailable(Unavailable {
                            source: request.source,
                        })
                    }
                    outcome => outcome,
                };
            };
            let prepared = match self.product_receipts.prepare(record.clone()) {
                Ok(prepared) => prepared,
                Err(_) => return AcquisitionOutcome::Corrupt(CorruptReason::Journal),
            };
            let publication = with_product_registry_journal_fence(
                &leases,
                &owner,
                request.source,
                &mut lease,
                |owner_guard, _capability, endpoint_fence, product_fence| {
                    if endpoint_fence.key != registry_journal_work_key(request.source)
                        || product_fence.key != product_key
                        || !product_record_matches_current_owner(&record, &request, owner_guard)
                    {
                        return Err(AcquisitionError::StaleReservation);
                    }
                    self.product_receipts
                        .publish_head_fenced(&prepared, endpoint_fence, product_fence)
                        .map_err(AcquisitionError::Io)
                },
            );
            match publication {
                Ok(Some(Ok(_))) => outcome,
                Ok(Some(Err(AcquisitionError::StaleReservation))) => {
                    AcquisitionOutcome::Unavailable(Unavailable {
                        source: request.source,
                    })
                }
                Ok(Some(Err(error))) => {
                    promote_bytes_outcome(registry_error_outcome(error, request.source, &breaker))
                }
                Ok(None) => AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                }),
                Err(error) => journal_fence_outcome(request.source, error),
            }
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

fn product_record_for_outcome(
    request: &AcquisitionRequest,
    outcome: &AcquisitionOutcome<Arc<RegistryAcquisitionResult>>,
    owner: &Arc<Mutex<RegistryOwner>>,
) -> Option<AcquisitionRecoveryRecord> {
    match outcome {
        AcquisitionOutcome::Hit(result) => {
            let receipt = &result.receipt;
            if request.policy_epoch != receipt.policy_epoch
                || receipt.source_intent != request.source_intent()
            {
                return None;
            }
            Some(AcquisitionRecoveryRecord {
                id: AcquisitionRecordId::from_encoded([0; ID_BYTES]),
                source_intent: request.source_intent(),
                source: request.source,
                coordinate: Arc::clone(&request.coordinate),
                schema: request.schema,
                policy_epoch: receipt.policy_epoch,
                owner_cursor: receipt.owner_cursor,
                facts_frontier: result.snapshot.facts_frontier(),
                metadata_digest: Some(receipt.metadata_digest),
                raw_object: Some(receipt.raw_object),
                observed_at_millis: receipt.observed_at_millis,
                terminal: AcquisitionProductTerminal::Published,
                base_snapshot: Some(Arc::clone(&result.base_snapshot)),
                target_snapshot: Some(Arc::clone(&result.snapshot)),
                delta: Some(Arc::clone(&result.delta)),
                receipt: Some(Arc::clone(&result.receipt)),
            })
        }
        AcquisitionOutcome::NegativeFact(fact) => {
            if request.policy_epoch != fact.policy_epoch {
                return None;
            }
            let owner = owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if owner.cursor().token() != fact.cursor || owner.policy_epoch() != fact.policy_epoch {
                return None;
            }
            let coordinate =
                crate::registry::PackageCoordinate::parse(request.coordinate.as_ref()).ok();
            let package = coordinate
                .as_ref()
                .and_then(|coordinate| owner.published(coordinate));
            let (metadata_digest, raw_object) = match fact.kind {
                NegativeFactKind::Yanked | NegativeFactKind::AdvisoryBlocked => {
                    let package = package?;
                    (
                        Some(package.metadata_evidence_digest()),
                        Some(package.raw_object),
                    )
                }
                NegativeFactKind::NotFound | NegativeFactKind::Unsupported => (None, None),
            };
            Some(AcquisitionRecoveryRecord {
                id: AcquisitionRecordId::from_encoded([0; ID_BYTES]),
                source_intent: request.source_intent(),
                source: request.source,
                coordinate: Arc::clone(&request.coordinate),
                schema: request.schema,
                policy_epoch: fact.policy_epoch,
                owner_cursor: fact.cursor,
                facts_frontier: owner.facts_frontier(),
                metadata_digest,
                raw_object,
                observed_at_millis: fact.observed_at_millis,
                terminal: AcquisitionProductTerminal::NegativeFact(*fact),
                base_snapshot: None,
                target_snapshot: None,
                delta: None,
                receipt: None,
            })
        }
        _ => None,
    }
}

fn negative_record_matches_owner(
    record: &AcquisitionRecoveryRecord,
    fact: &NegativeFact,
    owner: &RegistryOwner,
    request: &AcquisitionRequest,
) -> bool {
    if record.source != request.source
        || record.source_intent != request.source_intent()
        || record.coordinate.as_ref() != request.coordinate.as_ref()
        || record.owner_cursor != owner.cursor().token()
        || record.facts_frontier != owner.facts_frontier()
        || record.policy_epoch != owner.policy_epoch()
        || fact.authority != record.source
        || fact.cursor != record.owner_cursor
        || fact.source_proof != record.owner_cursor
        || fact.policy_epoch != record.policy_epoch
        || fact.observed_at_millis != record.observed_at_millis
    {
        return false;
    }
    match fact.kind {
        NegativeFactKind::NotFound | NegativeFactKind::Unsupported => {
            record.metadata_digest.is_none() && record.raw_object.is_none()
        }
        NegativeFactKind::Yanked | NegativeFactKind::AdvisoryBlocked => {
            let Ok(coordinate) = PackageCoordinate::parse(request.coordinate.as_ref()) else {
                return false;
            };
            let Some(package) = owner.published(&coordinate) else {
                return false;
            };
            let evidence_matches = record.metadata_digest
                == Some(package.metadata_evidence_digest())
                && record.raw_object == Some(package.raw_object)
                && request
                    .artifact
                    .is_none_or(|expected| expected == package.raw_object);
            let fact_matches = match fact.kind {
                NegativeFactKind::Yanked => matches!(
                    package.facts.standing(),
                    crate::registry::ReleaseStanding::Yanked
                ),
                NegativeFactKind::AdvisoryBlocked => !owner.cached_policy_allows(package),
                NegativeFactKind::NotFound | NegativeFactKind::Unsupported => false,
            };
            evidence_matches && fact_matches
        }
    }
}

fn product_record_matches_current_owner(
    record: &AcquisitionRecoveryRecord,
    request: &AcquisitionRequest,
    owner: &RegistryOwner,
) -> bool {
    if record.source != request.source
        || record.source_intent != request.source_intent()
        || record.coordinate.as_ref() != request.coordinate.as_ref()
        || record.schema != request.schema
        || record.policy_epoch != owner.policy_epoch()
        || record.owner_cursor != owner.cursor().token()
        || record.facts_frontier != owner.facts_frontier()
    {
        return false;
    }
    match record.terminal {
        AcquisitionProductTerminal::Published => {
            let Ok(coordinate) = PackageCoordinate::parse(request.coordinate.as_ref()) else {
                return false;
            };
            let Some(package) = owner.published(&coordinate) else {
                return false;
            };
            record.metadata_digest == Some(package.metadata_evidence_digest())
                && record.raw_object == Some(package.raw_object)
                && request
                    .artifact
                    .is_none_or(|expected| expected == package.raw_object)
        }
        AcquisitionProductTerminal::NegativeFact(fact) => {
            negative_record_matches_owner(record, &fact, owner, request)
        }
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
    let packages = owner.published_packages();
    let mut entries = Vec::with_capacity(packages.len());
    let mut claims = Vec::with_capacity(packages.len());
    for package in packages {
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
    let claim = ReleaseClaim::new_with_facts(
        request.source,
        package.coordinate.as_str(),
        admitted.version().as_str(),
        object,
        package.facts.version(),
    )
    .map_err(|_| AcquisitionOutcome::Rejected(RejectReason::Protocol))?;
    let metadata = Resolve::new(effective)
        .metadata(MetadataRecord {
            claim,
            length: package.bytes,
            source_proof: owner.cursor().token(),
            evidence_digest: package.metadata_evidence_digest(),
            source_intent: request.source_intent(),
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
        base_snapshot: Arc::new(base.clone()),
        snapshot: target,
        delta: published.delta,
        receipt: published.receipt,
    })
}

fn registry_result_from_record(
    owner: &RegistryOwner,
    record: &AcquisitionRecoveryRecord,
    package: &crate::registry::PublishedPackage,
    request: &AcquisitionRequest,
) -> Result<RegistryAcquisitionResult, AcquisitionOutcome<Arc<RegistryAcquisitionResult>>> {
    let (Some(base_snapshot), Some(snapshot), Some(delta), Some(receipt)) = (
        record.base_snapshot.as_ref(),
        record.target_snapshot.as_ref(),
        record.delta.as_ref(),
        record.receipt.as_ref(),
    ) else {
        return Err(AcquisitionOutcome::Corrupt(CorruptReason::Journal));
    };
    let admitted = admit_registry_coordinate(&package.coordinate)
        .map_err(|_| AcquisitionOutcome::Corrupt(CorruptReason::Journal))?;
    let claim = ReleaseClaim::new_with_facts(
        request.source,
        package.coordinate.as_str(),
        admitted.version().as_str(),
        package.raw_object,
        package.facts.version(),
    )
    .map_err(|_| AcquisitionOutcome::Corrupt(CorruptReason::Journal))?;
    let selects_package = snapshot.source() == request.source
        && snapshot
            .manifest()
            .entries()
            .binary_search_by(|entry| entry.path.as_ref().cmp(package.coordinate.as_str()))
            .is_ok_and(|index| snapshot.manifest().entries()[index].object == package.raw_object)
        && snapshot.claims().binary_search(&claim.id).is_ok();
    if record.terminal != AcquisitionProductTerminal::Published
        || record.source != request.source
        || record.source_intent != request.source_intent()
        || record.coordinate.as_ref() != request.coordinate.as_ref()
        || record.schema != request.schema
        || record.policy_epoch != owner.policy_epoch()
        || record.owner_cursor != owner.cursor().token()
        || record.facts_frontier != owner.facts_frontier()
        || record.metadata_digest != Some(package.metadata_evidence_digest())
        || record.raw_object != Some(package.raw_object)
        || receipt.source_intent != record.source_intent
        || receipt.owner_cursor != record.owner_cursor
        || receipt.metadata_digest != package.metadata_evidence_digest()
        || receipt.raw_object != package.raw_object
        || receipt.policy_epoch != record.policy_epoch
        || !receipt.validate(record.source)
        || receipt.base != base_snapshot.id()
        || receipt.target != snapshot.id()
        || receipt.delta != delta.id()
        || delta.base() != base_snapshot.id()
        || delta.target() != snapshot.id()
        || delta.validate(base_snapshot).is_err()
        || !selects_package
        || request
            .artifact
            .is_some_and(|artifact| artifact != package.raw_object)
    {
        return Err(AcquisitionOutcome::Corrupt(CorruptReason::Journal));
    }
    let artifact = owner
        .read_artifact(&package.coordinate)
        .map_err(|_| AcquisitionOutcome::Corrupt(CorruptReason::Journal))?
        .ok_or(AcquisitionOutcome::Corrupt(CorruptReason::Journal))?;
    if RawArchiveObjectId::from_bytes(artifact.bytes()) != package.raw_object {
        return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
    }
    Ok(RegistryAcquisitionResult {
        artifact,
        base_snapshot: Arc::clone(base_snapshot),
        snapshot: Arc::clone(snapshot),
        delta: Arc::clone(delta),
        receipt: Arc::clone(receipt),
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
