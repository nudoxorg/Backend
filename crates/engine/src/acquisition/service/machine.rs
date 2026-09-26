//! Acquisition state machine for one registry owner.
//!
//! Callers supply a transport for one effect. Negative facts, the breaker,
//! and the durable catalog decide whether that effect runs.

use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::registry::{
    AcquisitionError, AcquisitionError as RegistryAcquisitionError, CanonicalFeedV1, FeedSchema,
    PackageCoordinate, RegistryOwner, RegistryTransport, TransportFailure,
};

use super::super::freshness::{FactFreshness, observed_facts_at, remember_fact_observation};
use super::super::identity::{ID_BYTES, now_millis};
use super::super::lease::LeaseStore;
use super::super::outcome::{
    AcquisitionOutcome, NegativeCache, NegativeFact, NegativeFactKind, Offline, RejectReason,
    RetryAt, Unavailable, promote_bytes_outcome,
};
use super::super::phase::AcquisitionRequest;
use super::super::retry::{CircuitBreaker, RetryPolicy};
use super::super::telemetry::AcquisitionTelemetry;
use super::{
    AcquisitionCoordinator, AcquisitionService, ExistingRegistryTransport,
    RegistryAcquisitionResult, registry_catalog_snapshot, registry_error_outcome, registry_result,
};

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
        coordinate: &PackageCoordinate,
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
        coordinate: &PackageCoordinate,
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
