use super::super::CompletionReservation;
use super::{
    AcceptedAuthority, ActiveSemanticRollback, Arc, AttestationClass, AttestationVerifier,
    CompleteSemanticCoverage, DispatchCompletion, DispatchError, DispatchPlan, Dispatcher,
    FreshnessRollback, HedgeSide, LocalAuthorityVerifier, LowerOutputValidator, NonZeroU64,
    OutputAdmissionValidator, OutputVersion, Relation, ResultCoverage, ResultReceipt, Scheduled,
    SemanticCoverageValidator, UntrustedResultReceipt, local_statement_id,
};

struct LocalCompletionInput {
    bytes: Arc<Vec<u8>>,
    semantic: CompleteSemanticCoverage,
    now: u64,
    expected_generation: Option<u64>,
    authority_class: AttestationClass,
    retain_proof: bool,
}

struct LocalCompletionContext {
    route_bytes: u64,
    observed_remote_state: backend_execution::RemoteState,
    started_at: u64,
}

struct LocalCommitData<'a, R: Relation> {
    input: &'a LocalCompletionInput,
    prepared: &'a PreparedLocal<R>,
    context: &'a LocalCompletionContext,
    statement_id: [u8; 32],
    semantic_generation: Option<u64>,
    cacheable: bool,
    derived_proof: Option<crate::workspace::catalog::DerivedOutputProof>,
    receipt: backend_execution::ScheduleReceipt,
}

struct PreparedLocal<R: Relation> {
    identity: backend_execution::VersionedWorkIdentity<R>,
    output: OutputVersion,
    canonical_owner: Arc<Vec<u8>>,
    receipt: ResultReceipt<R>,
}

struct LocalPublication<'a, R: Relation> {
    identity: backend_execution::VersionedWorkIdentity<R>,
    semantic: &'a CompleteSemanticCoverage,
    authority_class: AttestationClass,
    output: OutputVersion,
    statement_id: [u8; 32],
    semantic_generation: Option<u64>,
    cacheable: bool,
    receipt: &'a backend_execution::ScheduleReceipt,
}

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Admits canonical local output with semantic proof and consumes the
    /// schedule guard.
    ///
    /// This borrowed compatibility path materializes one shared owner for
    /// `bytes`. Local workers or CAS adapters that already own an immutable
    /// buffer should use [`Self::complete_local_shared`] to preserve that
    /// allocation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_local<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        bytes: &[u8],
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let scheduled = match plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(output) => return Ok(DispatchCompletion::Reused(output)),
            DispatchPlan::Waiting(waiter) => return Ok(DispatchCompletion::Waiting(waiter)),
        };
        self.complete_scheduled_local(scheduled, bytes, semantic, now)
    }

    /// Admits an Arc-backed canonical local output without copying its bytes.
    ///
    /// The validator, receipt, scheduler publication, and reusable lookup all
    /// retain Arc handles to the supplied immutable allocation. The caller may
    /// drop its input handle after this method returns.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_local_shared<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        bytes: Arc<Vec<u8>>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let scheduled = match plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(output) => return Ok(DispatchCompletion::Reused(output)),
            DispatchPlan::Waiting(waiter) => return Ok(DispatchCompletion::Waiting(waiter)),
        };
        let input = LocalCompletionInput {
            bytes,
            semantic,
            now,
            expected_generation: None,
            authority_class: AttestationClass::LocallyVerifiable,
            retain_proof: true,
        };
        self.complete_scheduled_local_shared_with_input(scheduled, &input)
    }

    /// Rehydrates one store-admitted reusable output after a process restart.
    /// The persisted semantic generation is restored before scheduler
    /// publication, so the hot lookup cannot accidentally accept a fresh
    /// process-local registration as the old dependency proof.
    pub(crate) fn hydrate_local_shared<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        bytes: Arc<Vec<u8>>,
        semantic: CompleteSemanticCoverage,
        dependency_generation: u64,
        authority_class: AttestationClass,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let scheduled = match plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(output) => return Ok(DispatchCompletion::Reused(output)),
            DispatchPlan::Waiting(waiter) => return Ok(DispatchCompletion::Waiting(waiter)),
        };
        let input = LocalCompletionInput {
            bytes,
            semantic,
            now,
            expected_generation: Some(dependency_generation),
            authority_class,
            retain_proof: false,
        };
        self.complete_scheduled_local_shared_with_input(scheduled, &input)
    }

    pub(super) fn complete_scheduled_local<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        bytes: &[u8],
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let bytes = Arc::new(bytes.to_vec());
        let input = LocalCompletionInput {
            bytes,
            semantic,
            now,
            expected_generation: None,
            authority_class: AttestationClass::LocallyVerifiable,
            retain_proof: true,
        };
        self.complete_scheduled_local_shared_with_input(scheduled, &input)
    }

    fn complete_scheduled_local_shared_with_input<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        input: &LocalCompletionInput,
    ) -> Result<DispatchCompletion, DispatchError> {
        let prepared = self.prepare_local(&scheduled, input)?;
        let context = LocalCompletionContext {
            route_bytes: scheduled.charged_bytes(),
            observed_remote_state: scheduled.observed_remote_state(),
            started_at: scheduled.lease().observed_at(),
        };
        self.commit_local_completion(scheduled, input, &prepared, &context)
    }

    fn commit_local_completion<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        input: &LocalCompletionInput,
        prepared: &PreparedLocal<R>,
        context: &LocalCompletionContext,
    ) -> Result<DispatchCompletion, DispatchError> {
        let identity = prepared.identity;
        let mut active_rollback = ActiveSemanticRollback::new(self, identity.work_key());
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
            &input.semantic,
            &mut accepted,
        )?;
        let statement_id = local_statement_id(&input.semantic, prepared.output);
        self.check_local_authority(
            &accepted,
            identity.work_key(),
            input.authority_class,
            prepared.output,
            statement_id,
        )?;
        let mut semantic_reservation = self.reserve_semantic_at_generation(
            &identity,
            &input.semantic,
            input.expected_generation,
        )?;
        let semantic_generation = semantic_reservation
            .as_ref()
            .map(super::super::dependencies::SemanticDependencyReservation::generation);
        let reuse_generation = semantic_generation.and_then(NonZeroU64::new);
        let cacheable = input.semantic.dependency_manifest().is_some();
        let derived_proof = Self::prepare_local_proof(input, prepared, semantic_generation)?;
        if derived_proof.is_some() {
            self.reserve_derived_output_proof_slot(identity.work_key())?;
        }
        let completion_reservation = self.reserve_completion(&prepared.receipt, cacheable)?;
        let receipt = self
            .scheduler
            .complete_with_reuse_policy(
                scheduled,
                &prepared.receipt,
                input.now,
                cacheable,
                reuse_generation,
            )
            .map_err(DispatchError::Schedule)?;
        Ok(self.finish_local_completion(
            LocalCommitData {
                input,
                prepared,
                context,
                statement_id,
                semantic_generation,
                cacheable,
                derived_proof,
                receipt,
            },
            &mut accepted,
            &mut semantic_reservation,
            completion_reservation,
            &mut freshness_rollback,
            &mut active_rollback,
        ))
    }

    fn check_local_authority(
        &self,
        accepted: &std::collections::BTreeMap<backend_execution::WorkKey, AcceptedAuthority>,
        key: backend_execution::WorkKey,
        class: AttestationClass,
        output: OutputVersion,
        statement_id: [u8; 32],
    ) -> Result<(), DispatchError> {
        if let Err(error) = Self::check_authority_for_publication(accepted, key, class, output) {
            if matches!(error, DispatchError::AuthorityConflict)
                && let Some((previous_statement, previous_output)) = accepted
                    .get(&key)
                    .map(|authority| (authority.statement_id, authority.output))
            {
                self.quarantine_authority_conflict(
                    key,
                    previous_statement,
                    previous_output,
                    statement_id,
                    output,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn prepare_local_proof<R: Relation>(
        input: &LocalCompletionInput,
        prepared: &PreparedLocal<R>,
        semantic_generation: Option<u64>,
    ) -> Result<Option<crate::workspace::catalog::DerivedOutputProof>, DispatchError> {
        if !input.retain_proof {
            return Ok(None);
        }
        Self::prepare_derived_output_proof(
            &prepared.receipt,
            &input.semantic,
            Arc::clone(&prepared.canonical_owner),
            semantic_generation,
            input.authority_class,
        )
    }

    fn finish_local_completion<R: Relation>(
        &self,
        data: LocalCommitData<'_, R>,
        accepted: &mut std::collections::BTreeMap<backend_execution::WorkKey, AcceptedAuthority>,
        semantic_reservation: &mut Option<
            super::super::dependencies::SemanticDependencyReservation,
        >,
        completion_reservation: CompletionReservation,
        freshness_rollback: &mut FreshnessRollback<'_, V, A>,
        active_rollback: &mut ActiveSemanticRollback<'_, V, A>,
    ) -> DispatchCompletion {
        let LocalCommitData {
            input,
            prepared,
            context,
            statement_id,
            semantic_generation,
            cacheable,
            derived_proof,
            receipt,
        } = data;
        self.record_local_publication(
            &LocalPublication {
                identity: prepared.identity,
                semantic: &input.semantic,
                authority_class: input.authority_class,
                output: prepared.output,
                statement_id,
                semantic_generation,
                cacheable,
                receipt: &receipt,
            },
            accepted,
        );
        if input.authority_class == AttestationClass::LocallyVerifiable {
            self.clear_authority_quarantine(prepared.identity.work_key());
        }
        completion_reservation.commit();
        if let Some(reservation) = semantic_reservation.as_mut() {
            reservation.commit();
        }
        freshness_rollback.commit();
        self.retain_derived_output_proof(prepared.identity.work_key(), derived_proof);
        active_rollback.commit();
        let elapsed = input.now.saturating_sub(context.started_at);
        self.costs.record_local(
            &prepared.identity,
            context.route_bytes,
            context.observed_remote_state,
            elapsed,
            input.now,
        );
        DispatchCompletion::Accepted(receipt)
    }

    fn prepare_local<R: Relation>(
        &self,
        scheduled: &Scheduled<R>,
        input: &LocalCompletionInput,
    ) -> Result<PreparedLocal<R>, DispatchError> {
        let identity = scheduled.identity();
        self.validate_semantic_capability(&identity, &input.semantic)?;
        if scheduled
            .cancellation(HedgeSide::Local)
            .is_some_and(|cancellation| cancellation.is_cancelled())
        {
            return Err(DispatchError::Cancelled);
        }
        let output = OutputVersion::from_value(input.bytes.as_slice());
        let retained_output = OutputAdmissionValidator::validate_shared(
            &self.validator,
            output,
            &input.bytes,
            &input.semantic,
        )?;
        if retained_output.output() != output
            || OutputVersion::from_value(retained_output.canonical_bytes()) != output
        {
            return Err(DispatchError::OutputMismatch);
        }
        let canonical_owner = retained_output.canonical_bytes_arc();
        let lease = scheduled.lease();
        let wire = UntrustedResultReceipt {
            key: identity.work_key().to_bytes(),
            input: identity.input.to_bytes(),
            recipe: identity.recipe.to_bytes(),
            read_manifest: identity.read_manifest.to_bytes(),
            authority: identity.authority.to_bytes(),
            authority_epoch: input.semantic.authority_epoch().0,
            revocation_version: input.semantic.revocation_version().0,
            attestation: None,
            output_equivalence: identity.output_equivalence.to_bytes(),
            output: output.to_bytes(),
            output_canonical_bytes: retained_output.canonical_bytes_arc(),
            coverage: ResultCoverage::Complete,
            ordinal: lease.ordinal(),
            fence: *lease.fence().as_bytes(),
        };
        let receipt = ResultReceipt::admit_wire(
            identity,
            lease,
            wire,
            &LowerOutputValidator {
                validator: &self.validator,
                semantic: &input.semantic,
                canonical_owner: Some(&canonical_owner),
            },
            &LocalAuthorityVerifier {
                minimum_epoch: input.semantic.authority_epoch().0,
                minimum_revocation: input.semantic.revocation_version().0,
            },
        )
        .map_err(|_| DispatchError::ReceiptMismatch)?;
        Ok(PreparedLocal {
            identity,
            output,
            canonical_owner,
            receipt,
        })
    }

    fn record_local_publication<R: Relation>(
        &self,
        publication: &LocalPublication<'_, R>,
        accepted: &mut std::collections::BTreeMap<backend_execution::WorkKey, AcceptedAuthority>,
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
                class: publication.authority_class,
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
}
