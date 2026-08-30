//! One concrete in-process application service for every Wave C7 consumer.
//!
//! This portable crate owns semantic validation, ordering, terminals, replay, cancellation, and
//! typed diagnostics. Process, JSON-RPC, and GPUI crates only decode or project these values.

mod model;
mod service;
mod text;

pub use model::{
    APPLICATION_OPERATION, ApplicationEvent, ApplicationInput, ApplicationReply, Capability,
    CapabilityHealth, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail,
    MAX_PROGRESS_ROWS, MAX_REPLY_ROWS, MAX_SEMANTIC_TEXT_BYTES, OperationKey, ProgressCursor,
    ProgressEvent, ProgressEvents, ProgressPage, ReplyBody, Terminal,
};
pub use service::ApplicationService;
pub use text::{INPUT_TEXT_BYTES, InputText, InputTextError};
