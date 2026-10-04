//! Defines types terminal behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use backend_semantic::ir::{BuildError, FragmentError, FragmentView, Ir, PrepareError, WriteError};
use backend_semantic::vocabulary::{
    CompileRecipeFact, FrontendError, InvalidUtf8Fact, Language, LanguageProfile,
    LoweringUnsupported, NativeArtifactRole, NativeTool, NativeWorkPhase, NativeWorkerPanic, Stage,
};
use thiserror::Error;

use super::{AuthorityFailure, SourceIdentity, ToolchainSelectionFact};

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
    Prepare {
        /// Underlying cause returned by the operation named by this terminal.
        cause: NativeWorkError,
    },
    /// Starting the selected native executable failed.
    ToolStart {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Native child omitted its writable standard-input lease.
    MissingToolInput,
    /// Missing standard input also had a child-cleanup I/O failure.
    MissingToolInputCleanup {
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Native child omitted its readable standard-error lease.
    MissingToolDiagnostic,
    /// Missing standard error also had a child-cleanup I/O failure.
    MissingToolDiagnosticCleanup {
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Sending exact source to native standard input failed.
    ToolInput {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Source input and child cleanup both had exact I/O failures.
    ToolInputCleanup {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Interrupting a native child failed.
    ToolTerminate {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Polling a native child failed.
    ToolWait {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Polling and required child cleanup both had exact I/O failures.
    ToolWaitCleanup {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Reading bounded native diagnostics failed.
    ToolDiagnosticRead {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Diagnostic reading and required child cleanup both had exact I/O failures.
    ToolDiagnosticReadCleanup {
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// A scoped native I/O worker panicked after child ownership was established.
    WorkerPanic {
        /// Underlying cause returned by the operation named by this terminal.
        cause: NativeWorkerPanic,
    },
    /// Cancellation reaped the child before a semantic result.
    Cancelled {
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry reaped the child before a semantic result.
    DeadlineExceeded {
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Diagnostic capacity reaped the child before a semantic result.
    DiagnosticLimit {
        /// Configured maximum diagnostic byte count for this invocation.
        limit: usize,
        /// Total diagnostic bytes drained when the configured limit was reached.
        observed: usize,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native parser rejected source after reaping.
    NativeRejected {
        /// Exact process exit status returned by the native parser.
        status: std::process::ExitStatus,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
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

/// One fused public semantic result from exactly one authority traversal.
///
/// `ir` owns the exact source/recipe/scope header and every entity authority
/// plane from that transaction. `artifact` is its validated compact
/// projection. Only the compact fragment borrows caller output. If authority
/// entry, lowering, owned-tree build, compact write, or fragment validation
/// fails, this value is not returned.
pub struct CompiledSemantic<'artifact> {
    /// Validated compact artifact written after the owned semantic image built.
    pub artifact: CompiledFragment<'artifact>,
    /// Owned semantic image from the artifact's exact admitted fact lane.
    pub ir: Ir,
}

/// Queryable semantic image materialized directly from the same admitted fact
/// lane that writes the durable canonical fragment. It contains no second
/// frontend lowering or serialized intermediary.
pub struct CompiledIr {
    /// The image owns its source, recipe, scope, profile, and entity
    /// authority facts. No driver-side capture or provenance sidecar exists.
    pub ir: Ir,
}

/// Exact compile terminal with source-bearing native causes and bounded diagnostic facts.
#[derive(Debug, Error)]
pub enum CompileFailure<'diagnostic> {
    /// The source length could not fit the compact source identity width.
    #[error("source has {actual} bytes, which exceeds the compact source identity width")]
    SourceLength {
        /// Source byte length that could not fit the compact identity width.
        actual: usize,
        #[source]
        /// Concrete conversion error from the source length to its compact width.
        source: TryFromIntError,
    },
    /// The registry makes this requested semantic stage an explicit terminal.
    #[error("{language:?} {stage:?} does not support the requested semantic terminal")]
    UnsupportedStage {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Language selected by the compile request.
        language: Language,
        /// Semantic stage requested for the selected language.
        stage: Stage,
        /// Underlying cause returned by the operation named by this terminal.
        cause: FrontendError,
    },
    /// The request's closed selection cannot service the static registry row.
    #[error("{language:?} {stage:?} selected {selected:?}, not {provided:?}")]
    ToolchainSelectionMismatch {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Language selected by the compile request.
        language: Language,
        /// Semantic stage requested for the selected language.
        stage: Stage,
        /// Native tool selected by the closed registry and request.
        selected: NativeTool,
        /// Toolchain selection supplied for this compilation request.
        provided: ToolchainSelectionFact,
    },
    /// A resolved native executable does not match the closed registry tool selection.
    #[error("{language:?} {stage:?} resolved {resolved:?}, not selected {selected:?}")]
    ToolchainMismatch {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Language selected by the compile request.
        language: Language,
        /// Semantic stage requested for the selected language.
        stage: Stage,
        /// Native tool selected by the closed registry and request.
        selected: NativeTool,
        /// Native tool identity returned by executable resolution.
        resolved: NativeTool,
    },
    /// The caller-owned native work directory rejected this exact invocation phase.
    #[error("{recipe:?} native work {phase:?} failed")]
    NativeWork {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Exact native work phase that failed.
        phase: NativeWorkPhase,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: NativeWorkError,
    },
    /// A native terminal and cleanup failure are retained together without erasing either cause.
    #[error("{recipe:?} completed its native terminal but native work cleanup also failed")]
    NativeWorkCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Original native terminal preserved alongside the cleanup failure.
        primary: NativeWorkPrimary<'diagnostic>,
        #[source]
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: NativeWorkError,
    },
    /// The selected language has no locally reproducible native tooling adapter.
    #[error("{language:?} {stage:?} has no locally reproducible {tool:?} adapter")]
    ToolingUnavailable {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Language selected by the compile request.
        language: Language,
        /// Semantic stage requested for the selected language.
        stage: Stage,
        /// Native tool required by the selected language profile.
        tool: NativeTool,
    },
    /// Starting the native parser process preserved its concrete I/O cause.
    #[error("could not start {recipe:?}")]
    ToolStart {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// The native process did not provide a writable standard input lease.
    #[error("{recipe:?} did not expose standard input")]
    MissingToolInput {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
    },
    /// The child lacked stdin and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard input and cleanup failed")]
    MissingToolInputCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// The native process did not provide a readable standard error lease.
    #[error("{recipe:?} did not expose standard error")]
    MissingToolDiagnostic {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
    },
    /// The child lacked stderr and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard error and cleanup failed")]
    MissingToolDiagnosticCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Writing exact borrowed source to native stdin preserved its concrete I/O cause.
    #[error("could not send source for {recipe:?}")]
    ToolInput {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Source input failed and the required child cleanup retained a second concrete I/O cause.
    #[error("could not send source for {recipe:?}; cleanup also failed")]
    ToolInputCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// Killing an interrupted native child preserved its concrete I/O cause.
    #[error("could not terminate {recipe:?}")]
    ToolTerminate {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Waiting for the native parser process preserved its concrete I/O cause.
    #[error("could not observe {recipe:?}")]
    ToolWait {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Polling the child failed and required cleanup preserved a second concrete I/O cause.
    #[error("could not observe {recipe:?}; cleanup also failed")]
    ToolWaitCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// The bounded diagnostic reader retained its concrete I/O cause.
    #[error("could not read bounded native diagnostics for {recipe:?}")]
    ToolDiagnosticRead {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
    },
    /// Diagnostic reading failed and required child cleanup retained a second concrete I/O cause.
    #[error("could not read bounded native diagnostics for {recipe:?}; cleanup also failed")]
    ToolDiagnosticReadCleanup {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: std::io::Error,
        /// Concrete failure encountered while completing the required cleanup.
        cleanup: std::io::Error,
    },
    /// A scoped native I/O worker panicked after child ownership was established.
    #[error("native I/O worker panicked for {recipe:?}")]
    NativeWorkerPanic {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Underlying cause returned by the operation named by this terminal.
        cause: NativeWorkerPanic,
    },
    /// Cancellation killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} was cancelled")]
    Cancelled {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} exceeded its deadline")]
    DeadlineExceeded {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native stderr exceeded its caller-owned bounded lease and the child was killed and reaped.
    #[error("{recipe:?} exceeded diagnostic limit {limit} after {observed} bytes")]
    DiagnosticLimit {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Configured maximum diagnostic byte count for this invocation.
        limit: usize,
        /// Total diagnostic bytes drained when the configured limit was reached.
        observed: usize,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// The native parser rejected this exact source with bounded exact diagnostic facts.
    #[error("{recipe:?} rejected source with {status:?}")]
    NativeRejected {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Exact process exit status returned by the native parser.
        status: std::process::ExitStatus,
        /// Bounded diagnostic bytes and drain facts captured before child exit.
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// An exact language authority failed before canonical fact admission.
    ///
    /// The source error remains concrete and the companion diagnostic is a
    /// bounded typed transport projection, never a rendered replacement.
    #[error("{recipe:?} authority failed")]
    Authority {
        /// Exact source and binding identity selected by the request.
        source_identity: SourceIdentity,
        /// Exact recipe that selected this frontend authority.
        recipe: CompileRecipeFact,
        /// Original frontend error and its only valid diagnostic/phase projection.
        failure: AuthorityFailure<'diagnostic>,
    },
    /// A profile requiring a project-bearing semantic authority received none.
    #[error("{recipe:?} requires explicit semantic authority for {profile:?}")]
    AuthorityInputRequired {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Language profile that requires explicit semantic authority.
        profile: LanguageProfile,
    },
    /// A supplied project authority belongs to a language other than the selected profile.
    #[error("{recipe:?} cannot use the supplied semantic authority for {profile:?}")]
    AuthorityInputProfileMismatch {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Language profile that requires explicit semantic authority.
        profile: LanguageProfile,
    },
    /// Native syntax passed but its declaration lacks a closed compact semantic recipe.
    #[error("{recipe:?} has an unsupported LowerIr declaration recipe")]
    LoweringUnsupported {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: LoweringUnsupported,
    },
    /// A collector bound extension-fact coordinates that the same emission
    /// lane never admitted; the authority input and the lane disagree.
    #[error("{recipe:?} extension facts referenced unadmitted atoms at row {row}")]
    ExtensionAtomUnbound {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Emission row that referenced unadmitted authority coordinates.
        row: usize,
        /// Provisional atom coordinate rejected because the emission lane did not admit it.
        provisional: u32,
        /// Number of atoms admitted by the exact emission lane.
        atom_count: usize,
    },
    /// A generic extension named no exact admitted type-parameter list.
    #[error("{recipe:?} extension facts referenced unadmitted type parameters at row {row}")]
    ExtensionTypeParametersUnbound {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Emission row that referenced unadmitted authority coordinates.
        row: usize,
        /// First element index of the referenced type-parameter slice.
        start: u32,
        /// Number of type parameters referenced by the extension fact.
        length: u32,
        /// Number of type parameters admitted by the emission lane.
        element_count: usize,
    },
    /// Clang projection rejected one exact authority fact before canonical admission.
    #[error("{recipe:?} Clang projection failed: {fault:?}")]
    ClangProjection {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        /// Typed Clang projection failure returned by authority validation.
        fault: crate::driver::types::ClangProjectionFault,
    },
    /// Rich lowering could not condense the frontend tree into canonical IR.
    #[error("could not build canonical semantic IR for {recipe:?}")]
    Build {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: BuildError,
    },
    /// Lowering could not prepare the compact IR from its typed facts.
    #[error("could not prepare compact IR for {recipe:?}")]
    Prepare {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: PrepareError,
    },
    /// Caller output could not hold the prepared compact IR.
    #[error("could not write compact IR for {recipe:?}")]
    Write {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: WriteError,
    },
    /// Fresh compact IR failed its own borrowed validation.
    #[error("fresh compact IR failed validation for {recipe:?}")]
    Validate {
        /// Identity of the exact source bytes submitted for this compilation.
        source_identity: SourceIdentity,
        /// Closed recipe selected for this compilation.
        recipe: CompileRecipeFact,
        #[source]
        /// Underlying cause returned by the operation named by this terminal.
        cause: FragmentError,
    },
}
