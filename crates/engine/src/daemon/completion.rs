//! Completion capability retention and local result publication.

use super::{
    Arc, COMPLETION_RETAINED_BYTES, CompleteSemanticCoverage, Daemon, DaemonError,
    DerivedOutputEntry, DispatchCompletion, DispatchError, DispatchPlan, QueueSized, Relation,
    ScheduleRequest, WorkspaceModel,
};
use crate::workspace::owner::LatestQuery;

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    /// Removes an acknowledged completion capability and releases its
    /// retained-state budget. Completion capabilities are deliberately
    /// acknowledgement based so deduplication cannot become a permanent
    /// memory leak after a long running daemon.
    pub fn acknowledge_completion(&mut self, work_key: [u8; 32]) -> bool {
        let Some(next_bytes) = self.completion_bytes.checked_sub(COMPLETION_RETAINED_BYTES) else {
            return false;
        };
        let Some(notice) = self.completions.get(&work_key).cloned() else {
            return false;
        };
        self.completions.remove(&work_key);
        self.dispatcher.revoke_completion_fields(
            notice.work_key,
            notice.output,
            notice.ordinal,
            notice.fence,
        );
        self.completion_bytes = next_bytes;
        if let Some(sequence) = self.completion_sequences.remove(&work_key) {
            self.completion_order.remove(&sequence);
        }
        true
    }

    pub(super) fn evict_oldest_completion(&mut self) -> bool {
        loop {
            let Some((&sequence, &oldest)) = self.completion_order.iter().next() else {
                return false;
            };
            self.completion_order.remove(&sequence);
            let Some(notice) = self.completions.remove(&oldest) else {
                self.completion_sequences.remove(&oldest);
                continue;
            };
            let Some(next_bytes) = self.completion_bytes.checked_sub(COMPLETION_RETAINED_BYTES)
            else {
                self.completions.insert(oldest, notice);
                self.completion_order.insert(sequence, oldest);
                return false;
            };
            self.completion_sequences.remove(&oldest);
            self.dispatcher.revoke_completion_fields(
                notice.work_key,
                notice.output,
                notice.ordinal,
                notice.fence,
            );
            self.completion_bytes = next_bytes;
            return true;
        }
    }
    pub(super) fn persist_dispatch_completion(
        &mut self,
        completion: &DispatchCompletion,
    ) -> Result<(), DaemonError> {
        let DispatchCompletion::Accepted(receipt) = completion else {
            return Ok(());
        };
        let key = receipt.key();
        let expected = self.owner.head().expectation();
        let Some(staged) = self
            .dispatcher
            .stage_derived_output(key, |proof| {
                self.owner
                    .stage_derived_output(expected, proof.clone())
                    .map_err(|error| {
                        DispatchError::Workspace(format!("stage derived output: {error}"))
                    })
            })
            .map_err(DaemonError::dispatch)?
        else {
            return Ok(());
        };
        let mut staged = Some(staged);
        self.dispatcher
            .publish_derived_output_retryable(key, |_proof| {
                let staged = staged.take().ok_or(DispatchError::Workspace(
                    "staged output already consumed".to_owned(),
                ))?;
                self.owner
                    .publish_staged_derived_output(staged)
                    .map(|_| ())
                    .map_err(|error| {
                        DispatchError::Workspace(format!("publish derived output: {error}"))
                    })
            })
            .map_err(DaemonError::dispatch)?;
        Ok(())
    }

    /// Stages the catalog proof and its immutable payload objects, leaving
    /// publication to the caller. Remote dispatch uses this split to append
    /// its `Accepted` record between CAS staging and workspace selection.
    pub(super) fn stage_dispatch_completion(
        &mut self,
        completion: &DispatchCompletion,
    ) -> Result<Option<crate::workspace::catalog::StagedDerivedOutput>, DaemonError> {
        let DispatchCompletion::Accepted(receipt) = completion else {
            return Ok(None);
        };
        let key = receipt.key();
        let expected = self.owner.head().expectation();
        self.dispatcher
            .stage_derived_output(key, |proof| {
                self.owner
                    .stage_derived_output(expected, proof.clone())
                    .map_err(|error| {
                        DispatchError::Workspace(format!("stage derived output: {error}"))
                    })
            })
            .map_err(DaemonError::dispatch)
    }

    /// Completes a local plan and publishes any reusable result into the
    /// store-owned derived-output catalog before returning it to the caller.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_local<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: &[u8],
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let completion = self
            .dispatcher
            .complete_local(plan, bytes, semantic, now)
            .map_err(DaemonError::dispatch)?;
        self.persist_dispatch_completion(&completion)?;
        Ok(completion)
    }

    /// Allocation-preserving local completion with durable catalog
    /// publication owned by the daemon.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_local_shared<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: Arc<Vec<u8>>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let completion = self
            .dispatcher
            .complete_local_shared(plan, bytes, semantic, now)
            .map_err(DaemonError::dispatch)?;
        self.persist_dispatch_completion(&completion)?;
        Ok(completion)
    }

    fn validate_derived_entry<R: Relation>(
        request: &ScheduleRequest<R>,
        semantic: &CompleteSemanticCoverage,
        entry: &DerivedOutputEntry,
    ) -> Result<(), DaemonError> {
        let manifest = semantic
            .dependency_manifest()
            .ok_or_else(|| DaemonError::dispatch(DispatchError::OutputContract))?;
        if entry.key() != request.identity.work_key()
            || !semantic.binds(&request.identity)
            || entry.dependency_manifest_bytes() != manifest.canonical_bytes()
            || entry.coverage_identity() != semantic.identity().to_bytes()
            || entry.scope() != semantic.scope()
            || entry.read_manifest() != semantic.read_manifest().to_bytes()
            || entry.authority() != request.identity.authority.to_bytes()
            || entry.authority_epoch() != semantic.authority_epoch().0
            || entry.revocation_version() != semantic.revocation_version().0
            || entry.dependency_generation() == 0
        {
            return Err(DaemonError::dispatch(
                DispatchError::IncompleteSemanticCoverage,
            ));
        }
        Ok(())
    }

    /// Plans one execution while consulting the store-owned derived-output
    /// catalog under the current admitted semantic capability. The owner
    /// performs the exact catalog lookup, the dispatcher revalidates and
    /// restores the catalog generation, and only then is the output exposed
    /// as a reusable plan. A catalog miss falls through to ordinary routing;
    /// callers never need to coordinate a raw lookup and a separate hydrate
    /// operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the semantic capability, selected catalog entry,
    /// route admission, or durable publication fails.
    pub fn plan_with_durable_reuse<R: Relation>(
        &mut self,
        request: ScheduleRequest<R>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchPlan<R>, DaemonError> {
        self.dispatcher
            .validate_semantic_for_reuse(&request.identity, &semantic)
            .map_err(DaemonError::dispatch)?;
        let Some(manifest) = semantic.dependency_manifest() else {
            return self
                .dispatcher
                .plan_with_semantic(request, &semantic)
                .map_err(DaemonError::dispatch);
        };
        let key = request.identity.work_key();
        let manifest_bytes = manifest.canonical_bytes();
        let query = LatestQuery {
            key,
            dependency_manifest: &manifest_bytes,
            authority: request.identity.authority.to_bytes(),
            authority_epoch: semantic.authority_epoch().0,
            revocation_version: semantic.revocation_version().0,
            coverage_identity: semantic.identity().to_bytes(),
            scope: semantic.scope(),
            read_manifest: semantic.read_manifest().to_bytes(),
        };
        let Some(entry) = self
            .owner
            .lookup_latest_derived_output(&query)
            .map_err(DaemonError::from)?
        else {
            return self
                .dispatcher
                .plan_with_semantic(request, &semantic)
                .map_err(DaemonError::dispatch);
        };
        Self::validate_derived_entry(&request, &semantic, &entry)?;
        if !self
            .dispatcher
            .cached_authority_allowed(entry.authority_class())
        {
            return Err(DaemonError::dispatch(DispatchError::AuthorityRejected));
        }

        let plan = self
            .dispatcher
            .plan_with_semantic(request, &semantic)
            .map_err(DaemonError::dispatch)?;
        match plan {
            DispatchPlan::Reused(output) => {
                if output.output() != entry.output() || output.canonical_bytes() != entry.bytes() {
                    return Err(DaemonError::dispatch(DispatchError::OutputMismatch));
                }
                Ok(DispatchPlan::Reused(output))
            }
            DispatchPlan::Scheduled(_) | DispatchPlan::Waiting(_) => {
                let completion = self
                    .dispatcher
                    .hydrate_local_shared(
                        plan,
                        entry.bytes_arc(),
                        semantic,
                        entry.dependency_generation(),
                        entry.authority_class(),
                        now,
                    )
                    .map_err(DaemonError::dispatch)?;
                self.persist_dispatch_completion(&completion)?;
                match completion {
                    DispatchCompletion::Reused(output) => Ok(DispatchPlan::Reused(output)),
                    DispatchCompletion::Accepted(receipt) => receipt
                        .reusable()
                        .cloned()
                        .map(DispatchPlan::Reused)
                        .ok_or_else(|| DaemonError::dispatch(DispatchError::OutputContract)),
                    DispatchCompletion::Waiting(waiter) => Ok(DispatchPlan::Waiting(waiter)),
                }
            }
        }
    }
}
