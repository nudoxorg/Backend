//! Closed Go authority projection and image vocabulary.

use core::mem::size_of;

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
    Truncated {
        /// Number of header bytes available when the fixed Go image header was read.
        actual: u32,
    },
    /// Raw image magic.
    Magic {
        /// Four magic bytes read from the image header, retained exactly for diagnosis.
        found: [u8; 4],
    },
    /// Raw image version.
    Version {
        /// Raw image-format version encoded in the fixed header.
        found: u16,
    },
    /// Raw fixed-header length.
    Length {
        /// Encoded fixed-header length in bytes.
        found: u32,
    },
    /// Declared and observed body lengths.
    BodyLength {
        /// Body byte length encoded in the header.
        declared: u32,
        /// Body byte length actually supplied after the fixed header.
        actual: u32,
    },
    /// Non-zero reserved header bytes.
    Reserved,
    /// Number of module rows observed.
    ModuleCount {
        /// Observed number of module metadata rows; the image admits zero or one module row.
        found: u32,
    },
}

/// Exact closed rejection from every public Go authority-image accessor.
///
/// Every usize operand is converted by the driver with a checked conversion
/// before it reaches this type.  A conversion failure is itself represented
/// by the projection index-capacity phase, never by a fabricated sentinel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoImageFault {
    /// Fixed-header grammar rejection.
    Header {
        /// Specific fixed-header rule that rejected the Go image.
        cause: GoImageHeaderFault,
    },
    /// Image checksum rejection.
    Digest,
    /// An accessor selected a row outside its plane.
    RowBounds {
        /// Go image plane selected by the accessor.
        plane: GoImagePlane,
        /// Zero-based row requested from that plane.
        index: u32,
        /// Number of rows in the plane; valid row indices are less than this count.
        count: u32,
    },
    /// Unknown declaration kind tag.
    DeclarationKind {
        /// Zero-based declaration row carrying the unrecognized declaration-kind tag.
        index: u32,
        /// Raw one-byte declaration-kind tag before decoding into GoImageDeclarationKind.
        found: u8,
    },
    /// Invalid declaration exported flag.
    ExportedFlag {
        /// Zero-based declaration row carrying the invalid exported flag.
        index: u32,
        /// Raw exported-flag byte; the image grammar admits only its defined boolean values.
        found: u8,
    },
    /// Invalid declaration iota flag.
    DeclarationIota {
        /// Zero-based constant declaration row carrying the invalid iota marker.
        index: u32,
        /// Raw iota-flag byte; the image grammar admits only its defined boolean values.
        found: u8,
    },
    /// Declaration reserved byte/bit rejection.
    DeclarationReserved {
        /// Zero-based declaration row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Type-row reserved byte/bit rejection.
    TypeReserved {
        /// Zero-based recursive type row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Method-row reserved byte/bit rejection.
    MethodReserved {
        /// Zero-based method row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Member-row reserved byte/bit rejection.
    MemberReserved {
        /// Zero-based struct-field or interface-method row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Documentation-row reserved byte/bit rejection.
    DocReserved {
        /// Zero-based documentation row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Reference-row reserved byte/bit rejection.
    ReferenceReserved {
        /// Zero-based resolved-reference row with nonzero reserved bytes or flag bits.
        index: u32,
    },
    /// Declaration type root outside the type plane.
    DeclarationTypeRoot {
        /// Zero-based declaration row carrying the invalid type root.
        index: u32,
        /// Zero-based recursive type row referenced as the declaration type.
        root: u32,
        /// Number of rows in the type plane, which bounds valid roots.
        type_count: u32,
    },
    /// Declaration/method source span rejection.
    DeclarationSpan {
        /// Zero-based declaration or method row carrying the source span.
        index: u32,
        /// Inclusive source-byte start encoded by the declaration or method row.
        start: u32,
        /// Exclusive source-byte end encoded by the declaration or method row.
        end: u32,
    },
    /// Unknown type-row kind tag.
    TypeKind {
        /// Zero-based recursive type row carrying the unrecognized kind tag.
        index: u32,
        /// Raw one-byte Go type-kind tag before closed-value decoding.
        found: u8,
    },
    /// Required type-row name was empty.
    TypeNameRequired {
        /// Zero-based recursive type row whose kind requires a name but has none.
        index: u32,
        /// Closed Go type-row kind decoded from the offending row.
        kind: GoImageTypeKind,
    },
    /// Forbidden type-row name was present.
    TypeNameForbidden {
        /// Zero-based recursive type row whose kind forbids a name but carries one.
        index: u32,
        /// Closed Go type-row kind decoded from the offending row.
        kind: GoImageTypeKind,
    },
    /// Unknown channel direction tag.
    TypeDirection {
        /// Zero-based channel type row carrying the unrecognized direction tag.
        index: u32,
        /// Raw one-byte channel direction tag before closed-value decoding.
        found: u8,
    },
    /// Channel direction present on a non-channel row.
    TypeDirectionCell {
        /// Zero-based recursive type row with a channel-direction cell on a non-channel kind.
        index: u32,
        /// Closed Go type-row kind decoded from the offending row.
        kind: GoImageTypeKind,
    },
    /// Unknown variadic flag tag.
    TypeVariadicFlag {
        /// Zero-based function type row carrying the invalid variadic marker.
        index: u32,
        /// Raw variadic-flag byte; the image grammar admits only its defined boolean values.
        found: u8,
    },
    /// Variadic flag present on a non-function row.
    TypeVariadicCell {
        /// Zero-based recursive type row with a variadic marker on a non-function kind.
        index: u32,
        /// Closed Go type-row kind decoded from the offending row.
        kind: GoImageTypeKind,
    },
    /// Negative array length.
    ArrayLength {
        /// Zero-based array type row with the invalid extent.
        index: u32,
        /// Signed array element count from the source type; negative values are not valid Go array extents.
        length: i64,
    },
    /// Function parameter count mismatch.
    TypeParamCount {
        /// Zero-based function type row with inconsistent parameter metadata.
        index: u32,
        /// Closed Go type-row kind being checked.
        kind: GoImageTypeKind,
        /// Number of signature parameters declared by the row.
        param_count: u32,
        /// Number of child coordinates declared by this type row.
        child_count: u32,
    },
    /// Type-child range rejection.
    TypeChildRange {
        /// Zero-based type row whose child range was checked.
        index: u32,
        /// First child-row coordinate claimed by this type row.
        start: u32,
        /// Number of consecutive child rows claimed by this type row.
        count: u32,
        /// Total number of rows in the type-child plane.
        child_count: u32,
    },
    /// Type child target outside the type plane.
    TypeChildTarget {
        /// Zero-based child entry with the invalid target.
        index: u32,
        /// Zero-based recursive type row referenced by the child entry.
        target: u32,
        /// Number of rows in the type plane, which bounds valid targets.
        type_count: u32,
    },
    /// Undefined type-child flags.
    TypeChildFlags {
        /// Zero-based child row carrying the invalid bits.
        index: u32,
        /// Raw child-flag bitset; bits outside the defined Go image mask are rejected.
        flags: u32,
    },
    /// Child-plane tiling mismatch.
    TypeChildTiling {
        /// Total child references declared by all type rows.
        declared: u32,
        /// Number of child rows physically present in the child plane.
        plane: u32,
    },
    /// Type-member range rejection.
    TypeMemberRange {
        /// Zero-based type row whose member range was checked.
        index: u32,
        /// First member-row coordinate claimed by this type row.
        start: u32,
        /// Number of consecutive member rows claimed by this type row.
        count: u32,
        /// Total number of rows in the member plane.
        member_count: u32,
    },
    /// Method owner outside declaration plane.
    MethodOwner {
        /// Zero-based row containing the owner or subject coordinate.
        index: u32,
        /// Zero-based declaration row named as owner or subject.
        owner: u32,
        /// Number of declaration rows available as valid owners or subjects.
        declaration_count: u32,
    },
    /// Method flag cell rejection.
    MethodFlag {
        /// Zero-based method row with the invalid flag.
        index: u32,
        /// Closed method/member flag cell being checked.
        cell: GoImageFlagCell,
        /// Raw flag byte stored in that cell.
        found: u8,
    },
    /// Method type root outside type plane.
    MethodTypeRoot {
        /// Zero-based method row carrying the invalid result or receiver type root.
        index: u32,
        /// Zero-based recursive type row referenced by the method.
        root: u32,
        /// Number of rows in the type plane, which bounds valid roots.
        type_count: u32,
    },
    /// Method receiver parameter blob mismatch.
    MethodReceiverParams {
        /// Zero-based method row whose receiver-parameter payload was checked.
        index: u32,
        /// Number of receiver type-parameter names encoded by the method row.
        count: u32,
        /// Available bytes in the packed receiver-parameter payload.
        blob_bytes: u32,
    },
    /// Method ordering rejection.
    MethodSort {
        /// Zero-based method row that violates ordering within its owner’s method run.
        index: u32,
        /// Declaration ordinal named as this method's owner.
        owner: u32,
        /// Declaration ordinal named as the preceding method's owner; owner ordinals must be nondecreasing.
        previous: u32,
    },
    /// Type-parameter owner outside declaration plane.
    TypeParameterOwner {
        /// Zero-based row containing the owner or subject coordinate.
        index: u32,
        /// Zero-based declaration row named as owner or subject.
        owner: u32,
        /// Number of declaration rows available as valid owners or subjects.
        declaration_count: u32,
    },
    /// Type-parameter constraint root outside type plane.
    TypeParameterConstraint {
        /// Zero-based type-parameter row carrying the invalid constraint root.
        index: u32,
        /// Zero-based recursive type row referenced as the constraint.
        root: u32,
        /// Number of rows in the type plane, which bounds valid roots.
        type_count: u32,
    },
    /// Type-parameter ordering rejection.
    TypeParameterSort {
        /// Zero-based type-parameter row that violates ordering within its declaration’s parameter run.
        index: u32,
        /// Declaration ordinal named as this type parameter's owner.
        owner: u32,
        /// Declaration ordinal named as the preceding type parameter's owner; owner ordinals must be nondecreasing.
        previous: u32,
    },
    /// Unknown member kind tag.
    MemberKind {
        /// Zero-based member row carrying the unrecognized member-kind tag.
        index: u32,
        /// Raw one-byte member-kind tag before closed-value decoding.
        found: u8,
    },
    /// Member flag cell rejection.
    MemberFlag {
        /// Zero-based member row with the invalid flag.
        index: u32,
        /// Closed member flag cell being checked.
        cell: GoImageFlagCell,
        /// Raw flag byte stored in that cell.
        found: u8,
    },
    /// Embedded bit present on a non-field.
    MemberEmbedded {
        /// Zero-based member row whose embedded bit is set on a member kind other than a Go struct field.
        index: u32,
    },
    /// Member owner outside type plane.
    MemberOwner {
        /// Zero-based member row carrying the owner coordinate.
        index: u32,
        /// Zero-based type row named as the member owner.
        owner: u32,
        /// Number of rows in the type plane, which bounds valid owners.
        type_count: u32,
    },
    /// Member type root outside type plane.
    MemberTypeRoot {
        /// Zero-based member row carrying the invalid type root.
        index: u32,
        /// Zero-based recursive type row referenced as the member type.
        root: u32,
        /// Number of rows in the type plane, which bounds valid roots.
        type_count: u32,
    },
    /// Member ordering rejection.
    MemberSort {
        /// Zero-based member row that violates ordering within its type’s member run.
        index: u32,
        /// Type-row ordinal named as this member's owner.
        owner: u32,
        /// Type-row ordinal named as the preceding member's owner; owner ordinals must be nondecreasing.
        previous: u32,
    },
    /// Member-owner tiling rejection.
    MemberOwnerRange {
        /// Zero-based type row whose member run was expected.
        owner: u32,
        /// First member-row coordinate required by the owner-order tiling.
        start: u32,
        /// Number of member rows assigned to this owner.
        count: u32,
        /// First member-row coordinate actually observed for this owner.
        actual_start: u32,
        /// Number of consecutive member rows actually observed for this owner.
        actual_count: u32,
    },
    /// Unknown documentation owner-kind tag.
    DocOwnerKind {
        /// Zero-based documentation row carrying the unrecognized owner-kind tag.
        index: u32,
        /// Raw one-byte documentation owner-kind tag before closed-value decoding.
        found: u8,
    },
    /// Documentation owner outside its plane.
    DocOwner {
        /// Zero-based documentation row carrying the invalid owner coordinate.
        index: u32,
        /// Zero-based row named as the documentation owner.
        owner: u32,
        /// Number of rows in the plane selected by the decoded owner kind.
        bound: u32,
    },
    /// Empty documentation text.
    EmptyDoc {
        /// Zero-based documentation row whose required text atom is empty.
        index: u32,
    },
    /// Documentation ordering rejection.
    DocSort {
        /// Zero-based documentation row that violates canonical owner-kind/owner ordering.
        index: u32,
        /// Raw owner-kind tag on this documentation row.
        owner_kind: u8,
        /// Zero-based owner row on this documentation record.
        owner: u32,
        /// Owner-kind tag on the immediately preceding documentation record.
        previous_kind: u8,
        /// Owner row on the preceding documentation record; together the kind and row form its sort key.
        previous_owner: u32,
    },
    /// Reference owner outside declaration plane.
    ReferenceOwner {
        /// Zero-based row containing the owner or subject coordinate.
        index: u32,
        /// Zero-based declaration row named as owner or subject.
        owner: u32,
        /// Number of declaration rows available as valid owners or subjects.
        declaration_count: u32,
    },
    /// Reference call span rejection.
    ReferenceSpan {
        /// Zero-based resolved reference row carrying the call span.
        index: u32,
        /// Inclusive source-byte start of the call occurrence.
        start: u32,
        /// Exclusive source-byte end of the call occurrence.
        end: u32,
    },
    /// Unresolved method owner spelling lengths.
    ReferenceOwnerUnresolved {
        /// Zero-based reference row whose owner spelling could not resolve.
        index: u32,
        /// UTF-8 byte length of the owner spelling.
        owner_bytes: u32,
        /// UTF-8 byte length of the function spelling.
        function_bytes: u32,
    },
    /// Reference owner had no usable span.
    ReferenceOwnerSpan {
        /// Zero-based reference row whose declaration owner has no usable source span.
        index: u32,
    },
    /// Reference file mismatch.
    ReferenceFile {
        /// Zero-based reference row whose recorded source file differs from the owning declaration’s file.
        index: u32,
    },
    /// Reference containment rejection.
    ReferenceContainment {
        /// Zero-based reference row checked against its declaration.
        index: u32,
        /// Inclusive source-byte start of the reference occurrence.
        start: u32,
        /// Exclusive source-byte end of the reference occurrence.
        end: u32,
        /// Inclusive source-byte start of the owning declaration.
        owner_start: u32,
        /// Exclusive source-byte end of the owning declaration.
        owner_end: u32,
    },
    /// Reference ordering rejection.
    ReferenceSort {
        /// Zero-based reference row that violates the image’s canonical reference ordering.
        index: u32,
    },
    /// Empty build constraint.
    EmptyConstraint {
        /// Zero-based build-constraint row whose required spelling is empty.
        index: u32,
    },
    /// Build-constraint blob mismatch.
    ConstraintBlob {
        /// Zero-based build-constraint row whose packed name list was checked.
        index: u32,
        /// Number of constraint spellings claimed by this row.
        count: u32,
        /// Available bytes in the packed constraint-name payload.
        blob_bytes: u32,
    },
    /// Build-constraint ordering rejection.
    ConstraintSort {
        /// Zero-based build-constraint row that violates canonical ordering.
        index: u32,
    },
    /// Satisfaction subject outside declaration plane.
    SatisfactionSubject {
        /// Zero-based interface-satisfaction row carrying the subject coordinate.
        index: u32,
        /// Zero-based declaration row named as the interface-satisfaction subject.
        subject: u32,
        /// Number of declaration rows available as valid subjects.
        declaration_count: u32,
    },
    /// Satisfaction ordering rejection.
    SatisfactionSort {
        /// Zero-based interface-satisfaction row that violates ordering for its subject declaration.
        index: u32,
        /// Zero-based declaration row whose implemented-interface facts are being ordered.
        subject: u32,
        /// Subject declaration ordinal on the preceding satisfaction row; rows must be nondecreasing by this value.
        previous: u32,
    },
    /// Satisfaction subject had the wrong declaration kind.
    SatisfactionSubjectKind {
        /// Zero-based interface-satisfaction row with the wrong subject kind.
        index: u32,
        /// Closed declaration kind decoded for the subject row; satisfaction facts require a type declaration.
        kind: GoImageDeclarationKind,
    },
    /// Module path cell was empty.
    ModulePath,
    /// Package file blob mismatch.
    PackageFiles {
        /// Zero-based package row whose file list was checked.
        index: u32,
        /// Number of source-file entries claimed by the package row.
        count: u32,
        /// Available bytes in the packed package-file payload.
        blob_bytes: u32,
    },
    /// Package ordering rejection.
    PackageSort {
        /// Zero-based package row that violates canonical package ordering.
        index: u32,
    },
    /// Declaration package outside package plane.
    DeclarationPackage {
        /// Zero-based declaration row with the invalid package coordinate.
        index: u32,
        /// Number of package rows available in the image.
        package_count: u32,
    },
    /// Signature parameter position half-present.
    SignatureParameterPosition {
        /// Zero-based signature-parameter row with a half-present owner or position coordinate.
        index: u32,
    },
    /// Method-set owner outside type plane.
    MethodSetOwner {
        /// Zero-based method-set row carrying the owner coordinate.
        index: u32,
        /// Zero-based type row named as method-set owner.
        owner: u32,
        /// Number of rows in the type plane, which bounds valid owners.
        type_count: u32,
    },
    /// Method-set owner had the wrong type kind.
    MethodSetOwnerKind {
        /// Zero-based method-set row with the wrong owner kind.
        index: u32,
        /// Closed type-row kind decoded for the owner; Go method sets are recorded for interfaces.
        kind: GoImageTypeKind,
    },
    /// Method-set type root outside type plane.
    MethodSetTypeRoot {
        /// Zero-based method-set row carrying the invalid method type root.
        index: u32,
        /// Zero-based recursive type row referenced by the method-set entry.
        root: u32,
        /// Number of rows in the type plane, which bounds valid roots.
        type_count: u32,
    },
    /// Method-set ordering rejection.
    MethodSetSort {
        /// Zero-based method-set row that violates canonical ordering.
        index: u32,
    },
    /// Signature parameter owner outside type plane.
    SignatureParameterOwner {
        /// Zero-based signature-parameter row carrying the owner coordinate.
        index: u32,
        /// Zero-based function type row named as the parameter owner.
        owner: u32,
        /// Number of rows in the type plane, which bounds valid owners.
        type_count: u32,
    },
    /// Signature parameter owner disagreed with its containing row.
    SignatureParameterOwnerRow {
        /// Zero-based signature-parameter row being checked.
        index: u32,
        /// Function type row encoded as this parameter’s owner.
        owner: u32,
        /// Function type row whose signature contains this parameter.
        expected: u32,
    },
    /// Signature parameter ordinal disagreed with its run position.
    SignatureParameterOrdinal {
        /// Zero-based signature-parameter row being checked.
        index: u32,
        /// Parameter position encoded in the row.
        ordinal: u32,
        /// Zero-based position dictated by the row’s location within the owner’s parameter run.
        expected: u32,
    },
    /// Signature parameter plane tiling rejection.
    SignatureParameterTiling {
        /// Total signature-parameter entries declared by function rows.
        declared: u32,
        /// Number of parameter rows physically present in the signature-parameter plane.
        plane: u32,
    },
    /// Required name was empty.
    EmptyName {
        /// Go image plane containing the required name atom.
        plane: GoImagePlane,
        /// Zero-based row whose required name atom is empty.
        index: u32,
    },
    /// Atom range escaped the byte plane.
    AtomRange {
        /// Go image plane containing the atom reference.
        plane: GoImagePlane,
        /// Zero-based row whose atom reference was checked.
        index: u32,
        /// First byte of the atom in concatenated atom storage.
        offset: u32,
        /// Encoded atom length in bytes.
        length: u32,
        /// Total byte length of concatenated atom storage.
        atom_bytes: u32,
    },
    /// Atom bytes were not UTF-8.
    AtomUtf8 {
        /// Go image plane containing the atom reference.
        plane: GoImagePlane,
        /// Zero-based row whose atom bytes are not valid UTF-8.
        index: u32,
    },
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
    Image {
        /// Exact closed Go image rejection retained by the projection boundary.
        cause: GoImageFault,
    },
    /// A projection index could not fit the compact lane.
    IndexCapacity {
        /// Closed Go projection operation whose coordinate exceeded the compact lane width.
        phase: GoProjectionIndexPhase,
        /// Exact host-sized coordinate before checked conversion to the compact wire width.
        observed: u64,
    },
    /// The producer-bounded recursive type graph exceeded its depth budget.
    Depth {
        /// Zero-based recursive Go type row at which projection exceeded the producer’s depth budget.
        type_row: u32,
    },
    /// A variadic signature had no final typed parameter.
    VariadicWithoutParameter {
        /// Zero-based signature row marked variadic without a final typed parameter.
        signature: u32,
    },
    /// No pushed fact could own an anonymous compound row.
    Anchor {
        /// Zero-based authority row for an anonymous compound type that no emitted fact could own.
        owner: u32,
    },
    /// A field or method list exceeded its bounded pool.
    ListCapacity {
        /// Zero-based projected owner whose bounded member or method list was full.
        owner: u32,
        /// Closed list lane that reached its element limit.
        phase: GoProjectionListPhase,
    },
    /// A same-package reference named no declared target.
    OrphanTarget {
        /// Zero-based same-package reference row whose named declaration target was not emitted.
        reference: u32,
    },
    /// A foreign target key failed grammar validation.
    ForeignKey {
        /// Zero-based resolved reference row whose external path failed canonical-key grammar.
        reference: u32,
        /// Exact closed foreign-key grammar rule violated by the target spelling.
        cause: ProjectionForeignKeyFault,
    },
    /// A package lineage failed grammar validation.
    PackageLineage {
        /// Zero-based reference row whose package lineage failed canonical grammar.
        reference: u32,
        /// Exact closed package-lineage grammar rule violated by the target spelling.
        cause: ProjectionPackageLineageFault,
    },
    /// An authority atom violated its UTF-8 promise.
    AtomUtf8 {
        /// Go authority image plane that supplied the atom.
        plane: GoImagePlane,
        /// Zero-based row whose atom bytes did not satisfy the plane’s UTF-8 promise.
        row: u32,
    },
    /// A relative occurrence span was inverted.
    RelativeSpan {
        /// Zero-based authority row carrying the relative source span.
        row: u32,
        /// Inclusive source-byte start relative to the entered Go file.
        start: u32,
        /// Exclusive source-byte end relative to the entered Go file.
        end: u32,
    },
    /// A member, doc, or occurrence owner had no pushed fact.
    OrphanOwner {
        /// Zero-based authority owner row that had no corresponding emitted fact.
        owner: u32,
    },
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
const _: () = assert!(size_of::<GoProjectionFault>() <= 64);
