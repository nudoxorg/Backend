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
    CompilerAttempt, CompilerCapability, CompilerCause, CompilerDiagnostic,
    CompilerDiagnosticFacts, CompilerReadiness, CompilerRequest, CompilerTerminal, FragmentCause,
    GeneratedArtifact, GenerationAuthority, InvalidUtf8Fact, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeArtifactAction, NativeArtifactCause, NativeArtifactRole, NativeDirectoryCause,
    NativeIoFact, NativeIoPhase, NativePrimaryCause, NativeWorkCause, NativeWorkCleanupCause,
    NativeWorkPhase, NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass,
    NativeWorkerPanicMessage, PublicationAuthority, PublicationCause, PublicationPhase,
    SourceAuthority, UnavailableCompiler,
};
pub use model::{
    AdaptiveDisposition, ApplicationDisposition, ApplicationEvent, ApplicationInput,
    ApplicationObservation, ApplicationOutcome, ApplicationReply, Capability, CapabilityHealth,
    CapabilityTransition, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionReply, ExecutionState, InconsistentRecovery, MAX_REPLY_ROWS, MAX_SEMANTIC_TEXT_BYTES,
    OperationKey, ReplyBody,
};
pub use nudox_adaptive::{
    BatteryState, ByteCount, CapabilityDomain, CapabilityKind, ContentId, GenerationId,
    IndexSnapshotId, OperationBudget, Pin, Pressure, RecoveryCause, ResourceBudget, ResourceClass,
    RetryBudget,
};
pub use nudox_compile_vocab::{LoweringUnsupported, MAX_NATIVE_DIAGNOSTIC_BYTES};
pub use service::ApplicationService;
pub use text::{INPUT_TEXT_BYTES, InputText, InputTextError, InputTextJoinError};
