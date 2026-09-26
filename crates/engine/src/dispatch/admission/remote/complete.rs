//! Remote route admission and completion for one scheduled dispatch.

use super::super::super::retention::RetainedOutput;
use super::super::{
    AcceptedAuthority, ActiveSemanticRollback, AdmissionReservation, Arc, AttestationClass,
    AttestationVerifier, DispatchCompletion, DispatchError, DispatchPlan, Dispatcher,
    ExecutionResultExpectation, ExpectedIdentity, FenceBinding, FreshnessRollback, HedgeSide,
    LowerOutputValidator, NonZeroU64, OutputAdmissionValidator, OutputVersion, Relation,
    RemoteAdmission, RemoteAuthorityVerifier, RemoteDispatchContract, ReplicationError,
    ResultCoverage, ResultReceipt, Scheduled, SemanticCoverageValidator, UntrustedResultReceipt,
    WireRecipeResult, check_authority, request_expectation, statement_id_view, wire_request,
    worker_receipt_id,
};
use backend_execution::{VersionedWorkIdentity, WorkKey};

struct PreparedRemote<R: Relation> {
    identity: VersionedWorkIdentity<R>,
    route_bytes: u64,
    observed_local_state: backend_execution::LocalState,
    observed_remote_state: backend_execution::RemoteState,
    started_at: u64,
    statement_id: [u8; 32],
    output: OutputVersion,
    retained_owner: Arc<Vec<u8>>,
    receipt: ResultReceipt<R>,
    semantic: super::super::CompleteSemanticCoverage,
    class: AttestationClass,
    reservation: AdmissionReservation,
}

struct RemotePublication<'a, R: Relation> {
    identity: VersionedWorkIdentity<R>,
    semantic: &'a super::super::CompleteSemanticCoverage,
    class: AttestationClass,
    output: OutputVersion,
    statement_id: [u8; 32],
    semantic_generation: Option<u64>,
    cacheable: bool,
    receipt: &'a backend_execution::ScheduleReceipt,
}

struct ValidatedRemote {
    key: WorkKey,
    publishable: backend_replication::PublishableExecutionResult,
    semantic: super::super::CompleteSemanticCoverage,
    fence: FenceBinding,
    class: AttestationClass,
    retained_output: RetainedOutput,
    statement_id: [u8; 32],
    output: OutputVersion,
}

struct ValidatedWire {
    publishable: backend_replication::PublishableExecutionResult,
    retained_output: RetainedOutput,
    class: AttestationClass,
    statement_id: [u8; 32],
    output: OutputVersion,
}

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    pub(in crate::dispatch::admission) fn require_remote_route<R: Relation>(
        scheduled: &Scheduled<R>,
    ) -> Result<(), DispatchError> {
        if matches!(
            scheduled.decision(),
            backend_execution::PlacementDecision::Remote
                | backend_execution::PlacementDecision::Hedge
        ) {
            Ok(())
        } else {
            Err(DispatchError::AuthorityRejected)
        }
    }

    pub(in crate::dispatch::admission) fn check_authority_for_publication(
        accepted: &std::collections::BTreeMap<WorkKey, AcceptedAuthority>,
        key: WorkKey,
        class: AttestationClass,
        output: OutputVersion,
    ) -> Result<(), DispatchError> {
        check_authority(accepted, key, class, output)
    }

    fn handle_remote_authority_error(
        &self,
        accepted: std::sync::MutexGuard<'_, std::collections::BTreeMap<WorkKey, AcceptedAuthority>>,
        key: WorkKey,
        error: DispatchError,
        statement_id: [u8; 32],
        output: OutputVersion,
    ) -> DispatchError {
        let previous = accepted
            .get(&key)
            .map(|authority| (authority.statement_id, authority.output));
        drop(accepted);
        if matches!(error, DispatchError::AuthorityConflict)
            && let Some((previous_statement, previous_output)) = previous
        {
            self.quarantine_authority_conflict(
                key,
                previous_statement,
                previous_output,
                statement_id,
                output,
            );
        }
        error
    }

    /// Admits a remote result against exact typed request material. The
    /// returned capability is the only value accepted by `complete_remote`.
    ///
    /// This borrowed form is retained for callers that need to keep the wire
    /// envelope. The daemon's receive path uses [`Self::admit_remote_owned`]
    /// so admission does not retain a second copy of the complete envelope.
    #[cfg(test)]
    pub(crate) fn admit_remote<R: Relation>(
        &self,
        plan: &DispatchPlan<R>,
        wire: &WireRecipeResult,
        contract: &RemoteDispatchContract,
    ) -> Result<RemoteAdmission, DispatchError> {
        self.admit_remote_owned(plan, wire.clone(), contract)
    }

    /// Admits a remote result by consuming its wire envelope.
    ///
    /// The returned capability owns the exact envelope supplied by the
    /// transport. This is the primary path for large results: completion can
    /// move the canonical output buffer into the lower receipt without a
    /// second allocation.
    #[cfg(test)]
    pub(crate) fn admit_remote_owned<R: Relation>(
        &self,
        plan: &DispatchPlan<R>,
        wire: WireRecipeResult,
        contract: &RemoteDispatchContract,
    ) -> Result<RemoteAdmission, DispatchError> {
        let scheduled = match plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
                return Err(DispatchError::AuthorityRejected);
            }
        };
        self.admit_remote_scheduled(scheduled, wire, contract)
    }

    pub(in crate::dispatch::admission) fn admit_remote_scheduled<R: Relation>(
        &self,
        scheduled: &Scheduled<R>,
        wire: WireRecipeResult,
        contract: &RemoteDispatchContract,
    ) -> Result<RemoteAdmission, DispatchError> {
        let validated = self.validate_remote_scheduled(scheduled, wire, contract)?;
        let key = validated.key;
        let authority = match self.pending_authority.reserve(
            key,
            validated.statement_id,
            validated.class,
            validated.output,
        ) {
            Ok(authority) => authority,
            Err(error) => {
                if matches!(error, DispatchError::AuthorityConflict)
                    && let Some((previous_statement, previous_output)) = self
                        .pending_authority
                        .conflicting_candidate(key, validated.output)
                {
                    self.quarantine_authority_conflict(
                        key,
                        previous_statement,
                        previous_output,
                        validated.statement_id,
                        validated.output,
                    );
                }
                return Err(error);
            }
        };
        let replay = self.replayed.reserve(
            key,
            validated.statement_id,
            validated.semantic.dependency_manifest().is_some()
                && self
                    .scheduler
                    .output_lookup()
                    .can_retain_capacity(validated.publishable.wire().output_bytes.capacity()),
        )?;
        Ok(RemoteAdmission {
            publishable: validated.publishable,
            semantic: validated.semantic,
            fence: validated.fence,
            class: validated.class,
            retained_output: validated.retained_output,
            reservation: AdmissionReservation { authority, replay },
        })
    }

    fn validate_remote_scheduled<R: Relation>(
        &self,
        scheduled: &Scheduled<R>,
        wire: WireRecipeResult,
        contract: &RemoteDispatchContract,
    ) -> Result<ValidatedRemote, DispatchError> {
        Self::require_remote_route(scheduled)?;
        let identity = scheduled.identity();
        if self.is_authority_quarantined(scheduled.key()) {
            return Err(DispatchError::AuthorityConflict);
        }
        self.validate_semantic_capability(&identity, &contract.semantic)?;
        if contract.authority_epoch != contract.semantic.authority_epoch()
            || contract.revocation_version != contract.semantic.revocation_version()
        {
            return Err(DispatchError::IncompleteSemanticCoverage);
        }
        if scheduled
            .cancellation(HedgeSide::Remote)
            .is_some_and(|cancellation| cancellation.is_cancelled())
        {
            return Err(DispatchError::Cancelled);
        }
        let fence = FenceBinding::from_lease(scheduled.lease())?;
        if !fence.admits(scheduled.lease(), wire.fence) {
            return Err(DispatchError::Replication(ReplicationError::StaleFence));
        }
        let validated_wire = self.validate_remote_wire(scheduled, wire, contract)?;
        let ValidatedWire {
            publishable,
            retained_output,
            class,
            statement_id,
            output,
        } = validated_wire;
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        if let Err(error) =
            Self::check_authority_for_publication(&accepted, scheduled.key(), class, output)
        {
            return Err(self.handle_remote_authority_error(
                accepted,
                scheduled.key(),
                error,
                statement_id,
                output,
            ));
        }
        drop(accepted);
        Ok(ValidatedRemote {
            key: scheduled.key(),
            publishable,
            semantic: contract.semantic.clone(),
            fence,
            class,
            retained_output,
            statement_id,
            output,
        })
    }

    fn validate_remote_wire<R: Relation>(
        &self,
        scheduled: &Scheduled<R>,
        wire: WireRecipeResult,
        contract: &RemoteDispatchContract,
    ) -> Result<ValidatedWire, DispatchError> {
        let expected_request = request_expectation(scheduled, contract)?;
        let output = OutputVersion::from_value(wire.output_bytes.as_slice());
        let retained_output = OutputAdmissionValidator::validate_shared(
            &self.validator,
            output,
            &wire.output_bytes,
            &contract.semantic,
        )?;
        if retained_output.output() != output
            || OutputVersion::from_value(retained_output.canonical_bytes()) != output
        {
            return Err(DispatchError::OutputMismatch);
        }
        let expected_receipt = contract.expected_receipt.unwrap_or_else(|| {
            worker_receipt_id(
                &wire_request(scheduled, contract, &expected_request),
                output,
                &wire.output_bytes,
            )
        });
        let expected_result = ExecutionResultExpectation {
            output: ExpectedIdentity::from_typed(&output),
            receipt: ExpectedIdentity::from_typed(&expected_receipt),
            output_len: u64::try_from(wire.output_bytes.len())
                .map_err(|_| DispatchError::OutputContract)?,
            byte_coverage: wire.byte_coverage.clone(),
            semantic_coverage: contract.semantic.expectation(&scheduled.identity()),
        };
        let publishable = wire
            .admit_publishable_owned(
                &expected_request,
                &expected_result,
                contract.limits,
                contract.revocation_version,
                &self.authority,
            )
            .map_err(DispatchError::Replication)?;
        let class = publishable.class();
        if !contract.authority_policy.allows(class) {
            return Err(DispatchError::AuthorityRejected);
        }
        let admitted_wire = publishable.wire();
        let output_len = u64::try_from(admitted_wire.output_bytes.len())
            .map_err(|_| DispatchError::OutputContract)?;
        if !admitted_wire.byte_coverage.is_complete(output_len) {
            return Err(DispatchError::IncompleteCoverage);
        }
        let statement_id = statement_id_view(&admitted_wire.attestation_material_view());
        Ok(ValidatedWire {
            publishable,
            retained_output,
            class,
            statement_id,
            output,
        })
    }

    fn prepare_remote_completion<R: Relation>(
        &self,
        scheduled: &Scheduled<R>,
        admission: RemoteAdmission,
    ) -> Result<PreparedRemote<R>, DispatchError> {
        let identity = scheduled.identity();
        let route_bytes = scheduled.charged_bytes();
        let observed_local_state = scheduled.observed_local_state();
        let observed_remote_state = scheduled.observed_remote_state();
        let started_at = scheduled.lease().observed_at();
        let RemoteAdmission {
            publishable,
            semantic,
            fence,
            class,
            retained_output,
            reservation,
        } = admission;
        let statement_id = statement_id_view(&publishable.wire().attestation_material_view());
        self.validate_semantic_capability(&identity, &semantic)?;
        if semantic.authority_epoch() != publishable.wire().authority.epoch
            || semantic.revocation_version() != publishable.wire().revocation_version
        {
            return Err(DispatchError::IncompleteSemanticCoverage);
        }
        if scheduled
            .cancellation(HedgeSide::Remote)
            .is_some_and(|cancellation| cancellation.is_cancelled())
        {
            return Err(DispatchError::Cancelled);
        }
        if !fence.admits(scheduled.lease(), publishable.wire().fence) {
            return Err(DispatchError::Replication(ReplicationError::StaleFence));
        }
        let admitted_authority_epoch = publishable.wire().authority.epoch.0;
        let admitted_revocation_version = publishable.wire().revocation_version.0;
        // The retaining validator owns the canonical bytes that will remain
        // reachable after the transport envelope is dropped. For the normal
        // shared CAS path this is the same Arc allocation as the wire result;
        // a durable validator may instead return its pinned store owner.
        let retained_owner = retained_output.canonical_bytes_arc();
        let wire = publishable.into_admitted().into_wire();
        let output = OutputVersion::from_value(wire.output_bytes.as_slice());
        if retained_output.output() != output
            || retained_owner.as_slice() != wire.output_bytes.as_slice()
        {
            return Err(DispatchError::OutputMismatch);
        }
        let receipt = ResultReceipt::admit_wire(
            identity,
            scheduled.lease(),
            UntrustedResultReceipt {
                key: identity.work_key().to_bytes(),
                input: identity.input.to_bytes(),
                recipe: identity.recipe.to_bytes(),
                read_manifest: identity.read_manifest.to_bytes(),
                authority: identity.authority.to_bytes(),
                authority_epoch: wire.authority.epoch.0,
                revocation_version: wire.revocation_version.0,
                attestation: wire.attestation.map(|a| a.0),
                output_equivalence: identity.output_equivalence.to_bytes(),
                output: output.to_bytes(),
                output_canonical_bytes: Arc::clone(&retained_owner),
                coverage: ResultCoverage::Complete,
                ordinal: scheduled.lease().ordinal(),
                fence: *scheduled.lease().fence().as_bytes(),
            },
            &LowerOutputValidator {
                validator: &self.validator,
                semantic: &semantic,
                canonical_owner: Some(&retained_owner),
            },
            &RemoteAuthorityVerifier {
                minimum_epoch: admitted_authority_epoch,
                minimum_revocation: admitted_revocation_version,
                attestation_required: true,
            },
        )
        .map_err(|_| DispatchError::ReceiptMismatch)?;
        Ok(PreparedRemote {
            identity,
            route_bytes,
            observed_local_state,
            observed_remote_state,
            started_at,
            statement_id,
            output,
            retained_owner,
            receipt,
            semantic,
            class,
            reservation,
        })
    }

    /// Consumes a previously authenticated remote completion capability.
    pub(crate) fn complete_remote<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        admission: RemoteAdmission,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let scheduled = match plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(output) => return Ok(DispatchCompletion::Reused(output)),
            DispatchPlan::Waiting(waiter) => return Ok(DispatchCompletion::Waiting(waiter)),
        };
        let mut active_rollback =
            ActiveSemanticRollback::new(self, scheduled.identity().work_key());
        let mut prepared = self.prepare_remote_completion(&scheduled, admission)?;
        let statement_id = prepared.statement_id;
        let output = prepared.output;
        let identity = prepared.identity;
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let previous_freshness = self
            .authority_freshness
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&identity.work_key())
            .copied();
        let mut freshness_rollback =
            FreshnessRollback::new(self, identity.work_key(), previous_freshness);
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        self.apply_authority_freshness(
            identity.authority,
            identity.work_key(),
            &prepared.semantic,
            &mut accepted,
        )?;
        if self.is_authority_quarantined(identity.work_key()) {
            return Err(DispatchError::AuthorityConflict);
        }
        if let Err(error) = Self::check_authority_for_publication(
            &accepted,
            identity.work_key(),
            prepared.class,
            output,
        ) {
            return Err(self.handle_remote_authority_error(
                accepted,
                identity.work_key(),
                error,
                statement_id,
                output,
            ));
        }
        let mut semantic_reservation = self.reserve_semantic(&identity, &prepared.semantic)?;
        let semantic_generation = semantic_reservation
            .as_ref()
            .map(super::super::super::dependencies::SemanticDependencyReservation::generation);
        let reuse_generation = semantic_generation.and_then(NonZeroU64::new);
        let cacheable = prepared.semantic.dependency_manifest().is_some();
        let derived_proof = Self::prepare_derived_output_proof(
            &prepared.receipt,
            &prepared.semantic,
            Arc::clone(&prepared.retained_owner),
            semantic_generation,
            prepared.class,
        )?;
        if derived_proof.is_some() {
            self.reserve_derived_output_proof_slot(identity.work_key())?;
        }
        let completion_reservation = self.reserve_completion(&prepared.receipt, cacheable)?;
        // A remote-required route has no hedge race and is completed through
        // the scheduler's ordinary publication path. Only a true hedge uses
        // `complete_side`; the lower scheduler intentionally rejects a
        // remote side claim on a non-hedged route.
        let receipt = self.publish_remote_result(
            scheduled,
            &prepared.receipt,
            now,
            cacheable,
            reuse_generation,
        )?;
        self.record_remote_publication(
            &RemotePublication {
                identity,
                semantic: &prepared.semantic,
                class: prepared.class,
                output,
                statement_id,
                semantic_generation,
                cacheable,
                receipt: &receipt,
            },
            &mut accepted,
        );
        completion_reservation.commit();
        if let Some(reservation) = semantic_reservation.as_mut() {
            reservation.commit();
        }
        freshness_rollback.commit();
        self.retain_derived_output_proof(identity.work_key(), derived_proof);
        active_rollback.commit();
        self.record_remote_cost(&prepared, now);
        let result = DispatchCompletion::Accepted(receipt);
        prepared.reservation.commit();
        Ok(result)
    }

    fn record_remote_cost<R: Relation>(&self, prepared: &PreparedRemote<R>, now: u64) {
        let elapsed = now.saturating_sub(prepared.started_at);
        self.costs.record_remote(
            &prepared.identity,
            prepared.route_bytes,
            prepared.observed_local_state,
            prepared.observed_remote_state,
            backend_execution::CompletionCost {
                execution: elapsed,
                ..backend_execution::CompletionCost::default()
            },
            now,
        );
    }

    fn record_remote_publication<R: Relation>(
        &self,
        publication: &RemotePublication<'_, R>,
        accepted: &mut std::collections::BTreeMap<WorkKey, AcceptedAuthority>,
    ) {
        self.retire_evicted(
            publication.identity.work_key(),
            publication.receipt.evicted_keys(),
            accepted,
        );
        accepted.insert(
            publication.identity.work_key(),
            AcceptedAuthority {
                authority: publication.identity.authority,
                class: publication.class,
                output: publication.output,
                statement_id: publication.statement_id,
                semantic_generation: publication.semantic_generation,
                authority_epoch: publication.semantic.authority_epoch().0,
                revocation_version: publication.semantic.revocation_version().0,
                cacheable: publication.cacheable,
                semantic_manifest: publication
                    .semantic
                    .dependency_manifest()
                    .map(|manifest| manifest.version().to_bytes()),
                semantic_identity: publication.semantic.identity().to_bytes(),
                semantic_scope: publication.semantic.scope(),
                semantic_read_manifest: publication.semantic.read_manifest().to_bytes(),
                reuse_context: if publication.cacheable {
                    publication
                        .receipt
                        .reusable()
                        .map(backend_execution::ReusableOutput::context)
                } else {
                    None
                },
            },
        );
        self.record_authority_index(
            publication.identity.authority,
            publication.identity.work_key(),
        );
    }

    fn publish_remote_result<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
        cacheable: bool,
        reuse_generation: Option<NonZeroU64>,
    ) -> Result<backend_execution::ScheduleReceipt, DispatchError> {
        let result = if scheduled.is_hedged() {
            self.scheduler.complete_side_with_reuse_policy(
                scheduled,
                HedgeSide::Remote,
                receipt,
                now,
                cacheable,
                reuse_generation,
            )
        } else {
            self.scheduler.complete_with_reuse_policy(
                scheduled,
                receipt,
                now,
                cacheable,
                reuse_generation,
            )
        };
        result.map_err(DispatchError::Schedule)
    }

    /// Consumes an owned wire result through remote admission and completion.
    ///
    /// This is the allocation-preserving counterpart to
    /// [`Self::complete_remote_wire`].
    #[cfg(test)]
    pub(crate) fn complete_remote_wire_owned<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        wire: WireRecipeResult,
        contract: &RemoteDispatchContract,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let route = match &plan {
            DispatchPlan::Scheduled(scheduled) => Some((
                scheduled.identity(),
                scheduled.charged_bytes(),
                scheduled.observed_local_state(),
                scheduled.observed_remote_state(),
            )),
            DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => None,
        };
        let admission = match self.admit_remote_owned(&plan, wire, contract) {
            Ok(admission) => admission,
            Err(error) => {
                if let Some((identity, bytes, local, remote)) = route {
                    self.retire_active_semantic(identity.work_key());
                    self.costs
                        .record_remote_failure(&identity, bytes, local, remote, now);
                }
                return Err(error);
            }
        };
        match self.complete_remote(plan, admission, now) {
            Ok(completion) => Ok(completion),
            Err(error) => {
                if let Some((identity, bytes, local, remote)) = route {
                    self.costs
                        .record_remote_failure(&identity, bytes, local, remote, now);
                }
                Err(error)
            }
        }
    }
}
