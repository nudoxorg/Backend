mod machine;

use crate::registry::{
    AcquisitionError as RegistryAcquisitionError, RegistryOwner, RegistryTransport,
    TransportFailure, admit_registry_coordinate,
};
use backend_execution::{
    Cancellation, OutputAdmission, OutputValidationError, ResultCoverage, UntrustedOutputClaim,
    WorkInterner, WorkKey,
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::delta::{AcquisitionDelta, snapshot_changes};
use super::freshness::FactObservation;
use super::identity::{
    ID_BYTES, ManifestEntry, RawArchiveObjectId, ReleaseClaim, SourceSnapshot, TreeManifest,
    now_millis,
};
use super::lease::LeaseStore;
use super::outcome::{
    AcquisitionOutcome, CorruptReason, NegativeCache, RejectReason, Unavailable,
    promote_void_outcome,
};
use super::phase::{AcquisitionReceipt, AcquisitionRequest, MetadataRecord, Resolve};
use super::retry::CircuitBreaker;
use super::telemetry::Telemetry;

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
