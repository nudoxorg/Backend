//! Closed registry, language, toolchain, and lowering vocabulary.

use core::{fmt, str::FromStr};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::clang::ClangProjectionFault;
use super::csharp::CSharpProjectionFault;
use super::go::GoProjectionFault;
use super::java::JavaProjectionFault;
use super::projection::ProjectionAdmissionFault;
use super::python::PythonProjectionFault;
use super::typescript::TypeScriptProjectionFault;

/// Closed registry namespace shared by package contracts, acquisition, and compilers.
///
/// The variant names and canonical spellings follow package-url ecosystem
/// tokens. Configuration aliases are admitted only while parsing an outer
/// process boundary; persisted identities always use [`Self::as_str`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryEcosystem {
    /// crates.io compatible Cargo packages.
    Cargo = 1,
    /// npm packages.
    Npm = 2,
    /// Python Package Index packages.
    Pypi = 3,
    /// Maven packages.
    Maven = 4,
    /// NuGet packages.
    Nuget = 5,
    /// Go modules.
    Golang = 6,
    /// C and C++ packages.
    Cpp = 7,
}

impl RegistryEcosystem {
    /// Parses a canonical persisted package-url ecosystem token.
    pub fn parse_canonical(value: &str) -> Result<Self, UnknownRegistryEcosystem> {
        match value {
            "cargo" => Ok(Self::Cargo),
            "npm" => Ok(Self::Npm),
            "pypi" => Ok(Self::Pypi),
            "maven" => Ok(Self::Maven),
            "nuget" => Ok(Self::Nuget),
            "golang" => Ok(Self::Golang),
            "cpp" => Ok(Self::Cpp),
            _ => Err(UnknownRegistryEcosystem),
        }
    }

    /// Parses canonical tokens and human-facing configuration aliases.
    pub fn parse_alias(value: &str) -> Result<Self, UnknownRegistryEcosystem> {
        let normalized = value.trim();
        if normalized.eq_ignore_ascii_case("cargo")
            || normalized.eq_ignore_ascii_case("crates")
            || normalized.eq_ignore_ascii_case("crates.io")
        {
            return Ok(Self::Cargo);
        }
        if normalized.eq_ignore_ascii_case("npm") {
            return Ok(Self::Npm);
        }
        if normalized.eq_ignore_ascii_case("python") || normalized.eq_ignore_ascii_case("pypi") {
            return Ok(Self::Pypi);
        }
        if normalized.eq_ignore_ascii_case("maven") {
            return Ok(Self::Maven);
        }
        if normalized.eq_ignore_ascii_case("nuget") || normalized.eq_ignore_ascii_case("dotnet") {
            return Ok(Self::Nuget);
        }
        if normalized.eq_ignore_ascii_case("go") || normalized.eq_ignore_ascii_case("golang") {
            return Ok(Self::Golang);
        }
        if normalized.eq_ignore_ascii_case("cpp")
            || normalized.eq_ignore_ascii_case("c++")
            || normalized.eq_ignore_ascii_case("conan")
        {
            return Ok(Self::Cpp);
        }
        Err(UnknownRegistryEcosystem)
    }

    /// Returns the one canonical package-url token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Pypi => "pypi",
            Self::Maven => "maven",
            Self::Nuget => "nuget",
            Self::Golang => "golang",
            Self::Cpp => "cpp",
        }
    }
}

impl TryFrom<u8> for RegistryEcosystem {
    type Error = UnknownRegistryEcosystem;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Cargo),
            2 => Ok(Self::Npm),
            3 => Ok(Self::Pypi),
            4 => Ok(Self::Maven),
            5 => Ok(Self::Nuget),
            6 => Ok(Self::Golang),
            7 => Ok(Self::Cpp),
            _ => Err(UnknownRegistryEcosystem),
        }
    }
}

impl From<RegistryEcosystem> for u8 {
    fn from(value: RegistryEcosystem) -> Self {
        value as Self
    }
}

impl fmt::Display for RegistryEcosystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RegistryEcosystem {
    type Err = UnknownRegistryEcosystem;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_alias(value)
    }
}

/// An unknown registry ecosystem token or durable discriminant.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("unknown registry ecosystem")]
pub struct UnknownRegistryEcosystem;

/// Source-language family understood by the compiler plane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

    /// Returns the one native compiler/tool family authorized for this
    /// language family. Profile-specific standards remain part of
    /// [`LanguageProfile`]; this mapping prevents a capability from claiming
    /// one language while executing another language's tool.
    #[must_use]
    pub const fn native_tool(self) -> NativeTool {
        match self {
            Self::Rust => NativeTool::Rustc,
            Self::TypeScript => NativeTool::TypeScriptCompiler,
            Self::Python => NativeTool::Python,
            Self::Go => NativeTool::GoCompiler,
            Self::Java => NativeTool::JavaCompiler,
            Self::CSharp => NativeTool::CSharpCompiler,
            Self::Clang => NativeTool::Clang,
        }
    }

    /// Returns the canonical product and wire spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Clang => "clang",
        }
    }

    /// Returns the stable legacy source-record tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Rust => 1,
            Self::Python => 2,
            Self::TypeScript => 3,
            Self::Go => 4,
            Self::Java => 5,
            Self::CSharp => 6,
            Self::Clang => 7,
        }
    }

    /// Admits a stable legacy source-record tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Rust),
            2 => Some(Self::Python),
            3 => Some(Self::TypeScript),
            4 => Some(Self::Go),
            5 => Some(Self::Java),
            6 => Some(Self::CSharp),
            7 => Some(Self::Clang),
            _ => None,
        }
    }
}

/// Semantic operation supplied by one language authority.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum LanguageOracleTask {
    /// Syntax extraction.
    Parse = 0,
    /// Type and binding analysis.
    TypeCheck = 1,
    /// Complete semantic indexing.
    SemanticIndex = 2,
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
/// The concrete frontend error stays in the `backend-engine` driver; this compact class
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
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

impl NativeWorkerPanic {
    /// Captures the standard library's opaque join payload without allocating
    /// or discarding its supported message class. The retained prefix always
    /// ends at a UTF-8 boundary; `truncated` distinguishes it from the exact
    /// complete message.
    #[must_use]
    pub fn capture(worker: NativeWorker, payload: &(dyn core::any::Any + Send)) -> Self {
        let (class, message) = Self::capture_payload(payload);
        Self {
            worker,
            class,
            message,
        }
    }

    /// Captures only the bounded payload facts for a typed non-native worker owner.
    #[must_use]
    pub fn capture_payload(
        payload: &(dyn core::any::Any + Send),
    ) -> (NativeWorkerPanicClass, NativeWorkerPanicMessage) {
        let (class, message) = if let Some(message) = payload.downcast_ref::<&'static str>() {
            (NativeWorkerPanicClass::StaticMessage, *message)
        } else if let Some(message) = payload.downcast_ref::<String>() {
            (NativeWorkerPanicClass::OwnedMessage, message.as_str())
        } else {
            (NativeWorkerPanicClass::Opaque, "")
        };
        let mut retained = message.len().min(MAX_NATIVE_WORKER_PANIC_BYTES);
        while !message.is_char_boundary(retained) {
            retained -= 1;
        }
        let mut bytes = [0_u8; MAX_NATIVE_WORKER_PANIC_BYTES];
        bytes[..retained].copy_from_slice(&message.as_bytes()[..retained]);
        (
            class,
            NativeWorkerPanicMessage {
                bytes,
                byte_len: retained,
                truncated: message.len() > retained,
            },
        )
    }
}

impl core::fmt::Display for NativeWorkerPanic {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "native worker {:?} panicked with {:?} payload",
            self.worker, self.class
        )?;
        if let Some(bytes) = self.message.bytes.get(..self.message.byte_len)
            && let Ok(message) = core::str::from_utf8(bytes)
            && !message.is_empty()
        {
            write!(formatter, ": {message}")?;
        }
        if self.message.truncated {
            formatter.write_str(" (truncated)")?;
        }
        Ok(())
    }
}

impl core::error::Error for NativeWorkerPanic {}

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
    /// An extension row named a type-parameter interval the admitted lane
    /// never carried.
    #[error(
        "extension type-parameter range {start}..+{length} at row {row} was not bound in {element_count} elements"
    )]
    ExtensionTypeParametersUnbound {
        /// Zero-based extension row.
        row: u32,
        /// Provisional first type-parameter element.
        start: u32,
        /// Provisional element count.
        length: u32,
        /// Elements the admitted type-parameter lane actually held.
        element_count: u32,
    },
    /// Canonical admission rejected one exact fact.  This is the portable
    /// projection of the driver's complete rejection snapshot.
    #[error(
        "fact {fact} with name length {name_len} was rejected by the canonical admission lane: {cause:?}"
    )]
    FactRejected {
        /// Zero-based ordinal the fact would have occupied.
        fact: u64,
        /// Exact byte length of the rejected declaration name.
        name_len: u64,
        /// Closed admission cause and all of its source operands.
        cause: ProjectionAdmissionFault,
    },
    /// Rust function signatures need a distinct semantic recipe and are not lowered as constants.
    #[error("Rust function recipe is not represented")]
    RustFunction,
    /// Rust constant type is outside the closed Bool/I32/String recipe set.
    #[error("Rust constant type is not represented")]
    RustConstantType,
    /// Rust generic syntax needs a lossless declaration-scope transaction
    /// (including merged `where` predicates and forward local bounds) that
    /// this lowering pass has not yet entered. It is never folded into a
    /// name-only parameter or an external nominal.
    #[error("Rust generic parameter recipe is not represented")]
    RustGenericParameter,
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
    /// A Java image projection failed with its closed, source-bound fault.
    #[error("Java projection {fault}")]
    JavaProjection {
        /// Variant-specific coordinates are retained exactly where the image
        /// producer exposes them. Other variants retain their closed class;
        /// the enclosing compile terminal owns the source identity.
        fault: JavaProjectionFault,
    },
    /// A Go authority projection failed after retaining its compact context.
    #[error("Go projection {fault}")]
    GoProjection {
        /// Closed fault class plus exact bounded operands.
        fault: GoProjectionFault,
    },
    /// A TypeScript authority projection failed after retaining its compact context.
    #[error("TypeScript projection {fault}")]
    TypeScriptProjection {
        /// Closed fault class plus exact bounded operands.
        fault: TypeScriptProjectionFault,
    },
    /// A Python authority projection failed after retaining its compact context.
    #[error("Python projection {fault}")]
    PythonProjection {
        /// Closed fault class plus exact bounded operands.
        fault: PythonProjectionFault,
    },
    /// A C# authority projection failed after retaining its compact context.
    #[error("C# projection {fault}")]
    CSharpProjection {
        /// Closed fault class plus exact bounded operands.
        fault: CSharpProjectionFault,
    },
    /// A Clang authority projection failed after retaining its compact context.
    #[error("Clang projection {fault}")]
    ClangProjection {
        /// Closed fault class plus exact bounded operands.
        fault: ClangProjectionFault,
    },
}
const _: () = assert!(core::mem::size_of::<LoweringUnsupported>() <= 96);
