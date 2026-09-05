//! Closed compiler facts shared by registries, native drivers, publication, and interfaces.
//! This crate describes requests and failures but deliberately performs no compilation or I/O.
//! Stable numeric conversions belong here because those values participate in canonical identities.
#![no_std]
extern crate alloc;

use alloc::string::String;
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

/// One closed Java type kind retained when an image row violates its
/// type-specific grammar. This vocabulary mirrors the authority's public
/// closed tags without importing the Java image crate into this portable
/// terminal crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionTypeKind {
    /// Java primitive type row.
    Primitive,
    /// Java `void` type row.
    Void,
    /// Declared Java class, interface, enum, annotation, or record row.
    Declared,
    /// Java array type row.
    Array,
    /// Java declared type-variable row.
    Variable,
    /// Java wildcard row.
    Wildcard,
    /// Java intersection row.
    Intersection,
    /// Java multi-catch union row.
    Union,
    /// Javac error type row.
    Error,
    /// Javac no-type sentinel row.
    None,
    /// Java null type row.
    Null,
}

/// One exact foreign-key grammar rejection projected from a Java reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaForeignKeyFault {
    /// The authority's canonical foreign path was empty.
    EmptyPath,
    /// The authority's canonical foreign path contained a backslash.
    BackslashInPath,
}

/// One atom role in a resolved Java executable-symbol row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaSymbolAtom {
    /// The symbol's declaring owner atom.
    Owner,
    /// The symbol's member-name atom.
    Name,
}

/// Closed staging operation whose coordinate could not fit Java's compact
/// projection lane. This names the failed operation without inventing a raw
/// coordinate where the source operation did not expose one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionIndexPhase {
    /// A canonical fact ordinal could not fit the compact coordinate width.
    FactOrdinal,
    /// A recursive type-row coordinate could not fit.
    TypeRow,
    /// A recursive type-child coordinate could not fit.
    TypeChild,
    /// A qualified declaration-name index was full.
    NameIndex,
    /// An executable-symbol index was full.
    SymbolIndex,
    /// An overload-executable index was full.
    ExecutableIndex,
    /// A callable signature carrier coordinate could not fit.
    Signature,
    /// A documentation projection coordinate could not fit.
    Documentation,
    /// A UTF-16-to-byte conversion could not fit the source coordinate lane.
    Utf16,
}

/// One fixed Java authority-image plane in canonical directory order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImagePlane {
    /// Fixed-width atom offset and length rows.
    Atoms,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes,
    /// Recursive type rows.
    Types,
    /// Type-child coordinate rows.
    TypeChildren,
    /// Executable symbol rows.
    Symbols,
    /// Symbol-parameter coordinate rows.
    SymbolParameters,
    /// Declaration rows.
    Declarations,
    /// Resolved call-reference rows.
    References,
    /// Per-declaration extension ranges.
    DeclarationExtensions,
    /// Extension payload rows.
    ExtensionEntries,
}

/// Exact Java authority-image header rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageHeaderFault {
    /// The fixed header was truncated.
    Truncated { actual: u32 },
    /// The fixed magic bytes differed.
    Magic { found: [u8; 4] },
    /// The image version was unsupported.
    Version { found: u16 },
    /// The encoded fixed-header length differed.
    Length { found: u16 },
    /// The encoded Java release was unsupported.
    Release { found: u16 },
    /// The directory count differed from the grammar.
    SectionCount { found: u16 },
    /// Declared and supplied body lengths differed.
    BodyLength { declared: u32, actual: u32 },
}

/// Exact Java authority-image directory rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageSectionFault {
    /// A directory tag differed at its canonical plane position.
    Tag { expected: u16, found: u16 },
    /// A fixed row width differed.
    RowBytes { expected: u32, found: u32 },
    /// Count, row width, and encoded aggregate bytes disagreed.
    ByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    /// A plane offset was not contiguous.
    Offset { expected: u32, found: u32 },
    /// A declared plane range escaped the source image.
    Range {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

/// Exact Java atom-table rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageAtomFault {
    /// An atom began at a noncanonical byte offset.
    NonCanonicalOffset { found: u32 },
    /// An atom range escaped the atom-byte plane.
    Range,
    /// Atom bytes violated their UTF-8 promise.
    Utf8,
    /// Atom-byte data remained after the final atom.
    TrailingBytes,
}

/// Exact portable Java authority-image rejection.
///
/// All source image offsets are checked into the explicit `u32` image
/// coordinate width by the projection boundary. A native `usize` outside
/// that width instead produces the projection's separate `IndexCapacity`
/// terminal; it is never truncated or replaced by a sentinel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageFault {
    /// Header grammar rejected one exact nested fact.
    Header { cause: JavaImageHeaderFault },
    /// A canonical directory plane rejected one exact nested fact.
    Section {
        plane: JavaImagePlane,
        cause: JavaImageSectionFault,
    },
    /// The image checksum differed.
    Digest,
    /// An atom row rejected one exact nested fact.
    Atom {
        index: u32,
        cause: JavaImageAtomFault,
    },
    /// A required atom coordinate used the absent grammar value.
    AbsentAtom,
    /// A coordinate escaped its exact target plane.
    Coordinate {
        plane: JavaImagePlane,
        index: u32,
        upper_bound: u32,
    },
    /// A fixed-width row used an unknown closed tag.
    Tag { plane: JavaImagePlane, found: u8 },
    /// A child range overflowed or escaped its target plane.
    ChildRange {
        plane: JavaImagePlane,
        start: u32,
        count: u32,
        upper_bound: u32,
    },
    /// A reference range was inverted in Java UTF-16 coordinates.
    ReferenceRange { start: u32, end: u32 },
    /// Documentation flavor and atom presence disagreed.
    DocumentationPresence,
    /// A declaration modifier bitset carried unrecognized bits.
    ModifierBits { found: u32 },
    /// A record-component extension targeted a non-field declaration row.
    RecordComponentKind { index: u32 },
    /// Extension reserved bytes were nonzero.
    ExtensionReserved,
}

/// Closed Java authority projection terminal.
///
/// Each variant carries precisely the coordinates the projection had at its
/// failure site. Absence is encoded by choosing a variant with no coordinate,
/// never with an optional-coordinate bag or a sentinel value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionFault {
    /// A previously validated Java image row could not be reread.
    Image { cause: JavaImageFault },
    /// The producer-bounded recursive type graph exceeded its depth limit at
    /// this exact authority type row.
    Depth { type_row: u32 },
    /// A row of the named closed Java type kind lacked a required child.
    MalformedType {
        type_row: u32,
        kind: JavaProjectionTypeKind,
    },
    /// A primitive spelling in this exact authority type row lay outside the
    /// shared primitive vocabulary.
    Primitive { type_row: u32 },
    /// One exact atom in a resolved executable-symbol row failed its UTF-8
    /// promise.
    AtomUtf8 { symbol: u32, atom: JavaSymbolAtom },
    /// The compile source itself was not UTF-8 for Javac coordinate mapping.
    SourceUtf8,
    /// One Javac UTF-16 unit offset could not map into the source.
    Utf16Offset {
        /// Requested UTF-16 unit coordinate.
        units: u32,
        /// Exact UTF-16 length of the bound source.
        source_utf16_len: u32,
    },
    /// A Javac UTF-16 range was not an ordered relative byte span.
    Utf16Range {
        /// Reported source start coordinate.
        start: u32,
        /// Reported source end coordinate.
        end: u32,
    },
    /// A reference owner named no pushed executable.
    OrphanOwner { owner: u32 },
    /// A resolved external reference failed canonical key validation.
    ForeignKey { cause: JavaForeignKeyFault },
    /// The authority's sibling list for this executable symbol exceeded its
    /// bounded pooled width.
    SiblingCapacity { symbol: u32 },
    /// A checked projection coordinate could not fit the named operation.
    IndexCapacity { phase: JavaProjectionIndexPhase },
}

/// Exact foreign-key grammar rejection projected by Go, TypeScript, or
/// Python. The shared shape is closed and has no coordinate bag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionForeignKeyFault {
    /// The canonical path was empty.
    EmptyPath,
    /// The canonical path contained a backslash.
    BackslashInPath,
}

/// Exact rejected portion of one package lineage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionLineagePart {
    /// The ecosystem component.
    Ecosystem,
    /// The package-name component.
    Package,
    /// An explicitly identified non-canonical component segment.
    Invalid { segment: u8 },
}

/// Exact package-lineage grammar rejection projected by Go, TypeScript, or
/// Python.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionPackageLineageFault {
    /// The ecosystem component was empty.
    EmptyEcosystem,
    /// The package-name component was empty.
    EmptyPackage,
    /// The ecosystem contained the closed render separator.
    SeparatorInEcosystem,
    /// The package name contained the closed render separator.
    SeparatorInPackage,
    /// The named component contained a path separator.
    Backslash { part: ProjectionLineagePart },
}

/// Closed Go authority projection terminal.
///
/// The image reader deliberately exposes a fairly large closed error
/// vocabulary.  Keep that vocabulary here as value-only records: the
/// authority image is borrowed by the driver, while this crate is also used
/// by the application and interface terminals.  In particular, none of the
/// variants carries a borrowed plane name or a native-language enum.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImagePlane {
    /// Package/module declaration rows.
    Declaration,
    /// Recursive type rows.
    Type,
    /// Pooled type-child rows.
    TypeChild,
    /// Method rows.
    Method,
    /// Generic type-parameter rows.
    TypeParameter,
    /// Struct/interface member rows.
    Member,
    /// Documentation rows.
    Doc,
    /// Resolved reference rows.
    Reference,
    /// Resolved reference target atoms.
    ReferenceTarget,
    /// Build-constraint rows.
    BuildConstraint,
    /// Interface-satisfaction rows.
    Satisfaction,
    /// Interface-satisfaction target atoms.
    SatisfactionTarget,
    /// Package metadata rows.
    Package,
    /// Signature parameter/result rows.
    SignatureParameter,
    /// Interface method-set rows.
    MethodSet,
    /// The optional module metadata row.
    Module,
}

/// Closed Go flag cell whose raw value was rejected by the image reader.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageFlagCell {
    /// Method/member exported flag.
    Exported,
    /// Method pointer-receiver flag.
    PointerReceiver,
    /// Method promoted flag.
    Promoted,
    /// Member embedded flag.
    Embedded,
}

/// Portable copy of the Go declaration-kind cell.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageDeclarationKind {
    /// Named type declaration.
    Type,
    /// Type alias declaration.
    Alias,
    /// Function declaration.
    Function,
    /// Constant declaration.
    Constant,
    /// Variable declaration.
    Static,
}

/// Portable copy of the Go recursive type-row kind.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageTypeKind {
    /// Basic type row.
    Basic,
    /// Named type reference row.
    Named,
    /// Alias reference row.
    Alias,
    /// Type-parameter use row.
    TypeParameter,
    /// Pointer row.
    Pointer,
    /// Slice row.
    Slice,
    /// Array row.
    Array,
    /// Map row.
    Map,
    /// Channel row.
    Channel,
    /// Function row.
    Function,
    /// Struct row.
    Struct,
    /// Interface row.
    Interface,
    /// Constraint-union row.
    Union,
    /// Tuple row.
    Tuple,
    /// Honest invalid/unknown row.
    Invalid,
}

/// Portable copy of the Go member-kind cell.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageMemberKind {
    /// Struct field.
    Field,
    /// Interface method.
    Method,
}

/// Portable copy of the Go documentation owner-kind cell.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageDocOwnerKind {
    /// Package declaration owner.
    Declaration,
    /// Method owner.
    Method,
    /// Member owner.
    Member,
    /// Package documentation owner.
    Package,
}

/// Exact fixed-header rejection from a Go authority image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageHeaderFault {
    /// Header bytes ended before the fixed envelope.
    Truncated { actual: u32 },
    /// Raw image magic.
    Magic { found: [u8; 4] },
    /// Raw image version.
    Version { found: u16 },
    /// Raw fixed-header length.
    Length { found: u32 },
    /// Declared and observed body lengths.
    BodyLength { declared: u32, actual: u32 },
    /// Non-zero reserved header bytes.
    Reserved,
    /// Number of module rows observed.
    ModuleCount { found: u32 },
}

/// Exact closed rejection from every public Go authority-image accessor.
///
/// Every usize operand is converted by the driver with a checked conversion
/// before it reaches this type.  A conversion failure is itself represented
/// by the projection index-capacity phase, never by a fabricated sentinel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageFault {
    /// Fixed-header grammar rejection.
    Header { cause: GoImageHeaderFault },
    /// Image checksum rejection.
    Digest,
    /// An accessor selected a row outside its plane.
    RowBounds {
        plane: GoImagePlane,
        index: u32,
        count: u32,
    },
    /// Unknown declaration kind tag.
    DeclarationKind { index: u32, found: u8 },
    /// Invalid declaration exported flag.
    ExportedFlag { index: u32, found: u8 },
    /// Invalid declaration iota flag.
    DeclarationIota { index: u32, found: u8 },
    /// Declaration reserved byte/bit rejection.
    DeclarationReserved { index: u32 },
    /// Type-row reserved byte/bit rejection.
    TypeReserved { index: u32 },
    /// Method-row reserved byte/bit rejection.
    MethodReserved { index: u32 },
    /// Member-row reserved byte/bit rejection.
    MemberReserved { index: u32 },
    /// Documentation-row reserved byte/bit rejection.
    DocReserved { index: u32 },
    /// Reference-row reserved byte/bit rejection.
    ReferenceReserved { index: u32 },
    /// Declaration type root outside the type plane.
    DeclarationTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    /// Declaration/method source span rejection.
    DeclarationSpan { index: u32, start: u32, end: u32 },
    /// Unknown type-row kind tag.
    TypeKind { index: u32, found: u8 },
    /// Required type-row name was empty.
    TypeNameRequired { index: u32, kind: GoImageTypeKind },
    /// Forbidden type-row name was present.
    TypeNameForbidden { index: u32, kind: GoImageTypeKind },
    /// Unknown channel direction tag.
    TypeDirection { index: u32, found: u8 },
    /// Channel direction present on a non-channel row.
    TypeDirectionCell { index: u32, kind: GoImageTypeKind },
    /// Unknown variadic flag tag.
    TypeVariadicFlag { index: u32, found: u8 },
    /// Variadic flag present on a non-function row.
    TypeVariadicCell { index: u32, kind: GoImageTypeKind },
    /// Negative array length.
    ArrayLength { index: u32, length: i64 },
    /// Function parameter count mismatch.
    TypeParamCount {
        index: u32,
        kind: GoImageTypeKind,
        param_count: u32,
        child_count: u32,
    },
    /// Type-child range rejection.
    TypeChildRange {
        index: u32,
        start: u32,
        count: u32,
        child_count: u32,
    },
    /// Type child target outside the type plane.
    TypeChildTarget {
        index: u32,
        target: u32,
        type_count: u32,
    },
    /// Undefined type-child flags.
    TypeChildFlags { index: u32, flags: u32 },
    /// Child-plane tiling mismatch.
    TypeChildTiling { declared: u32, plane: u32 },
    /// Type-member range rejection.
    TypeMemberRange {
        index: u32,
        start: u32,
        count: u32,
        member_count: u32,
    },
    /// Method owner outside declaration plane.
    MethodOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    /// Method flag cell rejection.
    MethodFlag {
        index: u32,
        cell: GoImageFlagCell,
        found: u8,
    },
    /// Method type root outside type plane.
    MethodTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    /// Method receiver parameter blob mismatch.
    MethodReceiverParams {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    /// Method ordering rejection.
    MethodSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    /// Type-parameter owner outside declaration plane.
    TypeParameterOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    /// Type-parameter constraint root outside type plane.
    TypeParameterConstraint {
        index: u32,
        root: u32,
        type_count: u32,
    },
    /// Type-parameter ordering rejection.
    TypeParameterSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    /// Unknown member kind tag.
    MemberKind { index: u32, found: u8 },
    /// Member flag cell rejection.
    MemberFlag {
        index: u32,
        cell: GoImageFlagCell,
        found: u8,
    },
    /// Embedded bit present on a non-field.
    MemberEmbedded { index: u32 },
    /// Member owner outside type plane.
    MemberOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    /// Member type root outside type plane.
    MemberTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    /// Member ordering rejection.
    MemberSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    /// Member-owner tiling rejection.
    MemberOwnerRange {
        owner: u32,
        start: u32,
        count: u32,
        actual_start: u32,
        actual_count: u32,
    },
    /// Unknown documentation owner-kind tag.
    DocOwnerKind { index: u32, found: u8 },
    /// Documentation owner outside its plane.
    DocOwner { index: u32, owner: u32, bound: u32 },
    /// Empty documentation text.
    EmptyDoc { index: u32 },
    /// Documentation ordering rejection.
    DocSort {
        index: u32,
        owner_kind: u8,
        owner: u32,
        previous_kind: u8,
        previous_owner: u32,
    },
    /// Reference owner outside declaration plane.
    ReferenceOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    /// Reference call span rejection.
    ReferenceSpan { index: u32, start: u32, end: u32 },
    /// Unresolved method owner spelling lengths.
    ReferenceOwnerUnresolved {
        index: u32,
        owner_bytes: u32,
        function_bytes: u32,
    },
    /// Reference owner had no usable span.
    ReferenceOwnerSpan { index: u32 },
    /// Reference file mismatch.
    ReferenceFile { index: u32 },
    /// Reference containment rejection.
    ReferenceContainment {
        index: u32,
        start: u32,
        end: u32,
        owner_start: u32,
        owner_end: u32,
    },
    /// Reference ordering rejection.
    ReferenceSort { index: u32 },
    /// Empty build constraint.
    EmptyConstraint { index: u32 },
    /// Build-constraint blob mismatch.
    ConstraintBlob {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    /// Build-constraint ordering rejection.
    ConstraintSort { index: u32 },
    /// Satisfaction subject outside declaration plane.
    SatisfactionSubject {
        index: u32,
        subject: u32,
        declaration_count: u32,
    },
    /// Satisfaction ordering rejection.
    SatisfactionSort {
        index: u32,
        subject: u32,
        previous: u32,
    },
    /// Satisfaction subject had the wrong declaration kind.
    SatisfactionSubjectKind {
        index: u32,
        kind: GoImageDeclarationKind,
    },
    /// Module path cell was empty.
    ModulePath,
    /// Package file blob mismatch.
    PackageFiles {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    /// Package ordering rejection.
    PackageSort { index: u32 },
    /// Declaration package outside package plane.
    DeclarationPackage { index: u32, package_count: u32 },
    /// Signature parameter position half-present.
    SignatureParameterPosition { index: u32 },
    /// Method-set owner outside type plane.
    MethodSetOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    /// Method-set owner had the wrong type kind.
    MethodSetOwnerKind {
        index: u32,
        kind: GoImageTypeKind,
    },
    /// Method-set type root outside type plane.
    MethodSetTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    /// Method-set ordering rejection.
    MethodSetSort { index: u32 },
    /// Signature parameter owner outside type plane.
    SignatureParameterOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    /// Signature parameter owner disagreed with its containing row.
    SignatureParameterOwnerRow {
        index: u32,
        owner: u32,
        expected: u32,
    },
    /// Signature parameter ordinal disagreed with its run position.
    SignatureParameterOrdinal {
        index: u32,
        ordinal: u32,
        expected: u32,
    },
    /// Signature parameter plane tiling rejection.
    SignatureParameterTiling { declared: u32, plane: u32 },
    /// Required name was empty.
    EmptyName { plane: GoImagePlane, index: u32 },
    /// Atom range escaped the byte plane.
    AtomRange {
        plane: GoImagePlane,
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    /// Atom bytes were not UTF-8.
    AtomUtf8 { plane: GoImagePlane, index: u32 },
}

/// Closed operation phase for a Go coordinate conversion.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoProjectionIndexPhase {
    /// Image-header coordinate.
    ImageHeader,
    /// Image-plane row coordinate.
    ImageRow,
    /// Fact ordinal.
    FactOrdinal,
    /// Type row coordinate.
    TypeRow,
    /// Type-child row coordinate.
    TypeChild,
    /// Declaration row coordinate.
    Declaration,
    /// Method row coordinate.
    Method,
    /// Type-parameter row coordinate.
    TypeParameter,
    /// Member row coordinate.
    Member,
    /// Documentation row coordinate.
    Documentation,
    /// Reference row coordinate.
    Reference,
    /// Constraint row coordinate.
    Constraint,
    /// Satisfaction row coordinate.
    Satisfaction,
    /// Package row coordinate.
    Package,
    /// Signature-parameter row coordinate.
    SignatureParameter,
    /// Method-set row coordinate.
    MethodSet,
    /// Atom coordinate.
    Atom,
    /// Entity-list coordinate.
    EntityList,
}

/// Closed operation phase for a Go pooled-list capacity rejection.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoProjectionListPhase {
    /// Entity list.
    Entity,
    /// Type list.
    Type,
    /// Atom list.
    Atom,
    /// Type-parameter list.
    TypeParameter,
}

/// Closed Go authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoProjectionFault {
    /// A validated Go authority image row could not be reread.
    Image { cause: GoImageFault },
    /// A projection index could not fit the compact lane.
    IndexCapacity {
        phase: GoProjectionIndexPhase,
        observed: u64,
    },
    /// The producer-bounded recursive type graph exceeded its depth budget.
    Depth { type_row: u32 },
    /// A variadic signature had no final typed parameter.
    VariadicWithoutParameter { signature: u32 },
    /// No pushed fact could own an anonymous compound row.
    Anchor { owner: u32 },
    /// A field or method list exceeded its bounded pool.
    ListCapacity {
        owner: u32,
        phase: GoProjectionListPhase,
    },
    /// A same-package reference named no declared target.
    OrphanTarget { reference: u32 },
    /// A foreign target key failed grammar validation.
    ForeignKey {
        reference: u32,
        cause: ProjectionForeignKeyFault,
    },
    /// A package lineage failed grammar validation.
    PackageLineage {
        reference: u32,
        cause: ProjectionPackageLineageFault,
    },
    /// An authority atom violated its UTF-8 promise.
    AtomUtf8 { plane: GoImagePlane, row: u32 },
    /// A relative occurrence span was inverted.
    RelativeSpan { row: u32, start: u32, end: u32 },
    /// A member, doc, or occurrence owner had no pushed fact.
    OrphanOwner { owner: u32 },
}

/// Closed TypeScript authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeScriptProjectionFault {
    /// A foreign target key failed grammar validation.
    ForeignKey { cause: ProjectionForeignKeyFault },
    /// An import-module package lineage failed grammar validation.
    PackageLineage {
        cause: ProjectionPackageLineageFault,
    },
    /// A host-size coordinate could not fit the wire's `u32` coordinate.
    CoordinateOverflow { value: u64 },
    /// An import binding named no retained module row.
    MissingImportBinding { fact: u32 },
}

/// Closed Python authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PythonProjectionFault {
    /// An authority spelling that must become a foreign key was not UTF-8.
    ForeignSpellingUtf8,
    /// A foreign target key failed grammar validation.
    ForeignKey { cause: ProjectionForeignKeyFault },
    /// A foreign package lineage failed grammar validation.
    PackageLineage {
        cause: ProjectionPackageLineageFault,
    },
}

/// Closed C# authority projection terminal portable across the application
/// and interface boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpProjectionFault {
    /// A validated C# authority image row could not be reread.
    Image { cause: CSharpImageFault },
    /// The recursive authority type graph exceeded its projection budget.
    Depth { type_row: u32 },
    /// A declaration name span escaped the entered source.
    NameSpan { start: u32, end: u32 },
    /// A reference began before its owning declaration.
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    /// A foreign occurrence key could not be built from authority facts.
    Foreign { reference: u32 },
    /// The bounded attribute lane exhausted its element capacity.
    AttributeCapacity { spellings: u32 },
    /// A projection index could not fit the compact lane.
    IndexCapacity {
        phase: CSharpProjectionIndexPhase,
        observed: u64,
    },
    /// A rectangular-array row repeated distinct element references.
    HeterogeneousArrayRank { first: u32, observed: u32 },
}

/// Closed operation phase for a C# projection coordinate conversion.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpProjectionIndexPhase {
    /// Image-header coordinate.
    ImageHeader,
    /// Fact ordinal.
    FactOrdinal,
    /// Declaration row.
    Declaration,
    /// Type row.
    TypeRow,
    /// Type-child row.
    TypeChild,
    /// Attribute row.
    Attribute,
    /// Documentation row.
    Documentation,
    /// Reference row.
    Reference,
    /// Name/atom coordinate.
    Name,
    /// Source-span coordinate.
    SourceSpan,
}

/// One C# authority-image section in fixed directory order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageSection {
    /// Atom offsets and lengths.
    Atoms,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes,
    /// Namespace, type, and member declarations.
    Declarations,
    /// Callable parameter rows.
    Parameters,
    /// Generic parameter rows.
    TypeParameters,
    /// Generic constraint rows.
    TypeConstraints,
    /// Recursive type rows.
    Types,
    /// Type-child rows.
    TypeChildren,
    /// Applied attribute rows.
    Attributes,
    /// XML documentation rows.
    Docs,
    /// Resolved reference rows.
    References,
}

/// Exact C# image header rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageHeaderFault {
    /// The fixed header was truncated.
    Truncated { actual: u32 },
    /// The fixed magic bytes differed.
    Magic { found: [u8; 4] },
    /// The image version was unsupported.
    Version { found: u16 },
    /// The fixed header length differed.
    Length { found: u32 },
    /// The directory count differed.
    SectionCount { found: u32 },
    /// Declared and supplied body lengths differed.
    BodyLength { declared: u32, actual: u32 },
    /// Reserved header bytes were nonzero.
    Reserved,
    /// A directory tag differed from canonical position.
    DirectoryTag { expected: u16, found: u16 },
    /// A directory row width differed from its fixed grammar.
    DirectoryRowBytes { expected: u32, found: u32 },
    /// Directory count, width, and aggregate byte count disagreed.
    DirectoryByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    /// A directory offset was not canonical.
    DirectoryOffset { expected: u32, found: u32 },
    /// A directory range escaped the image.
    DirectoryRange {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

/// One closed C# authority type-node kind for an image child-law rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageTypeKind {
    /// Named type use.
    Named,
    /// Array type.
    Array,
    /// Pointer type.
    Pointer,
    /// Nullable value type.
    NullableValue,
    /// Tuple type.
    Tuple,
    /// Function-pointer type.
    FunctionPointer,
    /// Type-parameter use.
    TypeParameter,
    /// Dynamic type.
    Dynamic,
    /// Unbound Roslyn error type.
    Error,
}

/// Exact portable C# authority-image rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageFault {
    /// Fixed header grammar rejected one nested fact.
    Header { cause: CSharpImageHeaderFault },
    /// The image checksum differed.
    Digest,
    /// A closed row tag was unknown in the named section.
    DeclarationKind {
        index: u32,
        found: u8,
        plane: CSharpImageSection,
    },
    /// A row carried reserved bytes or flags in the named section.
    DeclarationReserved {
        index: u32,
        plane: CSharpImageSection,
    },
    /// An atom range escaped the atom-byte plane.
    NameRange {
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    /// A referenced atom violated UTF-8.
    NameUtf8 { index: u32 },
    /// A source or section range was inverted or escaped its bound.
    Span { index: u32, start: u32, end: u32 },
    /// A type-node child run violated its closed cardinality law.
    TypeChildCount {
        index: u32,
        kind: CSharpImageTypeKind,
        min: u32,
        max: u32,
        actual: u32,
    },
}

/// Closed native type kind retained by Clang projection terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionTypeKind {
    /// Unknown or unmodeled native type.
    Unknown,
    /// Native builtin type.
    Builtin,
    /// Declaration-named native type.
    Named,
    /// Native pointer.
    Pointer,
    /// Objective-C block pointer.
    BlockPointer,
    /// C++ member pointer.
    MemberPointer,
    /// C++ lvalue reference.
    LvalueReference,
    /// C++ rvalue reference.
    RvalueReference,
    /// Native array.
    Array,
    /// Native function type.
    Function,
}

/// Exact native qualifier fact retained by Clang projection terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClangProjectionQualifiers {
    /// Native `const` fact.
    pub is_const: bool,
    /// Native `volatile` fact.
    pub is_volatile: bool,
    /// Native `restrict` fact.
    pub is_restrict: bool,
}

/// Closed native declaration-identity availability in a Clang terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionDeclaration {
    /// The authority supplied a compact native declaration identity.
    Known { identity: [u8; 16] },
    /// The authority supplied no declaration identity.
    Unavailable,
}

/// Closed Clang authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionFault {
    /// An authority source range escaped the entered source lease.
    Span { start: u32, end: u32 },
    /// A declaration requiring a name had no nonempty authority name span.
    Nameless { declaration: u32 },
    /// An anonymous authority type had no representable owning declaration.
    Anchor,
    /// A projection index could not fit the compact lane.
    IndexCapacity,
    /// A C++ override named a foreign native identity with no public key.
    ForeignOverride { identity: [u8; 16] },
    /// A reference named a foreign native identity with no public key.
    ForeignReference { identity: [u8; 16] },
    /// Qualifiers named a type form on which they are not semantically legal.
    IllegalQualifierTarget {
        type_id: u32,
        kind: ClangProjectionTypeKind,
        qualifiers: ClangProjectionQualifiers,
    },
    /// A C++ member pointer named a non-class owner type.
    IllegalMemberPointerOwner {
        pointer: u32,
        owner: u32,
        kind: ClangProjectionTypeKind,
        declaration: ClangProjectionDeclaration,
    },
}

impl core::fmt::Display for GoProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl core::fmt::Display for TypeScriptProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl core::fmt::Display for PythonProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl core::fmt::Display for CSharpProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl core::fmt::Display for ClangProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl core::fmt::Display for JavaProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Image { cause } => write!(formatter, "image {cause:?}"),
            Self::Depth { type_row } => write!(formatter, "depth at type row {type_row}"),
            Self::MalformedType { type_row, kind } => {
                write!(formatter, "malformed {kind:?} type row {type_row}")
            }
            Self::Primitive { type_row } => write!(formatter, "primitive type row {type_row}"),
            Self::AtomUtf8 { symbol, atom } => {
                write!(formatter, "symbol {symbol} {atom:?} atom UTF-8")
            }
            Self::SourceUtf8 => formatter.write_str("source UTF-8"),
            Self::Utf16Offset {
                units,
                source_utf16_len,
            } => write!(formatter, "UTF-16 offset {units} of {source_utf16_len}"),
            Self::Utf16Range { start, end } => write!(formatter, "UTF-16 range {start}..{end}"),
            Self::OrphanOwner { owner } => write!(formatter, "orphan owner {owner}"),
            Self::ForeignKey { cause } => write!(formatter, "foreign key {cause:?}"),
            Self::SiblingCapacity { symbol } => write!(formatter, "sibling capacity {symbol}"),
            Self::IndexCapacity { phase } => write!(formatter, "{phase:?} capacity"),
        }
    }
}

const _: () = assert!(core::mem::size_of::<GoProjectionFault>() <= 32);
const _: () = assert!(core::mem::size_of::<TypeScriptProjectionFault>() <= 16);
const _: () = assert!(core::mem::size_of::<PythonProjectionFault>() <= 8);
const _: () = assert!(core::mem::size_of::<CSharpProjectionFault>() <= 32);
const _: () = assert!(core::mem::size_of::<ClangProjectionFault>() <= 32);
const _: () = assert!(core::mem::size_of::<JavaProjectionFault>() <= 32);

const _: () = assert!(core::mem::size_of::<LoweringUnsupported>() <= 96);

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
