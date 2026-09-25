//! Closed Go authority projection and image vocabulary.

use super::projection::{
    ProjectionAdmissionFault, ProjectionForeignKeyFault, ProjectionPackageLineageFault,
};

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
    MethodSetOwnerKind { index: u32, kind: GoImageTypeKind },
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
    /// Canonical fact admission rejected one exact projected fact.
    Admission {
        /// Zero-based fact ordinal that would have been occupied.
        fact: u32,
        /// Exact byte length of the rejected fact name.
        name_len: u32,
        /// Full closed admission cause.
        cause: ProjectionAdmissionFault,
    },
}
impl core::fmt::Display for GoProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(core::mem::size_of::<GoProjectionFault>() <= 64);
