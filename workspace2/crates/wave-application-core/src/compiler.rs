//! Lean, monomorphized compiler capability boundary for the application service.

use std::{io::ErrorKind, time::Duration};

use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_id::{
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
    /// Closed compiler language admitted by the application vocabulary.
    pub language: Language,
    /// Closed compiler stage admitted by the application vocabulary.
    pub stage: Stage,
    /// Exact transport-bounded source bytes to parse and lower.
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
        reason = "the fixed-capacity terminal deliberately retains exact compiler authorities and bounded diagnostics; boxing it would allocate on the failure path"
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

    #[allow(
        clippy::result_large_err,
        reason = "the fixed-capacity terminal deliberately retains exact compiler authorities and bounded diagnostics; boxing it would allocate on the failure path"
    )]
    fn generate(
        &mut self,
        request: CompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        Err(CompilerTerminal::Unavailable {
            language: request.language,
            stage: request.stage,
        })
    }
}

/// Bounded semantic terminal projected from a concrete compiler or publication adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        /// Bounded native diagnostic retained before cancellation won.
        diagnostic: CompilerDiagnostic,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerCause {
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
        /// Bounded diagnostic facts copied out of caller scratch.
        diagnostic: CompilerDiagnostic,
    },
    /// The native child exceeded its explicit deadline.
    DeadlineExceeded {
        /// Bounded diagnostic facts copied out of caller scratch.
        diagnostic: CompilerDiagnostic,
    },
    /// Native diagnostics exceeded their caller-provided capacity.
    DiagnosticLimit {
        /// Retained diagnostic capacity.
        limit: usize,
        /// Total diagnostic bytes observed before termination.
        observed: usize,
        /// Bounded diagnostic facts copied out of caller scratch.
        diagnostic: CompilerDiagnostic,
    },
    /// A successful parse had no admitted compact lowering recipe.
    Lowering(LoweringCause),
    /// Compact IR construction, output writing, or self-validation failed.
    Fragment(FragmentCause),
}

/// Exact caller-owned native-work phase projected without an allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        /// Portable and platform I/O facts.
        cause: NativeIoFact,
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

/// Exact caller-owned native work-directory phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkPhase {
    /// The directory was prepared and proved empty before native work.
    Prepare,
    /// The directory was cleaned and proved empty after native work.
    Cleanup,
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
    /// Materializing the artifact before or during native execution.
    Write,
    /// Creating the exact adapter-owned directory that contains an artifact family.
    CreateDirectory,
    /// Removing the artifact after native execution.
    Remove,
}

/// Closed artifact roles owned by one native adapter invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactRole {
    /// Rust's metadata-only parser probe output.
    RustMetadata,
    /// TypeScript source passed to the explicit compiler.
    TypeScriptSource,
    /// TypeScript's fixed adapter-owned work directory.
    TypeScriptWork,
    /// C# source passed through the explicit SDK project.
    CSharpSource,
    /// C# project that fixes the compilation shape.
    CSharpProject,
    /// C# `NuGet` configuration that clears remote package feeds.
    CSharpNuGetConfig,
    /// C# SDK restore/intermediate output directory.
    CSharpIntermediateOutput,
    /// C# SDK compiler output directory.
    CSharpBuildOutput,
    /// C#'s fixed adapter-owned work directory.
    CSharpWork,
    /// C#'s isolated .NET CLI home.
    CSharpDotnetHome,
    /// C#'s isolated `NuGet` package cache.
    CSharpNuGetPackages,
    /// Go source passed to the explicit compiler.
    GoSource,
    /// Go object output from the explicit compiler.
    GoObject,
    /// Go's fixed adapter-owned work directory.
    GoWork,
    /// Java source passed to the explicit compiler.
    JavaSource,
    /// Java argument file carrying the selected source name.
    JavaArguments,
    /// Java's fixed adapter-owned work directory.
    JavaWork,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePrimaryCause {
    /// Adapter preparation failed and subsequent cleanup also failed.
    PrepareDirectory(NativeDirectoryCause),
    /// A named preparation artifact operation and subsequent cleanup both failed.
    PrepareArtifact {
        /// Exact preparation action.
        action: NativeArtifactAction,
        /// Exact adapter-owned artifact.
        artifact: NativeArtifactRole,
        /// Portable and platform I/O facts.
        cause: NativeIoFact,
    },
    /// Native child startup failed.
    ToolStart(NativeIoFact),
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
    /// Reading native diagnostics failed.
    DiagnosticRead(NativeIoFact),
    /// Cancellation won and preserved bounded diagnostics.
    Cancelled(CompilerDiagnostic),
    /// Deadline expiry won and preserved bounded diagnostics.
    DeadlineExceeded(CompilerDiagnostic),
    /// Diagnostic capacity terminated native work with exact observed width.
    DiagnosticLimit {
        /// Retained diagnostic capacity.
        limit: usize,
        /// Total diagnostic bytes observed before termination.
        observed: usize,
        /// Copied bounded diagnostic facts.
        diagnostic: CompilerDiagnostic,
    },
    /// Native syntax rejection retained its exit code and bounded diagnostics.
    Rejected {
        /// Exit status code when the child exposed one.
        code: Option<i32>,
        /// Copied bounded diagnostic facts.
        diagnostic: CompilerDiagnostic,
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
        /// Portable and platform I/O facts.
        cause: NativeIoFact,
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

/// Closed compact-lowering rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoweringCause {
    /// No declaration recipe is represented.
    NoSupportedDeclaration,
    /// Rust function recipes are intentionally separate from constant recipes.
    RustFunction,
    /// Rust constant type is outside the compact recipe vocabulary.
    RustConstantType,
    /// Python assignment lacks a supported name.
    PythonAssignmentName,
    /// Python assignment value is outside the compact recipe vocabulary.
    PythonAssignmentValue,
    /// Clang declaration shape is outside the compact recipe vocabulary.
    ClangDeclarationForm,
    /// TypeScript declaration shape is outside the compact recipe vocabulary.
    TypeScriptDeclarationForm,
    /// TypeScript declaration type is outside the compact recipe vocabulary.
    TypeScriptDeclarationType,
    /// C# declaration shape is outside the compact recipe vocabulary.
    CSharpDeclarationForm,
    /// C# declaration type is outside the compact recipe vocabulary.
    CSharpDeclarationType,
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

/// Fixed-size retained native diagnostic. The source bytes never outlive the caller scratch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerDiagnostic {
    /// Exact retained diagnostic byte count.
    pub byte_len: usize,
    /// Whether native output exceeded the retained diagnostic or native-stream bound.
    pub truncated: bool,
    /// Zero-filled storage whose prefix through `byte_len` is the exact diagnostic prefix.
    pub bytes: [u8; MAX_COMPILER_DIAGNOSTIC_BYTES],
}

/// Fixed copied diagnostic width at the application boundary.
pub const MAX_COMPILER_DIAGNOSTIC_BYTES: usize = 8;

impl CompilerDiagnostic {
    /// Copies only the bounded diagnostic prefix that can survive the compiler scratch lease.
    #[must_use]
    pub fn copy_from(bytes: &[u8], truncated: bool) -> Self {
        let retained = bytes.len().min(MAX_COMPILER_DIAGNOSTIC_BYTES);
        let mut output = [0; MAX_COMPILER_DIAGNOSTIC_BYTES];
        output[..retained].copy_from_slice(&bytes[..retained]);
        Self {
            byte_len: retained,
            truncated: truncated || retained != bytes.len(),
            bytes: output,
        }
    }
}
