//! The `interface-core` crate exists to own the transport-independent application service and reply vocabulary.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! One concrete in-process application service for every Wave C7 consumer.
//!
//! This portable crate owns semantic validation, ordering, terminals, replay, cancellation, and
//! typed diagnostics. Process, JSON-RPC, and GPUI crates only decode or project these values.

mod compiler;
mod execution;
mod model;
mod package;
mod retrieval;
mod service;
mod source;
mod text;

pub use compiler::{
    CompilerAttempt, CompilerCapability, CompilerCause, CompilerDiagnostic,
    CompilerDiagnosticFacts, CompilerReadiness, CompilerRequest, CompilerTerminal, FragmentCause,
    GeneratedArtifact, GenerationAuthority, InvalidUtf8Fact, LoweringCause,
    MAX_NATIVE_WORKER_PANIC_BYTES, NativeArtifactAction, NativeArtifactCause, NativeArtifactRole,
    NativeDirectoryCause, NativeIoFact, NativeIoPhase, NativePrimaryCause, NativeWorkCause,
    NativeWorkCleanupCause, NativeWorkPhase, NativeWorker, NativeWorkerPanic,
    NativeWorkerPanicClass, NativeWorkerPanicMessage, PublicationAuthority, PublicationCause,
    PublicationPhase, SemanticImageAuthority, SourceAuthority, UnavailableCompiler,
};
pub use compiler_vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, LoweringUnsupported, MAX_NATIVE_DIAGNOSTIC_BYTES,
};
pub use heart_adaptive::{
    BatteryState, ByteCount, CapabilityDomain, CapabilityKind, ContentId, GenerationId,
    IndexSnapshotId, OperationBudget, Pin, Pressure, RecoveryCause, ResourceBudget, ResourceClass,
    RetryBudget,
};
pub use model::{
    AdaptiveDisposition, ApplicationDisposition, ApplicationEvent, ApplicationInput,
    ApplicationObservation, ApplicationOutcome, ApplicationReply, Capability, CapabilityHealth,
    CapabilityTransition, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionReply, ExecutionState, GenerateRequest, GenerateTarget, InconsistentRecovery,
    MAX_REPLY_ROWS, MAX_SEMANTIC_TEXT_BYTES, OperationKey, ReplyBody,
};
pub use package::{
    MAX_PACKAGE_URL_BYTES, PackageCompileFacts, PackageCompilePhase, PackageCompileRequest,
    PackageDeclarationScopeCause, PackageEcosystem, PackagePathComponentError,
    PackageProfileMismatch, PackageSourceCause, PackageSourceIoFact, PackageSourceIoPhase,
    PackageTextRange, PackageUrl, PackageUrlError, PackageUrlFacts, RejectedPackageUrl,
};
pub use retrieval::{
    DocSection, MAX_SIGNATURE_TOKENS, RetrievalCapability, RetrievalCause, RetrievalMode,
    RetrievalPhase, RetrievalQueryCause, RetrievalReadiness, RetrievalRequest, RetrievalRow,
    RetrievalRows, RetrievalRowsFacts, RetrievalSpan, SignatureToken, SignatureTokens,
    SnapshotFacts, TokenKind, UnavailableRetrieval, UnloadReceipt,
};
pub use service::ApplicationService;
pub use source::{
    PORTABLE_LOCAL_SOURCE_LIMIT, RejectedSourceText, SourceByteLimit, SourceText, SourceTextLimit,
};
pub use text::{INPUT_TEXT_BYTES, InputText, InputTextError, InputTextJoinError};
