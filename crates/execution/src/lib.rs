//! Typed admission, planning, placement, coalescing, and attempt ownership.
//!
//! The crate contains no semantic authority. Callers provide typed version
//! roots and manifests; this layer owns bounded control work, publication
//! fences, local/remote route selection, and affine resource guards.
#![forbid(unsafe_code)]

mod admission;
mod attempt;
mod cancel;
mod placement;
mod planner;
mod scheduler;
mod supervisor;
mod types;

pub use admission::{
    Admission, AdmissionError, AdmissionRequest, Budget, Envelope, EnvelopeBudgets, Reservation,
    ResourceVector,
};
pub use attempt::{
    AttemptError, AttemptFence, AttemptLease, AttemptManager, AttemptState,
    AuthorityValidationError, AuthorityVerifier, BoundOutputValidator, ExecutionAuthorityEvidence,
    OutputAdmission, OutputAdmissionError, OutputValidationError, OutputValidator, ResultCoverage,
    ResultReceipt, UntrustedAuthorityClaim, UntrustedOutputClaim, UntrustedResultReceipt,
};
pub use cancel::{CancelHandle, Cancellation, Waiter, WaiterError, WaiterTable};
pub use placement::{
    CompletionCost, CostObservation, CostSnapshot, CostVerifier, HedgeError, HedgeRace, HedgeSide,
    HedgeWinner, LocalCapability, LocalCapabilityVerifier, LocalState, ObservationError,
    OutputLookup, PlacementClass, PlacementDecision, PlacementPlan, PlacementPlanError,
    PlacementRequest, RemoteCapability, RemoteCapabilityVerifier, RemoteState, ReusableOutput,
    ReuseContext, SharedHedgeRace, admit_placement_plan, choose_placement_with,
    remote_state_from_capabilities,
};
pub use planner::{
    DeltaPlan, DeltaPlanner, RebuildScope, RefreshChoice, RefreshCost, choose_refresh,
};
pub use scheduler::{
    DeadlineQueue, DeadlineQueueError, RouteReservations, ScheduleError, ScheduleOutcome,
    ScheduleReceipt, ScheduleRequest, Scheduled, Scheduler,
};
pub use supervisor::{Supervisor, SupervisorSnapshot};
pub use types::{
    AdmittedWork, AuthorityVersion, AuthorityVersionSchema, CompletionReceipt, InternError,
    Interned, OutputEquivalence, OutputEquivalenceSchema, OutputSchema, OutputVersion,
    ReadManifestId, ReadManifestSchema, RecipeId, RecipeSchema, VersionedWorkIdentity,
    WorkInterner, WorkKey, WorkKeySchema, WorkTicket,
};

#[cfg(test)]
mod tests {
    use super::*;
    use backend_replication::NegotiatedCapabilities;
    use backend_version::{CoverageWitness, Relation, partial_coverage};
    use std::{num::NonZeroU64, sync::Arc};

    struct TestRelation;
    impl Relation for TestRelation {
        const DOMAIN: u8 = 1;
        const TYPE: u16 = 1;
        type Key = u64;
        type Value = u64;

        fn encode_key(value: &u64, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }

        fn encode_value(value: &u64, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn state(
        value: u64,
    ) -> Result<backend_version::StateRoot<TestRelation>, backend_version::StateError> {
        backend_version::RelationState::<TestRelation>::from_entries(
            [(value, value)],
            CoverageWitness::Partial(partial_coverage(1)),
        )
        .map(|state| state.root())
    }

    fn identity(
        value: u64,
    ) -> Result<VersionedWorkIdentity<TestRelation>, backend_version::StateError> {
        Ok(VersionedWorkIdentity::new(
            RecipeId::from_value(b"recipe"),
            state(value)?,
            ReadManifestId::from_value(b"reads"),
            AuthorityVersion::from_value(b"authority"),
            OutputEquivalence::from_value(b"equivalent"),
        ))
    }

    fn output(value: &[u8]) -> OutputVersion {
        OutputVersion::from_value(value)
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

    #[allow(clippy::unnecessary_wraps)]
    fn accept_authority(
        identity: &VersionedWorkIdentity<TestRelation>,
        _: &AttemptLease,
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

    fn admitted_output(value: &[u8]) -> OutputAdmission {
        OutputAdmission::admit(
            UntrustedOutputClaim {
                output: output(value).to_bytes(),
                canonical_bytes: Arc::new(value.to_vec()),
                coverage: ResultCoverage::Complete,
            },
            &accept_output,
        )
        .unwrap_or_else(|_| unreachable!("admitted output"))
    }

    fn reuse_context(identity: VersionedWorkIdentity<TestRelation>) -> ReuseContext {
        ReuseContext::test_new(identity.work_key(), identity.authority, 1, 1, 1)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn accept_local(
        _: &VersionedWorkIdentity<TestRelation>,
        _: LocalState,
    ) -> Result<(), ObservationError> {
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn accept_remote(
        _: &VersionedWorkIdentity<TestRelation>,
        _: &NegotiatedCapabilities,
    ) -> Result<RemoteState, ObservationError> {
        Ok(RemoteState::Available)
    }

    #[allow(clippy::unnecessary_wraps)]
    fn accept_cost(
        _: &VersionedWorkIdentity<TestRelation>,
        _: &CostObservation,
    ) -> Result<(), ObservationError> {
        Ok(())
    }

    fn schedule_request(
        identity: VersionedWorkIdentity<TestRelation>,
    ) -> Result<ScheduleRequest<TestRelation>, ObservationError> {
        let local = LocalCapability::admit(&identity, LocalState::Ready, 0, 100, &accept_local)
            .map_err(|_| ObservationError::Rejected)?;
        let remote = RemoteCapability::admit(
            &identity,
            &NegotiatedCapabilities {
                protocol: 1,
                schemas: Vec::new(),
                recipes: Vec::new(),
                limits: backend_replication::TransportLimits::default(),
                max_resources: backend_replication::ResourceEnvelope::UNBOUNDED,
            },
            0,
            100,
            &accept_remote,
        )
        .map_err(|_| ObservationError::Rejected)?;
        let costs = CostSnapshot::admit(
            &identity,
            CostObservation {
                local: 1,
                remote: CompletionCost::default(),
                observed_at: 0,
                expires_at: 100,
                confidence_per_mille: 1_000,
            },
            &accept_cost,
        )
        .map_err(|_| ObservationError::Rejected)?;
        Ok(ScheduleRequest::new(
            identity,
            1,
            0,
            PlacementClass::LocalPreferred,
            local,
            remote,
            costs,
        )
        .with_charge(4, ResourceVector::zero()))
    }

    fn receipt(
        identity: VersionedWorkIdentity<TestRelation>,
        lease: &AttemptLease,
        value: &[u8],
    ) -> Result<ResultReceipt<TestRelation>, AttemptError> {
        let admission = OutputAdmission::admit(
            UntrustedOutputClaim {
                output: output(value).to_bytes(),
                canonical_bytes: Arc::new(value.to_vec()),
                coverage: ResultCoverage::Complete,
            },
            &accept_output,
        )
        .map_err(|_| AttemptError::InvalidResult)?;
        let claim = UntrustedAuthorityClaim {
            authority: identity.authority.to_bytes(),
            authority_epoch: 1,
            revocation_version: 1,
            attestation: None,
        };
        accept_authority(&identity, lease, &claim, &admission)
            .map_err(|_| AttemptError::AuthorityRejected)?;
        let evidence = ExecutionAuthorityEvidence::mint(identity, lease, &claim, &admission);
        ResultReceipt::from_admission(identity, lease, admission, evidence)
    }

    #[test]
    fn no_op_reuses_previous_root() -> TestResult {
        let planner = DeltaPlanner::new(4);
        let old = state(1)?;
        assert_eq!(planner.plan(&old, &old, 0), DeltaPlan::NoOp { result: old });
        Ok(())
    }

    #[test]
    fn budget_conservation_and_rollback() -> TestResult {
        let admission = Admission::new(Budget {
            operations: 1,
            bytes: 10,
            hedges: 1,
            ..Budget::zero()
        });
        let reservation = admission.try_admit(AdmissionRequest::new(1, 8).hedged())?;
        assert!(admission.try_admit(AdmissionRequest::new(1, 1)).is_err());
        drop(reservation);
        assert!(admission.try_admit(AdmissionRequest::new(1, 10)).is_ok());
        Ok(())
    }

    #[test]
    fn interner_releases_on_terminal_leader_completion() -> TestResult {
        let interner = WorkInterner::new(2, 1);
        let key = identity(1)?.work_key();
        let leader = interner.intern(key)?;
        assert!(leader.is_leader());
        let follower = interner.register_waiter(key)?;
        let receipt = leader.complete(admitted_output(b"out"))?;
        assert_eq!(interner.output(key), Some(receipt.output()));
        assert_eq!(follower.output(), Some(receipt.output()));
        assert!(follower.is_terminal());
        drop(follower);
        assert!(interner.is_empty());
        Ok(())
    }

    #[test]
    fn coalesced_waiter_observes_the_leaders_shared_output_and_can_cancel() -> TestResult {
        let interner = WorkInterner::new(2, 4);
        let identity = identity(3)?;
        let key = identity.work_key();
        let leader = interner.intern(key)?;
        let waiters = (0..4)
            .map(|_| interner.register_waiter(key))
            .collect::<Result<Vec<_>, _>>()?;
        let bytes = Arc::new(b"shared".to_vec());
        let reusable = ReusableOutput::from_parts(
            reuse_context(identity),
            output(bytes.as_slice()),
            Arc::clone(&bytes),
        );
        let admission = OutputAdmission::admit(
            UntrustedOutputClaim {
                output: reusable.output().to_bytes(),
                canonical_bytes: Arc::clone(&bytes),
                coverage: ResultCoverage::Complete,
            },
            &accept_output,
        )?;
        assert!(!leader.is_cancelled());
        leader.prepare_complete(reusable.output())?;
        let _ = leader.complete_prepared_with_reusable(admission, Some(reusable.clone()));
        for (index, waiter) in waiters.iter().enumerate() {
            let observed = waiter
                .reusable_output()
                .ok_or("leader output not visible to waiter")?;
            assert!(
                Arc::ptr_eq(&observed.canonical_bytes_arc(), &bytes),
                "waiter {index} did not retain the leader allocation"
            );
        }
        waiters[0].cancel();
        assert!(waiters[0].is_cancelled());
        drop(waiters);
        assert!(interner.is_empty());
        Ok(())
    }

    #[test]
    fn follower_demand_survives_an_abandoned_leader_takeover() -> TestResult {
        let interner = WorkInterner::new(2, 2);
        let key = identity(31)?.work_key();
        let first_leader = interner.intern(key)?;
        let follower = interner.register_waiter(key)?;
        drop(first_leader);
        assert!(follower.is_terminal());

        let replacement = interner.intern(key)?;
        assert!(replacement.is_leader());
        assert!(!follower.is_terminal());
        assert_eq!(follower.output(), None);
        follower.cancel();
        assert!(!replacement.is_cancelled());
        replacement.complete(admitted_output(b"replacement"))?;
        assert!(follower.is_terminal());
        assert_eq!(follower.output(), Some(output(b"replacement")));

        drop(follower);
        assert!(interner.is_empty());
        assert_eq!(interner.follower_len(), 0);
        Ok(())
    }

    #[test]
    fn concurrent_four_thousand_keys_have_exact_cleanup() -> TestResult {
        let interner = WorkInterner::new(4_096, 4_096);
        let keys = (0..4_096_u64)
            .map(|value| identity(value).map(|item| item.work_key()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut threads = Vec::new();
        for chunk in keys.chunks(512) {
            let interner = Arc::clone(&interner);
            let keys = chunk.to_vec();
            threads.push(std::thread::spawn(move || {
                keys.into_iter()
                    .map(|key| interner.intern(key))
                    .collect::<Result<Vec<_>, _>>()
            }));
        }
        let mut leaders = Vec::with_capacity(4_096);
        for thread in threads {
            leaders.extend(thread.join().map_err(|_| "interner worker panicked")??);
        }
        assert_eq!(leaders.len(), 4_096);
        assert_eq!(interner.len(), 4_096);
        assert_eq!(interner.follower_len(), 0);
        drop(leaders);
        assert!(interner.is_empty());
        assert_eq!(interner.follower_len(), 0);
        Ok(())
    }

    #[test]
    fn expired_owner_can_take_over_and_late_result_is_rejected() -> TestResult {
        let identity = identity(2)?;
        let manager = AttemptManager::new();
        let first = manager.start(identity, 7, 10)?;
        let second = manager.take_over(identity, 7, 11)?;
        let old = receipt(identity, &first, b"old")?;
        assert_eq!(manager.accept(&old, 11), Err(AttemptError::Stale));
        let current = receipt(identity, &second, b"new")?;
        assert_eq!(manager.accept(&current, 11), Ok(output(b"new")));
        let second_receipt = receipt(identity, &second, b"again")?;
        assert_eq!(
            manager.accept(&second_receipt, 11),
            Err(AttemptError::AlreadyAccepted)
        );
        Ok(())
    }

    #[test]
    fn typed_receipt_claims_reject_changed_input_and_authority() -> TestResult {
        let identity = identity(4)?;
        let manager = AttemptManager::new();
        let lease = manager.start(identity, 5, 0)?;

        let mut wrong_input = receipt(identity, &lease, b"input")?;
        wrong_input.test_set_input(state(999)?);
        assert_eq!(
            manager.accept(&wrong_input, 0),
            Err(AttemptError::InputMismatch)
        );

        let mut wrong_reads = receipt(identity, &lease, b"reads")?;
        wrong_reads.test_set_read_manifest(ReadManifestId::from_value(b"new-reads"));
        assert_eq!(
            manager.accept(&wrong_reads, 0),
            Err(AttemptError::ReadManifestMismatch)
        );

        let mut wrong_authority = receipt(identity, &lease, b"authority")?;
        wrong_authority.test_set_authority(AuthorityVersion::from_value(b"revoked"));
        assert_eq!(
            manager.accept(&wrong_authority, 0),
            Err(AttemptError::AuthorityMismatch)
        );

        let valid_output = output(b"valid");
        let valid = ResultReceipt::admit_wire(
            identity,
            &lease,
            UntrustedResultReceipt {
                key: identity.work_key().to_bytes(),
                input: identity.input.to_bytes(),
                recipe: identity.recipe.to_bytes(),
                read_manifest: identity.read_manifest.to_bytes(),
                authority: identity.authority.to_bytes(),
                output_equivalence: identity.output_equivalence.to_bytes(),
                output: valid_output.to_bytes(),
                output_canonical_bytes: Arc::new(b"valid".to_vec()),
                coverage: ResultCoverage::Complete,
                ordinal: lease.ordinal(),
                fence: *lease.fence().as_bytes(),
                authority_epoch: 1,
                revocation_version: 1,
                attestation: None,
            },
            &accept_bound_output,
            &accept_authority,
        )?;
        assert_eq!(manager.accept(&valid, 0), Ok(output(b"valid")));
        Ok(())
    }

    #[test]
    fn cancellation_is_visible_without_waking_a_new_owner() -> TestResult {
        let table = WaiterTable::new(1);
        let waiter = table.register(identity(3)?.work_key())?;
        waiter.cancel();
        assert!(waiter.is_cancelled());
        assert_eq!(table.len(), 1);
        drop(waiter);
        assert!(table.is_empty());
        Ok(())
    }

    #[test]
    fn concurrent_reservations_never_exceed_capacity() {
        use std::sync::Arc;
        use std::thread;
        let admission = Arc::new(Admission::new(Budget {
            operations: 4,
            bytes: 40,
            hedges: 0,
            ..Budget::zero()
        }));
        let ready = Arc::new(std::sync::Barrier::new(9));
        let release = Arc::new(std::sync::Barrier::new(9));
        let joins = (0..8)
            .map(|_| {
                let admission = Arc::clone(&admission);
                let ready = Arc::clone(&ready);
                let release = Arc::clone(&release);
                thread::spawn(move || {
                    let reservation = admission.try_admit(AdmissionRequest::new(1, 10));
                    ready.wait();
                    release.wait();
                    reservation.is_ok()
                })
            })
            .collect::<Vec<_>>();
        ready.wait();
        assert_eq!(admission.available().operations, 0);
        release.wait();
        let accepted = joins
            .into_iter()
            .filter_map(|join| join.join().ok())
            .filter(|accepted| *accepted)
            .count();
        assert!(accepted <= 4);
        assert_eq!(admission.available().operations, 4);
    }

    #[test]
    fn reusable_output_lookup_is_bounded_by_count_and_bytes() -> TestResult {
        let lookup = OutputLookup::with_limits(2, 5);
        let first = identity(10)?;
        let second = identity(11)?;
        let third = identity(12)?;
        let first_reusable = ReusableOutput::from_parts(
            reuse_context(first),
            output(b"aa"),
            Arc::new(b"aa".to_vec()),
        );
        lookup.insert(first_reusable);
        let second_reusable = ReusableOutput::from_parts(
            reuse_context(second),
            output(b"bbb"),
            Arc::new(b"bbb".to_vec()),
        );
        lookup.insert(second_reusable);
        let third_reusable = ReusableOutput::from_parts(
            reuse_context(third),
            output(b"cccc"),
            Arc::new(b"cccc".to_vec()),
        );
        let third_context = third_reusable.context();
        lookup.insert(third_reusable);
        assert!(lookup.len() <= 2);
        assert!(lookup.retained_bytes() <= 5);
        assert!(lookup.lookup(third.work_key(), &third_context).is_some());

        let recency_lookup = OutputLookup::with_limits(2, 6);
        let first_reusable = ReusableOutput::from_parts(
            reuse_context(first),
            output(b"aa"),
            Arc::new(b"aa".to_vec()),
        );
        let first_context = first_reusable.context();
        recency_lookup.insert(first_reusable);
        let second_reusable = ReusableOutput::from_parts(
            reuse_context(second),
            output(b"bbb"),
            Arc::new(b"bbb".to_vec()),
        );
        let second_context = second_reusable.context();
        recency_lookup.insert(second_reusable);
        assert!(
            recency_lookup
                .lookup(first.work_key(), &first_context)
                .is_some()
        );
        let third_reusable = ReusableOutput::from_parts(
            reuse_context(third),
            output(b"cccc"),
            Arc::new(b"cccc".to_vec()),
        );
        let third_context = third_reusable.context();
        recency_lookup.insert(third_reusable);
        assert!(
            recency_lookup
                .lookup(first.work_key(), &first_context)
                .is_some()
        );
        assert!(
            recency_lookup
                .lookup(second.work_key(), &second_context)
                .is_none()
        );
        assert!(
            recency_lookup
                .lookup(third.work_key(), &third_context)
                .is_some()
        );

        let eviction_lookup = OutputLookup::with_limits(1, 3);
        let retained_identity = identity(13)?;
        let evicted_identity = identity(14)?;
        let retained_bytes = Arc::new(b"one".to_vec());
        let retained_weak = Arc::downgrade(&retained_bytes);
        eviction_lookup.insert(ReusableOutput::from_parts(
            reuse_context(retained_identity),
            output(b"one"),
            Arc::clone(&retained_bytes),
        ));
        drop(retained_bytes);
        assert!(retained_weak.upgrade().is_some());
        eviction_lookup.insert(ReusableOutput::from_parts(
            reuse_context(evicted_identity),
            output(b"two"),
            Arc::new(b"two".to_vec()),
        ));
        assert!(retained_weak.upgrade().is_none());
        assert_eq!(eviction_lookup.retained_bytes(), 3);
        Ok(())
    }

    #[test]
    fn reuse_context_requires_every_freshness_dimension() -> TestResult {
        let identity = identity(20)?;
        let key = identity.work_key();
        let authority = identity.authority;
        let bytes = Arc::new(b"freshness".to_vec());
        let expected = ReuseContext::test_new(key, authority, 3, 7, 11);
        let reusable =
            ReusableOutput::from_parts(expected, output(bytes.as_slice()), Arc::clone(&bytes));
        let lookup = OutputLookup::with_limits(4, 1024);
        lookup.insert(reusable);
        assert!(lookup.lookup(key, &expected).is_some());

        let wrong_epoch = ReuseContext::test_new(key, authority, 4, 7, 11);
        let wrong_revocation = ReuseContext::test_new(key, authority, 3, 8, 11);
        let wrong_generation = ReuseContext::test_new(key, authority, 3, 7, 12);
        let wrong_authority = ReuseContext::test_new(
            key,
            AuthorityVersion::from_value(b"other-authority"),
            3,
            7,
            11,
        );
        assert!(lookup.lookup(key, &wrong_epoch).is_none());
        assert!(lookup.lookup(key, &wrong_revocation).is_none());
        assert!(lookup.lookup(key, &wrong_generation).is_none());
        assert!(lookup.lookup(key, &wrong_authority).is_none());
        Ok(())
    }

    #[test]
    fn scheduler_without_context_cannot_reuse_and_reuse_requires_generation() -> TestResult {
        let first_identity = identity(21)?;
        let request = schedule_request(first_identity)?;
        let scheduler = Scheduler::new(Budget {
            operations: 4,
            bytes: 1024,
            ..Budget::zero()
        });
        let bytes = Arc::new(b"cached".to_vec());
        let context = reuse_context(first_identity);
        scheduler.output_lookup().insert(ReusableOutput::from_parts(
            context,
            output(bytes.as_slice()),
            bytes,
        ));
        let first_route = match scheduler.schedule_or_reuse(request)? {
            ScheduleOutcome::Scheduled(route) => route,
            ScheduleOutcome::Reused(_) => {
                return Err("scheduler self-authorized a retained context".into());
            }
            ScheduleOutcome::Waiting(_) => return Err("fresh scheduler request coalesced".into()),
        };
        let first_receipt = receipt(first_identity, first_route.lease(), b"new")?;
        assert_eq!(
            scheduler.complete_with_reuse_policy(*first_route, &first_receipt, 0, true, None,),
            Err(ScheduleError::ObservationMismatch)
        );

        let second_identity = identity(22)?;
        let request = schedule_request(second_identity)?;
        let second_route = match scheduler.schedule_or_reuse(request)? {
            ScheduleOutcome::Scheduled(route) => route,
            ScheduleOutcome::Reused(_) | ScheduleOutcome::Waiting(_) => {
                return Err("second scheduler request was not scheduled".into());
            }
        };
        let second_receipt = receipt(second_identity, second_route.lease(), b"published")?;
        let published = scheduler.complete_with_reuse_policy(
            *second_route,
            &second_receipt,
            0,
            true,
            NonZeroU64::new(1),
        )?;
        assert!(published.reusable().is_some());
        Ok(())
    }

    #[test]
    fn fallback_deadline_drain_activates_live_route_and_drops_cancelled_route() -> TestResult {
        let budgets = EnvelopeBudgets {
            interactive: Budget {
                operations: 8,
                bytes: 1024,
                ..Budget::zero()
            },
            background: Budget {
                operations: 8,
                bytes: 1024,
                ..Budget::zero()
            },
            transfer: Budget {
                operations: 8,
                bytes: 1024,
                ..Budget::zero()
            },
            compaction: Budget::zero(),
        };
        let scheduler = Scheduler::with_envelopes(budgets);
        let first_identity = identity(30)?;
        let mut first_request = schedule_request(first_identity)?.with_fallback(Some(10));
        first_request.class = PlacementClass::RemoteOptional;
        let mut first = match scheduler.schedule_or_reuse(first_request)? {
            ScheduleOutcome::Scheduled(route) => *route,
            ScheduleOutcome::Reused(_) | ScheduleOutcome::Waiting(_) => {
                return Err("fallback request was not scheduled".into());
            }
        };
        assert_eq!(first.decision(), PlacementDecision::Remote);
        assert_eq!(scheduler.deadline_queue_lengths(), (1, 1));
        assert!(scheduler.drain_due_fallbacks(9).is_empty());
        assert_eq!(scheduler.drain_due_fallbacks(10), vec![first.key()]);
        first.activate_fallback(10)?;
        assert_eq!(first.decision(), PlacementDecision::Local);
        assert_eq!(scheduler.deadline_queue_lengths(), (0, 0));

        let second_identity = identity(31)?;
        let mut second_request = schedule_request(second_identity)?.with_fallback(Some(10));
        second_request.class = PlacementClass::RemoteOptional;
        let second = match scheduler.schedule_or_reuse(second_request)? {
            ScheduleOutcome::Scheduled(route) => *route,
            ScheduleOutcome::Reused(_) | ScheduleOutcome::Waiting(_) => {
                return Err("cancelled fallback request was not scheduled".into());
            }
        };
        assert_eq!(scheduler.deadline_queue_lengths(), (1, 1));
        second.cancel();
        assert_eq!(scheduler.deadline_queue_lengths(), (0, 0));
        assert!(scheduler.drain_due_fallbacks(10).is_empty());
        Ok(())
    }
}
