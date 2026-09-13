//! Defines json wire compiler terminal behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::time::Duration;

use compiler_vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, CSharpImageFault, CSharpImageHeaderFault,
    CSharpImageSection, CSharpImageTypeKind, CSharpProjectionFault, ClangProjectionDeclaration,
    ClangProjectionFault, ClangProjectionQualifiers, ClangProjectionTypeKind, FrontendError,
    GoImageDeclarationKind, GoImageFault, GoImageFlagCell, GoImageHeaderFault, GoImagePlane,
    GoImageTypeKind, GoProjectionFault, GoProjectionIndexPhase, GoProjectionListPhase,
    JavaForeignKeyFault, JavaImageAtomFault, JavaImageFault, JavaImageHeaderFault, JavaImagePlane,
    JavaImageSectionFault, JavaProjectionFault, JavaProjectionIndexPhase, JavaProjectionTypeKind,
    JavaSymbolAtom, Language, LoweringUnsupported, NativeTool, ProjectionAdmissionFault,
    ProjectionChildRole, ProjectionConstructorFault, ProjectionConstructorTag, ProjectionFactLane,
    ProjectionForeignKeyFault, ProjectionLineagePart, ProjectionPackageLineageFault,
    ProjectionParentageState, ProjectionSemanticTypeFault, ProjectionSemanticTypeTag,
    ProjectionSpan, ProjectionTypeCell, ProjectionTypeChildLane, PythonProjectionFault, Stage,
    TypeScriptProjectionFault,
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
use backend_version::{CompilationTargetDomain, ContentId};

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
        fact: u64,
        name_len: u64,
        #[serde(with = "ProjectionAdmissionFaultWire")]
        cause: ProjectionAdmissionFault,
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

remote_unit_enum!(
    GoImagePlaneWire,
    "compiler_vocabulary::GoImagePlane",
    [
        Declaration,
        Type,
        TypeChild,
        Method,
        TypeParameter,
        Member,
        Doc,
        Reference,
        ReferenceTarget,
        BuildConstraint,
        Satisfaction,
        SatisfactionTarget,
        Package,
        SignatureParameter,
        MethodSet,
        Module
    ]
);
remote_unit_enum!(
    GoImageFlagCellWire,
    "compiler_vocabulary::GoImageFlagCell",
    [Exported, PointerReceiver, Promoted, Embedded]
);
remote_unit_enum!(
    GoImageDeclarationKindWire,
    "compiler_vocabulary::GoImageDeclarationKind",
    [Type, Alias, Function, Constant, Static]
);
remote_unit_enum!(
    GoImageTypeKindWire,
    "compiler_vocabulary::GoImageTypeKind",
    [
        Basic,
        Named,
        Alias,
        TypeParameter,
        Pointer,
        Slice,
        Array,
        Map,
        Channel,
        Function,
        Struct,
        Interface,
        Union,
        Tuple,
        Invalid
    ]
);
#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::GoImageHeaderFault",
    rename_all = "snake_case"
)]
enum GoImageHeaderFaultWire {
    Truncated { actual: u32 },
    Magic { found: [u8; 4] },
    Version { found: u16 },
    Length { found: u32 },
    BodyLength { declared: u32, actual: u32 },
    Reserved,
    ModuleCount { found: u32 },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::GoImageFault",
    rename_all = "snake_case"
)]
enum GoImageFaultWire {
    Header {
        #[serde(with = "GoImageHeaderFaultWire")]
        cause: GoImageHeaderFault,
    },
    Digest,
    RowBounds {
        #[serde(with = "GoImagePlaneWire")]
        plane: GoImagePlane,
        index: u32,
        count: u32,
    },
    DeclarationKind {
        index: u32,
        found: u8,
    },
    ExportedFlag {
        index: u32,
        found: u8,
    },
    DeclarationIota {
        index: u32,
        found: u8,
    },
    DeclarationReserved {
        index: u32,
    },
    TypeReserved {
        index: u32,
    },
    MethodReserved {
        index: u32,
    },
    MemberReserved {
        index: u32,
    },
    DocReserved {
        index: u32,
    },
    ReferenceReserved {
        index: u32,
    },
    DeclarationTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    DeclarationSpan {
        index: u32,
        start: u32,
        end: u32,
    },
    TypeKind {
        index: u32,
        found: u8,
    },
    TypeNameRequired {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
    },
    TypeNameForbidden {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
    },
    TypeDirection {
        index: u32,
        found: u8,
    },
    TypeDirectionCell {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
    },
    TypeVariadicFlag {
        index: u32,
        found: u8,
    },
    TypeVariadicCell {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
    },
    ArrayLength {
        index: u32,
        length: i64,
    },
    TypeParamCount {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
        param_count: u32,
        child_count: u32,
    },
    TypeChildRange {
        index: u32,
        start: u32,
        count: u32,
        child_count: u32,
    },
    TypeChildTarget {
        index: u32,
        target: u32,
        type_count: u32,
    },
    TypeChildFlags {
        index: u32,
        flags: u32,
    },
    TypeChildTiling {
        declared: u32,
        plane: u32,
    },
    TypeMemberRange {
        index: u32,
        start: u32,
        count: u32,
        member_count: u32,
    },
    MethodOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    MethodFlag {
        index: u32,
        #[serde(with = "GoImageFlagCellWire")]
        cell: GoImageFlagCell,
        found: u8,
    },
    MethodTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    MethodReceiverParams {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    MethodSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    TypeParameterOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    TypeParameterConstraint {
        index: u32,
        root: u32,
        type_count: u32,
    },
    TypeParameterSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    MemberKind {
        index: u32,
        found: u8,
    },
    MemberFlag {
        index: u32,
        #[serde(with = "GoImageFlagCellWire")]
        cell: GoImageFlagCell,
        found: u8,
    },
    MemberEmbedded {
        index: u32,
    },
    MemberOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    MemberTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    MemberSort {
        index: u32,
        owner: u32,
        previous: u32,
    },
    MemberOwnerRange {
        owner: u32,
        start: u32,
        count: u32,
        actual_start: u32,
        actual_count: u32,
    },
    DocOwnerKind {
        index: u32,
        found: u8,
    },
    DocOwner {
        index: u32,
        owner: u32,
        bound: u32,
    },
    EmptyDoc {
        index: u32,
    },
    DocSort {
        index: u32,
        owner_kind: u8,
        owner: u32,
        previous_kind: u8,
        previous_owner: u32,
    },
    ReferenceOwner {
        index: u32,
        owner: u32,
        declaration_count: u32,
    },
    ReferenceSpan {
        index: u32,
        start: u32,
        end: u32,
    },
    ReferenceOwnerUnresolved {
        index: u32,
        owner_bytes: u32,
        function_bytes: u32,
    },
    ReferenceOwnerSpan {
        index: u32,
    },
    ReferenceFile {
        index: u32,
    },
    ReferenceContainment {
        index: u32,
        start: u32,
        end: u32,
        owner_start: u32,
        owner_end: u32,
    },
    ReferenceSort {
        index: u32,
    },
    EmptyConstraint {
        index: u32,
    },
    ConstraintBlob {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    ConstraintSort {
        index: u32,
    },
    SatisfactionSubject {
        index: u32,
        subject: u32,
        declaration_count: u32,
    },
    SatisfactionSort {
        index: u32,
        subject: u32,
        previous: u32,
    },
    SatisfactionSubjectKind {
        index: u32,
        #[serde(with = "GoImageDeclarationKindWire")]
        kind: GoImageDeclarationKind,
    },
    ModulePath,
    PackageFiles {
        index: u32,
        count: u32,
        blob_bytes: u32,
    },
    PackageSort {
        index: u32,
    },
    DeclarationPackage {
        index: u32,
        package_count: u32,
    },
    SignatureParameterPosition {
        index: u32,
    },
    MethodSetOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    MethodSetOwnerKind {
        index: u32,
        #[serde(with = "GoImageTypeKindWire")]
        kind: GoImageTypeKind,
    },
    MethodSetTypeRoot {
        index: u32,
        root: u32,
        type_count: u32,
    },
    MethodSetSort {
        index: u32,
    },
    SignatureParameterOwner {
        index: u32,
        owner: u32,
        type_count: u32,
    },
    SignatureParameterOwnerRow {
        index: u32,
        owner: u32,
        expected: u32,
    },
    SignatureParameterOrdinal {
        index: u32,
        ordinal: u32,
        expected: u32,
    },
    SignatureParameterTiling {
        declared: u32,
        plane: u32,
    },
    EmptyName {
        #[serde(with = "GoImagePlaneWire")]
        plane: GoImagePlane,
        index: u32,
    },
    AtomRange {
        #[serde(with = "GoImagePlaneWire")]
        plane: GoImagePlane,
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    AtomUtf8 {
        #[serde(with = "GoImagePlaneWire")]
        plane: GoImagePlane,
        index: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::GoProjectionFault",
    rename_all = "snake_case"
)]
enum GoProjectionFaultWire {
    Image {
        #[serde(with = "GoImageFaultWire")]
        cause: GoImageFault,
    },
    IndexCapacity {
        #[serde(with = "GoProjectionIndexPhaseWire")]
        phase: GoProjectionIndexPhase,
        observed: u64,
    },
    Depth {
        type_row: u32,
    },
    VariadicWithoutParameter {
        signature: u32,
    },
    Anchor {
        owner: u32,
    },
    ListCapacity {
        owner: u32,
        #[serde(with = "GoProjectionListPhaseWire")]
        phase: GoProjectionListPhase,
    },
    OrphanTarget {
        reference: u32,
    },
    ForeignKey {
        reference: u32,
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        reference: u32,
        #[serde(with = "ProjectionPackageLineageFaultWire")]
        cause: ProjectionPackageLineageFault,
    },
    AtomUtf8 {
        #[serde(with = "GoImagePlaneWire")]
        plane: GoImagePlane,
        row: u32,
    },
    RelativeSpan {
        row: u32,
        start: u32,
        end: u32,
    },
    OrphanOwner {
        owner: u32,
    },
    Admission {
        fact: u32,
        name_len: u32,
        #[serde(with = "ProjectionAdmissionFaultWire")]
        cause: ProjectionAdmissionFault,
    },
}

remote_unit_enum!(
    GoProjectionIndexPhaseWire,
    "compiler_vocabulary::GoProjectionIndexPhase",
    [
        ImageHeader,
        ImageRow,
        FactOrdinal,
        TypeRow,
        TypeChild,
        Declaration,
        Method,
        TypeParameter,
        Member,
        Documentation,
        Reference,
        Constraint,
        Satisfaction,
        Package,
        SignatureParameter,
        MethodSet,
        Atom,
        EntityList
    ]
);
remote_unit_enum!(
    GoProjectionListPhaseWire,
    "compiler_vocabulary::GoProjectionListPhase",
    [Entity, Type, Atom, TypeParameter]
);

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::TypeScriptProjectionFault",
    rename_all = "snake_case"
)]
enum TypeScriptProjectionFaultWire {
    ForeignKey {
        start: u32,
        end: u32,
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        start: u32,
        end: u32,
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
    ForeignSpellingUtf8 {
        start: u32,
        end: u32,
    },
    ForeignKey {
        start: u32,
        end: u32,
        #[serde(with = "ProjectionForeignKeyFaultWire")]
        cause: ProjectionForeignKeyFault,
    },
    PackageLineage {
        start: u32,
        end: u32,
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
    Depth {
        type_row: u32,
    },
    NameSpan {
        start: u32,
        end: u32,
    },
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    Anchor {
        owner: u32,
    },
    Foreign {
        reference: u32,
    },
    AttributeCapacity {
        spellings: u32,
    },
    IndexCapacity {
        #[serde(with = "CSharpProjectionIndexPhaseWire")]
        phase: compiler_vocabulary::CSharpProjectionIndexPhase,
        observed: u64,
    },
    HeterogeneousArrayRank {
        first: u32,
        observed: u32,
    },
    ResultName {
        type_row: u32,
    },
    Admission {
        fact: u32,
        name_len: u32,
        #[serde(with = "ProjectionAdmissionFaultWire")]
        cause: ProjectionAdmissionFault,
    },
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

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionLineagePart",
    rename_all = "snake_case"
)]
enum ProjectionLineagePartWire {
    Ecosystem,
    Package,
    Invalid { segment: u8 },
}

remote_unit_enum!(
    CSharpProjectionIndexPhaseWire,
    "compiler_vocabulary::CSharpProjectionIndexPhase",
    [
        ImageHeader,
        FactOrdinal,
        Signature,
        Declaration,
        TypeRow,
        TypeChild,
        Attribute,
        Documentation,
        Reference,
        Name,
        SourceSpan
    ]
);

remote_unit_enum!(
    ProjectionConstructorTagWire,
    "compiler_vocabulary::ProjectionConstructorTag",
    [
        Function,
        Generic,
        Tuple,
        Array,
        Union,
        Intersection,
        Product
    ]
);
remote_unit_enum!(
    ProjectionChildRoleWire,
    "compiler_vocabulary::ProjectionChildRole",
    [
        FunctionParameter,
        FunctionResult,
        GenericArgument,
        TupleElement,
        ArrayElement,
        UnionMember,
        IntersectionMember,
        ProductMember
    ]
);
remote_unit_enum!(
    ProjectionSemanticTypeTagWire,
    "compiler_vocabulary::ProjectionSemanticTypeTag",
    [
        SelfType,
        Primitive,
        Tuple,
        Slice,
        Array,
        Union,
        Intersection,
        Never,
        Any,
        Unknown,
        Nominal,
        Apply,
        TypeVar,
        Wildcard,
        FunctionPointer,
        Annotated,
        Conditional,
        Mapped,
        TemplateLiteral,
        AnonymousRecord,
        ImplTrait,
        DynTrait,
        Inferred,
        QualifiedPath,
        Map,
        Channel,
        ArraySequence,
        ArrayRectangular,
        ArrayFixed,
        ArrayConstExpression,
        ArrayIncomplete,
        CQualified
    ]
);
remote_unit_enum!(
    ProjectionTypeCellWire,
    "compiler_vocabulary::ProjectionTypeCell",
    [Payload0, Payload1, Text, Text2, Nominal]
);
remote_unit_enum!(
    ProjectionTypeChildLaneWire,
    "compiler_vocabulary::ProjectionTypeChildLane",
    [Declared, Anonymous, Computed]
);
remote_unit_enum!(
    ProjectionFactLaneWire,
    "compiler_vocabulary::ProjectionFactLane",
    [
        TypeRows,
        ReservedTypeRows,
        ComputedOwners,
        Extensions,
        ReplacementTypeParameterRange,
        TypeParameterRanges,
        CapturedTypeParameterRange,
        EntityMembers,
        EntityParentage,
        EntityParents,
        EntitySourceSpans,
        TypeParameters,
        TypeLists,
        EntityLists,
        AtomLists
    ]
);

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionConstructorFault",
    rename_all = "snake_case"
)]
enum ProjectionConstructorFaultWire {
    Tag {
        actual: u32,
    },
    ReservedPayload {
        #[serde(with = "ProjectionConstructorTagWire")]
        tag: ProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    ArityOverflow {
        #[serde(with = "ProjectionConstructorTagWire")]
        tag: ProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    Arity {
        #[serde(with = "ProjectionConstructorTagWire")]
        tag: ProjectionConstructorTag,
        expected: u32,
        actual: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionSemanticTypeFault",
    rename_all = "snake_case"
)]
enum ProjectionSemanticTypeFaultWire {
    Tag {
        actual: u8,
    },
    ReservedCell {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        #[serde(with = "ProjectionTypeCellWire")]
        cell: ProjectionTypeCell,
        actual: u32,
    },
    MissingCell {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        #[serde(with = "ProjectionTypeCellWire")]
        cell: ProjectionTypeCell,
    },
    Reason {
        actual: u32,
    },
    PrimitiveShape {
        actual: u32,
    },
    CvQualifiers {
        actual: u32,
    },
    Width {
        actual: u32,
    },
    ChildCount {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        min: u32,
        max: u32,
        actual: u32,
    },
    ChildNameForbidden {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
    ChildNameRequired {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
    ChildFlagsForbidden {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        position: u32,
        actual: u8,
    },
    VariadicParameter {
        position: u32,
        actual: u8,
    },
    ChildTextForbidden {
        #[serde(with = "ProjectionSemanticTypeTagWire")]
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionParentageState",
    rename_all = "snake_case"
)]
enum ProjectionParentageStateWire {
    Unavailable,
    Root,
    Bound { parent: u32 },
    UnrepresentedAuthorityOwner { identity: [u8; 16] },
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionSpan",
    rename_all = "snake_case"
)]
struct ProjectionSpanWire {
    start: u32,
    end: u32,
}

#[derive(Serialize)]
#[serde(
    remote = "compiler_vocabulary::ProjectionAdmissionFault",
    rename_all = "snake_case"
)]
enum ProjectionAdmissionFaultWire {
    EmptyName,
    Capacity,
    ChildCapacity,
    ProductChildPoolCapacity {
        used: u64,
        requested: u64,
        capacity: u64,
    },
    Constructor {
        #[serde(with = "ProjectionConstructorFaultWire")]
        cause: ProjectionConstructorFault,
    },
    ChildRole {
        position: u64,
        #[serde(with = "ProjectionChildRoleWire")]
        expected: ProjectionChildRole,
        #[serde(with = "ProjectionChildRoleWire")]
        actual: ProjectionChildRole,
    },
    ChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeRecord {
        #[serde(with = "ProjectionSemanticTypeFaultWire")]
        cause: ProjectionSemanticTypeFault,
    },
    TypeChild {
        position: u64,
        #[serde(with = "ProjectionSemanticTypeFaultWire")]
        cause: ProjectionSemanticTypeFault,
    },
    TypeChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeChildCapacity,
    TypeChildPoolCapacity {
        #[serde(with = "ProjectionTypeChildLaneWire")]
        lane: ProjectionTypeChildLane,
        used: u64,
        requested: u64,
        capacity: u64,
    },
    TypeRowCapacity,
    ComputedRowCapacity,
    OccurrenceOwner {
        owner: u32,
        fact_count: u64,
    },
    OccurrenceCapacity,
    DocOwner {
        owner: u32,
        fact_count: u64,
    },
    DocCapacity,
    ExtensionAtomCapacity,
    TypeParameterCapacity,
    TypeParameterBoundCapacity {
        requested: u64,
        available: u64,
    },
    RefListCapacity,
    RefListElements,
    RefTarget {
        #[serde(with = "ProjectionFactLaneWire")]
        lane: ProjectionFactLane,
        raw: u32,
        fact_count: u64,
    },
    SourceSpan {
        entity: u32,
        start: u32,
        end: u32,
        source_len: u32,
    },
    ConflictingSourceSpan {
        entity: u32,
        #[serde(with = "ProjectionSpanWire")]
        existing: ProjectionSpan,
        #[serde(with = "ProjectionSpanWire")]
        requested: ProjectionSpan,
    },
    ConflictingParentage {
        entity: u32,
        #[serde(with = "ProjectionParentageStateWire")]
        existing: ProjectionParentageState,
        #[serde(with = "ProjectionParentageStateWire")]
        requested: ProjectionParentageState,
    },
}
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
