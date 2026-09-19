//! Public protocol facade for the focused dispatch contract modules.

mod contract;
mod inflight;
mod wire;

pub use contract::{
    ExpectedInput, FenceBinding, RemoteDispatchContract, WorkerReceiptId, WorkerReceiptSchema,
};
pub(super) use inflight::RemoteAdmission;
pub use inflight::{
    DispatchCompletion, DispatchPlan, DispatchTicket, InFlightRemote, PendingRemoteEnvelope,
    PendingRemoteKey, RemoteCorrelationKey,
};
pub use wire::{cancellation_id_for, request_expectation, wire_request, worker_receipt_id};
