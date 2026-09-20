//! Admission queue, live-work coalescing, and route reservation.

use super::engine::Scheduler;
use super::guard::Scheduled;
use super::outcome::{ScheduleError, ScheduleOutcome};
use super::request::ScheduleRequest;
use super::route::{RouteCancellation, RouteReservations};
use crate::{
    AdmissionError, AdmissionRequest, Envelope, HedgeRace, PlacementClass, PlacementDecision,
    PlacementRequest, choose_placement_with,
};
use backend_version::Relation;
use std::sync::Arc;

fn local_fallback_valid<R: Relation>(request: &ScheduleRequest<R>, key: crate::WorkKey) -> bool {
    request
        .local
        .valid_for(key, request.identity.authority, request.now)
        && matches!(
            request.local.state(),
            crate::LocalState::Ready | crate::LocalState::Overloaded
        )
}

impl Scheduler {
    fn validate_request<R: Relation>(
        request: &ScheduleRequest<R>,
        key: crate::WorkKey,
    ) -> Result<(), ScheduleError> {
        if request.local.key() != key
            || request.remote.key() != key
            || request.costs.key() != key
            || request.local.authority() != request.identity.authority
            || request.remote.authority() != request.identity.authority
        {
            return Err(ScheduleError::ObservationMismatch);
        }
        Ok(())
    }

    fn choose_decision<R: Relation>(
        request: &ScheduleRequest<R>,
    ) -> Result<PlacementDecision, ScheduleError> {
        if request.hedge && !request.pure {
            return Err(ScheduleError::HedgeNotAllowed);
        }
        let decision = choose_placement_with(PlacementRequest {
            local: request.local,
            remote: request.remote,
            costs: request.costs,
            now: request.now,
            class: request.class,
            pure: request.pure,
            hedge_budget: request.hedge,
            fallback_reserved: request.fallback_reserved,
        });
        match decision {
            PlacementDecision::RemoteOnlyUnavailable => Err(ScheduleError::RemoteOnlyUnavailable),
            PlacementDecision::MissingLocalInput => Err(ScheduleError::MissingLocalInput),
            PlacementDecision::CapabilityUnavailable => Err(ScheduleError::CapabilityUnavailable),
            PlacementDecision::Local
            | PlacementDecision::Remote
            | PlacementDecision::Hedge
            | PlacementDecision::WaitLocal => Ok(decision),
        }
    }

    fn intern_work(&self, key: crate::WorkKey) -> Result<crate::Interned, ScheduleError> {
        self.interner.intern(key).map_err(|error| match error {
            crate::InternError::Full | crate::InternError::FollowersFull => {
                ScheduleError::Admission(AdmissionError::Operations)
            }
            crate::InternError::Overflow => ScheduleError::Admission(AdmissionError::Overflow),
            crate::InternError::NotLeader
            | crate::InternError::Unknown
            | crate::InternError::AlreadyTerminal => ScheduleError::Coalesced,
        })
    }

    fn schedule_leader<R: Relation>(
        &self,
        request: &ScheduleRequest<R>,
        key: crate::WorkKey,
        mut decision: PlacementDecision,
        interned: crate::Interned,
    ) -> Result<ScheduleOutcome<R>, ScheduleError> {
        let reservations = match self.reserve_routes(decision, request, key) {
            Ok(reservations) => reservations,
            Err(_error)
                if matches!(
                    decision,
                    PlacementDecision::Remote | PlacementDecision::Hedge
                ) && request.class != PlacementClass::RemoteRequired
                    && local_fallback_valid(request, key) =>
            {
                // A remote route is opportunistic. If its envelope cannot be
                // admitted, release any partial remote/transfer reservation
                // and start the feasible local route immediately.
                decision = PlacementDecision::Local;
                self.reserve_routes(decision, request, key)?
            }
            Err(error) => return Err(error),
        };
        let fallback_deadline = if decision == PlacementDecision::Remote
            && request.fallback_reserved
            && local_fallback_valid(request, key)
        {
            Self::fallback_deadline(request)?
        } else {
            None
        };
        let lease = self
            .attempts
            .start(request.identity, request.epoch, request.now)
            .map_err(ScheduleError::Attempt)?;
        if let Some(deadline) = fallback_deadline {
            let deadline_failed = self
                .deadlines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .schedule(key, deadline)
                .is_err();
            if deadline_failed {
                let _ = self.attempts.transition(
                    lease.key(),
                    lease.fence(),
                    crate::AttemptState::Cancelled,
                );
                return Err(ScheduleError::Admission(AdmissionError::Overflow));
            }
        }
        let race = (decision == PlacementDecision::Hedge).then(|| Arc::new(HedgeRace::new().0));
        let route_cancellation = RouteCancellation::for_decision(
            decision,
            request.fallback_reserved && local_fallback_valid(request, key),
        );
        Ok(ScheduleOutcome::Scheduled(Box::new(Scheduled {
            decision,
            lease,
            reservations,
            race,
            route_cancellation,
            fallback_deadline,
            identity: request.identity,
            key,
            local_state: request.local.state(),
            remote_state: request.remote.state(),
            bytes: request.bytes,
            manager: Arc::clone(&self.attempts),
            deadlines: Arc::clone(&self.deadlines),
            interned: Some(interned),
        })))
    }

    fn fallback_deadline<R: Relation>(
        request: &ScheduleRequest<R>,
    ) -> Result<Option<u64>, ScheduleError> {
        if !request.fallback_reserved {
            return Ok(None);
        }
        let deadline = request.fallback_deadline.unwrap_or(
            request
                .now
                .checked_add(request.costs.local())
                .ok_or(ScheduleError::Admission(AdmissionError::Overflow))?,
        );
        Ok(Some(deadline))
    }

    fn reserve_routes<R: Relation>(
        &self,
        decision: PlacementDecision,
        request: &ScheduleRequest<R>,
        key: crate::WorkKey,
    ) -> Result<RouteReservations, ScheduleError> {
        let local_needed = matches!(
            decision,
            PlacementDecision::Local | PlacementDecision::WaitLocal | PlacementDecision::Hedge
        ) || (decision == PlacementDecision::Remote
            && request.fallback_reserved
            && request.class != PlacementClass::RemoteRequired
            && local_fallback_valid(request, key));
        let remote_needed = matches!(
            decision,
            PlacementDecision::Remote | PlacementDecision::Hedge
        );
        let local = local_needed
            .then(|| {
                self.admission.try_admit_in(
                    Envelope::Interactive,
                    AdmissionRequest::new(1, request.bytes).with_resources(request.resources),
                )
            })
            .transpose()
            .map_err(ScheduleError::Admission)?;
        let remote = remote_needed
            .then(|| {
                self.admission.try_admit_in(
                    Envelope::Background,
                    AdmissionRequest::new(1, request.bytes)
                        .hedged_if(decision == PlacementDecision::Hedge)
                        .with_resources(request.resources),
                )
            })
            .transpose()
            .map_err(ScheduleError::Admission)?;
        let transfer = (remote_needed && request.bytes > 0)
            .then(|| {
                self.admission
                    .try_admit_in(Envelope::Transfer, AdmissionRequest::new(1, request.bytes))
            })
            .transpose()
            .map_err(ScheduleError::Admission)?;
        Ok(RouteReservations {
            local,
            remote,
            transfer,
        })
    }

    /// Starts or coalesces a fresh attempt.  Reuse requires an explicit
    /// engine-issued context through
    /// [`Self::schedule_or_reuse_with_context`].
    /// # Errors
    ///
    /// Returns a placement error for unavailable capability or an
    /// admission/attempt error when bounded resources or ownership cannot be
    /// acquired.
    pub fn schedule_or_reuse<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
    ) -> Result<ScheduleOutcome<R>, ScheduleError> {
        // A lower scheduler has no authority source from which it can mint a
        // current freshness context.  In particular, feeding the retained
        // entry's own context back into `lookup` would make an old epoch or
        // revocation observation self-authorizing.  Engine owners must call
        // `schedule_or_reuse_with_context` with a context obtained from their
        // validated publication authority; this convenience path always
        // starts/coalesces a fresh attempt.
        self.schedule_or_reuse_with_context(request, None)
    }

    /// Schedules a request while requiring an exact retained-output context
    /// for reuse. The dispatcher supplies the context emitted by a prior
    /// accepted publication; a missing or mismatched context always starts a
    /// fresh attempt.
    ///
    /// # Errors
    ///
    /// Returns a placement, capacity, identity, or coalescing error when the
    /// request cannot acquire a bounded route reservation.
    pub fn schedule_or_reuse_with_context<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
        reuse_context: Option<&crate::ReuseContext>,
    ) -> Result<ScheduleOutcome<R>, ScheduleError> {
        let key = request.identity.work_key();
        Self::validate_request(&request, key)?;
        if let Some(context) = reuse_context
            && let Some(output) = self.lookup.lookup(key, context)
        {
            return Ok(ScheduleOutcome::Reused(output));
        }
        let decision = Self::choose_decision(&request)?;
        // Coalesce live work before consuming scarce route resources. The
        // follower handle is released on drop; only a leader can complete.
        let interned = self.intern_work(key)?;
        if !interned.is_leader() {
            return Ok(ScheduleOutcome::Waiting(interned));
        }
        self.schedule_leader(&request, key, decision, interned)
    }
}
