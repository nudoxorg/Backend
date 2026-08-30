//! One concrete in-process application service for every Wave C7 consumer.
//!
//! This portable crate owns semantic validation, ordering, terminals, replay, cancellation, and
//! typed diagnostics. Process, JSON-RPC, and GPUI crates only decode or project these values.

mod execution;
mod model;
mod service;
mod text;

pub use model::{
    APPLICATION_OPERATION, AdaptiveDisposition, ApplicationEvent, ApplicationInput,
    ApplicationReply, Capability, CapabilityHealth, CapabilityTransition, CorrelationId,
    Diagnostic, DiagnosticCode, DiagnosticDetail, ExecutionState, MAX_REPLY_ROWS,
    MAX_SEMANTIC_TEXT_BYTES, OperationKey, ReplyBody, Terminal,
};
pub use nudox_adaptive::{
    BatteryState, ByteCount, CapabilityDomain, CapabilityKind, ContentId, GenerationId,
    IndexSnapshotId, OperationBudget, Pin, Pressure, ResourceBudget, ResourceClass, RetryBudget,
};
pub use service::ApplicationService;
pub use text::{INPUT_TEXT_BYTES, InputText, InputTextError};
