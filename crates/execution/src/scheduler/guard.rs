//! Affine scheduled attempt guard and lifecycle transitions.

use super::engine::Scheduler;
use super::outcome::ScheduleError;
use super::route::{RouteCancellation, RouteReservations};
use crate::{
    AttemptError, AttemptLease, AttemptManager, Cancellation, HedgeError, HedgeRace, HedgeSide,
    Interned, LocalState, PlacementDecision, RemoteState, ResultReceipt, VersionedWorkIdentity,
    WorkKey,
};
use backend_version::Relation;
use std::sync::Arc;

/// A scheduled attempt with affine resource and publication guards.
#[must_use = "complete or cancel the scheduled attempt so its guards are released"]
pub struct Scheduled<R: Relation> {
    /// Chosen route.
    pub(super) decision: PlacementDecision,
    /// One owner lease/fence for result publication.
    pub(super) lease: AttemptLease,
    /// Reservations held by this route.
    pub(super) reservations: RouteReservations,
    /// Optional first-valid-winner race for pure hedges.
    pub(super) race: Option<Arc<HedgeRace>>,
    /// Process/remote cancellation observations and controls for non-hedged
    /// routes. Hedge routes use the shared race's controls instead.
    pub(super) route_cancellation: RouteCancellation,
    /// Latest owner-local logical time at which a held local fallback may be
    /// activated.
    pub(super) fallback_deadline: Option<u64>,
    /// Identity bound to the attempt.
    pub(super) identity: VersionedWorkIdentity<R>,
    /// Key derived once during scheduler admission.
    pub(super) key: WorkKey,
    /// Coarse route observations retained for owner cost learning.
    pub(super) local_state: LocalState,
    pub(super) remote_state: RemoteState,
    /// Input/output charge used to select the model size bucket.
    pub(super) bytes: u64,
    pub(super) manager: Arc<AttemptManager>,
    pub(super) deadlines: Arc<std::sync::Mutex<super::deadline::DeadlineQueue<WorkKey>>>,
    pub(super) interned: Option<Interned>,
}

impl<R: Relation> std::fmt::Debug for Scheduled<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduled")
            .field("decision", &self.decision)
            .field("lease", &self.lease)
            .field("reservations", &self.reservations)
            .field("race", &self.race)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl<R: Relation> Scheduled<R> {
    /// Returns the selected placement route.
    #[must_use]
    pub const fn decision(&self) -> PlacementDecision {
        self.decision
    }

    /// Returns the checked local capability state observed when this route
    /// was admitted.  It is retained only for owner-side cost accounting.
    #[must_use]
    pub const fn observed_local_state(&self) -> LocalState {
        self.local_state
    }

    /// Returns the checked remote capability state observed when this route
    /// was admitted.  It is retained only for owner-side cost accounting.
    #[must_use]
    pub const fn observed_remote_state(&self) -> RemoteState {
        self.remote_state
    }

    /// Returns the bounded route byte charge used by owner cost accounting.
    #[must_use]
    pub const fn charged_bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the current publication lease snapshot.
    #[must_use]
    pub const fn lease(&self) -> &AttemptLease {
        &self.lease
    }

    /// Returns the affine route reservations.
    pub const fn reservations(&self) -> &RouteReservations {
        &self.reservations
    }

    /// Returns the shared hedge race, if this is a hedge.
    #[must_use]
    pub const fn race(&self) -> Option<&Arc<HedgeRace>> {
        self.race.as_ref()
    }

    /// Returns the cancellation token for the selected local or remote route.
    /// A worker/process adapter should observe this token and propagate it to
    /// its child process or remote cancellation request.
    #[must_use]
    pub fn cancellation(&self, side: HedgeSide) -> Option<Cancellation> {
        self.race.as_ref().map_or_else(
            || self.route_cancellation.cancellation(side),
            |race| Some(race.cancellation(side)),
        )
    }

    /// Claims a validated candidate for one side of a pure hedge.
    ///
    /// Workers use this method at their completion boundary when they race in
    /// separate threads or processes. The receipt is checked against this
    /// scheduled identity and publication fence before the first-valid winner
    /// cancels its loser. The schedule remains borrowed so the winning caller
    /// can hand it to [`Scheduler::complete_side`] after its worker has
    /// observed the cancellation signal.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::Attempt`] for a stale or expired receipt,
    /// [`ScheduleError::KeyMismatch`] for a receipt from another work item,
    /// or [`ScheduleError::Hedge`] when another valid side has already won.
    pub fn claim(
        &self,
        side: HedgeSide,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<(), ScheduleError> {
        Scheduler::check_scheduled_receipt(self, receipt)?;
        self.manager
            .validate_for_scheduler(receipt, now)
            .map_err(ScheduleError::Attempt)?;
        if let Some(race) = &self.race {
            Scheduler::claim_race(race, side, receipt)
        } else if side == HedgeSide::Local {
            Ok(())
        } else {
            Err(ScheduleError::Hedge(HedgeError::AlreadyWon))
        }
    }

    /// Returns the complete identity bound to this attempt.
    #[must_use]
    pub const fn identity(&self) -> VersionedWorkIdentity<R> {
        self.identity
    }

    /// Returns the optional latest-start fallback deadline.
    #[must_use]
    pub const fn fallback_deadline(&self) -> Option<u64> {
        self.fallback_deadline
    }

    /// Returns this attempt's semantic key.
    #[must_use]
    pub fn key(&self) -> WorkKey {
        self.key
    }

    /// Renews this scheduled owner's lease and updates the guard snapshot.
    /// The returned guard remains the same affine schedule, so reservations,
    /// cancellation tokens, and interner ownership stay attached to it.
    /// # Errors
    ///
    /// Returns [`ScheduleError::Attempt`] when the owner lease is stale,
    /// expired, or time-regressed.
    pub fn heartbeat(&mut self, now: u64) -> Result<(), ScheduleError> {
        match self.manager.heartbeat(&self.lease, now) {
            Ok(lease) => {
                self.lease = lease;
                Ok(())
            }
            Err(error) => {
                if error == AttemptError::Expired {
                    self.lease.set_state(crate::AttemptState::Expired);
                }
                Err(ScheduleError::Attempt(error))
            }
        }
    }

    /// Returns whether this schedule selected a pure hedge.
    #[must_use]
    pub const fn is_hedged(&self) -> bool {
        matches!(self.decision, PlacementDecision::Hedge)
    }

    /// Cancels publication rights immediately and releases resources on drop.
    pub fn cancel(mut self) {
        self.deadlines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel(self.key);
        if let Some(race) = &self.race {
            race.cancel(HedgeSide::Local);
            race.cancel(HedgeSide::Remote);
        } else {
            self.route_cancellation.cancel_all();
        }
        if self
            .manager
            .transition(
                self.lease.key(),
                self.lease.fence(),
                crate::AttemptState::Cancelled,
            )
            .is_ok()
        {
            self.lease.set_state(crate::AttemptState::Cancelled);
        }
        self.interned.take();
    }

    /// Returns the local reservation to make a fallback deadline explicit.
    /// The caller must then schedule/execute local work under that reservation.
    #[must_use]
    pub fn has_fallback(&self) -> bool {
        self.reservations.has_local()
    }

    /// Returns whether the fallback deadline has elapsed.
    #[must_use]
    pub fn fallback_due(&self, now: u64) -> bool {
        self.fallback_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    /// Activates a previously reserved local fallback and releases the remote
    /// reservation. This gives deadline policy an executable transition rather
    /// than merely recording an estimate.
    /// # Errors
    ///
    /// Returns [`ScheduleError::FallbackNotDue`] before the promised deadline,
    /// [`ScheduleError::FallbackUnavailable`] without a held local reserve,
    /// or [`ScheduleError::Attempt`] when the old publication fence is stale.
    pub fn activate_fallback(&mut self, now: u64) -> Result<(), ScheduleError> {
        self.activate_fallback_inner(now, true)
    }

    /// Activates a reserved local fallback immediately after the owner has
    /// observed a terminal remote-path failure. The explicit operation keeps
    /// transport failure separate from the deadline policy: callers cannot
    /// accidentally treat an ordinary early hedge as failed merely by passing
    /// a later timestamp.
    /// # Errors
    ///
    /// Returns [`ScheduleError::FallbackUnavailable`] without a held local
    /// reserve, or [`ScheduleError::Attempt`] when the old publication fence
    /// is stale.
    pub fn activate_fallback_after_remote_failure(
        &mut self,
        now: u64,
    ) -> Result<(), ScheduleError> {
        self.activate_fallback_inner(now, false)
    }

    fn activate_fallback_inner(
        &mut self,
        now: u64,
        deadline_required: bool,
    ) -> Result<(), ScheduleError> {
        if !matches!(self.decision, PlacementDecision::Remote) {
            return Err(ScheduleError::FallbackUnavailable);
        }
        if deadline_required && !self.fallback_due(now) {
            return Err(ScheduleError::FallbackNotDue);
        }
        if !self.has_fallback() {
            return Err(ScheduleError::FallbackUnavailable);
        }
        let lease = self
            .manager
            .replace_for_fallback(self.identity, &self.lease, now)
            .map_err(ScheduleError::Attempt)?;
        self.deadlines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel(self.key);
        self.reservations.remote.take();
        self.reservations.transfer.take();
        self.route_cancellation.cancel(HedgeSide::Remote);
        self.lease = lease;
        self.decision = PlacementDecision::Local;
        Ok(())
    }
}

impl<R: Relation> Drop for Scheduled<R> {
    fn drop(&mut self) {
        self.deadlines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancel(self.key);
        let state = self.lease.state();
        if matches!(
            state,
            crate::AttemptState::Active | crate::AttemptState::Expired
        ) {
            if let Some(race) = &self.race {
                race.cancel(HedgeSide::Local);
                race.cancel(HedgeSide::Remote);
            } else {
                self.route_cancellation.cancel_all();
            }
            if state == crate::AttemptState::Active {
                let _ = self.manager.transition(
                    self.lease.key(),
                    self.lease.fence(),
                    crate::AttemptState::Frozen,
                );
            }
        }
        self.interned.take();
    }
}
