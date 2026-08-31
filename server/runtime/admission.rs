//! Defines admission behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the admission invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Public scoped producer surface over the fabric's linear admission bundles.
#![allow(
    missing_docs,
    reason = "the compact public admission vocabulary is documented at its closed type boundary"
)]
#![allow(
    clippy::indexing_slicing,
    clippy::missing_errors_doc,
    reason = "a work permit can only be issued for a validated in-bounds physical slot"
)]

use heart_observe::Probe;
use thiserror::Error;

use crate::metrics::Accounting;
use crate::{
    BoundedWork, CancelResult, RuntimeAdmission, RuntimeProbeEvent, WorkHandle,
    admission_bundle::{BytesHeld, WorkHeld},
    runtime::Fabric,
    slot::SlotClaimError,
    storage::{RuntimePayloadTable, RuntimeStorage},
    waiter::WaiterRegistrationError,
};

/// Bounded MPMC producer handle scoped to its owning [`Runtime`](crate::Runtime).
pub struct Admission<
    'runtime,
    Generation,
    Work,
    AccountingPolicy = (),
    StoragePolicy: RuntimeStorage<Generation, Work, Failure> = crate::RemoteStorage,
    Failure = core::convert::Infallible,
> {
    pub(crate) fabric: &'runtime Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
}

impl<
    Generation,
    Work,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> Copy for Admission<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
{
}
impl<
    Generation,
    Work,
    AccountingPolicy,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> Clone for Admission<'_, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
{
    fn clone(&self) -> Self {
        *self
    }
}

/// Rejection preserving concrete caller-owned work.
#[derive(Debug, Eq, PartialEq)]
pub struct RejectedWork<Work> {
    pub work: Work,
    pub reason: RejectionReason,
}

/// Exact unavailable physical resource.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RejectionReason {
    #[error("all physical work slots are occupied")]
    WorkSlots,
    #[error("the physical byte budget is exhausted")]
    ByteBudget,
}

/// Admission either returns caller work unchanged or preserves the exact retired-slot source.
#[derive(Debug, Error)]
pub enum AdmissionError<Work> {
    #[error("work was rejected: {rejected:?}")]
    Rejected { rejected: RejectedWork<Work> },
    #[error("the unique work permit addressed a retired slot")]
    SlotRetired {
        work: Work,
        #[source]
        source: SlotClaimError,
    },
    #[error("the bounded async waiter table is occupied")]
    WaiterCapacity { work: Work },
    #[error("async registration failed before it could wait for ordinary admission")]
    WaiterRegistration {
        work: Work,
        #[source]
        source: WaiterRegistrationError,
    },
}

impl<
    'runtime,
    Generation,
    Work: BoundedWork,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
    Failure,
> Admission<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    /// Consumes resources in the order work slot → bytes → slot claim → publication.
    /// Each named bundle owns only the resources it must restore on its failing edge.
    pub fn admit(
        &self,
        generation: Generation,
        work: Work,
    ) -> Result<WorkHandle, AdmissionError<Work>> {
        let work = WorkHeld::acquire(self.fabric, generation, work)
            .map_err(|rejected| self.record_rejection(rejected))?;
        let bytes = BytesHeld::reserve(self.fabric, work)
            .map_err(|rejected| self.record_rejection(rejected))?;
        let queued = match bytes.claim(self.fabric) {
            Ok(bundle) => bundle,
            Err((bytes, source)) => {
                return Err(AdmissionError::SlotRetired {
                    work: bytes.retire_after_slot_exhaustion(self.fabric),
                    source,
                });
            }
        };
        let handle = queued.handle;
        let index = handle.index();
        self.fabric.payload_slot(index).publish(queued);
        self.fabric.ready.publish(index);
        Ok(handle)
    }

    /// Admits one item and lazily records only its aggregate physical outcome.
    pub fn admit_with_probe<Observation>(
        &self,
        generation: Generation,
        work: Work,
        probe: &mut Observation,
    ) -> Result<WorkHandle, AdmissionError<Work>>
    where
        Observation: Probe<RuntimeProbeEvent>,
    {
        let result = self.admit(generation, work);
        let outcome = match &result {
            Ok(_) => RuntimeAdmission::Admitted,
            Err(AdmissionError::Rejected { rejected }) => {
                RuntimeAdmission::Rejected(rejected.reason)
            }
            Err(AdmissionError::SlotRetired { .. }) => RuntimeAdmission::SlotRetired,
            Err(AdmissionError::WaiterCapacity { .. }) => RuntimeAdmission::WaiterCapacity,
            Err(AdmissionError::WaiterRegistration { .. }) => RuntimeAdmission::WaiterRegistration,
        };
        probe.record_with(|| RuntimeProbeEvent::Admission(outcome));
        result
    }

    fn record_rejection(self, rejected: RejectedWork<Work>) -> AdmissionError<Work> {
        match rejected.reason {
            RejectionReason::WorkSlots => {
                self.fabric.accounting.reject_work_slot();
            }
            RejectionReason::ByteBudget => {
                self.fabric.accounting.reject_byte_budget();
            }
        }
        AdmissionError::Rejected { rejected }
    }

    /// Returns a concrete future which retains caller work until regular admission can succeed.
    pub fn admit_when_ready(
        &self,
        generation: Generation,
        work: Work,
    ) -> crate::AdmissionWaitResult<
        'runtime,
        Generation,
        Work,
        AccountingPolicy,
        StoragePolicy,
        Failure,
    >
    where
        Generation: Copy,
    {
        match self.admit(generation, work) {
            Ok(handle) => Ok(crate::AdmissionFuture::ready(*self, handle)),
            Err(AdmissionError::Rejected { rejected }) => {
                crate::async_admission::register_future(*self, generation, rejected.work)
            }
            Err(error) => Err(error),
        }
    }

    /// Atomically marks exactly one queued handle. A foreign fabric cannot change this fabric.
    #[must_use]
    pub fn cancel_queued(&self, handle: WorkHandle) -> CancelResult {
        if handle.runtime != self.fabric.identity {
            return CancelResult::ForeignRuntime;
        }
        self.fabric.payload_slot(handle.index()).cancel(handle)
    }
}
