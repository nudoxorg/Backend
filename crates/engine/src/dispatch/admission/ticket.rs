use super::{
    ActiveSemanticRollback, Arc, AttestationVerifier, DispatchCompletion, DispatchError,
    DispatchPlan, DispatchTicket, Dispatcher, OutputAdmissionValidator, PendingRemoteEnvelope,
    Relation, RemoteDispatchContract, SemanticCoverageValidator, WireRecipeRequest,
    WireRecipeResult, request_expectation,
};

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Cancels a scheduled route and retains loser reservations until the
    /// affine guard is consumed.
    pub fn cancel<R: Relation>(&self, plan: DispatchPlan<R>) {
        if let DispatchPlan::Scheduled(schedule) = plan {
            let key = schedule.key();
            self.retire_active_semantic(key);
            schedule.cancel();
            // A cancelled route has no retained publication that could keep
            // its stale ordinal useful. Retire the lower terminal record and
            // its history together so cancellation floods cannot grow the
            // attempt table independently of output retention.
            let _ = self.scheduler.attempts().retire_and_forget(key);
        }
    }

    /// Binds a scheduled remote plan to its exact contract and request
    /// expectation. The returned ticket is the sole public completion handle
    /// for that remote attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn dispatch_ticket<R: Relation>(
        &self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
    ) -> Result<DispatchTicket<R>, DispatchError> {
        let key = match &plan {
            DispatchPlan::Scheduled(scheduled) => scheduled.key(),
            DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
                return Err(DispatchError::AuthorityRejected);
            }
        };
        let active_rollback = ActiveSemanticRollback::new(self, key);
        let scheduled = match &plan {
            DispatchPlan::Scheduled(scheduled) => scheduled,
            DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
                return Err(DispatchError::AuthorityRejected);
            }
        };
        Self::require_remote_route(scheduled)?;
        let identity = scheduled.identity();
        self.validate_semantic_capability(&identity, &contract.semantic)?;
        if contract.authority_epoch != contract.semantic.authority_epoch()
            || contract.revocation_version != contract.semantic.revocation_version()
        {
            return Err(DispatchError::IncompleteSemanticCoverage);
        }
        let expected = request_expectation(scheduled, &contract)?;
        expected
            .validate(contract.limits)
            .map_err(DispatchError::Replication)?;
        let DispatchPlan::Scheduled(scheduled) = plan else {
            return Err(DispatchError::AuthorityRejected);
        };
        active_rollback.keep();
        Ok(DispatchTicket::new(
            scheduled,
            contract,
            expected,
            Arc::clone(&self.active_semantic),
        ))
    }

    /// Erases the relation marker while retaining the typed ticket in a
    /// checked completion callback for a single-owner daemon map.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn into_pending_remote<R: Relation + Send>(
        &self,
        ticket: DispatchTicket<R>,
    ) -> Result<PendingRemoteEnvelope<V, A>, DispatchError> {
        PendingRemoteEnvelope::from_ticket(ticket, Arc::clone(&self.active_semantic))
    }

    /// Consumes a ticket into a relation-erased pending envelope and the
    /// already checked wire request. A transport owner can send the returned
    /// request and register the envelope; the envelope retains the exact
    /// request for reconnect/retry and keeps the scheduler ticket live until
    /// completion, fallback, or cancellation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn begin_remote<R: Relation + Send>(
        &self,
        ticket: DispatchTicket<R>,
    ) -> Result<(PendingRemoteEnvelope<V, A>, WireRecipeRequest), DispatchError> {
        let request = ticket.wire_request()?;
        let envelope = self.into_pending_remote(ticket)?;
        Ok((envelope, request))
    }

    /// Completes a result through the exact plan, contract, expectation, and
    /// cancellation identity retained by a ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_remote_ticket<R: Relation>(
        &self,
        ticket: DispatchTicket<R>,
        wire: WireRecipeResult,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let mut ticket = ticket;
        self.complete_remote_ticket_checked(&mut ticket, wire, now)
    }

    /// Completes a ticket only after exact correlation succeeds. A malformed
    /// or stale frame leaves the borrowed ticket armed so its owner can retry,
    /// cancel, or explicitly activate local fallback.
    pub(crate) fn complete_remote_ticket_checked<R: Relation>(
        &self,
        ticket: &mut DispatchTicket<R>,
        wire: WireRecipeResult,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        if ticket.is_consumed() {
            return Err(DispatchError::TicketConsumed);
        }
        if !ticket.matches_wire(&wire)? {
            return Err(DispatchError::TicketMismatch);
        }
        // Perform every fallible wire/authority check while the ticket still
        // owns its scheduler guard. An invalid attestation, output, or
        // semantic witness therefore leaves the caller's ticket available for
        // explicit fallback or cancellation.
        let route = ticket.scheduled().ok().map(|scheduled| {
            (
                scheduled.identity(),
                scheduled.charged_bytes(),
                scheduled.observed_local_state(),
                scheduled.observed_remote_state(),
            )
        });
        let admission =
            match self.admit_remote_scheduled(ticket.scheduled()?, wire, ticket.contract()?) {
                Ok(admission) => admission,
                Err(error) => {
                    if let Some((identity, bytes, local, remote)) = route {
                        self.costs
                            .record_remote_failure(&identity, bytes, local, remote, now);
                    }
                    return Err(error);
                }
            };
        let (plan, _contract, _expected, _cancellation) = ticket.take_parts()?;
        // `admission` already performed exact validation and retained the
        // owned wire; completion only performs the publication transaction.
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

    /// Activates a retained local fallback and publishes its result while
    /// consuming the remote ticket and its semantic capability.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fallback_remote_ticket<R: Relation>(
        &self,
        ticket: DispatchTicket<R>,
        bytes: &[u8],
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.fallback_remote_ticket_shared(ticket, Arc::new(bytes.to_vec()), now)
    }

    /// Activates a remote ticket's local fallback while transferring an
    /// immutable output owner through the scheduler without a second copy.
    pub(crate) fn fallback_remote_ticket_shared<R: Relation>(
        &self,
        ticket: DispatchTicket<R>,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        let mut ticket = ticket;
        self.fallback_remote_ticket_checked(&mut ticket, bytes, now)
    }

    pub(crate) fn fallback_remote_ticket_checked<R: Relation>(
        &self,
        ticket: &mut DispatchTicket<R>,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.fallback_remote_ticket_checked_with(ticket, bytes, now, false)
    }

    pub(crate) fn fallback_remote_ticket_checked_with<R: Relation>(
        &self,
        ticket: &mut DispatchTicket<R>,
        bytes: Arc<Vec<u8>>,
        now: u64,
        remote_failed: bool,
    ) -> Result<DispatchCompletion, DispatchError> {
        if ticket.is_consumed() {
            return Err(DispatchError::TicketConsumed);
        }
        let scheduled = ticket.scheduled_mut()?;
        let key = scheduled.key();
        let activated = if remote_failed {
            scheduled.activate_fallback_after_remote_failure(now)
        } else {
            scheduled.activate_fallback(now)
        };
        activated.map_err(|error| {
            self.retire_active_semantic(key);
            DispatchError::Schedule(error)
        })?;
        let (plan, contract, _expected, _cancellation) = ticket.take_parts()?;
        let DispatchPlan::Scheduled(scheduled) = plan else {
            return Err(DispatchError::AuthorityRejected);
        };
        self.complete_local_shared(
            DispatchPlan::Scheduled(scheduled),
            bytes,
            contract.semantic,
            now,
        )
    }

    /// Cancels the exact scheduled route owned by a ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancel_ticket<R: Relation>(
        &self,
        ticket: DispatchTicket<R>,
    ) -> Result<(), DispatchError> {
        let (plan, _contract, _expected, _cancellation) = ticket.into_parts()?;
        self.cancel(plan);
        Ok(())
    }
}
