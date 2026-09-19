//! Black-box contention tests over the public execution APIs.
#![cfg(test)]
#![forbid(unsafe_code)]
#![allow(
    clippy::panic,
    clippy::result_large_err,
    reason = "Adversarial black-box tests keep rich diagnostics and public result shapes."
)]

use backend_execution::{
    Admission, AdmissionRequest, AttemptError, AttemptManager, AttemptState,
    AuthorityValidationError, AuthorityVersion, Budget, CompletionCost, CostObservation,
    CostSnapshot, DeltaPlan, DeltaPlanner, Envelope, EnvelopeBudgets, HedgeSide, LocalCapability,
    LocalState, ObservationError, OutputAdmission, OutputEquivalence, OutputLookup,
    OutputValidationError, OutputVersion, PlacementClass, PlacementDecision, PlacementRequest,
    ReadManifestId, RecipeId, RemoteCapability, RemoteState, ResourceVector, ResultCoverage,
    ResultReceipt, ScheduleOutcome, ScheduleRequest, Scheduler, UntrustedAuthorityClaim,
    UntrustedOutputClaim, UntrustedResultReceipt, VersionedWorkIdentity, WaiterError, WorkInterner,
};
use backend_version::{CoverageWitness, Relation, partial_coverage};
use std::num::NonZeroU64;
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;

#[derive(Debug)]
struct TestRelation;
impl Relation for TestRelation {
    const DOMAIN: u8 = 0x22;
    const TYPE: u16 = 0x0001;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Debug)]
struct OtherRelation;
impl Relation for OtherRelation {
    const DOMAIN: u8 = 0x22;
    const TYPE: u16 = 0x0002;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn root(value: u64) -> backend_version::StateRoot<TestRelation> {
    backend_version::RelationState::<TestRelation>::from_entries(
        [(value, value)],
        CoverageWitness::Partial(partial_coverage(value)),
    )
    .unwrap_or_else(|_| unreachable!("unique test relation key"))
    .root()
}

fn identity(value: u64) -> VersionedWorkIdentity<TestRelation> {
    VersionedWorkIdentity::new(
        RecipeId::from_value(b"recipe-v1"),
        root(value),
        ReadManifestId::from_value(b"reads-v1"),
        AuthorityVersion::from_value(b"authority-v1"),
        OutputEquivalence::from_value(b"bytes-equivalent"),
    )
}

fn other_identity(value: u64) -> VersionedWorkIdentity<OtherRelation> {
    VersionedWorkIdentity::new(
        RecipeId::from_value(b"recipe-v1"),
        backend_version::RelationState::<OtherRelation>::from_entries(
            [(value, value)],
            CoverageWitness::Partial(partial_coverage(value)),
        )
        .unwrap_or_else(|_| unreachable!("other relation key"))
        .root(),
        ReadManifestId::from_value(b"reads-v1"),
        AuthorityVersion::from_value(b"authority-v1"),
        OutputEquivalence::from_value(b"bytes-equivalent"),
    )
}

fn output(bytes: &[u8]) -> OutputVersion {
    OutputVersion::from_value(bytes)
}

#[allow(clippy::unnecessary_wraps)]
fn accept_output(
    _: OutputVersion,
    _: &[u8],
    _: ResultCoverage,
) -> Result<(), OutputValidationError> {
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn accept_bound_output(
    _: &VersionedWorkIdentity<TestRelation>,
    _: OutputVersion,
    _: &[u8],
    _: ResultCoverage,
) -> Result<(), OutputValidationError> {
    Ok(())
}

fn reject_output(
    _: OutputVersion,
    _: &[u8],
    _: ResultCoverage,
) -> Result<(), OutputValidationError> {
    Err(OutputValidationError::ContractMismatch)
}

#[allow(clippy::unnecessary_wraps)]
fn accept_authority<R: Relation>(
    identity: &VersionedWorkIdentity<R>,
    _: &backend_execution::AttemptLease,
    claim: &UntrustedAuthorityClaim,
    admission: &OutputAdmission,
) -> Result<(), AuthorityValidationError> {
    if claim.authority != identity.authority.to_bytes()
        || admission.coverage() != ResultCoverage::Complete
    {
        return Err(AuthorityValidationError::BindingMismatch);
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn accept_local(
    _: &VersionedWorkIdentity<TestRelation>,
    _: LocalState,
) -> Result<(), ObservationError> {
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn accept_cost(
    _: &VersionedWorkIdentity<TestRelation>,
    _: &CostObservation,
) -> Result<(), ObservationError> {
    Ok(())
}

#[allow(clippy::default_constructed_unit_structs)]
fn capabilities() -> backend_replication::NegotiatedCapabilities {
    backend_replication::NegotiatedCapabilities {
        protocol: 1,
        schemas: Vec::new(),
        recipes: Vec::new(),
        limits: backend_replication::TransportLimits::default(),
        max_resources: backend_replication::ResourceEnvelope::UNBOUNDED,
    }
}

#[allow(clippy::too_many_arguments)]
fn observations(
    work: &VersionedWorkIdentity<TestRelation>,
    local: LocalState,
    remote: RemoteState,
    local_p95: u64,
    remote_p95: u64,
    upload: u64,
    download: u64,
    queue: u64,
    remote_warm: bool,
) -> (LocalCapability, RemoteCapability, CostSnapshot) {
    let local = LocalCapability::admit(work, local, 0, 100, &accept_local)
        .unwrap_or_else(|_| unreachable!("local capability"));
    let capabilities = capabilities();
    let remote_verifier = |_: &VersionedWorkIdentity<TestRelation>,
                           _: &backend_replication::NegotiatedCapabilities|
     -> Result<RemoteState, ObservationError> { Ok(remote) };
    let remote = RemoteCapability::admit(work, &capabilities, 0, 100, &remote_verifier)
        .unwrap_or_else(|_| unreachable!("remote capability"));
    let local_total = local_p95
        .checked_add(queue)
        .unwrap_or_else(|| unreachable!("local cost"));
    let cost = CostSnapshot::admit(
        work,
        CostObservation {
            local: local_total,
            remote: CompletionCost {
                client_queue: 0,
                round_trip: 0,
                input_transfer: upload,
                worker_queue: queue,
                warmup: u64::from(!remote_warm),
                execution: remote_p95,
                output_transfer: download,
                validation: 0,
                contention: 0,
            },
            observed_at: 0,
            expires_at: 100,
            confidence_per_mille: 1_000,
        },
        &accept_cost,
    )
    .unwrap_or_else(|_| unreachable!("cost snapshot"));
    (local, remote, cost)
}

fn admitted_output(bytes: &[u8]) -> OutputAdmission {
    OutputAdmission::admit(
        UntrustedOutputClaim {
            output: output(bytes).to_bytes(),
            canonical_bytes: Arc::new(bytes.to_vec()),
            coverage: ResultCoverage::Complete,
        },
        &accept_output,
    )
    .unwrap_or_else(|_| unreachable!("admitted output"))
}

fn admitted(
    identity: VersionedWorkIdentity<TestRelation>,
    lease: &backend_execution::AttemptLease,
    bytes: &[u8],
) -> ResultReceipt<TestRelation> {
    admitted_with_authority(identity, lease, bytes, 1, 1)
}

fn admitted_with_authority(
    identity: VersionedWorkIdentity<TestRelation>,
    lease: &backend_execution::AttemptLease,
    bytes: &[u8],
    authority_epoch: u64,
    revocation_version: u64,
) -> ResultReceipt<TestRelation> {
    ResultReceipt::admit_wire(
        identity,
        lease,
        UntrustedResultReceipt {
            key: identity.work_key().to_bytes(),
            input: identity.input.to_bytes(),
            recipe: identity.recipe.to_bytes(),
            read_manifest: identity.read_manifest.to_bytes(),
            authority: identity.authority.to_bytes(),
            output_equivalence: identity.output_equivalence.to_bytes(),
            output: output(bytes).to_bytes(),
            output_canonical_bytes: Arc::new(bytes.to_vec()),
            coverage: ResultCoverage::Complete,
            ordinal: lease.ordinal(),
            fence: *lease.fence().as_bytes(),
            authority_epoch,
            revocation_version,
            attestation: None,
        },
        &accept_bound_output,
        &accept_authority,
    )
    .unwrap_or_else(|_| unreachable!("admitted output"))
}

fn untrusted_receipt(
    identity: VersionedWorkIdentity<TestRelation>,
    lease: &backend_execution::AttemptLease,
    bytes: &[u8],
    authority_epoch: u64,
    revocation_version: u64,
) -> UntrustedResultReceipt {
    UntrustedResultReceipt {
        key: identity.work_key().to_bytes(),
        input: identity.input.to_bytes(),
        recipe: identity.recipe.to_bytes(),
        read_manifest: identity.read_manifest.to_bytes(),
        authority: identity.authority.to_bytes(),
        output_equivalence: identity.output_equivalence.to_bytes(),
        output: output(bytes).to_bytes(),
        output_canonical_bytes: Arc::new(bytes.to_vec()),
        coverage: ResultCoverage::Complete,
        ordinal: lease.ordinal(),
        fence: *lease.fence().as_bytes(),
        authority_epoch,
        revocation_version,
        attestation: None,
    }
}

#[test]
fn one_leader_and_live_non_eviction_under_real_contention() {
    let interner = WorkInterner::new(1, 32);
    let key = identity(1).work_key();
    let barrier = Arc::new(Barrier::new(17));
    let mut joins = Vec::new();
    for _ in 0..16 {
        let interner = Arc::clone(&interner);
        let barrier = Arc::clone(&barrier);
        joins.push(thread::spawn(move || {
            barrier.wait();
            interner.intern(key).ok()
        }));
    }
    barrier.wait();
    let handles = joins
        .into_iter()
        .filter_map(|join| join.join().ok().flatten())
        .collect::<Vec<_>>();
    assert_eq!(
        handles.iter().filter(|handle| handle.is_leader()).count(),
        1
    );
    assert!(interner.intern(identity(2).work_key()).is_err());
    drop(handles);
    assert!(interner.is_empty());
    let leader = interner
        .intern(identity(2).work_key())
        .unwrap_or_else(|_| unreachable!("released interner capacity"));
    assert!(leader.is_leader());
    leader
        .complete(admitted_output(b"done"))
        .unwrap_or_else(|_| unreachable!("leader completion"));
    assert!(interner.is_empty());
}

#[test]
fn work_key_retains_relation_schema_domain() {
    assert_ne!(identity(19).work_key(), other_identity(19).work_key());
    assert_eq!(identity(19), identity(19).clone());
}

#[test]
fn abandoned_leader_can_be_taken_over_without_evicting_live_work() {
    let interner = WorkInterner::new(2, 16);
    let abandoned_key = identity(20).work_key();
    let live_key = identity(21).work_key();
    let abandoned = interner
        .intern(abandoned_key)
        .unwrap_or_else(|_| unreachable!("abandoned leader"));
    let abandoned_follower = interner
        .register_waiter(abandoned_key)
        .unwrap_or_else(|_| unreachable!("abandoned follower"));
    let live = interner
        .intern(live_key)
        .unwrap_or_else(|_| unreachable!("live leader"));
    drop(abandoned);
    assert!(abandoned_follower.is_terminal());

    let barrier = Arc::new(Barrier::new(9));
    let joins = (0..8)
        .map(|_| {
            let interner = Arc::clone(&interner);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                interner.intern(abandoned_key).ok()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let mut replacements = joins
        .into_iter()
        .filter_map(|join| join.join().ok().flatten())
        .collect::<Vec<_>>();
    assert_eq!(
        replacements
            .iter()
            .filter(|handle| handle.is_leader())
            .count(),
        1
    );
    assert!(interner.intern(identity(22).work_key()).is_err());
    let leader_index = replacements
        .iter()
        .position(backend_execution::Interned::is_leader)
        .unwrap_or_else(|| unreachable!("takeover leader"));
    let replacement = replacements.swap_remove(leader_index);
    replacement
        .complete(admitted_output(b"replacement"))
        .unwrap_or_else(|_| unreachable!("replacement completion"));
    assert_eq!(abandoned_follower.output(), Some(output(b"replacement")));
    drop(abandoned_follower);
    drop(replacements);
    drop(live);
    assert!(interner.is_empty());
}

#[test]
fn waiter_registration_requires_an_existing_entry_and_bounds_followers() {
    let interner = WorkInterner::new(1, 2);
    let key = identity(23).work_key();
    let absent_barrier = Arc::new(Barrier::new(5));
    let absent_joins = (0..4)
        .map(|_| {
            let interner = Arc::clone(&interner);
            let barrier = Arc::clone(&absent_barrier);
            thread::spawn(move || {
                barrier.wait();
                interner.register_waiter(key)
            })
        })
        .collect::<Vec<_>>();
    absent_barrier.wait();
    for join in absent_joins {
        let result = join
            .join()
            .unwrap_or_else(|_| unreachable!("absent waiter thread"));
        assert!(matches!(result, Err(WaiterError::Unknown)));
    }

    let leader = interner
        .intern(key)
        .unwrap_or_else(|_| unreachable!("leader"));
    let follower_barrier = Arc::new(Barrier::new(5));
    let follower_joins = (0..4)
        .map(|_| {
            let interner = Arc::clone(&interner);
            let barrier = Arc::clone(&follower_barrier);
            thread::spawn(move || {
                barrier.wait();
                interner.register_waiter(key)
            })
        })
        .collect::<Vec<_>>();
    follower_barrier.wait();
    let mut followers = Vec::new();
    for join in follower_joins {
        let result = join
            .join()
            .unwrap_or_else(|_| unreachable!("follower thread"));
        match result {
            Ok(follower) => followers.push(follower),
            Err(WaiterError::Full) => {}
            Err(other) => unreachable!("unexpected waiter result: {other:?}"),
        }
    }
    assert_eq!(followers.len(), 2);
    drop(
        followers
            .pop()
            .unwrap_or_else(|| unreachable!("bounded follower")),
    );
    let third = interner
        .register_waiter(key)
        .unwrap_or_else(|_| unreachable!("bounded follower slot"));
    drop(followers);
    drop(third);
    drop(leader);
    assert!(interner.is_empty());
}

#[test]
fn simultaneous_terminal_completion_and_follower_drop_releases_entry() {
    let interner = WorkInterner::new(1, 8);
    let key = identity(24).work_key();
    let leader = interner
        .intern(key)
        .unwrap_or_else(|_| unreachable!("leader"));
    let followers = (0..8)
        .map(|_| {
            interner
                .register_waiter(key)
                .unwrap_or_else(|_| unreachable!("follower"))
        })
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(9));
    let joins = followers
        .into_iter()
        .map(|follower| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                drop(follower);
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    leader
        .complete(admitted_output(b"terminal"))
        .unwrap_or_else(|_| unreachable!("terminal completion"));
    for join in joins {
        assert!(join.join().is_ok());
    }
    assert!(interner.is_empty());
}

#[test]
fn admission_is_conserved_across_threads_and_envelopes() {
    let admission = Arc::new(Admission::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 4,
            bytes: 40,
            hedges: 0,
            ..Budget::zero()
        },
        background: Budget {
            operations: 2,
            bytes: 20,
            hedges: 1,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 2,
            bytes: 100,
            hedges: 0,
            ..Budget::zero()
        },
        compaction: Budget {
            operations: 1,
            bytes: 100,
            hedges: 0,
            ..Budget::zero()
        },
    }));
    let ready = Arc::new(Barrier::new(9));
    let release = Arc::new(Barrier::new(9));
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut joins = Vec::new();
    for _ in 0..8 {
        let admission = Arc::clone(&admission);
        let ready = Arc::clone(&ready);
        let release = Arc::clone(&release);
        let accepted = Arc::clone(&accepted);
        joins.push(thread::spawn(move || {
            let reservation =
                admission.try_admit_in(Envelope::Interactive, AdmissionRequest::new(1, 10));
            if reservation.is_ok() {
                accepted.fetch_add(1, Ordering::Relaxed);
            }
            ready.wait();
            release.wait();
            drop(reservation);
        }));
    }
    ready.wait();
    assert!(accepted.load(Ordering::Acquire) <= 4);
    assert_eq!(admission.available_in(Envelope::Interactive).operations, 0);
    release.wait();
    for join in joins {
        assert!(join.join().is_ok());
    }
    assert_eq!(admission.available_envelopes().interactive.operations, 4);

    let background = admission
        .try_admit_in(Envelope::Background, AdmissionRequest::new(1, 10).hedged())
        .unwrap_or_else(|_| unreachable!("background envelope"));
    assert_eq!(admission.available_in(Envelope::Background).hedges, 0);
    drop(background);
    assert_eq!(
        admission.available_in(Envelope::Background),
        Budget {
            operations: 2,
            bytes: 20,
            hedges: 1,
            ..Budget::zero()
        }
    );
}

#[test]
fn checked_admission_rejects_counter_overflow_without_mutation() {
    let admission = Admission::new(Budget {
        operations: u64::MAX,
        bytes: u64::MAX,
        hedges: 0,
        ..Budget::zero()
    });
    let first = admission
        .try_admit(AdmissionRequest::new(u64::MAX, u64::MAX))
        .unwrap_or_else(|_| unreachable!("maximum reservation fits exactly"));
    assert!(matches!(
        admission.try_admit(AdmissionRequest::new(1, 1)),
        Err(backend_execution::AdmissionError::Operations
            | backend_execution::AdmissionError::Overflow)
    ));
    drop(first);
    assert_eq!(
        admission.available(),
        Budget {
            operations: u64::MAX,
            bytes: u64::MAX,
            hedges: 0,
            ..Budget::zero()
        }
    );
}

#[test]
fn multidimensional_resources_are_checked_and_released() {
    let capacity = ResourceVector {
        cpu_millis: 10,
        memory_bytes: 20,
        network_bytes: 30,
        storage_bytes: 40,
    };
    let admission = Admission::new(Budget {
        operations: 2,
        bytes: 100,
        hedges: 0,
        resources: capacity,
    });
    let reservation = admission
        .try_admit(AdmissionRequest::new(1, 1).with_resources(ResourceVector {
            cpu_millis: 10,
            memory_bytes: 20,
            network_bytes: 30,
            storage_bytes: 40,
        }))
        .unwrap_or_else(|_| unreachable!("resource vector fits"));
    assert_eq!(admission.available().resources, ResourceVector::zero());
    assert!(matches!(
        admission.try_admit(AdmissionRequest::new(1, 1).with_resources(ResourceVector {
            cpu_millis: 1,
            memory_bytes: 0,
            network_bytes: 0,
            storage_bytes: 0,
        })),
        Err(backend_execution::AdmissionError::Resources)
    ));
    drop(reservation);
    assert_eq!(admission.available().resources, capacity);
}

#[test]
fn output_admission_requires_canonical_bytes_and_validator_acceptance() {
    let mismatched = OutputAdmission::admit(
        UntrustedOutputClaim {
            output: output(b"claimed").to_bytes(),
            canonical_bytes: Arc::new(b"different".to_vec()),
            coverage: ResultCoverage::Complete,
        },
        &accept_output,
    );
    assert!(matches!(
        mismatched,
        Err(backend_execution::OutputAdmissionError::ContentMismatch)
    ));

    assert!(matches!(
        OutputAdmission::admit(
            UntrustedOutputClaim {
                output: output(b"claimed").to_bytes(),
                canonical_bytes: Arc::new(b"claimed".to_vec()),
                coverage: ResultCoverage::Complete,
            },
            &reject_output,
        ),
        Err(backend_execution::OutputAdmissionError::Rejected(
            OutputValidationError::ContractMismatch
        ))
    ));
}

#[test]
fn concurrent_receipts_have_one_fenced_winner() {
    let manager = Arc::new(AttemptManager::new());
    let work = identity(3);
    let lease = manager
        .start(work, 4, 0)
        .unwrap_or_else(|_| unreachable!("attempt start"));
    let barrier = Arc::new(Barrier::new(9));
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut joins = Vec::new();
    for n in 0..8u8 {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        let accepted = Arc::clone(&accepted);
        let lease = lease.clone();
        joins.push(thread::spawn(move || {
            barrier.wait();
            let receipt = admitted(work, &lease, &[n]);
            if manager.accept(&receipt, 0).is_ok() {
                accepted.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    barrier.wait();
    for join in joins {
        assert!(join.join().is_ok());
    }
    assert_eq!(accepted.load(Ordering::Acquire), 1);
    assert_eq!(
        manager.current(work.work_key()).map(|lease| lease.state()),
        Some(AttemptState::Accepted)
    );
}

#[test]
fn heartbeat_and_takeover_preserve_monotonic_ordinal_and_reject_late_owner() {
    let manager = AttemptManager::with_lease_duration(2);
    let work = identity(5);
    let first = manager
        .start(work, 8, 10)
        .unwrap_or_else(|_| unreachable!("attempt start"));
    assert_eq!(
        manager.heartbeat(&first, 9),
        Err(AttemptError::TimeRegression)
    );
    let renewed = manager
        .heartbeat(&first, 11)
        .unwrap_or_else(|_| unreachable!("heartbeat"));
    assert_eq!(renewed.ordinal(), first.ordinal());
    assert!(manager.take_over(work, 8, 12).is_err());
    let second = manager
        .take_over(work, 9, 13)
        .unwrap_or_else(|_| unreachable!("expired takeover"));
    assert_eq!(second.ordinal(), first.ordinal() + 1);
    assert_ne!(second.fence(), first.fence());
    let late = admitted(work, &first, b"late");
    assert_eq!(manager.accept(&late, 13), Err(AttemptError::Stale));
    manager
        .transition(second.key(), second.fence(), AttemptState::Cancelled)
        .unwrap_or_else(|_| unreachable!("cancel expired replacement"));
    let restarted_without_retire = manager
        .start(work, 10, 14)
        .unwrap_or_else(|_| unreachable!("terminal cancellation is restartable"));
    assert_eq!(restarted_without_retire.ordinal(), second.ordinal() + 1);
    manager
        .transition(
            restarted_without_retire.key(),
            restarted_without_retire.fence(),
            AttemptState::Cancelled,
        )
        .unwrap_or_else(|_| unreachable!("cancel restarted attempt"));
    manager
        .retire(restarted_without_retire.key())
        .unwrap_or_else(|_| unreachable!("retire restarted attempt"));
    let third = manager
        .start(work, 11, 15)
        .unwrap_or_else(|_| unreachable!("monotonic restart"));
    assert_eq!(third.ordinal(), restarted_without_retire.ordinal() + 1);
    assert_eq!(third.owner_epoch(), 11);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The race test keeps barrier, winner, cancellation, and completion assertions together."
)]
fn scheduler_hedge_claim_race_selects_one_valid_winner_and_cancels_loser() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 32,
            ..Budget::zero()
        },
        background: Budget {
            operations: 1,
            bytes: 32,
            hedges: 1,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 32,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(23);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 16,
        resources: ResourceVector::zero(),
        hedge: true,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("hedge route"));
    let receipt = admitted(work, scheduled.lease(), b"hedge-winner");
    let scheduled = Arc::new(scheduled);
    let barrier = Arc::new(Barrier::new(3));
    let local_thread = {
        let scheduled = Arc::clone(&scheduled);
        let barrier = Arc::clone(&barrier);
        let receipt = receipt.clone();
        thread::spawn(move || {
            barrier.wait();
            (
                HedgeSide::Local,
                scheduled.claim(HedgeSide::Local, &receipt, 0),
            )
        })
    };
    let remote_thread = {
        let scheduled = Arc::clone(&scheduled);
        let barrier = Arc::clone(&barrier);
        let receipt = receipt.clone();
        thread::spawn(move || {
            barrier.wait();
            (
                HedgeSide::Remote,
                scheduled.claim(HedgeSide::Remote, &receipt, 0),
            )
        })
    };
    barrier.wait();
    let local_result = local_thread
        .join()
        .unwrap_or_else(|_| unreachable!("local hedge worker"));
    let remote_result = remote_thread
        .join()
        .unwrap_or_else(|_| unreachable!("remote hedge worker"));
    let results = [local_result, remote_result];
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        1
    );
    let winner = results
        .iter()
        .find_map(|(side, result)| result.is_ok().then_some(*side))
        .unwrap_or_else(|| unreachable!("one hedge winner"));
    let race = scheduled
        .race()
        .unwrap_or_else(|| unreachable!("hedge race"));
    assert_eq!(race.winner(), Some(winner));
    let loser = match winner {
        HedgeSide::Local => HedgeSide::Remote,
        HedgeSide::Remote => HedgeSide::Local,
    };
    assert!(race.is_cancelled(loser));
    let scheduled = Arc::try_unwrap(scheduled)
        .unwrap_or_else(|_| unreachable!("only completion owner remains"));
    engine
        .complete_side(scheduled, winner, &receipt, 0)
        .unwrap_or_else(|_| unreachable!("hedge completion"));
}

#[test]
fn scheduler_hedge_cancellation_fences_late_candidate() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 16,
            ..Budget::zero()
        },
        background: Budget {
            operations: 1,
            bytes: 16,
            hedges: 1,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 16,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(24);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 8,
        resources: ResourceVector::zero(),
        hedge: true,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("hedge route"));
    let receipt = admitted(work, scheduled.lease(), b"winner");
    let race = scheduled
        .race()
        .cloned()
        .unwrap_or_else(|| unreachable!("hedge race"));
    scheduled
        .claim(HedgeSide::Remote, &receipt, 0)
        .unwrap_or_else(|_| unreachable!("first valid side"));
    assert!(race.is_cancelled(HedgeSide::Local));
    assert_eq!(
        scheduled.claim(HedgeSide::Local, &receipt, 0),
        Err(backend_execution::ScheduleError::Hedge(
            backend_execution::HedgeError::AlreadyWon
        ))
    );
    engine
        .complete_side(scheduled, HedgeSide::Remote, &receipt, 0)
        .unwrap_or_else(|_| unreachable!("hedge completion"));
}

#[test]
fn scheduler_local_reservation_completes_and_reuses_exact_output() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 2,
            bytes: 20,
            hedges: 0,
            ..Budget::zero()
        },
        background: Budget {
            operations: 2,
            bytes: 20,
            hedges: 0,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 20,
            hedges: 0,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(6);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Available,
        2,
        20,
        10,
        10,
        0,
        false,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector::zero(),
        hedge: false,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled_attempt = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("local route"));
    assert_eq!(scheduled_attempt.decision(), PlacementDecision::Local);
    assert_eq!(engine.admission().available().operations, 1);
    let receipt = admitted(work, scheduled_attempt.lease(), b"result");
    let completed = engine
        .complete_with_reuse_policy(
            scheduled_attempt,
            &receipt,
            request.now,
            true,
            NonZeroU64::new(1),
        )
        .unwrap_or_else(|_| unreachable!("completion"));
    assert_eq!(completed.output(), output(b"result"));
    assert_eq!(engine.admission().available().operations, 2);
    let context = completed
        .reusable()
        .unwrap_or_else(|| unreachable!("reusable completion"))
        .context();
    let reused = engine.schedule_or_reuse_with_context(request, Some(&context));
    assert!(matches!(
        reused,
        Ok(ScheduleOutcome::Reused(found)) if found.output() == output(b"result")
    ));
}

#[test]
fn scheduler_completion_keeps_the_original_scheduled_fence() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 2,
            bytes: 20,
            hedges: 0,
            ..Budget::zero()
        },
        background: Budget::zero(),
        transfer: Budget::zero(),
        compaction: Budget::zero(),
    });
    let work = identity(27);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Unavailable,
        1,
        100,
        0,
        0,
        0,
        false,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector::zero(),
        hedge: false,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("local route"));
    let replacement = engine
        .attempts()
        .take_over(work, 2, 1)
        .unwrap_or_else(|_| unreachable!("expired replacement"));
    let replacement_receipt = admitted(work, &replacement, b"replacement");
    assert_eq!(
        engine.complete(scheduled, &replacement_receipt, 1),
        Err(backend_execution::ScheduleError::Attempt(
            AttemptError::Stale
        ))
    );
    engine
        .attempts()
        .transition(
            replacement.key(),
            replacement.fence(),
            AttemptState::Cancelled,
        )
        .unwrap_or_else(|_| unreachable!("cancel replacement"));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The test keeps every reservation, fence, token, and deadline assertion together."
)]
fn remote_fallback_reservation_is_held_and_transitions_at_deadline() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            ..Budget::zero()
        },
        background: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(25);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::RemoteOptional,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector::zero(),
        hedge: false,
        pure: true,
        fallback_reserved: true,
        fallback_deadline: Some(10),
    };
    let mut scheduled_attempt = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("remote route with fallback"));
    let remote_token = scheduled_attempt
        .cancellation(HedgeSide::Remote)
        .unwrap_or_else(|| unreachable!("remote cancellation token"));
    let local_token = scheduled_attempt
        .cancellation(HedgeSide::Local)
        .unwrap_or_else(|| unreachable!("fallback cancellation token"));
    assert!(!remote_token.is_cancelled());
    assert!(!local_token.is_cancelled());
    let old_lease = scheduled_attempt.lease().clone();
    assert_eq!(scheduled_attempt.decision(), PlacementDecision::Remote);
    assert!(scheduled_attempt.has_fallback());
    assert_eq!(engine.admission().available().operations, 0);
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .operations,
        0
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Transfer)
            .operations,
        0
    );
    assert_eq!(
        scheduled_attempt.activate_fallback(9),
        Err(backend_execution::ScheduleError::FallbackNotDue)
    );
    scheduled_attempt
        .activate_fallback(10)
        .unwrap_or_else(|_| unreachable!("fallback deadline"));
    assert!(remote_token.is_cancelled());
    assert!(!local_token.is_cancelled());
    assert_eq!(scheduled_attempt.decision(), PlacementDecision::Local);
    let late_remote = admitted(work, &old_lease, b"remote-late");
    assert_eq!(
        engine.attempts().accept(&late_remote, 10),
        Err(AttemptError::Stale)
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .operations,
        1
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Transfer)
            .operations,
        1
    );
    let local_receipt = admitted(work, scheduled_attempt.lease(), b"local-fallback");
    engine
        .complete(scheduled_attempt, &local_receipt, 10)
        .unwrap_or_else(|_| unreachable!("local fallback completion"));
    assert_eq!(engine.admission().available().operations, 1);
}

#[test]
fn scheduler_hedge_accounts_both_sides_and_cancels_loser() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            ..Budget::zero()
        },
        background: Budget {
            operations: 1,
            bytes: 10,
            hedges: 1,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(26);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector::zero(),
        hedge: true,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled_attempt = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("hedge route"));
    assert_eq!(scheduled_attempt.decision(), PlacementDecision::Hedge);
    assert!(scheduled_attempt.is_hedged());
    assert_eq!(engine.admission().available().operations, 0);
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .operations,
        0
    );
    assert_eq!(
        engine.admission().available_in(Envelope::Background).hedges,
        0
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Transfer)
            .operations,
        0
    );
    let race = scheduled_attempt.race().map(Arc::clone);
    assert!(race.is_some());
    let Some(race) = race else { return };
    let receipt = admitted(work, scheduled_attempt.lease(), b"remote");
    let completion = engine
        .complete_side(scheduled_attempt, HedgeSide::Remote, &receipt, request.now)
        .unwrap_or_else(|_| unreachable!("hedge completion"));
    assert_eq!(completion.decision(), PlacementDecision::Hedge);
    assert_eq!(race.winner(), Some(HedgeSide::Remote));
    assert!(race.is_cancelled(HedgeSide::Local));
    assert_eq!(engine.admission().available().operations, 1);
    assert_eq!(
        engine.admission().available_in(Envelope::Background).hedges,
        1
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Transfer)
            .operations,
        1
    );
}

#[test]
fn scheduler_charges_multidimensional_demand_for_each_hedged_side() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 10,
            hedges: 0,
            resources: ResourceVector {
                cpu_millis: 7,
                ..ResourceVector::zero()
            },
        },
        background: Budget {
            operations: 1,
            bytes: 10,
            hedges: 1,
            resources: ResourceVector {
                cpu_millis: 7,
                ..ResourceVector::zero()
            },
        },
        transfer: Budget {
            operations: 1,
            bytes: 10,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    });
    let work = identity(35);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector {
            cpu_millis: 7,
            ..ResourceVector::zero()
        },
        hedge: true,
        pure: true,
        fallback_reserved: false,
        fallback_deadline: None,
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("hedged route"));
    assert_eq!(
        engine.admission().available().resources,
        ResourceVector::zero()
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .resources,
        ResourceVector::zero()
    );
    let receipt = admitted(work, scheduled.lease(), b"winner");
    engine
        .complete_side(scheduled, HedgeSide::Remote, &receipt, 0)
        .unwrap_or_else(|_| unreachable!("hedge completion"));
    assert_eq!(
        engine.admission().available().resources,
        ResourceVector {
            cpu_millis: 7,
            ..ResourceVector::zero()
        }
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .resources,
        ResourceVector {
            cpu_millis: 7,
            ..ResourceVector::zero()
        }
    );
}

#[test]
fn remote_only_policy_is_explicit_and_completion_cost_is_complete() {
    let work = identity(31);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Unavailable,
        RemoteState::Unavailable,
        1,
        1,
        0,
        0,
        0,
        true,
    );
    let request = PlacementRequest {
        local,
        remote,
        costs,
        now: 0,
        class: PlacementClass::RemoteRequired,
        pure: true,
        hedge_budget: false,
        fallback_reserved: false,
    };
    assert_eq!(
        backend_execution::choose_placement_with(request),
        PlacementDecision::RemoteOnlyUnavailable
    );
    assert_eq!(request.costs.remote().total(), 1);
}

#[test]
fn forged_remote_warm_state_cannot_be_admitted_or_routed() {
    let work = identity(32);
    let capabilities = capabilities();
    let rejected = RemoteCapability::admit(
        &work,
        &capabilities,
        0,
        10,
        &|_: &VersionedWorkIdentity<TestRelation>,
          _: &backend_replication::NegotiatedCapabilities|
         -> Result<RemoteState, ObservationError> { Err(ObservationError::Rejected) },
    );
    assert_eq!(rejected, Err(ObservationError::Rejected));

    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Unavailable,
        10,
        1,
        0,
        0,
        0,
        true,
    );
    assert_eq!(
        backend_execution::choose_placement_with(PlacementRequest {
            local,
            remote,
            costs,
            now: 0,
            class: PlacementClass::RemoteOptional,
            pure: true,
            hedge_budget: false,
            fallback_reserved: true,
        }),
        PlacementDecision::Local
    );
}

#[test]
fn expired_or_low_confidence_costs_keep_optional_work_local() {
    let work = identity(33);
    let local = LocalCapability::admit(&work, LocalState::Ready, 0, 10, &accept_local)
        .unwrap_or_else(|_| unreachable!("local capability"));
    let capabilities = capabilities();
    let remote = RemoteCapability::admit(
        &work,
        &capabilities,
        0,
        10,
        &|_: &VersionedWorkIdentity<TestRelation>,
          _: &backend_replication::NegotiatedCapabilities|
         -> Result<RemoteState, ObservationError> { Ok(RemoteState::Warm) },
    )
    .unwrap_or_else(|_| unreachable!("remote capability"));
    for confidence in [1, 499, 500] {
        let costs = CostSnapshot::admit(
            &work,
            CostObservation {
                local: 100,
                remote: CompletionCost {
                    execution: 1,
                    ..CompletionCost::default()
                },
                observed_at: 0,
                expires_at: 2,
                confidence_per_mille: confidence,
            },
            &accept_cost,
        )
        .unwrap_or_else(|_| unreachable!("cost snapshot"));
        let now = if confidence == 500 { 2 } else { 1 };
        let decision = backend_execution::choose_placement_with(PlacementRequest {
            local,
            remote,
            costs,
            now,
            class: PlacementClass::RemoteOptional,
            pure: true,
            hedge_budget: false,
            fallback_reserved: true,
        });
        assert_eq!(decision, PlacementDecision::Local);
    }
}

#[test]
fn local_preferred_and_remote_outage_start_local_without_waiting() {
    let work = identity(34);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Unavailable,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    assert_eq!(
        backend_execution::choose_placement_with(PlacementRequest {
            local,
            remote,
            costs,
            now: 0,
            class: PlacementClass::LocalPreferred,
            pure: true,
            hedge_budget: false,
            fallback_reserved: false,
        }),
        PlacementDecision::Local
    );
    assert_eq!(
        backend_execution::choose_placement_with(PlacementRequest {
            local,
            remote,
            costs,
            now: 0,
            class: PlacementClass::RemoteOptional,
            pure: true,
            hedge_budget: false,
            fallback_reserved: false,
        }),
        PlacementDecision::Local
    );
}

#[test]
fn remote_admission_outage_falls_back_to_local_immediately() {
    let engine = Scheduler::with_envelopes(EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 10,
            ..Budget::zero()
        },
        background: Budget::zero(),
        transfer: Budget::zero(),
        compaction: Budget::zero(),
    });
    let work = identity(36);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::RemoteOptional,
        local,
        remote,
        costs,
        bytes: 10,
        resources: ResourceVector::zero(),
        hedge: false,
        pure: true,
        fallback_reserved: true,
        fallback_deadline: Some(20),
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|_| unreachable!("local fallback route"));
    assert_eq!(scheduled.decision(), PlacementDecision::Local);
    assert!(scheduled.cancellation(HedgeSide::Local).is_some());
    assert_eq!(engine.admission().available().operations, 0);
    scheduled.cancel();
    assert_eq!(engine.admission().available().operations, 1);
}

#[test]
fn delta_planner_binds_base_and_target_roots() {
    let planner = DeltaPlanner::new(2);
    let base = root(7);
    let target = root(8);
    assert_eq!(
        planner.plan(&base, &target, 1),
        DeltaPlan::Update { base, target }
    );
    assert_eq!(
        planner.plan(&base, &target, 9).scope(),
        Some(backend_execution::RebuildScope::Product)
    );
}

fn local_request(value: u64, now: u64) -> ScheduleRequest<TestRelation> {
    let work = identity(value);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Unavailable,
        1,
        100,
        0,
        0,
        0,
        false,
    );
    ScheduleRequest::new(
        work,
        1,
        now,
        PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
    )
    .with_charge(4, ResourceVector::zero())
    .pure(true)
}

#[test]
fn concurrent_dispatch_coalesces_followers_and_preserves_one_output_owner() {
    const WORKERS: usize = 32;
    let engine = Arc::new(Scheduler::new(Budget {
        operations: 1,
        bytes: 64,
        ..Budget::zero()
    }));
    let request = local_request(40, 0);
    let barrier = Arc::new(Barrier::new(WORKERS + 1));
    let mut joins = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let engine = Arc::clone(&engine);
        let barrier = Arc::clone(&barrier);
        joins.push(thread::spawn(move || {
            barrier.wait();
            engine.schedule_or_reuse(request)
        }));
    }
    barrier.wait();

    let mut leader = None;
    let mut waiters = Vec::new();
    for join in joins {
        let outcome = join
            .join()
            .unwrap_or_else(|_| unreachable!("dispatch worker panicked"))
            .unwrap_or_else(|error| panic!("coalesced dispatch rejected: {error:?}"));
        match outcome {
            ScheduleOutcome::Scheduled(scheduled) => {
                assert!(leader.is_none(), "two leaders escaped the interner");
                leader = Some(scheduled);
            }
            ScheduleOutcome::Waiting(waiter) => waiters.push(waiter),
            ScheduleOutcome::Reused(_) => {
                unreachable!("a cache hit is impossible before the leader publishes")
            }
        }
    }
    assert_eq!(waiters.len(), WORKERS - 1);
    let scheduled = leader.unwrap_or_else(|| unreachable!("one leader is required"));
    let key = request.identity.work_key();
    let bytes = b"one-shared-result";
    let receipt = admitted(request.identity, scheduled.lease(), bytes);
    let completed = engine
        .complete_with_reuse_policy(*scheduled, &receipt, 0, true, NonZeroU64::new(1))
        .unwrap_or_else(|error| panic!("leader completion failed: {error:?}"));
    let reusable = completed
        .reusable()
        .unwrap_or_else(|| unreachable!("explicit reusable publication"));
    let owner = reusable.canonical_bytes_arc();

    let context = reusable.context();
    let retained = engine
        .output_lookup()
        .lookup(key, &context)
        .unwrap_or_else(|| unreachable!("published output must be reusable"));
    assert!(Arc::ptr_eq(&owner, &retained.canonical_bytes_arc()));

    // Cancelling a follower is a demand-local operation. It must not revoke
    // the shared publication or cause another follower to rerun the work.
    waiters[0].cancel();
    for waiter in &waiters {
        let output = waiter
            .reusable_output()
            .unwrap_or_else(|| unreachable!("follower must observe terminal output"));
        assert_eq!(output.output(), completed.output());
        assert!(Arc::ptr_eq(&owner, &output.canonical_bytes_arc()));
    }
    drop(waiters);
    assert!(engine.interner().is_empty());
}

#[test]
fn authority_epoch_and_revocation_are_checked_before_receipt_minting() {
    let manager = AttemptManager::new();
    let work = identity(41);
    let lease = manager
        .start(work, 1, 0)
        .unwrap_or_else(|_| unreachable!("attempt start"));
    let strict = |identity: &VersionedWorkIdentity<TestRelation>,
                  _: &backend_execution::AttemptLease,
                  claim: &UntrustedAuthorityClaim,
                  admission: &OutputAdmission|
     -> Result<(), AuthorityValidationError> {
        if claim.authority != identity.authority.to_bytes()
            || claim.authority_epoch != 7
            || claim.revocation_version != 11
            || admission.coverage() != ResultCoverage::Complete
        {
            return Err(AuthorityValidationError::Revoked);
        }
        Ok(())
    };

    let old_epoch = ResultReceipt::admit_wire(
        work,
        &lease,
        untrusted_receipt(work, &lease, b"epoch-old", 6, 11),
        &accept_bound_output,
        &strict,
    );
    assert_eq!(old_epoch, Err(AttemptError::AuthorityRejected));

    let old_revocation = ResultReceipt::admit_wire(
        work,
        &lease,
        untrusted_receipt(work, &lease, b"revocation-old", 7, 10),
        &accept_bound_output,
        &strict,
    );
    assert_eq!(old_revocation, Err(AttemptError::AuthorityRejected));

    let admitted = ResultReceipt::admit_wire(
        work,
        &lease,
        untrusted_receipt(work, &lease, b"current", 7, 11),
        &accept_bound_output,
        &strict,
    )
    .unwrap_or_else(|error| panic!("current authority should admit: {error:?}"));
    assert_eq!(admitted.authority_evidence().authority_epoch(), 7);
    assert_eq!(admitted.authority_evidence().revocation_version(), 11);
}

#[test]
fn simultaneous_expiry_takeover_has_one_new_fence_and_monotonic_epoch() {
    const CONTENDERS: usize = 24;
    let manager = Arc::new(AttemptManager::with_lease_duration(1));
    let work = identity(42);
    let first = manager
        .start(work, 3, 0)
        .unwrap_or_else(|_| unreachable!("attempt start"));
    let barrier = Arc::new(Barrier::new(CONTENDERS + 1));
    let mut joins = Vec::with_capacity(CONTENDERS);
    for index in 0..CONTENDERS {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        joins.push(thread::spawn(move || {
            barrier.wait();
            manager.take_over(work, 4 + u64::try_from(index).unwrap_or(u64::MAX), 1)
        }));
    }
    barrier.wait();

    let mut winners = Vec::new();
    let mut busy = 0usize;
    for join in joins {
        match join
            .join()
            .unwrap_or_else(|_| unreachable!("takeover worker panicked"))
        {
            Ok(lease) => winners.push(lease),
            Err(AttemptError::Busy) => busy += 1,
            Err(error) => panic!("unexpected takeover result: {error:?}"),
        }
    }
    assert_eq!(winners.len(), 1);
    assert_eq!(busy, CONTENDERS - 1);
    let winner = &winners[0];
    assert_eq!(winner.ordinal(), first.ordinal() + 1);
    assert!(winner.owner_epoch() >= first.owner_epoch());
    assert_ne!(winner.fence(), first.fence());
    assert_eq!(
        manager.accept(&admitted(work, &first, b"late"), 1),
        Err(AttemptError::Stale)
    );
    manager
        .transition(winner.key(), winner.fence(), AttemptState::Cancelled)
        .unwrap_or_else(|error| panic!("cancel takeover winner: {error:?}"));
}

#[test]
fn reusable_output_index_stays_bounded_during_high_churn_and_reuses_lru() {
    let engine = Scheduler::new(Budget {
        operations: 2,
        bytes: 8,
        ..Budget::zero()
    });
    let mut newest_context = None;
    for value in 0..128u64 {
        let request = local_request(value, 0);
        let scheduled = engine
            .schedule(request)
            .unwrap_or_else(|error| panic!("churn schedule {value}: {error:?}"));
        let byte = u8::try_from(value).unwrap_or_else(|_| unreachable!("churn value fits u8"));
        let receipt = admitted(request.identity, scheduled.lease(), &[byte; 4]);
        let completed = engine
            .complete_with_reuse_policy(scheduled, &receipt, 0, true, NonZeroU64::new(1))
            .unwrap_or_else(|error| panic!("churn completion {value}: {error:?}"));
        for key in completed.evicted_keys() {
            engine
                .attempts()
                .retire_and_forget(*key)
                .unwrap_or_else(|error| panic!("retire evicted attempt {value}: {error:?}"));
        }
        if value == 127 {
            newest_context = Some(
                completed
                    .reusable()
                    .unwrap_or_else(|| unreachable!("reusable churn completion"))
                    .context(),
            );
        }
        assert!(engine.output_lookup().len() <= 2);
        assert!(engine.output_lookup().retained_bytes() <= 8);
    }

    let newest = local_request(127, 0);
    let newest_context = newest_context.unwrap_or_else(|| unreachable!("newest context"));
    assert!(matches!(
        engine.schedule_or_reuse_with_context(newest, Some(&newest_context)),
        Ok(ScheduleOutcome::Reused(_))
    ));
    let evicted = local_request(0, 0);
    assert!(matches!(
        engine.schedule_or_reuse_with_context(evicted, None),
        Ok(ScheduleOutcome::Scheduled(_))
    ));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The test keeps both linearization outcomes and every cancellation/resource assertion together."
)]
fn fallback_activation_and_late_remote_workers_have_one_linearization_winner() {
    const LATE_WORKERS: usize = 15;
    let budgets = EnvelopeBudgets {
        interactive: Budget {
            operations: 1,
            bytes: 16,
            ..Budget::zero()
        },
        background: Budget {
            operations: 1,
            bytes: 16,
            ..Budget::zero()
        },
        transfer: Budget {
            operations: 1,
            bytes: 16,
            ..Budget::zero()
        },
        compaction: Budget::zero(),
    };
    let engine = Arc::new(Scheduler::with_components(
        Admission::with_envelopes(budgets),
        Arc::new(AttemptManager::with_lease_duration(100)),
        Arc::new(OutputLookup::with_limits(4, 64)),
        WorkInterner::new(4, 4),
    ));
    let work = identity(43);
    let (local, remote, costs) = observations(
        &work,
        LocalState::Ready,
        RemoteState::Warm,
        100,
        1,
        0,
        0,
        0,
        true,
    );
    let request = ScheduleRequest {
        identity: work,
        epoch: 1,
        now: 0,
        class: PlacementClass::RemoteOptional,
        local,
        remote,
        costs,
        bytes: 16,
        resources: ResourceVector::zero(),
        hedge: false,
        pure: true,
        fallback_reserved: true,
        fallback_deadline: Some(10),
    };
    let scheduled = engine
        .schedule(request)
        .unwrap_or_else(|error| panic!("remote fallback route: {error:?}"));
    assert_eq!(scheduled.decision(), PlacementDecision::Remote);
    let remote_cancel = scheduled
        .cancellation(HedgeSide::Remote)
        .unwrap_or_else(|| unreachable!("remote cancellation"));
    let local_cancel = scheduled
        .cancellation(HedgeSide::Local)
        .unwrap_or_else(|| unreachable!("local fallback cancellation"));
    let old_receipt = admitted(work, scheduled.lease(), b"late-remote");
    let slot = Arc::new(Mutex::new(Some(scheduled)));
    let barrier = Arc::new(Barrier::new(LATE_WORKERS + 2));

    let fallback_slot = Arc::clone(&slot);
    let fallback_barrier = Arc::clone(&barrier);
    let fallback = thread::spawn(move || {
        fallback_barrier.wait();
        let mut guard = fallback_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .as_mut()
            .unwrap_or_else(|| unreachable!("scheduled fallback guard"))
            .activate_fallback(10)
    });

    let mut late_workers = Vec::with_capacity(LATE_WORKERS);
    for _ in 0..LATE_WORKERS {
        let engine = Arc::clone(&engine);
        let barrier = Arc::clone(&barrier);
        let receipt = old_receipt.clone();
        late_workers.push(thread::spawn(move || {
            barrier.wait();
            engine.attempts().accept(&receipt, 10)
        }));
    }
    barrier.wait();

    let activation = fallback
        .join()
        .unwrap_or_else(|_| unreachable!("fallback worker panicked"));
    let mut accepted = 0usize;
    let mut stale = 0usize;
    let mut already_accepted = 0usize;
    for worker in late_workers {
        match worker
            .join()
            .unwrap_or_else(|_| unreachable!("late remote worker panicked"))
        {
            Ok(_) => accepted += 1,
            Err(AttemptError::Stale) => stale += 1,
            Err(AttemptError::AlreadyAccepted) => already_accepted += 1,
            Err(error) => panic!("unexpected late worker result: {error:?}"),
        }
    }
    assert!(accepted <= 1);
    assert_eq!(accepted + stale + already_accepted, LATE_WORKERS);

    let scheduled = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .unwrap_or_else(|| unreachable!("fallback schedule remains affine"));
    match activation {
        Ok(()) => {
            assert_eq!(accepted, 0);
            assert_eq!(already_accepted, 0);
            assert_eq!(stale, LATE_WORKERS);
            assert!(remote_cancel.is_cancelled());
            assert!(!local_cancel.is_cancelled());
            assert_eq!(scheduled.decision(), PlacementDecision::Local);
            let receipt = admitted(work, scheduled.lease(), b"local-fallback");
            engine
                .complete(scheduled, &receipt, 10)
                .unwrap_or_else(|error| panic!("complete fallback winner: {error:?}"));
        }
        Err(backend_execution::ScheduleError::Attempt(AttemptError::AlreadyAccepted)) => {
            assert_eq!(accepted, 1);
            assert_eq!(stale, 0);
            assert_eq!(already_accepted, LATE_WORKERS - 1);
            assert!(!remote_cancel.is_cancelled());
            assert!(!local_cancel.is_cancelled());
            // The late remote worker won the attempt CAS directly. Dropping
            // the affine scheduler guard still releases every route reserve,
            // while no fallback may be activated after acceptance.
            drop(scheduled);
        }
        Err(error) => panic!("unexpected fallback race result: {error:?}"),
    }
    assert_eq!(engine.admission().available().operations, 1);
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Background)
            .operations,
        1
    );
    assert_eq!(
        engine
            .admission()
            .available_in(Envelope::Transfer)
            .operations,
        1
    );
    assert!(engine.interner().is_empty());
}
