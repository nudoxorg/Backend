//! Go image and projection faults on the compiler terminal wire.
//!
//! The image header, row, and projection causes stay one closed serde
//! projection. The compiler terminal names that projection through one
//! serializer.

use backend_semantic::vocabulary::{
    GoImageDeclarationKind, GoImageFault, GoImageFlagCell, GoImageHeaderFault, GoImagePlane,
    GoImageTypeKind, GoProjectionFault, GoProjectionIndexPhase, GoProjectionListPhase,
    ProjectionAdmissionFault, ProjectionForeignKeyFault, ProjectionPackageLineageFault,
};

use super::{
    ProjectionAdmissionFaultWire, ProjectionForeignKeyFaultWire, ProjectionPackageLineageFaultWire,
};
use serde::{Serialize, Serializer};

macro_rules! remote_unit_enum {
    ($wire:ident, $remote:literal, [$($variant:ident),+ $(,)?]) => {
        #[derive(Serialize)]
        #[serde(remote = $remote, rename_all = "snake_case")]
        enum $wire { $($variant),+ }
    };
}

remote_unit_enum!(
    GoImagePlaneWire,
    "backend_semantic::vocabulary::GoImagePlane",
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
    "backend_semantic::vocabulary::GoImageFlagCell",
    [Exported, PointerReceiver, Promoted, Embedded]
);
remote_unit_enum!(
    GoImageDeclarationKindWire,
    "backend_semantic::vocabulary::GoImageDeclarationKind",
    [Type, Alias, Function, Constant, Static]
);
remote_unit_enum!(
    GoImageTypeKindWire,
    "backend_semantic::vocabulary::GoImageTypeKind",
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
    remote = "backend_semantic::vocabulary::GoImageHeaderFault",
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
    remote = "backend_semantic::vocabulary::GoImageFault",
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
    remote = "backend_semantic::vocabulary::GoProjectionFault",
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
    "backend_semantic::vocabulary::GoProjectionIndexPhase",
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
    "backend_semantic::vocabulary::GoProjectionListPhase",
    [Entity, Type, Atom, TypeParameter]
);

pub(super) fn serialize_go_projection_fault<Output: Serializer>(
    fault: &GoProjectionFault,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    GoProjectionFaultWire::serialize(fault, serializer)
}
