//! Versioned local-first control plane for delegated agent work.
//!
//! The crate exposes a checked work specification, a persistent relation
//! state machine, and wire/durable adapters.  JSON and Nu are transport
//! boundaries only; lifecycle identities and exact-base deltas remain typed
//! `backend-version` values until they leave this crate.
#![forbid(unsafe_code)]

pub mod context;
mod custody;
mod error;
pub mod evidence;
pub mod ids;
pub mod ledger;
pub mod record;
pub mod spec;
pub mod wire;

pub use context::{ContextAudience, ContextDelta, ContextItem, MAX_CONTEXT_ITEMS};
pub use error::ControlError;
pub use ids::{ControlSummary, ControlSummarySchema, Identity, SummaryVersion};
pub use ledger::{
    ActiveLeaseMarker, Admission, Candidate, CandidateStage, ControlCommit, ControlDelta,
    DecisionResult, Evaluated, EvaluationResult, Frozen, Held, Lease, LeaseFence, LeaseMarker,
    PlanResult, Renewed, ReviewResult, Reviewed, SchedulerLimits, StageResult,
    VersionedControlPlane,
};
pub use record::{
    AttemptOutcome, Authority, ControlRelation, ControlRoot, CustodyReceipt, CustodyStage,
    CustodyVerdict, DecisionReceipt, DurableReceipt, EvaluationReceipt, ReviewReceipt, WorkRecord,
    WorkStatus,
};
pub use spec::{Effort, WorkDependency, WorkSpec};
pub use wire::{WireWorkKey, WireWorkSpec};

mod durable;
pub use durable::{ControlPage, DurableControlPlane};
pub use durable::{DEFAULT_MAX_PACK_BYTES, MAX_STATUS_PAGE};

#[cfg(test)]
mod tests;
