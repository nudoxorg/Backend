//! Route cancellation and resource reservation guards.

use crate::{CancelHandle, Cancellation, HedgeSide, PlacementDecision};

/// Reservations owned by one scheduled route.
#[must_use = "retain scheduled reservations until completion or cancellation"]
pub struct RouteReservations {
    /// Immediate local interactive reservation, when local work is admitted.
    pub(super) local: Option<crate::Reservation>,
    /// Remote background reservation, when a remote route is admitted.
    pub(super) remote: Option<crate::Reservation>,
    /// Transfer envelope reservation for remote input/output bytes.
    pub(super) transfer: Option<crate::Reservation>,
}

impl std::fmt::Debug for RouteReservations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteReservations")
            .field("local", &self.local)
            .field("remote", &self.remote)
            .field("transfer", &self.transfer)
            .finish()
    }
}

impl RouteReservations {
    /// Returns the held local reservation, if this route starts or preserves
    /// local capacity.
    #[must_use]
    pub const fn local(&self) -> Option<&crate::Reservation> {
        self.local.as_ref()
    }

    /// Returns the held remote reservation, if this route dispatches remotely.
    #[must_use]
    pub const fn remote(&self) -> Option<&crate::Reservation> {
        self.remote.as_ref()
    }

    /// Returns the transfer reservation held for remote object movement.
    #[must_use]
    pub const fn transfer(&self) -> Option<&crate::Reservation> {
        self.transfer.as_ref()
    }

    /// Returns whether a local fallback reservation exists.
    #[must_use]
    pub const fn has_local(&self) -> bool {
        self.local.is_some()
    }

    /// Returns whether a remote reservation exists.
    #[must_use]
    pub const fn has_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Returns whether a transfer reservation exists.
    #[must_use]
    pub const fn has_transfer(&self) -> bool {
        self.transfer.is_some()
    }

    /// Explicitly releases all route reservations.
    pub fn release(self) {
        drop(self);
    }
}

struct CancellationSlot {
    observation: Cancellation,
    handle: CancelHandle,
}

impl std::fmt::Debug for CancellationSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationSlot")
            .field("cancelled", &self.observation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl CancellationSlot {
    fn new() -> Self {
        let (observation, handle) = Cancellation::new();
        Self {
            observation,
            handle,
        }
    }

    fn cancellation(&self) -> Cancellation {
        self.observation.clone()
    }

    fn cancel(&self) {
        self.handle.cancel();
    }
}

pub(super) struct RouteCancellation {
    local: Option<CancellationSlot>,
    remote: Option<CancellationSlot>,
}

impl std::fmt::Debug for RouteCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteCancellation")
            .field("local", &self.local)
            .field("remote", &self.remote)
            .finish()
    }
}

impl RouteCancellation {
    pub(super) fn for_decision(decision: PlacementDecision, fallback_reserved: bool) -> Self {
        let local = matches!(
            decision,
            PlacementDecision::Local | PlacementDecision::WaitLocal | PlacementDecision::Remote
        ) && (decision != PlacementDecision::Remote || fallback_reserved);
        let remote = matches!(decision, PlacementDecision::Remote);
        Self {
            local: local.then(CancellationSlot::new),
            remote: remote.then(CancellationSlot::new),
        }
    }

    pub(super) fn cancellation(&self, side: HedgeSide) -> Option<Cancellation> {
        match side {
            HedgeSide::Local => self.local.as_ref().map(CancellationSlot::cancellation),
            HedgeSide::Remote => self.remote.as_ref().map(CancellationSlot::cancellation),
        }
    }

    pub(super) fn cancel(&self, side: HedgeSide) {
        match side {
            HedgeSide::Local => {
                if let Some(slot) = &self.local {
                    slot.cancel();
                }
            }
            HedgeSide::Remote => {
                if let Some(slot) = &self.remote {
                    slot.cancel();
                }
            }
        }
    }

    pub(super) fn cancel_all(&self) {
        self.cancel(HedgeSide::Local);
        self.cancel(HedgeSide::Remote);
    }
}
