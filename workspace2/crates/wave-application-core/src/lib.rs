//! One concrete in-process application service for every Wave C7 consumer.
//!
//! This portable crate owns semantic validation, ordering, terminals, replay, cancellation, and
//! typed diagnostics. Process, JSON-RPC, and GPUI crates only decode or project these values.

mod compiler;
mod execution;
mod model;
mod service;
mod text;

pub use compiler::{
    CompilerAttempt, CompilerCapability, CompilerCause, CompilerDiagnostic, CompilerReadiness,
    CompilerRequest, CompilerTerminal, FragmentCause, GeneratedArtifact, GenerationAuthority,
    LoweringCause, MAX_COMPILER_DIAGNOSTIC_BYTES, NativeArtifactAction, NativeArtifactRole,
    NativeDirectoryCause, NativeIoFact, NativeIoPhase, NativePrimaryCause, NativeWorkCause,
    NativeWorkCleanupCause, NativeWorkPhase, PublicationAuthority, PublicationCause,
    PublicationPhase, SourceAuthority, UnavailableCompiler,
};
pub use model::{
    AdaptiveDisposition, ApplicationEvent, ApplicationInput, ApplicationReply, Capability,
    CapabilityHealth, CapabilityTransition, CorrelationId, Diagnostic, DiagnosticCode,
    DiagnosticDetail, ExecutionState, InconsistentRecovery, MAX_REPLY_ROWS,
    MAX_SEMANTIC_TEXT_BYTES, OperationKey, ReplyBody, Terminal,
};
pub use nudox_adaptive::{
    BatteryState, ByteCount, CapabilityDomain, CapabilityKind, ContentId, GenerationId,
    IndexSnapshotId, OperationBudget, Pin, Pressure, RecoveryCause, ResourceBudget, ResourceClass,
    RetryBudget,
};
pub use service::ApplicationService;
pub use text::{INPUT_TEXT_BYTES, InputText, InputTextError, InputTextJoinError};
