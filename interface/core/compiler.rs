//! Defines compiler behavior for `interface-core`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Lean, monomorphized compiler capability boundary for the application service.

use std::{io::ErrorKind, ops::Deref, time::Duration};

use compiler_vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, CompileRecipeFact, Language, LanguageProfile,
    LoweringUnsupported, MAX_NATIVE_DIAGNOSTIC_BYTES, NativeTool, Stage,
};
pub use compiler_vocabulary::{
    InvalidUtf8Fact, MAX_NATIVE_WORKER_PANIC_BYTES, NativeArtifactRole, NativeWorkPhase,
    NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass, NativeWorkerPanicMessage,
};
use heart_identity::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, CompileRecipeDomain,
    ContentId, DependencySetDomain, GenerationId, IrFragmentDomain, IrFragmentEncoding,
    IrManifestDomain, IrManifestEncoding, SourceFactDomain,
};

/// Typed source authority copied from a validated compiler result without importing its format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceAuthority {
    /// Central identity of the exact native compiler input.
    pub identity: ContentId<SourceFactDomain>,
    /// Exact input byte length bound into the compact IR fragment.
    pub byte_len: u32,
}

/// Typed verified-generation authority copied without importing the hydration planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationAuthority {
    /// Generation whose complete closure was verified before durable publication.
    pub pinned_root: GenerationId,
    /// Identity of that exact complete dependency set.
    pub dep_set: ContentId<DependencySetDomain>,
}

/// Compact authority retained by every compiler failure after its borrowed driver result ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerAttempt {
    /// Exact native input identity and byte length.
    pub source: SourceAuthority,
    /// Canonical identity of the full language, stage, toolchain, and source recipe.
    pub recipe: ContentId<CompileRecipeDomain>,
}

/// Exact public facts emitted only after a local compiler established durable publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeneratedArtifact {
    /// Source authority measured and accepted by the native compiler.
    pub source: SourceAuthority,
    /// Exact language, stage, toolchain, and source recipe authority.
    pub recipe: CompileRecipeFact,
    /// Identity of the validated compact IR fragment.
    pub fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    /// Complete durable-publication authority.
    pub publication: PublicationAuthority,
}

/// Immutable authorities that connect a generated fragment to a stable local publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationAuthority {
    /// Generation whose complete object closure was verified before journal publication.
    pub generation: GenerationAuthority,
    /// Identity of the complete package manifest that names this fragment.
    pub manifest: ArtifactId<IrManifestEncoding, IrManifestDomain>,
    /// Identity of the immutable generation-to-manifest binding.
    pub binding: ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>,
}

/// Borrowed semantic source-compilation request passed through the monomorphized local capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerRequest<'input> {
    /// Closed source-language profile admitted by the application vocabulary.
    pub profile: LanguageProfile,
    /// Closed compiler stage admitted by the application vocabulary.
    pub stage: Stage,
    /// Exact application-admitted source bytes to parse and lower.
    pub source: &'input str,
}

/// Whether this service specialization has a real local compiler owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerReadiness {
    /// No compiler owner is linked into this portable application specialization.
    Unavailable,
    /// A concrete local adapter owns explicit toolchain and publication capabilities.
    Ready,
}

/// Closed compiler result boundary: no dynamic errors or borrowed scratch escape the call.
pub trait CompilerCapability {
    /// Reports whether this concrete service specialization owns a local compiler capability.
    fn readiness(&self) -> CompilerReadiness;

    /// Compiles, validates, and durably publishes one bounded source request.
    ///
    /// # Errors
    ///
    /// Returns a closed [`CompilerTerminal`] retaining every application-level authority and
    /// bounded diagnostic fact when no generated durable artifact becomes visible.
    #[allow(
        clippy::result_large_err,
        reason = "the terminal keeps independent source, recipe, and publication authorities inline; only an emitted native diagnostic owns its single cold box, so boxing this outer terminal would introduce a second error-path allocation"
    )]
    fn generate(
        &mut self,
        request: CompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal>;
}

/// Portable specialization with no linked compiler or publication dependencies.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableCompiler;

impl CompilerCapability for UnavailableCompiler {
    fn readiness(&self) -> CompilerReadiness {
        CompilerReadiness::Unavailable
    }

    fn generate(
        &mut self,
        request: CompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        Err(CompilerTerminal::Unavailable {
            language: request.profile.language(),
            stage: request.stage,
        })
    }
}

/// Bounded semantic terminal projected from a concrete compiler or publication adapter.
#[derive(Debug, Eq, PartialEq)]
pub enum CompilerTerminal {
    /// Source width exceeded the compiler identity representation before an authority existed.
    SourceLength {
        /// Exact source byte length that could not fit the compact source fact width.
        actual: usize,
    },
    /// The portable service carries no compiler owner for this accepted language/stage.
    Unavailable {
        /// Requested closed language family.
        language: Language,
        /// Requested closed compiler stage.
        stage: Stage,
    },
    /// The compiler registry rejects this stage before any native process starts.
    UnsupportedStage {
        /// Identity and byte length of the exact source under evaluation.
        source: SourceAuthority,
        /// Requested closed language family.
        language: Language,
        /// Requested closed compiler stage.
        stage: Stage,
    },
    /// A fresh bounded timeout could not be represented by this platform's monotonic clock.
    DeadlineConstruction {
        /// Identity and byte length of the exact source under evaluation.
        source: SourceAuthority,
        /// Requested closed language family.
        language: Language,
        /// Requested closed compiler stage.
        stage: Stage,
        /// Validated per-invocation timeout that could not be added to the current clock instant.
        timeout: Duration,
    },
    /// The configured local toolchain cannot service this registry-selected native tool.
    Toolchain {
        /// Identity and byte length of the exact source under evaluation.
        source: SourceAuthority,
        /// Requested closed language family.
        language: Language,
        /// Requested closed compiler stage.
        stage: Stage,
        /// Tool selected by the closed compiler registry.
        selected: NativeTool,
        /// Tool actually carried by the configured adapter, if any.
        configured: Option<NativeTool>,
    },
    /// Native tooling is deliberately absent for this language/tool pair.
    ToolingUnavailable {
        /// Identity and byte length of the exact source under evaluation.
        source: SourceAuthority,
        /// Requested closed language family.
        language: Language,
        /// Requested closed compiler stage.
        stage: Stage,
        /// Native tool whose absence is explicit rather than an ambient lookup failure.
        tool: NativeTool,
    },
    /// Cancellation won before an IR fragment could become visible.
    Cancelled {
        /// Exact source and canonical recipe identity under evaluation.
        attempted: CompilerAttempt,
        /// Native diagnostic retained only when native work emitted one before cancellation won.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// Native work, parsing, lowering, or fragment validation failed under exact authorities.
    Compile {
        /// Exact source and canonical recipe identity under evaluation.
        attempted: CompilerAttempt,
        /// Closed bounded compiler cause.
        cause: CompilerCause,
    },
    /// IR existed but no stable durable publication became visible.
    Publication {
        /// Exact source and canonical recipe identity bound into the attempted fragment.
        attempted: CompilerAttempt,
        /// Closed publication phase/cause, without a stringly error detail.
        cause: PublicationCause,
    },
}

/// Typed non-publication failure class compact enough to cross every application adapter.
#[derive(Debug, Eq, PartialEq)]
pub enum CompilerCause {
    /// A language semantic authority failed before a canonical IR fragment existed.
    Authority {
        /// Exact authority transaction phase that failed.
        phase: AuthorityPhase,
        /// Exact source-diagnostic class derived from the retained frontend error.
        class: AuthorityDiagnosticClass,
        /// Bounded authority diagnostic projection, when the authority exposed bytes.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// A caller-owned native-work directory rejected preparation or cleanup.
    NativeWork(NativeWorkCause),
    /// The native child lifecycle failed in a named I/O phase.
    NativeIo {
        /// Precise child lifecycle phase.
        phase: NativeIoPhase,
        /// Preserved standard-library and platform I/O facts without formatting the OS error.
        cause: NativeIoFact,
    },
    /// Native syntax rejected the bounded source.
    NativeRejected {
        /// Exit status code when the child exposed one.
        code: Option<i32>,
        /// Native diagnostic copied out of caller scratch only when one was emitted.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// The native child exceeded its explicit deadline.
    DeadlineExceeded {
        /// Native diagnostic copied out of caller scratch only when one was emitted.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// Native diagnostics exceeded their caller-provided capacity.
    DiagnosticLimit {
        /// Retained diagnostic capacity.
        limit: usize,
        /// Total diagnostic bytes observed before termination.
        observed: usize,
        /// Native diagnostic copied out of caller scratch only when one was emitted.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// A successful parse had no admitted compact lowering recipe.
    Lowering(LoweringUnsupported),
    /// Compact IR construction, output writing, or self-validation failed.
    Fragment(FragmentCause),
}

/// Exact caller-owned native-work phase projected without an allocation.
#[derive(Debug, Eq, PartialEq)]
pub enum NativeWorkCause {
    /// One exact caller-owned work-directory phase failed before or after native execution.
    Directory {
        /// Exact work-directory phase retaining this terminal.
        phase: NativeWorkPhase,
        /// Closed typed directory failure cause.
        cause: NativeDirectoryCause,
    },
    /// A named native adapter artifact could not be materialized or removed.
    Artifact {
        /// Native-work lifecycle phase owning the operation.
        phase: NativeWorkPhase,
        /// Exact filesystem action the adapter attempted.
        action: NativeArtifactAction,
        /// Closed owned artifact role.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
    /// A native child reached one exact terminal without a second work-directory terminal.
    Primary(NativePrimaryCause),
    /// A native primary terminal and cleanup terminal both occurred; neither is fabricated.
    PrimaryAndCleanup {
        /// Closed primary class.
        primary: NativePrimaryCause,
        /// Exact cleanup class.
        cleanup: NativeWorkCleanupCause,
    },
}

/// Exact named-artifact rejection without formatting a filesystem or text error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactCause {
    /// The filesystem operation retained portable and platform I/O facts.
    Io(NativeIoFact),
    /// A source-derived artifact name was not valid UTF-8.
    InvalidText(InvalidUtf8Fact),
}

/// Closed native work-directory failure cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeDirectoryCause {
    /// The configured work directory was relative.
    RelativeDirectory,
    /// The configured work directory was not empty.
    NotEmpty,
    /// Inspecting the configured work directory had this I/O category.
    InspectIo(NativeIoFact),
}

/// Exact filesystem action applied to one named native adapter artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactAction {
    /// Converting a source-derived artifact name to its required text representation.
    ResolveText,
    /// Materializing the artifact before or during native execution.
    Write,
    /// Creating the exact adapter-owned directory that contains an artifact family.
    CreateDirectory,
    /// Removing the artifact after native execution.
    Remove,
}

/// Allocation-free native I/O facts that retain both the portable class and platform code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeIoFact {
    /// Portable standard-library I/O category.
    pub kind: ErrorKind,
    /// Exact platform error number when the operating system supplied one.
    pub raw_os_code: Option<i32>,
}

/// Named primary native terminal retained when cleanup also failed.
#[derive(Debug, Eq, PartialEq)]
pub enum NativePrimaryCause {
    /// Adapter preparation failed and subsequent cleanup also failed.
    PrepareDirectory(NativeDirectoryCause),
    /// A named preparation artifact operation and subsequent cleanup both failed.
    PrepareArtifact {
        /// Exact preparation action.
        action: NativeArtifactAction,
        /// Exact adapter-owned artifact.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
    /// Native child startup failed.
    ToolStart(NativeIoFact),
    /// A scoped native I/O worker panicked after child ownership was established.
    WorkerPanic(NativeWorkerPanic),
    /// Native standard input was unavailable.
    MissingInput {
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Native standard error was unavailable.
    MissingDiagnostic {
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Writing source to native standard input failed.
    Input {
        /// Exact I/O category from source write.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Interrupting native work failed.
    Terminate(NativeIoFact),
    /// Waiting for native work failed.
    Wait {
        /// Exact I/O category from child wait.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Reading native diagnostics failed, with any paired child cleanup retained.
    DiagnosticRead {
        /// Exact diagnostic-reader I/O category.
        cause: NativeIoFact,
        /// Optional child-cleanup I/O category retained from the paired terminal.
        cleanup: Option<NativeIoFact>,
    },
    /// Cancellation won and retained native diagnostics only when native work emitted them.
    Cancelled(Option<CompilerDiagnostic>),
    /// Deadline expiry won and retained native diagnostics only when native work emitted them.
    DeadlineExceeded(Option<CompilerDiagnostic>),
    /// Diagnostic capacity terminated native work with exact observed width.
    DiagnosticLimit {
        /// Retained diagnostic capacity.
        limit: usize,
        /// Total diagnostic bytes observed before termination.
        observed: usize,
        /// Native diagnostic copied only when native work emitted one.
        diagnostic: Option<CompilerDiagnostic>,
    },
    /// Native syntax rejection retained its exit code and bounded diagnostics.
    Rejected {
        /// Exit status code when the child exposed one.
        code: Option<i32>,
        /// Native diagnostic copied only when native work emitted one.
        diagnostic: Option<CompilerDiagnostic>,
    },
}

/// Cleanup class that accompanies a primary native terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkCleanupCause {
    /// The directory was relative during cleanup.
    RelativeDirectory,
    /// The directory retained unknown files after cleanup.
    NotEmpty,
    /// Cleanup inspection or removal had this I/O category.
    Io(NativeIoFact),
    /// Cleanup of one named native adapter artifact failed.
    Artifact {
        /// Exact cleanup operation.
        action: NativeArtifactAction,
        /// Exact adapter-owned artifact.
        artifact: NativeArtifactRole,
        /// Exact materialization or text-conversion rejection.
        cause: NativeArtifactCause,
    },
}

/// Named native child lifecycle phase for compact I/O diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeIoPhase {
    /// Starting the configured executable.
    Start,
    /// Writing exact source bytes to child standard input.
    Input,
    /// Terminating an interrupted child.
    Terminate,
    /// Waiting for child termination.
    Wait,
    /// Reading bounded child diagnostics.
    DiagnosticRead,
}

/// Closed compact-IR construction phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentCause {
    /// Preparing canonical IR facts failed.
    Prepare,
    /// Caller-provided fragment output was insufficient or rejected the write.
    Write,
    /// Fresh output rejected its independent IR validation.
    Validate,
}

/// Closed durable-publication phase; individual lower errors remain owned in the concrete adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationCause {
    /// Cancellation won before immutable artifact storage.
    CancelledBeforeStorage,
    /// Cancellation won after immutable storage but before journal admission.
    CancelledBeforeAdmission,
    /// Cancellation won after admission before a stable receipt.
    CancelledAfterAdmission,
    /// Publisher admission was full.
    AdmissionFull,
    /// Publisher admission was closed or its owner disappeared.
    AdmissionClosed,
    /// Immutable storage, manifest, generation, binding, or durable receipt rejected publication.
    Rejected(PublicationPhase),
    /// Stable receipt and pre-journal binding named different generations.
    StableGenerationMismatch,
}

/// Named non-cancellation publication phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationPhase {
    /// Canonical package construction.
    Canonical,
    /// Canonical manifest write or immutable storage.
    Manifest,
    /// Immutable fragment storage or range construction.
    Fragment,
    /// Complete generation construction.
    Generation,
    /// Generation-to-manifest binding write or storage.
    Binding,
    /// Durable publisher owner failed after admission.
    Durable,
}

/// One concrete cold allocation retaining a native diagnostic after its scratch lease ends.
#[derive(Debug, Eq, PartialEq)]
pub struct CompilerDiagnostic(Box<CompilerDiagnosticFacts>);

/// Full admitted native diagnostic facts exposed through [`CompilerDiagnostic`] dereferencing.
#[derive(Debug, Eq, PartialEq)]
pub struct CompilerDiagnosticFacts {
    /// Exact retained diagnostic byte count.
    pub byte_len: usize,
    /// Exact total native diagnostic bytes drained before this terminal.
    pub observed: usize,
    /// Whether native output exceeded the retained diagnostic or native-stream bound.
    pub truncated: bool,
    /// Zero-filled storage whose prefix through `byte_len` is the exact diagnostic prefix.
    pub bytes: [u8; MAX_NATIVE_DIAGNOSTIC_BYTES],
}

impl Deref for CompilerDiagnostic {
    type Target = CompilerDiagnosticFacts;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl CompilerDiagnostic {
    /// Copies an emitted native diagnostic before the compiler scratch lease ends.
    ///
    /// An empty, complete native stream has no diagnostic fact and deliberately owns no heap
    /// allocation. Any retained bytes, truncated stream, or positive observed width owns exactly
    /// one immutable fact.
    #[must_use]
    pub fn from_native(bytes: &[u8], observed: usize, truncated: bool) -> Option<Self> {
        if bytes.is_empty() && observed == 0 && !truncated {
            return None;
        }
        let retained = bytes.len().min(MAX_NATIVE_DIAGNOSTIC_BYTES);
        let mut output = [0; MAX_NATIVE_DIAGNOSTIC_BYTES];
        let (Some(source), Some(destination)) = (bytes.get(..retained), output.get_mut(..retained))
        else {
            return None;
        };
        destination.copy_from_slice(source);
        Some(Self(Box::new(CompilerDiagnosticFacts {
            byte_len: retained,
            observed,
            truncated: truncated || retained != bytes.len(),
            bytes: output,
        })))
    }
}
