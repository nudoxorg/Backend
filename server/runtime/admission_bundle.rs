//! Defines admission-bundle behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the admission-bundle invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Linear admission bundles. Every stage owns exactly the resources it has acquired.
#![allow(
    clippy::indexing_slicing,
    reason = "a work permit can only be issued for a validated in-bounds physical slot"
)]

use crate::metrics::Accounting;
use crate::{
    BoundedWork, RejectedWork, RejectionReason, WorkHandle,
    budget::{ReservedCredits, credits_for},
    runtime::Fabric,
    slot::{QueuedSlot, SlotClaimError},
    storage::{RuntimePayloadTable, RuntimeStorage},
    work_permit::WorkPermit,
};

/// Caller work plus its exclusive physical work-slot permit.
pub(crate) struct WorkHeld<Generation, Work> {
    permit: WorkPermit,
    pub(crate) generation: Generation,
    pub(crate) work: Work,
}

/// Work with quantized byte credits.
pub(crate) struct BytesHeld<Generation, Work> {
    permit: WorkPermit,
    pub(crate) generation: Generation,
    pub(crate) work: Work,
    bytes: ReservedCredits,
}

/// Fully admitted work with every permit and queue proof in one direct payload record.
pub(crate) struct QueuedWork<Generation, Work> {
    permit: WorkPermit,
    byte_reservation: ReservedCredits,
    pub(crate) slot: QueuedSlot,
    pub(crate) handle: WorkHandle,
    pub(crate) generation: Generation,
    pub(crate) work: Work,
}

/// Work removed from its payload cell and ready for one-owner execution.
pub(crate) struct DequeuedWork<Generation, Work> {
    permit: WorkPermit,
    byte_reservation: ReservedCredits,
    handle: WorkHandle,
    pub(crate) generation: Generation,
    pub(crate) work: Work,
}

/// Named linear completion capability after execution or deterministic quarantine.
pub(crate) struct CompletedWork<Generation, Work, Failure> {
    pub(crate) permit: WorkPermit,
    pub(crate) byte_reservation: ReservedCredits,
    pub(crate) event: crate::TerminalEvent<Generation, Failure>,
    pub(crate) work: Work,
}

impl<Generation, Work> WorkHeld<Generation, Work> {
    pub(crate) fn acquire<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
        generation: Generation,
        work: Work,
    ) -> Result<Self, RejectedWork<Work>>
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        let Some(permit) = fabric.work_permits.acquire() else {
            return Err(RejectedWork {
                work,
                reason: RejectionReason::WorkSlots,
            });
        };
        fabric.accounting.enter(fabric.checked_out());
        Ok(Self {
            permit,
            generation,
            work,
        })
    }

    fn reject<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        self,
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
        reason: RejectionReason,
    ) -> RejectedWork<Work>
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        let work = self.release(fabric);
        RejectedWork { work, reason }
    }

    fn release<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        self,
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    ) -> Work
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        fabric.work_permits.restore(self.permit);
        fabric.wake_ready_waiters();
        self.work
    }
}

impl<Generation, Work: BoundedWork> BytesHeld<Generation, Work> {
    pub(crate) fn reserve<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
        work: WorkHeld<Generation, Work>,
    ) -> Result<Self, RejectedWork<Work>>
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        let credits = credits_for(work.work.retained_bytes(), fabric.budget.quantum);
        let Some(bytes) = fabric.byte_credits.reserve(credits) else {
            return Err(work.reject(fabric, RejectionReason::ByteBudget));
        };
        let WorkHeld {
            permit,
            generation,
            work,
        } = work;
        Ok(Self {
            permit,
            generation,
            work,
            bytes,
        })
    }

    fn retire<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        self,
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    ) -> Work
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        fabric.byte_credits.release(self.bytes);
        fabric.work_permits.retire(self.permit);
        fabric.wake_ready_waiters();
        self.work
    }

    pub(crate) fn claim<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        self,
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    ) -> Result<QueuedWork<Generation, Work>, (Self, SlotClaimError)>
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        let index = self.permit.index;
        match fabric.payload_slot(index).claim(fabric.identity, index) {
            Ok((handle, slot)) => Ok(QueuedWork {
                permit: self.permit,
                byte_reservation: self.bytes,
                slot,
                handle,
                generation: self.generation,
                work: self.work,
            }),
            Err(source) => Err((self, source)),
        }
    }
}

impl<Generation, Work> QueuedWork<Generation, Work> {
    pub(crate) fn into_dequeued(self) -> DequeuedWork<Generation, Work> {
        let Self {
            permit,
            byte_reservation,
            slot: _,
            generation,
            work,
            handle,
        } = self;
        DequeuedWork {
            permit,
            byte_reservation,
            handle,
            generation,
            work,
        }
    }
}

impl<Generation, Work: BoundedWork> BytesHeld<Generation, Work> {
    pub(crate) fn retire_after_slot_exhaustion<
        Failure,
        AccountingPolicy: Accounting,
        StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    >(
        self,
        fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    ) -> Work
    where
        StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
    {
        self.retire(fabric)
    }
}

impl<Generation, Work> DequeuedWork<Generation, Work> {
    pub(crate) fn complete<Failure>(
        self,
        outcome: crate::TerminalOutcome<Failure>,
    ) -> CompletedWork<Generation, Work, Failure> {
        CompletedWork {
            permit: self.permit,
            byte_reservation: self.byte_reservation,
            event: crate::TerminalEvent {
                handle: self.handle,
                generation: self.generation,
                outcome,
            },
            work: self.work,
        }
    }
}
