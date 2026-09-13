use super::{
    ActiveSemantic, Arc, AttestationVerifier, Cancellation, CompleteSemanticCoverage,
    CompletionIdentity, CompletionReservation, DispatchError, DispatchPlan, Dispatcher, HedgeSide,
    OutputAdmissionValidator, Relation, ResultReceipt, ScheduleOutcome, ScheduleRequest,
    SemanticCoverageValidator, authority, dependencies,
};

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Plans exactly once while retaining all scheduler guards.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn plan<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        // Reuse lookup and accepted-authority state are one publication view.
        // Serializing planning with completion prevents a caller from seeing
        // the scheduler's cache entry in the small interval between scheduler
        // publication and the accepted-map commit.
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        // The plain planning API has no engine-minted freshness capability.
        // It may start work, but it cannot turn a retained entry's own context
        // into authority for reuse.
        self.plan_locked(request, None, false)
    }

    pub(super) fn plan_locked<R: Relation>(
        &self,
        mut request: ScheduleRequest<R>,
        requested_reuse_context: Option<backend_execution::ReuseContext>,
        allow_reuse: bool,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        let key = request.identity.work_key();
        let quarantined = self.is_authority_quarantined(key);
        if quarantined {
            self.normalize_quarantined_request(&mut request, key)?;
        }
        let accepted_authority = self.current_accepted_authority(key)?;
        let reusable = self.reusable_authority(key, accepted_authority.as_ref());
        if !allow_reuse && reusable {
            return Err(DispatchError::ReuseContextRequired);
        }
        if !reusable {
            self.retire_nonreusable(key, accepted_authority.is_some())?;
        }
        // Request.costs remains on the lower DTO for source compatibility,
        // but the dispatcher always chooses from its bounded owner model.
        request.costs = self
            .costs
            .snapshot(
                &request.identity,
                request.bytes,
                request.local.state(),
                request.remote.state(),
                request.now,
            )
            .map_err(|_| DispatchError::CostUnavailable)?;
        let reuse_context = if allow_reuse && reusable {
            accepted_authority
                .and_then(|entry| entry.reuse_context)
                .filter(|stored| requested_reuse_context == Some(*stored))
        } else {
            None
        };
        if allow_reuse && reusable && reuse_context.is_none() {
            return Err(DispatchError::ReuseContextRequired);
        }
        let plan = match self
            .scheduler
            .schedule_or_reuse_with_context(request, reuse_context.as_ref())
            .map_err(DispatchError::Schedule)?
        {
            ScheduleOutcome::Reused(output) => Ok(DispatchPlan::Reused(output)),
            ScheduleOutcome::Scheduled(schedule) => Ok(DispatchPlan::Scheduled(*schedule)),
            ScheduleOutcome::Waiting(waiter) => Ok(DispatchPlan::Waiting(waiter)),
        }?;
        if quarantined && matches!(&plan, DispatchPlan::Reused(_) | DispatchPlan::Waiting(_)) {
            self.cancel(plan);
            return Err(DispatchError::AuthorityConflict);
        }
        Ok(plan)
    }

    fn normalize_quarantined_request<R: Relation>(
        &self,
        request: &mut ScheduleRequest<R>,
        key: backend_execution::WorkKey,
    ) -> Result<(), DispatchError> {
        if request.class == backend_execution::PlacementClass::RemoteRequired
            && self.scheduler.attempts().current(key).is_some()
        {
            return Err(DispatchError::AuthorityConflict);
        }
        if request.class != backend_execution::PlacementClass::RemoteRequired {
            request.class = backend_execution::PlacementClass::LocalPreferred;
            request.hedge = false;
            request.fallback_reserved = false;
            request.fallback_deadline = None;
        }
        Ok(())
    }

    fn current_accepted_authority(
        &self,
        key: backend_execution::WorkKey,
    ) -> Result<Option<authority::AcceptedAuthority>, DispatchError> {
        self.accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)
            .map(|accepted| accepted.get(&key).copied())
    }

    fn reusable_authority(
        &self,
        key: backend_execution::WorkKey,
        accepted: Option<&authority::AcceptedAuthority>,
    ) -> bool {
        accepted.is_some_and(|accepted| {
            accepted.cacheable
                && accepted.reuse_context.is_some()
                && self
                    .authority_freshness
                    .lock()
                    .ok()
                    .and_then(|freshness| freshness.get(&key).copied())
                    .is_some_and(|freshness| {
                        freshness.authority_epoch == accepted.authority_epoch
                            && freshness.revocation_version == accepted.revocation_version
                    })
        })
    }

    fn retire_nonreusable(
        &self,
        key: backend_execution::WorkKey,
        has_accepted: bool,
    ) -> Result<(), DispatchError> {
        let _ = self.scheduler.output_lookup().invalidate(key);
        if !has_accepted {
            return Ok(());
        }
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let removed = accepted.remove(&key);
        let semantic_generation = removed
            .as_ref()
            .and_then(|accepted| accepted.semantic_generation);
        if let Some(accepted) = removed {
            self.remove_authority_index(accepted.authority, key);
        }
        self.replayed.release_committed_for_key(key);
        self.completed
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .remove(&key.to_bytes());
        if let Ok(mut freshness) = self.authority_freshness.lock() {
            freshness.remove(&key);
        }
        if let Some(generation) = semantic_generation {
            self.semantic.release(key, generation);
        }
        self.retire_active_semantic(key);
        self.retire_or_quarantine_attempt(key);
        Ok(())
    }

    /// Plans with a currently admitted semantic capability. A changed
    /// dependency manifest retires any previous publication before the lower
    /// scheduler is allowed to consult its hot lookup; the lower work key is
    /// therefore never treated as a complete semantic identity by itself.
    /// Plans with an owner admitted semantic capability, allowing an exact
    /// reuse lookup only when the retained dependency context still matches.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError`] when the capability is stale or mismatched,
    /// the retained output binding is invalid, or bounded scheduling cannot
    /// admit the request.
    pub fn plan_with_semantic<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        // Keep semantic validation, freshness observation, invalidation, and
        // the lower scheduler lookup under the same authority transaction.
        // Otherwise a selector change could pass validation, race a plan, and
        // install an active registration after invalidation had already run.
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        self.validate_semantic_capability(&request.identity, semantic)?;
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        self.apply_authority_freshness(
            request.identity.authority,
            request.identity.work_key(),
            semantic,
            &mut accepted,
        )?;
        drop(accepted);
        let expected_manifest = semantic
            .dependency_manifest()
            .map(|manifest| manifest.version().to_bytes());
        let expected_identity = semantic.identity().to_bytes();
        let expected_scope = semantic.scope();
        let expected_read_manifest = semantic.read_manifest().to_bytes();
        let key = request.identity.work_key();
        let semantic_binding_changed = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&key)
            .is_some_and(|accepted| {
                accepted.semantic_manifest != expected_manifest
                    || accepted.semantic_identity != expected_identity
                    || accepted.semantic_scope != expected_scope
                    || accepted.semantic_read_manifest != expected_read_manifest
            });
        if semantic_binding_changed {
            self.revoke_published_output_locked(key);
        }

        // Reuse is enabled only after this semantic capability has been
        // validated and its exact binding still matches the accepted row. The
        // context comes from that prior engine publication; the lower lookup
        // never manufactures one from its own retained bytes.
        let requested_reuse_context = if semantic_binding_changed {
            None
        } else {
            self.accepted
                .lock()
                .map_err(|_| DispatchError::AuthorityRejected)?
                .get(&key)
                .filter(|accepted| {
                    accepted.cacheable
                        && accepted.semantic_manifest == expected_manifest
                        && accepted.semantic_identity == expected_identity
                        && accepted.semantic_scope == expected_scope
                        && accepted.semantic_read_manifest == expected_read_manifest
                        && accepted.authority_epoch == semantic.authority_epoch().0
                        && accepted.revocation_version == semantic.revocation_version().0
                        && self
                            .authority_freshness
                            .lock()
                            .ok()
                            .and_then(|freshness| freshness.get(&key).copied())
                            .is_some_and(|freshness| {
                                freshness.authority_epoch == accepted.authority_epoch
                                    && freshness.revocation_version == accepted.revocation_version
                            })
                })
                .and_then(|accepted| accepted.reuse_context)
        };
        let active_reservation = if let Some(active) = self
            .active_semantic
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&key)
        {
            if !active.matches(semantic) {
                return Err(DispatchError::IncompleteSemanticCoverage);
            }
            None
        } else if semantic.dependency_manifest().is_some() {
            self.reserve_semantic(&request.identity, semantic)?
        } else {
            None
        };
        let plan = match self.plan_locked(request, requested_reuse_context, true) {
            Ok(plan) => plan,
            Err(error) => {
                drop(active_reservation);
                return Err(error);
            }
        };
        if let DispatchPlan::Scheduled(_) = &plan
            && let Some(reservation) = active_reservation
        {
            let mut active = self
                .active_semantic
                .lock()
                .map_err(|_| DispatchError::AuthorityRejected)?;
            if active.len() >= authority::MAX_ACCEPTED_AUTHORITY_ENTRIES {
                drop(reservation);
                return Err(DispatchError::AuthorityRejected);
            }
            active.insert(key, ActiveSemantic::new(reservation, semantic));
        }
        Ok(plan)
    }

    /// Returns the scheduler cancellation observation for one route.
    #[must_use]
    pub fn cancellation<R: Relation>(
        &self,
        plan: &DispatchPlan<R>,
        side: HedgeSide,
    ) -> Option<Cancellation> {
        match plan {
            DispatchPlan::Scheduled(schedule) => schedule.cancellation(side),
            DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => None,
        }
    }

    /// Checks an opaque completion claim against a receipt accepted by this
    /// dispatcher. A matching digest tuple by itself is insufficient: the
    /// tuple is inserted only after scheduler publication and can be revoked
    /// when the daemon acknowledges the capability.
    pub(crate) fn admit_completion_fields(
        &self,
        work_key: [u8; 32],
        output: [u8; 32],
        ordinal: u32,
        fence: [u8; 32],
    ) -> Result<(), DispatchError> {
        let completed = self
            .completed
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let expected = CompletionIdentity {
            work_key,
            output,
            ordinal,
            fence,
        };
        if completed
            .get(&work_key)
            .is_some_and(|actual| *actual == expected)
        {
            Ok(())
        } else {
            Err(DispatchError::ReceiptMismatch)
        }
    }

    /// Revokes one daemon completion capability after its acknowledgement.
    pub(crate) fn revoke_completion_fields(
        &self,
        work_key: [u8; 32],
        output: [u8; 32],
        ordinal: u32,
        fence: [u8; 32],
    ) {
        if let Ok(mut completed) = self.completed.lock()
            && completed.get(&work_key).is_some_and(|actual| {
                *actual
                    == CompletionIdentity {
                        work_key,
                        output,
                        ordinal,
                        fence,
                    }
            })
        {
            completed.remove(&work_key);
        }
    }

    pub(crate) fn reserve_completion(
        &self,
        receipt: &ResultReceipt<impl Relation>,
        cacheable: bool,
    ) -> Result<CompletionReservation, DispatchError> {
        use authority::MAX_ACCEPTED_AUTHORITY_ENTRIES;
        let id = CompletionIdentity {
            work_key: receipt.key().to_bytes(),
            output: receipt.result().to_bytes(),
            ordinal: receipt.ordinal(),
            fence: *receipt.fence().as_bytes(),
        };
        let mut completed = self
            .completed
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        // Completion capabilities are meaningful only while the lower
        // scheduler can retain/retrieve the exact canonical payload. This
        // ties bounded completion metadata to the same publication lease as
        // the reusable lookup instead of creating an unbounded digest index.
        let same_work_key = completed.contains_key(&id.work_key);
        let previous = completed.get(&id.work_key).copied();
        if completed
            .get(&id.work_key)
            .is_some_and(|existing| *existing == id)
            || (cacheable
                && !self
                    .scheduler
                    .output_lookup()
                    .can_retain_capacity(receipt.canonical_bytes_capacity()))
            || (completed.len() >= MAX_ACCEPTED_AUTHORITY_ENTRIES && !same_work_key)
        {
            return Err(DispatchError::AuthorityRejected);
        }
        completed.insert(id.work_key, id);
        Ok(CompletionReservation {
            completed: Arc::clone(&self.completed),
            id,
            previous,
            committed: false,
        })
    }

    pub(super) fn reserve_semantic<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<Option<dependencies::SemanticDependencyReservation>, DispatchError> {
        self.reserve_semantic_at_generation(identity, semantic, None)
    }

    pub(super) fn reserve_semantic_at_generation<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        semantic: &CompleteSemanticCoverage,
        generation: Option<u64>,
    ) -> Result<Option<dependencies::SemanticDependencyReservation>, DispatchError> {
        let Some(manifest) = semantic.dependency_manifest() else {
            return Ok(None);
        };
        let reservation = match generation {
            Some(generation) => self
                .semantic
                .reserve_at_generation(identity, manifest, generation),
            None => self.semantic.reserve(identity, manifest),
        };
        reservation
            .map(Some)
            .map_err(DispatchError::SemanticDependency)
    }
}
