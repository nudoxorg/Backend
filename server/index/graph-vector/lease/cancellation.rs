//! Defines lease cancellation behavior for `server-index-graph-vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the lease cancellation invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed cancellation fan-out with one atomic wake cell per leased stream.

use core::sync::atomic::{AtomicBool, Ordering};

use super::{contract::StreamCapacityError, wake::WakeCell};
use crate::MAX_PARTITIONS;

#[derive(Debug)]
struct CancellationSlot {
    claimed: AtomicBool,
    wake: WakeCell,
}

/// The single-use right to release one occupied cancellation cell.
///
/// Only [`Cancellation::reserve`] constructs it; consuming it in `release` makes duplicate or
/// stale release impossible after this fixed slot has been reused by another leased stream.
#[derive(Debug)]
pub(super) struct CancellationReservation {
    index: usize,
}

impl CancellationSlot {
    fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            wake: WakeCell::new(),
        }
    }
}

/// Stack-owned cancellation authority for at most [`MAX_PARTITIONS`] leased streams.
///
/// Winning `cancel()` is the linearization point. It releases the cancellation fact before taking
/// every registered waker. A stream registers before its second cancellation observation, so it
/// either observes cancellation itself or receives a wake. Released slots are reusable only while
/// cancellation remains clear; no generation can be mistaken for a later stream because a
/// cancelled authority admits no new stream.
#[derive(Debug)]
pub struct Cancellation {
    cancelled: AtomicBool,
    slots: [CancellationSlot; MAX_PARTITIONS],
}

impl Cancellation {
    /// Creates a cancellation authority with fixed, allocation-free fan-out storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            slots: core::array::from_fn(|_| CancellationSlot::new()),
        }
    }

    /// Cancels every admitted stream and wakes each pending consumer once.
    pub fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        for slot in &self.slots {
            if slot.claimed.load(Ordering::Acquire) {
                slot.wake.wake();
            }
        }
    }

    pub(super) fn reserve(&self) -> Result<CancellationReservation, StreamCapacityError> {
        if self.is_cancelled() {
            return Err(StreamCapacityError::StreamClosed);
        }
        let Some(reservation) = self.claim_vacant() else {
            return Err(StreamCapacityError::CancellationCapacity {
                maximum: MAX_PARTITIONS,
            });
        };
        self.retain_claim(reservation)
    }

    fn claim_vacant(&self) -> Option<CancellationReservation> {
        self.slots.iter().enumerate().find_map(|(index, slot)| {
            slot.claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()
                .map(|_| CancellationReservation { index })
        })
    }

    // The final acquire observes a cancel that won while this call claimed a vacant slot. That
    // cancel wins admission: the exact claim is relinquished before an endpoint can escape.
    fn retain_claim(
        &self,
        reservation: CancellationReservation,
    ) -> Result<CancellationReservation, StreamCapacityError> {
        if self.is_cancelled() {
            self.release(reservation);
            return Err(StreamCapacityError::StreamClosed);
        }
        Ok(reservation)
    }

    pub(super) fn register(
        &self,
        reservation: &CancellationReservation,
        waker: &core::task::Waker,
    ) {
        let Some(slot) = self.slots.get(reservation.index) else {
            return;
        };
        if !slot.claimed.load(Ordering::Acquire) {
            return;
        }
        slot.wake.register(waker);
        if self.is_cancelled() {
            slot.wake.wake();
        }
    }

    pub(super) fn release(&self, reservation: CancellationReservation) {
        let Some(slot) = self.slots.get(reservation.index) else {
            return;
        };
        slot.wake.take();
        slot.claimed.store(false, Ordering::Release);
    }

    /// Observes whether cancellation won before a synchronous boundary begins work.
    ///
    /// A blocking adapter samples this fact once before it mutates caller-owned buffers or starts
    /// transport. It does not claim that cancellation can interrupt an already-running blocking
    /// operation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use super::Cancellation;
    use crate::StreamCapacityError;

    #[derive(Debug)]
    enum CancellationTestError {
        ReservationMissing,
        CancelledReservationEscaped,
        CancelledClaimRemainedOccupied,
    }

    #[test]
    fn post_claim_cancellation_relinquishes_the_exact_slot() -> Result<(), CancellationTestError> {
        #[cfg(feature = "loom-model")]
        {
            loom::model(|| assert!(check_post_claim_cancellation().is_ok()));
            Ok(())
        }
        #[cfg(not(feature = "loom-model"))]
        {
            check_post_claim_cancellation()
        }
    }

    fn check_post_claim_cancellation() -> Result<(), CancellationTestError> {
        let cancellation = Cancellation::new();
        let Some(reservation) = cancellation.claim_vacant() else {
            return Err(CancellationTestError::ReservationMissing);
        };
        let reservation_index = reservation.index;
        cancellation.cancel();
        if !matches!(
            cancellation.retain_claim(reservation),
            Err(StreamCapacityError::StreamClosed)
        ) {
            return Err(CancellationTestError::CancelledReservationEscaped);
        }
        if cancellation.slots[reservation_index]
            .claimed
            .load(Ordering::Acquire)
        {
            return Err(CancellationTestError::CancelledClaimRemainedOccupied);
        }
        Ok(())
    }
}
