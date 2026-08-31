//! The `server-workflow` crate exists to reduce durable workflow events into deterministic recovery state.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![forbid(unsafe_code)]
//! Bounded durable stage events, pure reduction, and at-least-once recovery.

extern crate alloc;

mod durable;
mod event;
mod key;
mod log;
mod recovery;
mod reduce;

pub use durable::{
    CommittedReduction, DurableAppend, DurableCommitError, ReplayError, WORKFLOW_RECORD_BYTES,
    WorkflowRecord, WorkflowRecordError, append_committed, replay_stream,
};
pub use event::{EventKind, EventName, FailureCode, StageId, WorkflowEvent, WorkflowVersion};
pub use key::{
    CapabilityDomain, CapabilityId, ConfigurationDomain, ConfigurationId, StageInput, StageKey,
    StageOutput,
};
pub use log::{CommitError, LogConfigError, MemoryWorkflowLog};
pub use recovery::Recovery;
pub use reduce::{
    Effect, EffectAction, Phase, PhaseName, PriorFacts, Reduction, ReductionError,
    WorkflowDisposition, WorkflowProbeEvent, WorkflowRejection, WorkflowState, reduce,
    reduce_with_probe,
};

#[cfg(test)]
mod tests;
