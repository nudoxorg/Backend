//! Closed compiler facts shared by registries, native drivers, publication, and interfaces.
//! This crate describes requests and failures but deliberately performs no compilation or I/O.
//! Stable numeric conversions belong here because those values participate in canonical identities.
#![no_std]

use heart_identity::{CompileRecipeDomain, ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

mod profile;

pub use profile::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, TypeScriptSource, UnknownLanguageProfile,
};

/// Source-language family understood by the compiler plane.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Language {
    /// Rust source compiled through `rustc` and lowered from Rust syntax.
    Rust,
    /// TypeScript source checked by `tsc` and lowered from TypeScript syntax.
    TypeScript,
    /// Python source checked by the selected Python frontend and lowered from Python syntax.
    Python,
    /// Go source compiled by the Go toolchain and lowered from Go syntax.
    Go,
    /// Java source compiled by `javac` and lowered from Java syntax.
    Java,
    /// C# source compiled by the .NET SDK and lowered from C# syntax.
    CSharp,
    /// C-family source compiled and lowered through Clang.
    Clang,
}

impl From<Language> for u8 {
    /// Encodes the stable one-byte language discriminant used by canonical recipes.
    fn from(value: Language) -> Self {
        match value {
            Language::Rust => 0,
            Language::TypeScript => 1,
            Language::Python => 2,
            Language::Go => 3,
            Language::Java => 4,
            Language::CSharp => 5,
            Language::Clang => 6,
        }
    }
}

impl TryFrom<u8> for Language {
    type Error = u8;

    /// Decodes a stable language discriminant, returning the unknown byte unchanged.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rust),
            1 => Ok(Self::TypeScript),
            2 => Ok(Self::Python),
            3 => Ok(Self::Go),
            4 => Ok(Self::Java),
            5 => Ok(Self::CSharp),
            6 => Ok(Self::Clang),
            value => Err(value),
        }
    }
}

impl Language {
    /// Canonical language-family order used by compiler schedules and reports.
    pub const ALL: [Self; 7] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::Clang,
    ];
}

/// Closed phase of one language semantic-authority transaction.
///
/// This vocabulary is shared by the concrete compiler error, durable
/// application terminal, CLI, MCP, and GPUI projections.  It names the phase
/// only; the driver retains the frontend's concrete source error separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityPhase {
    /// Opening a caller-selected project, native library, or authority image failed.
    Open,
    /// Source or authority-image parsing failed.
    Parse,
    /// Cross-file, symbol, or import resolution failed.
    Resolve,
    /// Type checking or recursive type extraction failed.
    TypeCheck,
    /// Projecting valid authority facts into canonical IR failed.
    Project,
}

/// Closed class of diagnostic authority retained at application boundaries.
///
/// The concrete frontend error stays in `compiler-driver`; this compact class
/// lets CLI, MCP, and GPUI distinguish syntax, binding, type, infrastructure,
/// and canonical-projection failures without rendering an error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityDiagnosticClass {
    /// The authority rejected source or image syntax.
    Syntax,
    /// The authority could not bind a symbol, import, package, or project graph.
    Binding,
    /// The authority could not establish a required type fact.
    Type,
    /// Loading or running the selected authority itself failed.
    Authority,
    /// Canonical admission rejected a complete authority fact set.
    Projection,
}

/// Semantic compiler phase requested by an application operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Validate syntax without producing canonical IR.
    Parse,
    /// Validate syntax and lower semantic facts into canonical IR.
    LowerIr,
}

impl From<Stage> for u8 {
    /// Encodes the stable one-byte stage discriminant used by canonical recipes.
    fn from(value: Stage) -> Self {
        match value {
            Stage::Parse => 0,
            Stage::LowerIr => 1,
        }
    }
}

impl TryFrom<u8> for Stage {
    type Error = u8;

    /// Decodes a stable stage discriminant, returning the unknown byte unchanged.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Parse),
            1 => Ok(Self::LowerIr),
            value => Err(value),
        }
    }
}

/// Concrete native tool family selected only by the closed compiler registry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NativeTool {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
    CSharpCompiler,
}

impl From<NativeTool> for u8 {
    /// Encodes the stable one-byte native-tool discriminant used by canonical recipes.
    fn from(value: NativeTool) -> Self {
        match value {
            NativeTool::Rustc => 0,
            NativeTool::Clang => 1,
            NativeTool::Python => 2,
            NativeTool::TypeScriptCompiler => 3,
            NativeTool::GoCompiler => 4,
            NativeTool::JavaCompiler => 5,
            NativeTool::CSharpCompiler => 6,
        }
    }
}

impl TryFrom<u8> for NativeTool {
    type Error = u8;

    /// Decodes a stable native-tool discriminant, returning the unknown byte unchanged.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rustc),
            1 => Ok(Self::Clang),
            2 => Ok(Self::Python),
            3 => Ok(Self::TypeScriptCompiler),
            4 => Ok(Self::GoCompiler),
            5 => Ok(Self::JavaCompiler),
            6 => Ok(Self::CSharpCompiler),
            value => Err(value),
        }
    }
}

impl NativeTool {
    /// Canonical native-tool order used by compiler capability schedules and reports.
    pub const ALL: [Self; 7] = [
        Self::Rustc,
        Self::Clang,
        Self::Python,
        Self::TypeScriptCompiler,
        Self::GoCompiler,
        Self::JavaCompiler,
        Self::CSharpCompiler,
    ];
}

/// Maximum native diagnostic bytes retained by the portable compiler service boundary.
pub const MAX_NATIVE_DIAGNOSTIC_BYTES: usize = 256;

/// Exact caller-owned native work-directory phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkPhase {
    /// The directory was prepared and proved empty before native work.
    Prepare,
    /// The directory was cleaned and proved empty after native work.
    Cleanup,
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

/// Exact malformed-UTF-8 position retained without carrying a runtime error object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUtf8Fact {
    /// Valid source-byte prefix immediately before the malformed UTF-8 sequence.
    pub valid_up_to: usize,
    /// Exact malformed sequence length when known; `None` denotes incomplete trailing bytes.
    pub error_len: Option<usize>,
}

/// Closed worker identity retained when a scoped native I/O worker panics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorker {
    /// The worker sending exact source through native standard input.
    SourceWriter,
    /// The worker draining native standard output into the bounded diagnostic lease.
    StandardOutputReader,
    /// The worker draining native standard error into the bounded diagnostic lease.
    StandardErrorReader,
}

/// Closed class of payload produced by a scoped native I/O worker panic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkerPanicClass {
    /// A static string payload was retained.
    StaticMessage,
    /// An owned string payload was retained.
    OwnedMessage,
    /// The panic payload had no supported text representation.
    Opaque,
}

/// Fixed retained byte capacity for one native worker panic message.
pub const MAX_NATIVE_WORKER_PANIC_BYTES: usize = 96;

/// Bounded UTF-8 message fact preserved from a scoped native worker panic payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWorkerPanicMessage {
    /// Exact retained UTF-8 byte prefix.
    pub bytes: [u8; MAX_NATIVE_WORKER_PANIC_BYTES],
    /// Number of meaningful bytes in `bytes`.
    pub byte_len: usize,
    /// Whether the original panic text exceeded the retained prefix.
    pub truncated: bool,
}

/// Typed scoped-worker panic terminal retained instead of unwinding a compiler caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWorkerPanic {
    /// Exact native I/O worker whose join reported the panic.
    pub worker: NativeWorker,
    /// Closed panic-payload class.
    pub class: NativeWorkerPanicClass,
    /// Bounded exact message facts, empty only for an opaque payload.
    pub message: NativeWorkerPanicMessage,
}

/// Closed semantic terminal for syntax-native source whose declaration facts lack a compact recipe.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LoweringUnsupported {
    /// No declaration form has a compact semantic recipe in this compiler slice.
    #[error("no supported declaration form")]
    NoSupportedDeclaration,
    /// An extension fact bound an atom coordinate the admitted lane never
    /// carried; the bounded projection of the driver's exact terminal.
    #[error("extension atom {provisional} at row {row} was not bound in {atom_count} atoms")]
    ExtensionAtomUnbound {
        /// Zero-based emission row that named the unbound atom.
        row: u32,
        /// Provisional atom coordinate the fact named.
        provisional: u32,
        /// Atoms the admitted lane actually held.
        atom_count: u32,
    },
    /// Canonical admission rejected the fact at this ordinal; the bounded
    /// projection of the driver's exact terminal.
    #[error("fact {fact} was rejected by the canonical admission lane")]
    FactRejected {
        /// Zero-based ordinal the fact would have occupied.
        fact: u32,
    },
    /// Rust function signatures need a distinct semantic recipe and are not lowered as constants.
    #[error("Rust function recipe is not represented")]
    RustFunction,
    /// Rust constant type is outside the closed Bool/I32/String recipe set.
    #[error("Rust constant type is not represented")]
    RustConstantType,
    /// Python assignment has no nonempty identifier fact.
    #[error("Python assignment identifier is not represented")]
    PythonAssignmentName,
    /// Python assignment value is outside the closed Bool/I32/String recipe set.
    #[error("Python assignment value is not represented")]
    PythonAssignmentValue,
    /// Clang declaration is outside the closed `const char *` String recipe.
    #[error("Clang declaration form is not represented")]
    ClangDeclarationForm,
    /// TypeScript is outside the closed top-level `const` or return-typed `function` subset.
    #[error("TypeScript declaration form is not represented")]
    TypeScriptDeclarationForm,
    /// TypeScript declaration type is outside the closed `boolean`/`number`/`string` recipe.
    #[error("TypeScript declaration type is not represented")]
    TypeScriptDeclarationType,
    /// C# is outside the closed `const` or return-typed `static` method subset.
    #[error("C# declaration form is not represented")]
    CSharpDeclarationForm,
    /// C# declaration type is outside the closed `bool`/`int`/`string` recipe.
    #[error("C# declaration type is not represented")]
    CSharpDeclarationType,
    /// Go is outside the closed top-level `const` or return-typed `func` subset.
    #[error("Go declaration form is not represented")]
    GoDeclarationForm,
    /// Go declaration type is outside the closed `bool`/`int`/`string` recipe.
    #[error("Go declaration type is not represented")]
    GoDeclarationType,
    /// Java lacks the closed top-level type or class-member recipe required for compact IR.
    #[error("Java declaration form is not represented")]
    JavaDeclarationForm,
}

/// Copyable canonical recipe facts retained by compact compiler artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileRecipeFact {
    /// Central typed identity over language, stage, source fact, and resolved toolchain fact.
    pub identity: ContentId<CompileRecipeDomain>,
    /// Closed source-language profile bound into `identity`.
    pub profile: LanguageProfile,
    /// Closed semantic stage bound into `identity`.
    pub stage: Stage,
    /// Concrete tool family bound into `identity`.
    pub tool: NativeTool,
    /// Central resolved-toolchain identity bound into `identity`.
    pub toolchain: ContentId<ToolchainDomain>,
}

impl CompileRecipeFact {
    /// Derives the only canonical recipe identity from all semantic recipe authorities.
    #[must_use]
    pub fn derive(
        profile: LanguageProfile,
        stage: Stage,
        tool: NativeTool,
        source: ContentId<SourceFactDomain>,
        toolchain: ContentId<ToolchainDomain>,
    ) -> Self {
        let mut canonical = [0; 68];
        canonical[..2].copy_from_slice(&<[u8; 2]>::from(profile));
        canonical[2] = u8::from(stage);
        canonical[3] = u8::from(tool);
        canonical[4..36].copy_from_slice(source.as_ref());
        canonical[36..68].copy_from_slice(toolchain.as_ref());
        Self {
            identity: ContentId::<CompileRecipeDomain>::from_canonical_bytes(&canonical),
            profile,
            stage,
            tool,
            toolchain,
        }
    }
}

/// Registry rejection for a language and stage pair with no valid frontend route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendError {
    /// The language is known, but the requested semantic stage is not implemented for it.
    UnsupportedStage {
        /// Language whose registry row was requested.
        language: Language,
        /// Unsupported stage requested for that language.
        stage: Stage,
    },
}
