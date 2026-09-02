//! Defines types terminal behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use compiler_ir::{FragmentError, FragmentView, PrepareError, WriteError};
use compiler_vocabulary::{
    CompileRecipeFact, FrontendError, InvalidUtf8Fact, Language, LoweringUnsupported,
    NativeArtifactRole, NativeTool, NativeWorkPhase, NativeWorkerPanic, Stage,
};
use thiserror::Error;

use super::{SourceIdentity, ToolchainSelectionFact};

/// Validation or I/O failure for the bounded caller-owned native work directory.
#[derive(Debug, Error)]
pub enum NativeWorkError {
    /// Relative work paths would reintroduce ambient process working-directory state.
    #[error("native work directory is not absolute")]
    RelativeDirectory,
    /// The caller-owned work directory could not be inspected.
    #[error("could not inspect the native work directory")]
    Inspect(#[source] std::io::Error),
    /// Compilation may begin only with a known empty caller-owned work directory.
    #[error("native work directory was not empty")]
    NotEmpty,
    /// Materializing an exact compiler input or configuration artifact failed.
    #[error("could not write native {artifact:?} artifact")]
    WriteArtifact {
        /// Closed role of the exact artifact the selected native adapter owns.
        artifact: NativeArtifactRole,
        /// Concrete filesystem cause retained from the materialization attempt.
        #[source]
        cause: std::io::Error,
    },
    /// Creating an exact adapter-owned artifact directory failed.
    #[error("could not create native {artifact:?} artifact directory")]
    CreateArtifactDirectory {
        /// Closed role of the exact adapter-owned directory.
        artifact: NativeArtifactRole,
        /// Concrete filesystem cause retained from the creation attempt.
        #[source]
        cause: std::io::Error,
    },
    /// Removing an exact compiler-owned input or output artifact failed after child reaping.
    #[error("could not remove native {artifact:?} artifact")]
    RemoveArtifact {
        /// Closed role of the exact artifact the selected native adapter owns.
        artifact: NativeArtifactRole,
        /// Concrete filesystem cause retained from the cleanup attempt.
        #[source]
        cause: std::io::Error,
    },
    /// An adapter-owned artifact name could not be represented as required text.
    #[error("native {artifact:?} artifact text was not valid UTF-8")]
    ArtifactText {
        /// Closed role of the artifact whose text representation was required.
        artifact: NativeArtifactRole,
        /// Compact exact malformed-UTF-8 fact from the source-derived name.
        fact: InvalidUtf8Fact,
    },
}

/// Exact native terminal retained when its post-reap work cleanup also fails.
#[derive(Debug)]
pub enum NativeWorkPrimary<'diagnostic> {
    /// Exact adapter input/configuration materialization failed before child spawn.
    Prepare { cause: NativeWorkError },
    /// Starting the selected native executable failed.
    ToolStart { cause: std::io::Error },
    /// Native child omitted its writable standard-input lease.
    MissingToolInput,
    /// Missing standard input also had a child-cleanup I/O failure.
    MissingToolInputCleanup { cleanup: std::io::Error },
    /// Native child omitted its readable standard-error lease.
    MissingToolDiagnostic,
    /// Missing standard error also had a child-cleanup I/O failure.
    MissingToolDiagnosticCleanup { cleanup: std::io::Error },
    /// Sending exact source to native standard input failed.
    ToolInput { cause: std::io::Error },
    /// Source input and child cleanup both had exact I/O failures.
    ToolInputCleanup {
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Interrupting a native child failed.
    ToolTerminate { cause: std::io::Error },
    /// Polling a native child failed.
    ToolWait { cause: std::io::Error },
    /// Polling and required child cleanup both had exact I/O failures.
    ToolWaitCleanup {
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Reading bounded native diagnostics failed.
    ToolDiagnosticRead { cause: std::io::Error },
    /// Diagnostic reading and required child cleanup both had exact I/O failures.
    ToolDiagnosticReadCleanup {
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// A scoped native I/O worker panicked after child ownership was established.
    WorkerPanic { cause: NativeWorkerPanic },
    /// Cancellation reaped the child before a semantic result.
    Cancelled {
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry reaped the child before a semantic result.
    DeadlineExceeded {
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Diagnostic capacity reaped the child before a semantic result.
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native parser rejected source after reaping.
    NativeRejected {
        status: std::process::ExitStatus,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
}

/// Separate caller-owned compact IR output region reused across compile calls.
pub struct CompileOutput<'output> {
    /// Output storage for the compact validated-source fragment.
    pub fragment_output: &'output mut [u8],
}

/// Borrowed bounded diagnostic emitted by a reaped native adapter terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeDiagnostic<'diagnostic> {
    /// Exact retained prefix in the caller-owned diagnostic lease.
    pub bytes: &'diagnostic [u8],
    /// Exact total byte count drained from both native diagnostic streams.
    pub observed: usize,
    /// Whether additional diagnostic bytes were drained without retention.
    pub truncated: bool,
}

/// Borrowed compact IR emitted only after native parser admission and typed lowering succeed.
pub struct CompiledFragment<'artifact> {
    /// Exact source fact persisted by the semantic fragment.
    pub source: SourceIdentity,
    /// Closed recipe fact bound to the returned lowering terminal.
    pub recipe: CompileRecipeFact,
    /// Validated compact IR borrowing only the separate caller-owned output region.
    pub fragment: FragmentView<'artifact>,
}

/// Exact compile terminal with source-bearing native causes and bounded diagnostic facts.
#[derive(Debug, Error)]
pub enum CompileFailure<'diagnostic> {
    /// The source length could not fit the compact source identity width.
    #[error("source has {actual} bytes, which exceeds the compact source identity width")]
    SourceLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    /// The registry makes this requested semantic stage an explicit terminal.
    #[error("{language:?} {stage:?} does not support the requested semantic terminal")]
    UnsupportedStage {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        cause: FrontendError,
    },
    /// The request's closed selection cannot service the static registry row.
    #[error("{language:?} {stage:?} selected {selected:?}, not {provided:?}")]
    ToolchainSelectionMismatch {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        selected: NativeTool,
        provided: ToolchainSelectionFact,
    },
    /// A resolved native executable does not match the closed registry tool selection.
    #[error("{language:?} {stage:?} resolved {resolved:?}, not selected {selected:?}")]
    ToolchainMismatch {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        selected: NativeTool,
        resolved: NativeTool,
    },
    /// The caller-owned native work directory rejected this exact invocation phase.
    #[error("{recipe:?} native work {phase:?} failed")]
    NativeWork {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        phase: NativeWorkPhase,
        #[source]
        cause: NativeWorkError,
    },
    /// A native terminal and cleanup failure are retained together without erasing either cause.
    #[error("{recipe:?} completed its native terminal but native work cleanup also failed")]
    NativeWorkCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        primary: NativeWorkPrimary<'diagnostic>,
        #[source]
        cleanup: NativeWorkError,
    },
    /// The selected language has no locally reproducible native tooling adapter.
    #[error("{language:?} {stage:?} has no locally reproducible {tool:?} adapter")]
    ToolingUnavailable {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        tool: NativeTool,
    },
    /// Starting the native parser process preserved its concrete I/O cause.
    #[error("could not start {recipe:?}")]
    ToolStart {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// The native process did not provide a writable standard input lease.
    #[error("{recipe:?} did not expose standard input")]
    MissingToolInput {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// The child lacked stdin and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard input and cleanup failed")]
    MissingToolInputCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cleanup: std::io::Error,
    },
    /// The native process did not provide a readable standard error lease.
    #[error("{recipe:?} did not expose standard error")]
    MissingToolDiagnostic {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// The child lacked stderr and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard error and cleanup failed")]
    MissingToolDiagnosticCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cleanup: std::io::Error,
    },
    /// Writing exact borrowed source to native stdin preserved its concrete I/O cause.
    #[error("could not send source for {recipe:?}")]
    ToolInput {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Source input failed and the required child cleanup retained a second concrete I/O cause.
    #[error("could not send source for {recipe:?}; cleanup also failed")]
    ToolInputCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Killing an interrupted native child preserved its concrete I/O cause.
    #[error("could not terminate {recipe:?}")]
    ToolTerminate {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Waiting for the native parser process preserved its concrete I/O cause.
    #[error("could not observe {recipe:?}")]
    ToolWait {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Polling the child failed and required cleanup preserved a second concrete I/O cause.
    #[error("could not observe {recipe:?}; cleanup also failed")]
    ToolWaitCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// The bounded diagnostic reader retained its concrete I/O cause.
    #[error("could not read bounded native diagnostics for {recipe:?}")]
    ToolDiagnosticRead {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Diagnostic reading failed and required child cleanup retained a second concrete I/O cause.
    #[error("could not read bounded native diagnostics for {recipe:?}; cleanup also failed")]
    ToolDiagnosticReadCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// A scoped native I/O worker panicked after child ownership was established.
    #[error("native I/O worker panicked for {recipe:?}")]
    NativeWorkerPanic {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        cause: NativeWorkerPanic,
    },
    /// Cancellation killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} was cancelled")]
    Cancelled {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} exceeded its deadline")]
    DeadlineExceeded {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native stderr exceeded its caller-owned bounded lease and the child was killed and reaped.
    #[error("{recipe:?} exceeded diagnostic limit {limit} after {observed} bytes")]
    DiagnosticLimit {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        limit: usize,
        observed: usize,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// The native parser rejected this exact source with bounded exact diagnostic facts.
    #[error("{recipe:?} rejected source with {status:?}")]
    NativeRejected {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        status: std::process::ExitStatus,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native syntax passed but its declaration lacks a closed compact semantic recipe.
    #[error("{recipe:?} has an unsupported LowerIr declaration recipe")]
    LoweringUnsupported {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: LoweringUnsupported,
    },
    /// Lowering could not prepare the compact IR from its typed facts.
    #[error("could not prepare compact IR for {recipe:?}")]
    Prepare {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: PrepareError,
    },
    /// Caller output could not hold the prepared compact IR.
    #[error("could not write compact IR for {recipe:?}")]
    Write {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: WriteError,
    },
    /// Fresh compact IR failed its own borrowed validation.
    #[error("fresh compact IR failed validation for {recipe:?}")]
    Validate {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: FragmentError,
    },
    /// The direct Clang semantic frontend failed with its exact typed cause.
    #[error("{recipe:?} direct Clang frontend: {cause}")]
    ClangFrontend {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: crate::ClangFailure,
    },
}
