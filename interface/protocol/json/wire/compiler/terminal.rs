//! Defines json wire compiler terminal behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::time::Duration;

use compiler_vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, CSharpImageFault, CSharpImageHeaderFault,
    CSharpImageSection, CSharpImageTypeKind, CSharpProjectionFault, ClangProjectionDeclaration,
    ClangProjectionFault, ClangProjectionQualifiers, ClangProjectionTypeKind, FrontendError,
    GoProjectionFault, JavaForeignKeyFault, JavaImageAtomFault, JavaImageFault,
    JavaImageHeaderFault, JavaImagePlane, JavaImageSectionFault, JavaProjectionFault,
    JavaProjectionIndexPhase, JavaProjectionTypeKind, JavaSymbolAtom, Language,
    LoweringUnsupported, NativeTool, ProjectionForeignKeyFault, ProjectionLineagePart,
    ProjectionPackageLineageFault, PythonProjectionFault, Stage, TypeScriptProjectionFault,
};
use interface_core::{
    CompilerAttempt, CompilerCause, CompilerDiagnostic, CompilerRuntimeCause, FragmentCause,
    PackageCompilePhase, PackageSourceCause, PublicationCause, SourceAuthority,
};
use serde::{Serialize, Serializer, ser::SerializeStruct};

use super::super::scalar::{
    AuthorityDiagnosticClassWire, AuthorityPhaseWire, LanguageWire, NativeToolWire, StageWire,
    serialize_content,
};
use super::authority::{CompilerAttemptWire, SourceAuthorityWire};
use super::native::{NativeIoFactRef, NativeIoPhaseRef, NativeWorkCauseWire};
use super::package::{CompilerRuntimeCauseWire, PackageCompilePhaseWire, PackageSourceCauseWire};
use super::publication::serialize_publication_cause;
use heart_identity::{CompilationTargetDomain, ContentId};

/// Remote serde definition for the closed compiler terminal.
///
/// All terminal variants are structs in the core model, so serde can perform the exhaustive
/// projection directly.  The cause fields below retain their established named payload shape
/// through the small custom serializers for tuple variants.
#[derive(Serialize)]
#[serde(
    remote = "interface_core::CompilerTerminal",
    tag = "kind",
    rename_all = "snake_case"
)]
pub(crate) enum CompilerTerminalWire {
    PackageSource {
        #[serde(serialize_with = "serialize_content")]
        target: ContentId<CompilationTargetDomain>,
        #[serde(with = "PackageCompilePhaseWire")]
        phase: PackageCompilePhase,
        #[serde(with = "PackageSourceCauseWire")]
        cause: PackageSourceCause,
    },
    PackageCancelled {
        #[serde(serialize_with = "serialize_content")]
        target: ContentId<CompilationTargetDomain>,
        #[serde(with = "PackageCompilePhaseWire")]
        phase: PackageCompilePhase,
    },
    Runtime {
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        #[serde(serialize_with = "serialize_optional_target")]
        target: Option<ContentId<CompilationTargetDomain>>,
        #[serde(with = "CompilerRuntimeCauseWire")]
        cause: CompilerRuntimeCause,
    },
    SourceLength {
        actual: usize,
    },
    Unavailable {
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
    },
    UnsupportedStage {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "CompilerFrontendErrorWire")]
        cause: FrontendError,
    },
    DeadlineConstruction {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        timeout: Duration,
    },
    Toolchain {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        #[serde(with = "NativeToolWire")]
        selected: NativeTool,
        #[serde(serialize_with = "serialize_optional_native_tool")]
        configured: Option<NativeTool>,
    },
    ToolingUnavailable {
        #[serde(with = "SourceAuthorityWire")]
        source: SourceAuthority,
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
        #[serde(with = "NativeToolWire")]
        tool: NativeTool,
    },
    Cancelled {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_diagnostic_option")]
        diagnostic: Option<CompilerDiagnostic>,
    },
    Compile {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_compiler_cause")]
        cause: CompilerCause,
    },
    Publication {
        #[serde(with = "CompilerAttemptWire")]
        attempted: CompilerAttempt,
        #[serde(serialize_with = "serialize_publication_cause")]
        cause: PublicationCause,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::FrontendError",
    tag = "kind",
    rename_all = "snake_case"
)]
enum CompilerFrontendErrorWire {
    UnsupportedStage {
        #[serde(with = "LanguageWire")]
        language: Language,
        #[serde(with = "StageWire")]
        stage: Stage,
    },
}

fn serialize_optional_target<Output: Serializer>(
    target: &Option<ContentId<CompilationTargetDomain>>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    match target {
        Some(target) => serialize_content(target, serializer),
        None => serializer.serialize_none(),
    }
}

/// Closed lowering vocabulary projected as the value of a named `cause` field.
#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::LoweringUnsupported",
    rename_all = "snake_case"
)]
pub(crate) enum LoweringUnsupportedWire {
    NoSupportedDeclaration,
    ExtensionAtomUnbound {
        row: u32,
        provisional: u32,
        atom_count: u32,
    },
    ExtensionTypeParametersUnbound {
        row: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    FactRejected {
        fact: u32,
    },
    RustFunction,
    RustConstantType,
    RustGenericParameter,
    PythonAssignmentName,
    PythonAssignmentValue,
    ClangDeclarationForm,
    TypeScriptDeclarationForm,
    TypeScriptDeclarationType,
    CSharpDeclarationForm,
    CSharpDeclarationType,
    GoDeclarationForm,
    GoDeclarationType,
    JavaDeclarationForm,
    JavaProjection {
        #[serde(with = "JavaProjectionFaultWire")]
        fault: JavaProjectionFault,
    },
    GoProjection {
        #[serde(serialize_with = "serialize_go_projection_fault")]
        fault: GoProjectionFault,
    },
    TypeScriptProjection {
        #[serde(serialize_with = "serialize_typescript_projection_fault")]
        fault: TypeScriptProjectionFault,
    },
    PythonProjection {
        #[serde(serialize_with = "serialize_python_projection_fault")]
        fault: PythonProjectionFault,
    },
    CSharpProjection {
        #[serde(serialize_with = "serialize_csharp_projection_fault")]
        fault: CSharpProjectionFault,
    },
    ClangProjection {
        #[serde(serialize_with = "serialize_clang_projection_fault")]
        fault: ClangProjectionFault,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::JavaProjectionFault",
    rename_all = "snake_case"
)]
enum JavaProjectionFaultWire {
    Image {
        #[serde(with = "JavaImageFaultWire")]
        cause: JavaImageFault,
    },
    Depth {
        type_row: u32,
    },
    MalformedType {
        type_row: u32,
        #[serde(with = "JavaProjectionTypeKindWire")]
        kind: JavaProjectionTypeKind,
    },
    Primitive {
        type_row: u32,
    },
    AtomUtf8 {
        symbol: u32,
        #[serde(with = "JavaSymbolAtomWire")]
        atom: JavaSymbolAtom,
    },
    SourceUtf8,
    Utf16Offset {
        units: u32,
        source_utf16_len: u32,
    },
    Utf16Range {
        start: u32,
        end: u32,
    },
    OrphanOwner {
        owner: u32,
    },
    ForeignKey {
        #[serde(with = "JavaForeignKeyFaultWire")]
        cause: JavaForeignKeyFault,
    },
    SiblingCapacity {
        symbol: u32,
    },
    IndexCapacity {
        #[serde(with = "JavaProjectionIndexPhaseWire")]
        phase: JavaProjectionIndexPhase,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::JavaImageFault",
    rename_all = "snake_case"
)]
enum JavaImageFaultWire {
    Header {
        #[serde(with = "JavaImageHeaderFaultWire")]
        cause: JavaImageHeaderFault,
    },
    Section {
        #[serde(with = "JavaImagePlaneWire")]
        plane: JavaImagePlane,
        #[serde(with = "JavaImageSectionFaultWire")]
        cause: JavaImageSectionFault,
    },
    Digest,
    Atom {
        index: u32,
        #[serde(with = "JavaImageAtomFaultWire")]
        cause: JavaImageAtomFault,
    },
    AbsentAtom,
    Coordinate {
        #[serde(with = "JavaImagePlaneWire")]
        plane: JavaImagePlane,
        index: u32,
        upper_bound: u32,
    },
    Tag {
        #[serde(with = "JavaImagePlaneWire")]
        plane: JavaImagePlane,
        found: u8,
    },
    ChildRange {
        #[serde(with = "JavaImagePlaneWire")]
        plane: JavaImagePlane,
        start: u32,
        count: u32,
        upper_bound: u32,
    },
    ReferenceRange {
        start: u32,
        end: u32,
    },
    DocumentationPresence,
    ModifierBits {
        found: u32,
    },
    RecordComponentKind {
        index: u32,
    },
    ExtensionReserved,
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::JavaImageHeaderFault",
    rename_all = "snake_case"
)]
enum JavaImageHeaderFaultWire {
    Truncated { actual: u32 },
    Magic { found: [u8; 4] },
    Version { found: u16 },
    Length { found: u16 },
    Release { found: u16 },
    SectionCount { found: u16 },
    BodyLength { declared: u32, actual: u32 },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::JavaImageSectionFault",
    rename_all = "snake_case"
)]
enum JavaImageSectionFaultWire {
    Tag {
        expected: u16,
        found: u16,
    },
    RowBytes {
        expected: u32,
        found: u32,
    },
    ByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    Offset {
        expected: u32,
        found: u32,
    },
    Range {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::JavaImageAtomFault",
    rename_all = "snake_case"
)]
enum JavaImageAtomFaultWire {
    NonCanonicalOffset { found: u32 },
    Range,
    Utf8,
    TrailingBytes,
}

macro_rules! remote_unit_enum {
    ($wire:ident, $remote:literal, [$($variant:ident),+ $(,)?]) => {
        #[derive(Serialize)]
        #[serde(remote = $remote, rename_all = "snake_case")]
        enum $wire { $($variant),+ }
    };
}

remote_unit_enum!(
    JavaProjectionTypeKindWire,
    "compiler_vocabulary::JavaProjectionTypeKind",
    [
        Primitive,
        Void,
        Declared,
        Array,
        Variable,
        Wildcard,
        Intersection,
        Union,
        Error,
        None,
        Null
    ]
);
remote_unit_enum!(
    JavaSymbolAtomWire,
    "compiler_vocabulary::JavaSymbolAtom",
    [Owner, Name]
);
remote_unit_enum!(
    JavaForeignKeyFaultWire,
    "compiler_vocabulary::JavaForeignKeyFault",
    [EmptyPath, BackslashInPath]
);
remote_unit_enum!(
    JavaProjectionIndexPhaseWire,
    "compiler_vocabulary::JavaProjectionIndexPhase",
    [
        FactOrdinal,
        TypeRow,
        TypeChild,
        NameIndex,
        SymbolIndex,
        ExecutableIndex,
        Signature,
        Documentation,
        Utf16
    ]
);
remote_unit_enum!(
    JavaImagePlaneWire,
    "compiler_vocabulary::JavaImagePlane",
    [
        Atoms,
        AtomBytes,
        Types,
        TypeChildren,
        Symbols,
        SymbolParameters,
        Declarations,
        References,
        DeclarationExtensions,
        ExtensionEntries
    ]
);
remote_unit_enum!(
    CSharpImageSectionWire,
    "compiler_vocabulary::CSharpImageSection",
    [
        Atoms,
        AtomBytes,
        Declarations,
        Parameters,
        TypeParameters,
        TypeConstraints,
        Types,
        TypeChildren,
        Attributes,
        Docs,
        References
    ]
);
remote_unit_enum!(
    CSharpImageTypeKindWire,
    "compiler_vocabulary::CSharpImageTypeKind",
    [
        Named,
        Array,
        Pointer,
        NullableValue,
        Tuple,
        FunctionPointer,
        TypeParameter,
        Dynamic,
        Error
    ]
);

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::GoProjectionFault",
    rename_all = "snake_case"
)]
enum GoProjectionFaultWire {
    Image,
    IndexCapacity,
    Depth,
    VariadicWithoutParameter {
        signature: u32,
    },
    Anchor,
    ListCapacity,
    OrphanTarget,
    ForeignKey {
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        #[serde(with = "ProjectionPackageLineageFaultWire")]
        cause: ProjectionPackageLineageFault,
    },
    AtomUtf8,
    RelativeSpan {
        start: u32,
        end: u32,
    },
    OrphanOwner {
        owner: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::TypeScriptProjectionFault",
    rename_all = "snake_case"
)]
enum TypeScriptProjectionFaultWire {
    ForeignKey {
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        #[serde(with = "ProjectionPackageLineageFaultWire")]
        cause: ProjectionPackageLineageFault,
    },
    CoordinateOverflow {
        value: u64,
    },
    MissingImportBinding {
        fact: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::PythonProjectionFault",
    rename_all = "snake_case"
)]
enum PythonProjectionFaultWire {
    ForeignSpellingUtf8,
    ForeignKey {
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        #[serde(with = "ProjectionPackageLineageFaultWire")]
        cause: ProjectionPackageLineageFault,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::CSharpProjectionFault",
    rename_all = "snake_case"
)]
enum CSharpProjectionFaultWire {
    Image {
        #[serde(with = "CSharpImageFaultWire")]
        cause: CSharpImageFault,
    },
    Depth,
    NameSpan {
        start: u32,
        end: u32,
    },
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    Foreign,
    AttributeCapacity {
        spellings: u32,
    },
    IndexCapacity,
    HeterogeneousArrayRank,
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::CSharpImageFault",
    rename_all = "snake_case"
)]
enum CSharpImageFaultWire {
    Header {
        #[serde(with = "CSharpImageHeaderFaultWire")]
        cause: CSharpImageHeaderFault,
    },
    Digest,
    DeclarationKind {
        index: u32,
        found: u8,
        #[serde(with = "CSharpImageSectionWire")]
        plane: CSharpImageSection,
    },
    DeclarationReserved {
        index: u32,
        #[serde(with = "CSharpImageSectionWire")]
        plane: CSharpImageSection,
    },
    NameRange {
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    NameUtf8 {
        index: u32,
    },
    Span {
        index: u32,
        start: u32,
        end: u32,
    },
    TypeChildCount {
        index: u32,
        #[serde(with = "CSharpImageTypeKindWire")]
        kind: CSharpImageTypeKind,
        min: u32,
        max: u32,
        actual: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::CSharpImageHeaderFault",
    rename_all = "snake_case"
)]
enum CSharpImageHeaderFaultWire {
    Truncated {
        actual: u32,
    },
    Magic {
        found: [u8; 4],
    },
    Version {
        found: u16,
    },
    Length {
        found: u32,
    },
    SectionCount {
        found: u32,
    },
    BodyLength {
        declared: u32,
        actual: u32,
    },
    Reserved,
    DirectoryTag {
        expected: u16,
        found: u16,
    },
    DirectoryRowBytes {
        expected: u32,
        found: u32,
    },
    DirectoryByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    DirectoryOffset {
        expected: u32,
        found: u32,
    },
    DirectoryRange {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ClangProjectionFault",
    rename_all = "snake_case"
)]
enum ClangProjectionFaultWire {
    Span {
        start: u32,
        end: u32,
    },
    Nameless {
        declaration: u32,
    },
    Anchor,
    IndexCapacity,
    ForeignOverride {
        identity: [u8; 16],
    },
    ForeignReference {
        identity: [u8; 16],
    },
    IllegalQualifierTarget {
        type_id: u32,
        #[serde(with = "ClangProjectionTypeKindWire")]
        kind: ClangProjectionTypeKind,
        #[serde(with = "ClangProjectionQualifiersWire")]
        qualifiers: ClangProjectionQualifiers,
    },
    IllegalMemberPointerOwner {
        pointer: u32,
        owner: u32,
        #[serde(with = "ClangProjectionTypeKindWire")]
        kind: ClangProjectionTypeKind,
        #[serde(with = "ClangProjectionDeclarationWire")]
        declaration: ClangProjectionDeclaration,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionPackageLineageFault",
    rename_all = "snake_case"
)]
enum ProjectionPackageLineageFaultWire {
    EmptyEcosystem,
    EmptyPackage,
    SeparatorInEcosystem,
    SeparatorInPackage,
    Backslash {
        #[serde(with = "ProjectionLineagePartWire")]
        part: ProjectionLineagePart,
    },
}

#[derive(Serialize)]
#[serde(remote = "compiler_vocabulary::ClangProjectionQualifiers")]
struct ClangProjectionQualifiersWire {
    is_const: bool,
    is_volatile: bool,
    is_restrict: bool,
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ClangProjectionDeclaration",
    rename_all = "snake_case"
)]
enum ClangProjectionDeclarationWire {
    Known { identity: [u8; 16] },
    Unavailable,
}

remote_unit_enum!(
    ProjectionForeignKeyFaultWire,
    "compiler_vocabulary::ProjectionForeignKeyFault",
    [EmptyPath, BackslashInPath]
);
remote_unit_enum!(
    ProjectionLineagePartWire,
    "compiler_vocabulary::ProjectionLineagePart",
    [Ecosystem, Package]
);
remote_unit_enum!(
    ClangProjectionTypeKindWire,
    "compiler_vocabulary::ClangProjectionTypeKind",
    [
        Unknown,
        Builtin,
        Named,
        Pointer,
        BlockPointer,
        MemberPointer,
        LvalueReference,
        RvalueReference,
        Array,
        Function
    ]
);

fn serialize_go_projection_fault<Output: Serializer>(
    fault: &GoProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    GoProjectionFaultWire::serialize(fault, serializer)
}
fn serialize_typescript_projection_fault<Output: Serializer>(
    fault: &TypeScriptProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    TypeScriptProjectionFaultWire::serialize(fault, serializer)
}
fn serialize_python_projection_fault<Output: Serializer>(
    fault: &PythonProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    PythonProjectionFaultWire::serialize(fault, serializer)
}
fn serialize_csharp_projection_fault<Output: Serializer>(
    fault: &CSharpProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    CSharpProjectionFaultWire::serialize(fault, serializer)
}
fn serialize_clang_projection_fault<Output: Serializer>(
    fault: &ClangProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    ClangProjectionFaultWire::serialize(fault, serializer)
}

/// Closed compact-IR failure vocabulary projected as the value of a named `cause` field.
#[derive(Serialize)]
#[serde(remote = "interface_core::FragmentCause", rename_all = "snake_case")]
pub(crate) enum FragmentCauseWire {
    Prepare,
    Write,
    Validate,
}

/// Bounded compiler diagnostic facts.  Only the meaningful retained prefix crosses the wire.
#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize_diagnostic_option<Output: Serializer>(
    diagnostic: &Option<CompilerDiagnostic>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    match diagnostic.as_ref() {
        Some(diagnostic) => serialize_compiler_diagnostic(diagnostic, serializer),
        None => serializer.serialize_none(),
    }
}

fn serialize_compiler_diagnostic<Output: Serializer>(
    diagnostic: &CompilerDiagnostic,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("CompilerDiagnostic", 4)?;
    state.serialize_field("byte_len", &diagnostic.byte_len)?;
    state.serialize_field("observed", &diagnostic.observed)?;
    state.serialize_field("truncated", &diagnostic.truncated)?;
    state.serialize_field("bytes", &diagnostic.bytes[..diagnostic.byte_len])?;
    state.end()
}

pub(crate) struct LoweringUnsupportedRef<'value>(pub(crate) &'value LoweringUnsupported);

impl Serialize for LoweringUnsupportedRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        LoweringUnsupportedWire::serialize(self.0, serializer)
    }
}

pub(crate) struct FragmentCauseRef<'value>(pub(crate) &'value FragmentCause);

impl Serialize for FragmentCauseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        FragmentCauseWire::serialize(self.0, serializer)
    }
}

pub(crate) struct AuthorityPhaseRef<'value>(pub(crate) &'value AuthorityPhase);

impl Serialize for AuthorityPhaseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        AuthorityPhaseWire::serialize(self.0, serializer)
    }
}

pub(crate) struct AuthorityDiagnosticClassRef<'value>(pub(crate) &'value AuthorityDiagnosticClass);

impl Serialize for AuthorityDiagnosticClassRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        AuthorityDiagnosticClassWire::serialize(self.0, serializer)
    }
}

/// Projects the named wire shape for compiler causes whose core enum contains tuple variants.
pub(crate) fn serialize_compiler_cause<Output: Serializer>(
    cause: &CompilerCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("CompilerCause", 4)?;
    match cause {
        CompilerCause::Authority {
            phase,
            class,
            diagnostic,
        } => {
            state.serialize_field("kind", "authority")?;
            state.serialize_field("phase", &AuthorityPhaseRef(phase))?;
            state.serialize_field("class", &AuthorityDiagnosticClassRef(class))?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::NativeWork(cause) => {
            state.serialize_field("kind", "native_work")?;
            state.serialize_field("cause", &NativeWorkCauseWire(cause))?;
        }
        CompilerCause::NativeIo { phase, cause } => {
            state.serialize_field("kind", "native_io")?;
            state.serialize_field("phase", &NativeIoPhaseRef(phase))?;
            state.serialize_field("cause", &NativeIoFactRef(cause))?;
        }
        CompilerCause::NativeRejected { code, diagnostic } => {
            state.serialize_field("kind", "native_rejected")?;
            state.serialize_field("code", code)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::DeadlineExceeded { diagnostic } => {
            state.serialize_field("kind", "deadline_exceeded")?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => {
            state.serialize_field("kind", "diagnostic_limit")?;
            state.serialize_field("limit", limit)?;
            state.serialize_field("observed", observed)?;
            state.serialize_field("diagnostic", &DiagnosticRef(diagnostic))?;
        }
        CompilerCause::Lowering(cause) => {
            state.serialize_field("kind", "lowering")?;
            state.serialize_field("cause", &LoweringUnsupportedRef(cause))?;
        }
        CompilerCause::Fragment(cause) => {
            state.serialize_field("kind", "fragment")?;
            state.serialize_field("cause", &FragmentCauseRef(cause))?;
        }
    }
    state.end()
}

pub(crate) struct DiagnosticRef<'value>(pub(crate) &'value Option<CompilerDiagnostic>);

impl Serialize for DiagnosticRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serialize_diagnostic_option(self.0, serializer)
    }
}

#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize_optional_native_tool<Output: Serializer>(
    tool: &Option<NativeTool>,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    match tool {
        Some(tool) => NativeToolWire::serialize(tool, serializer),
        None => serializer.serialize_none(),
    }
}
