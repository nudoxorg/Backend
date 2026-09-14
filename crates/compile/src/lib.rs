//! Versioned compilation contracts and bounded authority execution.
//!
//! The crate is deliberately independent of concrete language frontends.  It
//! owns the immutable discovery contract, the validated session protocol,
//! bounded process supervision, cancellation, and reusable scratch buffers.
//! Native crates implement [`Authority`] and retain their language-specific
//! interpretation and diagnostics.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Bounded registry acquisition and durable sparse-index ingestion.
pub mod acquire;

mod cancel;
mod contract;
mod embedding;
mod errors;
mod frame;
mod native;
mod native_adapter;
mod native_protocol;
mod native_semantic;
mod pool;
mod prepared;
mod process;
mod profile;
mod session;
mod session_cache;
mod supervisor;
mod syntax;
mod syntax_kind;

pub use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageAdmissionError, CoverageWitness, ScopeRoot,
    UntrustedCoverageScope, admit_complete_scope,
};
pub use cancel::{CancelHandle, Cancellation, CancellationError};
pub use contract::{
    Authority, AuthorityEpoch, AuthorityEpochSchema, AuthorityError, AuthorityIdentity, CommandId,
    CommandSchema, ContractId, ContractSchema, DiscoveryDelta, DiscoverySnapshot, Extraction,
    ExtractionError, FactChange, FactDelta, FactDeltaChange, FactDeltaError, FactEvidence, FactKey,
    FactKeySchema, FactKind, FactRecord, FactSchema, FactSnapshot, FactSnapshotError,
    FactValueSchema, FactValueVersion, FactVersion, FlowId, FlowSchema, Input, InputChange,
    InputContentSchema, InputContentVersion, InputKind, InputManifest, InputManifestId,
    InputManifestSchema, ManifestError, PreparedFactDelta, ProducerId, ProducerSchema, ProfileId,
    ProfileSchema, SemanticBasisId, SemanticBasisSchema, SessionId, SessionKey, SessionSchema,
    SyntaxProducerId, SyntaxProducerSchema, ToolchainId, ToolchainSchema, partial_coverage,
    typed_of,
};
pub use embedding::{
    EmbeddingArtifact, EmbeddingArtifactId, EmbeddingCoordinates, EmbeddingExecutable,
    EmbeddingExecutableError, EmbeddingInvocation, EmbeddingNormalization, EmbeddingPurpose,
};
pub use errors::{FrameError, PoolError, ProcessError, UnsupportedLimit};
pub use frame::{
    FrameKind, MAX_FRAME_BYTES, PROTOCOL_VERSION, SessionFrame, SessionFrameVersion,
    UntrustedSessionFrame,
};
pub use native::{
    NativeAuthorityRunner, NativeError, NativeExit, NativeObservation, NativeRunner,
    NativeRunnerError, PersistentNativeSession, ProtocolDescriptor,
};
pub use native_adapter::{
    NativeTemplate, PreparedNativeInvocation, default_native_limits, extract_native,
    extract_native_cached, extract_native_checked, extract_native_with_adapter,
    native_executable_evidence, native_executable_id, native_input, native_request,
    prepare_native_invocation, unavailable_extraction,
};
pub use native_protocol::{
    Bound, EnvelopeState, MAX_NATIVE_INPUTS, MAX_NATIVE_KEY_BYTES, MAX_NATIVE_PAYLOAD_BYTES,
    MAX_NATIVE_RECORDS, MAX_NATIVE_REQUEST_BYTES, MAX_NATIVE_VALUE_BYTES, NATIVE_PAYLOAD_VERSION,
    NativeCoverage, NativeEnvelope, NativeProtocolError, NativeRecord, NativeRecordKind,
    NativeRequest, NativeRequestInput, Unbound,
};
pub use native_semantic::{
    NativeSemanticAdapter, NativeSemanticRequest, native_helper_evidence, native_semantic_evidence,
    native_semantic_input,
};
pub use pool::{BufferLease, BufferPool, PoolStats};
pub use prepared::{
    PreparationCache, PreparationCacheConfig, PreparationCacheStats, PreparationError,
    PreparationKey, PreparedCompilationCache, PreparedRequest,
};
pub use process::{
    ExecutableIdentity, MAX_EXECUTABLE_BYTES, ProcessEnvironment, ProcessLimits, ProcessReceipt,
    ProcessStdin, ProcessTerminal, Stdin, StdinSpec, SupervisedCommand, ToolchainArtifact,
    VerifiedExecutable,
};
pub use profile::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, TypeScriptSource, UnsupportedProfile,
};
pub use session::{
    Broken, Cold, ErasedSession, Handshaking, Pending, PersistentSession, Ready, RequestToken,
    SessionState,
};
pub use session_cache::{
    PersistentRequest, PersistentSessionCache, SessionCacheConfig, SessionCacheError,
    SessionCacheStats,
};
pub use supervisor::{ProcessSupervisor, RunningProcess};
pub use syntax::{
    DeclarationKind, GrammarVariant, SourceAnalysis, SourceDeclaration, SourceExcerpt,
    SourceExcerptExtent, SourceLanguage, SourceLocation, SyntaxError, SyntaxFrontend,
};

#[cfg(test)]
mod tests;
