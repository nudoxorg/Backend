//! Typed JSON oracle for compiler lowering terminals.

use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLoweringCause {
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
        cause: GoldenProjectionAdmissionFault,
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
        fault: GoldenJavaProjectionFault,
    },
    GoProjection {
        fault: GoldenGoProjectionFault,
    },
    TypeScriptProjection {
        fault: GoldenTypeScriptProjectionFault,
    },
    PythonProjection {
        fault: GoldenPythonProjectionFault,
    },
    CSharpProjection {
        fault: GoldenCSharpProjectionFault,
    },
    ClangProjection {
        fault: GoldenClangProjectionFault,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionConstructorTag {
    Function,
    Generic,
    Tuple,
    Array,
    Union,
    Intersection,
    Product,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionConstructorFault {
    Tag {
        actual: u32,
    },
    ReservedPayload {
        tag: GoldenProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    ArityOverflow {
        tag: GoldenProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    Arity {
        tag: GoldenProjectionConstructorTag,
        expected: u32,
        actual: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionChildRole {
    FunctionParameter,
    FunctionResult,
    GenericArgument,
    TupleElement,
    ArrayElement,
    UnionMember,
    IntersectionMember,
    ProductMember,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionSemanticTypeTag {
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
    CQualified,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionTypeCell {
    Payload0,
    Payload1,
    Text,
    Text2,
    Nominal,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionSemanticTypeFault {
    Tag {
        actual: u8,
    },
    ReservedCell {
        tag: GoldenProjectionSemanticTypeTag,
        cell: GoldenProjectionTypeCell,
        actual: u32,
    },
    MissingCell {
        tag: GoldenProjectionSemanticTypeTag,
        cell: GoldenProjectionTypeCell,
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
        tag: GoldenProjectionSemanticTypeTag,
        min: u32,
        max: u32,
        actual: u32,
    },
    ChildNameForbidden {
        tag: GoldenProjectionSemanticTypeTag,
        position: u32,
    },
    ChildNameRequired {
        tag: GoldenProjectionSemanticTypeTag,
        position: u32,
    },
    ChildFlagsForbidden {
        tag: GoldenProjectionSemanticTypeTag,
        position: u32,
        actual: u8,
    },
    VariadicParameter {
        position: u32,
        actual: u8,
    },
    ChildTextForbidden {
        tag: GoldenProjectionSemanticTypeTag,
        position: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionTypeChildLane {
    Declared,
    Anonymous,
    Computed,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionFactLane {
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
    AtomLists,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionParentageState {
    Unavailable,
    Root,
    Bound { parent: u32 },
    UnrepresentedAuthorityOwner { identity: [u8; 16] },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenProjectionSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenProjectionAdmissionFault {
    EmptyName,
    Capacity,
    ChildCapacity,
    ProductChildPoolCapacity {
        used: u64,
        requested: u64,
        capacity: u64,
    },
    Constructor {
        cause: GoldenProjectionConstructorFault,
    },
    ChildRole {
        position: u64,
        expected: GoldenProjectionChildRole,
        actual: GoldenProjectionChildRole,
    },
    ChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeRecord {
        cause: GoldenProjectionSemanticTypeFault,
    },
    TypeChild {
        position: u64,
        cause: GoldenProjectionSemanticTypeFault,
    },
    TypeChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    TypeChildCapacity,
    TypeChildPoolCapacity {
        lane: GoldenProjectionTypeChildLane,
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
        lane: GoldenProjectionFactLane,
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
        existing: GoldenProjectionSpan,
        requested: GoldenProjectionSpan,
    },
    ConflictingParentage {
        entity: u32,
        existing: GoldenProjectionParentageState,
        requested: GoldenProjectionParentageState,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaProjectionFault {
    Image {
        cause: GoldenJavaImageFault,
    },
    Depth {
        type_row: u32,
    },
    MalformedType {
        type_row: u32,
        kind: GoldenJavaTypeKind,
    },
    Primitive {
        type_row: u32,
    },
    AtomUtf8 {
        symbol: u32,
        atom: GoldenJavaSymbolAtom,
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
        cause: GoldenForeignKeyFault,
    },
    SiblingCapacity {
        symbol: u32,
    },
    IndexCapacity {
        phase: GoldenJavaIndexPhase,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaImageFault {
    Header {
        cause: GoldenJavaHeaderFault,
    },
    Section {
        plane: GoldenJavaImagePlane,
        cause: GoldenJavaSectionFault,
    },
    Digest,
    Atom {
        index: u32,
        cause: GoldenJavaAtomFault,
    },
    AbsentAtom,
    Coordinate {
        plane: GoldenJavaImagePlane,
        index: u32,
        upper_bound: u32,
    },
    Tag {
        plane: GoldenJavaImagePlane,
        found: u8,
    },
    ChildRange {
        plane: GoldenJavaImagePlane,
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

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaHeaderFault {
    Truncated { actual: u32 },
    Magic { found: [u8; 4] },
    Version { found: u16 },
    Length { found: u16 },
    Release { found: u16 },
    SectionCount { found: u16 },
    BodyLength { declared: u32, actual: u32 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaSectionFault {
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

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaAtomFault {
    NonCanonicalOffset { found: u32 },
    Range,
    Utf8,
    TrailingBytes,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaTypeKind {
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
    Null,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaSymbolAtom {
    Owner,
    Name,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaIndexPhase {
    FactOrdinal,
    TypeRow,
    TypeChild,
    NameIndex,
    SymbolIndex,
    ExecutableIndex,
    Signature,
    Documentation,
    Utf16,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaImagePlane {
    Atoms,
    AtomBytes,
    Types,
    TypeChildren,
    Symbols,
    SymbolParameters,
    Declarations,
    References,
    DeclarationExtensions,
    ExtensionEntries,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenForeignKeyFault {
    EmptyPath,
    BackslashInPath,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageLineageFault {
    EmptyEcosystem,
    EmptyPackage,
    SeparatorInEcosystem,
    SeparatorInPackage,
    Backslash { part: GoldenLineagePart },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLineagePart {
    Ecosystem,
    Package,
    Invalid { segment: u8 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoProjectionFault {
    Image {
        cause: GoldenGoImageFault,
    },
    IndexCapacity {
        phase: GoldenGoProjectionIndexPhase,
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
        phase: GoldenGoProjectionListPhase,
    },
    OrphanTarget {
        reference: u32,
    },
    ForeignKey {
        reference: u32,
        cause: GoldenForeignKeyFault,
    },
    PackageLineage {
        reference: u32,
        cause: GoldenPackageLineageFault,
    },
    AtomUtf8 {
        plane: GoldenGoImagePlane,
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
        cause: GoldenProjectionAdmissionFault,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImagePlane {
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
    Module,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImageFlagCell {
    Exported,
    PointerReceiver,
    Promoted,
    Embedded,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImageDeclarationKind {
    Type,
    Alias,
    Function,
    Constant,
    Static,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImageTypeKind {
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
    Invalid,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImageHeaderFault {
    Truncated { actual: u32 },
    Magic { found: [u8; 4] },
    Version { found: u16 },
    Length { found: u32 },
    BodyLength { declared: u32, actual: u32 },
    Reserved,
    ModuleCount { found: u32 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoImageFault {
    Header {
        cause: GoldenGoImageHeaderFault,
    },
    Digest,
    RowBounds {
        plane: GoldenGoImagePlane,
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
        kind: GoldenGoImageTypeKind,
    },
    TypeNameForbidden {
        index: u32,
        kind: GoldenGoImageTypeKind,
    },
    TypeDirection {
        index: u32,
        found: u8,
    },
    TypeDirectionCell {
        index: u32,
        kind: GoldenGoImageTypeKind,
    },
    TypeVariadicFlag {
        index: u32,
        found: u8,
    },
    TypeVariadicCell {
        index: u32,
        kind: GoldenGoImageTypeKind,
    },
    ArrayLength {
        index: u32,
        length: i64,
    },
    TypeParamCount {
        index: u32,
        kind: GoldenGoImageTypeKind,
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
        cell: GoldenGoImageFlagCell,
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
        cell: GoldenGoImageFlagCell,
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
        kind: GoldenGoImageDeclarationKind,
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
        kind: GoldenGoImageTypeKind,
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
        plane: GoldenGoImagePlane,
        index: u32,
    },
    AtomRange {
        plane: GoldenGoImagePlane,
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    AtomUtf8 {
        plane: GoldenGoImagePlane,
        index: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoProjectionIndexPhase {
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
    EntityList,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoProjectionListPhase {
    Entity,
    Type,
    Atom,
    TypeParameter,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenTypeScriptProjectionFault {
    ForeignKey {
        start: u32,
        end: u32,
        cause: GoldenForeignKeyFault,
    },
    PackageLineage {
        start: u32,
        end: u32,
        cause: GoldenPackageLineageFault,
    },
    CoordinateOverflow {
        value: u64,
    },
    MissingImportBinding {
        fact: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPythonProjectionFault {
    ForeignSpellingUtf8 {
        start: u32,
        end: u32,
    },
    ForeignKey {
        start: u32,
        end: u32,
        cause: GoldenForeignKeyFault,
    },
    PackageLineage {
        start: u32,
        end: u32,
        cause: GoldenPackageLineageFault,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpProjectionFault {
    Image {
        cause: GoldenCSharpImageFault,
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
        phase: GoldenCSharpProjectionIndexPhase,
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
        cause: GoldenProjectionAdmissionFault,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpProjectionIndexPhase {
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
    SourceSpan,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageFault {
    Header {
        cause: GoldenCSharpImageHeaderFault,
    },
    Digest,
    DeclarationKind {
        index: u32,
        found: u8,
        plane: GoldenCSharpImageSection,
    },
    DeclarationReserved {
        index: u32,
        plane: GoldenCSharpImageSection,
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
        kind: GoldenCSharpImageTypeKind,
        min: u32,
        max: u32,
        actual: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageHeaderFault {
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

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageSection {
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
    References,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageTypeKind {
    Named,
    Array,
    Pointer,
    NullableValue,
    Tuple,
    FunctionPointer,
    TypeParameter,
    Dynamic,
    Error,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangProjectionFault {
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
        kind: GoldenClangTypeKind,
        qualifiers: GoldenClangQualifiers,
    },
    IllegalMemberPointerOwner {
        pointer: u32,
        owner: u32,
        kind: GoldenClangTypeKind,
        declaration: GoldenClangDeclaration,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenClangQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangDeclaration {
    Known { identity: [u8; 16] },
    Unavailable,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangTypeKind {
    Unknown,
    Builtin,
    Named,
    Pointer,
    BlockPointer,
    MemberPointer,
    LvalueReference,
    RvalueReference,
    Array,
    Function,
}

macro_rules! impl_unit_conversion {
    ($source:ident => $target:ident [$($variant:ident),+ $(,)?]) => {
        impl From<compiler_vocabulary::$source> for $target {
            fn from(source: compiler_vocabulary::$source) -> Self {
                match source {
                    $(compiler_vocabulary::$source::$variant => Self::$variant),+
                }
            }
        }
    };
}

impl From<compiler_vocabulary::LoweringUnsupported> for GoldenLoweringCause {
    fn from(cause: compiler_vocabulary::LoweringUnsupported) -> Self {
        use compiler_vocabulary::LoweringUnsupported;
        match cause {
            LoweringUnsupported::NoSupportedDeclaration => Self::NoSupportedDeclaration,
            LoweringUnsupported::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            } => Self::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            },
            LoweringUnsupported::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            } => Self::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            },
            LoweringUnsupported::FactRejected {
                fact,
                name_len,
                cause,
            } => Self::FactRejected {
                fact,
                name_len,
                cause: cause.into(),
            },
            LoweringUnsupported::RustFunction => Self::RustFunction,
            LoweringUnsupported::RustConstantType => Self::RustConstantType,
            LoweringUnsupported::RustGenericParameter => Self::RustGenericParameter,
            LoweringUnsupported::PythonAssignmentName => Self::PythonAssignmentName,
            LoweringUnsupported::PythonAssignmentValue => Self::PythonAssignmentValue,
            LoweringUnsupported::ClangDeclarationForm => Self::ClangDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationForm => Self::TypeScriptDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationType => Self::TypeScriptDeclarationType,
            LoweringUnsupported::CSharpDeclarationForm => Self::CSharpDeclarationForm,
            LoweringUnsupported::CSharpDeclarationType => Self::CSharpDeclarationType,
            LoweringUnsupported::GoDeclarationForm => Self::GoDeclarationForm,
            LoweringUnsupported::GoDeclarationType => Self::GoDeclarationType,
            LoweringUnsupported::JavaDeclarationForm => Self::JavaDeclarationForm,
            LoweringUnsupported::JavaProjection { fault } => Self::JavaProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::GoProjection { fault } => Self::GoProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::TypeScriptProjection { fault } => Self::TypeScriptProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::PythonProjection { fault } => Self::PythonProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::CSharpProjection { fault } => Self::CSharpProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::ClangProjection { fault } => Self::ClangProjection {
                fault: fault.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::JavaProjectionFault> for GoldenJavaProjectionFault {
    fn from(fault: compiler_vocabulary::JavaProjectionFault) -> Self {
        use compiler_vocabulary::JavaProjectionFault;
        match fault {
            JavaProjectionFault::Image { cause } => Self::Image {
                cause: cause.into(),
            },
            JavaProjectionFault::Depth { type_row } => Self::Depth { type_row },
            JavaProjectionFault::MalformedType { type_row, kind } => Self::MalformedType {
                type_row,
                kind: kind.into(),
            },
            JavaProjectionFault::Primitive { type_row } => Self::Primitive { type_row },
            JavaProjectionFault::AtomUtf8 { symbol, atom } => Self::AtomUtf8 {
                symbol,
                atom: atom.into(),
            },
            JavaProjectionFault::SourceUtf8 => Self::SourceUtf8,
            JavaProjectionFault::Utf16Offset {
                units,
                source_utf16_len,
            } => Self::Utf16Offset {
                units,
                source_utf16_len,
            },
            JavaProjectionFault::Utf16Range { start, end } => Self::Utf16Range { start, end },
            JavaProjectionFault::OrphanOwner { owner } => Self::OrphanOwner { owner },
            JavaProjectionFault::ForeignKey { cause } => Self::ForeignKey {
                cause: GoldenForeignKeyFault::from(cause),
            },
            JavaProjectionFault::SiblingCapacity { symbol } => Self::SiblingCapacity { symbol },
            JavaProjectionFault::IndexCapacity { phase } => Self::IndexCapacity {
                phase: phase.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::JavaImageFault> for GoldenJavaImageFault {
    fn from(fault: compiler_vocabulary::JavaImageFault) -> Self {
        use compiler_vocabulary::JavaImageFault;
        match fault {
            JavaImageFault::Header { cause } => Self::Header {
                cause: cause.into(),
            },
            JavaImageFault::Section { plane, cause } => Self::Section {
                plane: plane.into(),
                cause: cause.into(),
            },
            JavaImageFault::Digest => Self::Digest,
            JavaImageFault::Atom { index, cause } => Self::Atom {
                index,
                cause: cause.into(),
            },
            JavaImageFault::AbsentAtom => Self::AbsentAtom,
            JavaImageFault::Coordinate {
                plane,
                index,
                upper_bound,
            } => Self::Coordinate {
                plane: plane.into(),
                index,
                upper_bound,
            },
            JavaImageFault::Tag { plane, found } => Self::Tag {
                plane: plane.into(),
                found,
            },
            JavaImageFault::ChildRange {
                plane,
                start,
                count,
                upper_bound,
            } => Self::ChildRange {
                plane: plane.into(),
                start,
                count,
                upper_bound,
            },
            JavaImageFault::ReferenceRange { start, end } => Self::ReferenceRange { start, end },
            JavaImageFault::DocumentationPresence => Self::DocumentationPresence,
            JavaImageFault::ModifierBits { found } => Self::ModifierBits { found },
            JavaImageFault::RecordComponentKind { index } => Self::RecordComponentKind { index },
            JavaImageFault::ExtensionReserved => Self::ExtensionReserved,
        }
    }
}

impl From<compiler_vocabulary::JavaImageHeaderFault> for GoldenJavaHeaderFault {
    fn from(fault: compiler_vocabulary::JavaImageHeaderFault) -> Self {
        use compiler_vocabulary::JavaImageHeaderFault;
        match fault {
            JavaImageHeaderFault::Truncated { actual } => Self::Truncated { actual },
            JavaImageHeaderFault::Magic { found } => Self::Magic { found },
            JavaImageHeaderFault::Version { found } => Self::Version { found },
            JavaImageHeaderFault::Length { found } => Self::Length { found },
            JavaImageHeaderFault::Release { found } => Self::Release { found },
            JavaImageHeaderFault::SectionCount { found } => Self::SectionCount { found },
            JavaImageHeaderFault::BodyLength { declared, actual } => {
                Self::BodyLength { declared, actual }
            }
        }
    }
}

impl From<compiler_vocabulary::JavaImageSectionFault> for GoldenJavaSectionFault {
    fn from(fault: compiler_vocabulary::JavaImageSectionFault) -> Self {
        use compiler_vocabulary::JavaImageSectionFault;
        match fault {
            JavaImageSectionFault::Tag { expected, found } => Self::Tag { expected, found },
            JavaImageSectionFault::RowBytes { expected, found } => {
                Self::RowBytes { expected, found }
            }
            JavaImageSectionFault::ByteCount {
                count,
                row_bytes,
                found,
            } => Self::ByteCount {
                count,
                row_bytes,
                found,
            },
            JavaImageSectionFault::Offset { expected, found } => Self::Offset { expected, found },
            JavaImageSectionFault::Range {
                offset,
                length,
                image_bytes,
            } => Self::Range {
                offset,
                length,
                image_bytes,
            },
        }
    }
}

impl From<compiler_vocabulary::JavaImageAtomFault> for GoldenJavaAtomFault {
    fn from(fault: compiler_vocabulary::JavaImageAtomFault) -> Self {
        use compiler_vocabulary::JavaImageAtomFault;
        match fault {
            JavaImageAtomFault::NonCanonicalOffset { found } => Self::NonCanonicalOffset { found },
            JavaImageAtomFault::Range => Self::Range,
            JavaImageAtomFault::Utf8 => Self::Utf8,
            JavaImageAtomFault::TrailingBytes => Self::TrailingBytes,
        }
    }
}

impl_unit_conversion!(JavaProjectionTypeKind => GoldenJavaTypeKind [Primitive, Void, Declared, Array, Variable, Wildcard, Intersection, Union, Error, None, Null]);
impl_unit_conversion!(JavaSymbolAtom => GoldenJavaSymbolAtom [Owner, Name]);
impl_unit_conversion!(JavaForeignKeyFault => GoldenForeignKeyFault [EmptyPath, BackslashInPath]);
impl_unit_conversion!(JavaProjectionIndexPhase => GoldenJavaIndexPhase [FactOrdinal, TypeRow, TypeChild, NameIndex, SymbolIndex, ExecutableIndex, Signature, Documentation, Utf16]);
impl_unit_conversion!(JavaImagePlane => GoldenJavaImagePlane [Atoms, AtomBytes, Types, TypeChildren, Symbols, SymbolParameters, Declarations, References, DeclarationExtensions, ExtensionEntries]);
impl_unit_conversion!(ProjectionForeignKeyFault => GoldenForeignKeyFault [EmptyPath, BackslashInPath]);
impl From<compiler_vocabulary::ProjectionLineagePart> for GoldenLineagePart {
    fn from(part: compiler_vocabulary::ProjectionLineagePart) -> Self {
        match part {
            compiler_vocabulary::ProjectionLineagePart::Ecosystem => Self::Ecosystem,
            compiler_vocabulary::ProjectionLineagePart::Package => Self::Package,
            compiler_vocabulary::ProjectionLineagePart::Invalid { segment } => {
                Self::Invalid { segment }
            }
        }
    }
}

impl From<compiler_vocabulary::ProjectionPackageLineageFault> for GoldenPackageLineageFault {
    fn from(fault: compiler_vocabulary::ProjectionPackageLineageFault) -> Self {
        use compiler_vocabulary::ProjectionPackageLineageFault;
        match fault {
            ProjectionPackageLineageFault::EmptyEcosystem => Self::EmptyEcosystem,
            ProjectionPackageLineageFault::EmptyPackage => Self::EmptyPackage,
            ProjectionPackageLineageFault::SeparatorInEcosystem => Self::SeparatorInEcosystem,
            ProjectionPackageLineageFault::SeparatorInPackage => Self::SeparatorInPackage,
            ProjectionPackageLineageFault::Backslash { part } => {
                Self::Backslash { part: part.into() }
            }
        }
    }
}

impl_unit_conversion!(ProjectionConstructorTag => GoldenProjectionConstructorTag [Function, Generic, Tuple, Array, Union, Intersection, Product]);
impl_unit_conversion!(ProjectionChildRole => GoldenProjectionChildRole [FunctionParameter, FunctionResult, GenericArgument, TupleElement, ArrayElement, UnionMember, IntersectionMember, ProductMember]);
impl_unit_conversion!(ProjectionSemanticTypeTag => GoldenProjectionSemanticTypeTag [SelfType, Primitive, Tuple, Slice, Array, Union, Intersection, Never, Any, Unknown, Nominal, Apply, TypeVar, Wildcard, FunctionPointer, Annotated, Conditional, Mapped, TemplateLiteral, AnonymousRecord, ImplTrait, DynTrait, Inferred, QualifiedPath, Map, Channel, ArraySequence, ArrayRectangular, ArrayFixed, ArrayConstExpression, ArrayIncomplete, CQualified]);
impl_unit_conversion!(ProjectionTypeCell => GoldenProjectionTypeCell [Payload0, Payload1, Text, Text2, Nominal]);
impl_unit_conversion!(ProjectionTypeChildLane => GoldenProjectionTypeChildLane [Declared, Anonymous, Computed]);
impl_unit_conversion!(ProjectionFactLane => GoldenProjectionFactLane [TypeRows, ReservedTypeRows, ComputedOwners, Extensions, ReplacementTypeParameterRange, TypeParameterRanges, CapturedTypeParameterRange, EntityMembers, EntityParentage, EntityParents, EntitySourceSpans, TypeParameters, TypeLists, EntityLists, AtomLists]);

impl From<compiler_vocabulary::ProjectionConstructorFault> for GoldenProjectionConstructorFault {
    fn from(fault: compiler_vocabulary::ProjectionConstructorFault) -> Self {
        use compiler_vocabulary::ProjectionConstructorFault;
        match fault {
            ProjectionConstructorFault::Tag { actual } => Self::Tag { actual },
            ProjectionConstructorFault::ReservedPayload {
                tag,
                payload0,
                payload1,
            } => Self::ReservedPayload {
                tag: tag.into(),
                payload0,
                payload1,
            },
            ProjectionConstructorFault::ArityOverflow {
                tag,
                payload0,
                payload1,
            } => Self::ArityOverflow {
                tag: tag.into(),
                payload0,
                payload1,
            },
            ProjectionConstructorFault::Arity {
                tag,
                expected,
                actual,
            } => Self::Arity {
                tag: tag.into(),
                expected,
                actual,
            },
        }
    }
}

impl From<compiler_vocabulary::ProjectionSemanticTypeFault> for GoldenProjectionSemanticTypeFault {
    fn from(fault: compiler_vocabulary::ProjectionSemanticTypeFault) -> Self {
        use compiler_vocabulary::ProjectionSemanticTypeFault;
        match fault {
            ProjectionSemanticTypeFault::Tag { actual } => Self::Tag { actual },
            ProjectionSemanticTypeFault::ReservedCell { tag, cell, actual } => Self::ReservedCell {
                tag: tag.into(),
                cell: cell.into(),
                actual,
            },
            ProjectionSemanticTypeFault::MissingCell { tag, cell } => Self::MissingCell {
                tag: tag.into(),
                cell: cell.into(),
            },
            ProjectionSemanticTypeFault::Reason { actual } => Self::Reason { actual },
            ProjectionSemanticTypeFault::PrimitiveShape { actual } => {
                Self::PrimitiveShape { actual }
            }
            ProjectionSemanticTypeFault::CvQualifiers { actual } => Self::CvQualifiers { actual },
            ProjectionSemanticTypeFault::Width { actual } => Self::Width { actual },
            ProjectionSemanticTypeFault::ChildCount {
                tag,
                min,
                max,
                actual,
            } => Self::ChildCount {
                tag: tag.into(),
                min,
                max,
                actual,
            },
            ProjectionSemanticTypeFault::ChildNameForbidden { tag, position } => {
                Self::ChildNameForbidden {
                    tag: tag.into(),
                    position,
                }
            }
            ProjectionSemanticTypeFault::ChildNameRequired { tag, position } => {
                Self::ChildNameRequired {
                    tag: tag.into(),
                    position,
                }
            }
            ProjectionSemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual,
            } => Self::ChildFlagsForbidden {
                tag: tag.into(),
                position,
                actual,
            },
            ProjectionSemanticTypeFault::VariadicParameter { position, actual } => {
                Self::VariadicParameter { position, actual }
            }
            ProjectionSemanticTypeFault::ChildTextForbidden { tag, position } => {
                Self::ChildTextForbidden {
                    tag: tag.into(),
                    position,
                }
            }
        }
    }
}

impl From<compiler_vocabulary::ProjectionParentageState> for GoldenProjectionParentageState {
    fn from(state: compiler_vocabulary::ProjectionParentageState) -> Self {
        match state {
            compiler_vocabulary::ProjectionParentageState::Unavailable => Self::Unavailable,
            compiler_vocabulary::ProjectionParentageState::Root => Self::Root,
            compiler_vocabulary::ProjectionParentageState::Bound { parent } => {
                Self::Bound { parent }
            }
            compiler_vocabulary::ProjectionParentageState::UnrepresentedAuthorityOwner {
                identity,
            } => Self::UnrepresentedAuthorityOwner { identity },
        }
    }
}

impl From<compiler_vocabulary::ProjectionSpan> for GoldenProjectionSpan {
    fn from(span: compiler_vocabulary::ProjectionSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}

impl From<compiler_vocabulary::ProjectionAdmissionFault> for GoldenProjectionAdmissionFault {
    fn from(fault: compiler_vocabulary::ProjectionAdmissionFault) -> Self {
        use compiler_vocabulary::ProjectionAdmissionFault;
        match fault {
            ProjectionAdmissionFault::EmptyName => Self::EmptyName,
            ProjectionAdmissionFault::Capacity => Self::Capacity,
            ProjectionAdmissionFault::ChildCapacity => Self::ChildCapacity,
            ProjectionAdmissionFault::ProductChildPoolCapacity {
                used,
                requested,
                capacity,
            } => Self::ProductChildPoolCapacity {
                used,
                requested,
                capacity,
            },
            ProjectionAdmissionFault::Constructor { cause } => Self::Constructor {
                cause: cause.into(),
            },
            ProjectionAdmissionFault::ChildRole {
                position,
                expected,
                actual,
            } => Self::ChildRole {
                position,
                expected: expected.into(),
                actual: actual.into(),
            },
            ProjectionAdmissionFault::ChildTarget {
                position,
                target,
                fact_count,
            } => Self::ChildTarget {
                position,
                target,
                fact_count,
            },
            ProjectionAdmissionFault::TypeRecord { cause } => Self::TypeRecord {
                cause: cause.into(),
            },
            ProjectionAdmissionFault::TypeChild { position, cause } => Self::TypeChild {
                position,
                cause: cause.into(),
            },
            ProjectionAdmissionFault::TypeChildTarget {
                position,
                target,
                fact_count,
            } => Self::TypeChildTarget {
                position,
                target,
                fact_count,
            },
            ProjectionAdmissionFault::TypeChildCapacity => Self::TypeChildCapacity,
            ProjectionAdmissionFault::TypeChildPoolCapacity {
                lane,
                used,
                requested,
                capacity,
            } => Self::TypeChildPoolCapacity {
                lane: lane.into(),
                used,
                requested,
                capacity,
            },
            ProjectionAdmissionFault::TypeRowCapacity => Self::TypeRowCapacity,
            ProjectionAdmissionFault::ComputedRowCapacity => Self::ComputedRowCapacity,
            ProjectionAdmissionFault::OccurrenceOwner { owner, fact_count } => {
                Self::OccurrenceOwner { owner, fact_count }
            }
            ProjectionAdmissionFault::OccurrenceCapacity => Self::OccurrenceCapacity,
            ProjectionAdmissionFault::DocOwner { owner, fact_count } => {
                Self::DocOwner { owner, fact_count }
            }
            ProjectionAdmissionFault::DocCapacity => Self::DocCapacity,
            ProjectionAdmissionFault::ExtensionAtomCapacity => Self::ExtensionAtomCapacity,
            ProjectionAdmissionFault::TypeParameterCapacity => Self::TypeParameterCapacity,
            ProjectionAdmissionFault::TypeParameterBoundCapacity {
                requested,
                available,
            } => Self::TypeParameterBoundCapacity {
                requested,
                available,
            },
            ProjectionAdmissionFault::RefListCapacity => Self::RefListCapacity,
            ProjectionAdmissionFault::RefListElements => Self::RefListElements,
            ProjectionAdmissionFault::RefTarget {
                lane,
                raw,
                fact_count,
            } => Self::RefTarget {
                lane: lane.into(),
                raw,
                fact_count,
            },
            ProjectionAdmissionFault::SourceSpan {
                entity,
                start,
                end,
                source_len,
            } => Self::SourceSpan {
                entity,
                start,
                end,
                source_len,
            },
            ProjectionAdmissionFault::ConflictingSourceSpan {
                entity,
                existing,
                requested,
            } => Self::ConflictingSourceSpan {
                entity,
                existing: existing.into(),
                requested: requested.into(),
            },
            ProjectionAdmissionFault::ConflictingParentage {
                entity,
                existing,
                requested,
            } => Self::ConflictingParentage {
                entity,
                existing: existing.into(),
                requested: requested.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::GoProjectionFault> for GoldenGoProjectionFault {
    fn from(fault: compiler_vocabulary::GoProjectionFault) -> Self {
        use compiler_vocabulary::GoProjectionFault;
        match fault {
            GoProjectionFault::Image { cause } => Self::Image {
                cause: cause.into(),
            },
            GoProjectionFault::IndexCapacity { phase, observed } => Self::IndexCapacity {
                phase: phase.into(),
                observed,
            },
            GoProjectionFault::Depth { type_row } => Self::Depth { type_row },
            GoProjectionFault::VariadicWithoutParameter { signature } => {
                Self::VariadicWithoutParameter { signature }
            }
            GoProjectionFault::Anchor { owner } => Self::Anchor { owner },
            GoProjectionFault::ListCapacity { owner, phase } => Self::ListCapacity {
                owner,
                phase: phase.into(),
            },
            GoProjectionFault::OrphanTarget { reference } => Self::OrphanTarget { reference },
            GoProjectionFault::ForeignKey { reference, cause } => Self::ForeignKey {
                reference,
                cause: cause.into(),
            },
            GoProjectionFault::PackageLineage { reference, cause } => Self::PackageLineage {
                reference,
                cause: cause.into(),
            },
            GoProjectionFault::AtomUtf8 { plane, row } => Self::AtomUtf8 {
                plane: plane.into(),
                row,
            },
            GoProjectionFault::RelativeSpan { row, start, end } => {
                Self::RelativeSpan { row, start, end }
            }
            GoProjectionFault::OrphanOwner { owner } => Self::OrphanOwner { owner },
            GoProjectionFault::Admission {
                fact,
                name_len,
                cause,
            } => Self::Admission {
                fact,
                name_len,
                cause: cause.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::GoImageFault> for GoldenGoImageFault {
    fn from(fault: compiler_vocabulary::GoImageFault) -> Self {
        use compiler_vocabulary::GoImageFault;
        match fault {
            GoImageFault::Header { cause } => Self::Header {
                cause: cause.into(),
            },
            GoImageFault::Digest => Self::Digest,
            GoImageFault::RowBounds {
                plane,
                index,
                count,
            } => Self::RowBounds {
                plane: plane.into(),
                index,
                count,
            },
            GoImageFault::DeclarationKind { index, found } => {
                Self::DeclarationKind { index, found }
            }
            GoImageFault::ExportedFlag { index, found } => Self::ExportedFlag { index, found },
            GoImageFault::DeclarationIota { index, found } => {
                Self::DeclarationIota { index, found }
            }
            GoImageFault::DeclarationReserved { index } => Self::DeclarationReserved { index },
            GoImageFault::TypeReserved { index } => Self::TypeReserved { index },
            GoImageFault::MethodReserved { index } => Self::MethodReserved { index },
            GoImageFault::MemberReserved { index } => Self::MemberReserved { index },
            GoImageFault::DocReserved { index } => Self::DocReserved { index },
            GoImageFault::ReferenceReserved { index } => Self::ReferenceReserved { index },
            GoImageFault::DeclarationTypeRoot {
                index,
                root,
                type_count,
            } => Self::DeclarationTypeRoot {
                index,
                root,
                type_count,
            },
            GoImageFault::DeclarationSpan { index, start, end } => {
                Self::DeclarationSpan { index, start, end }
            }
            GoImageFault::TypeKind { index, found } => Self::TypeKind { index, found },
            GoImageFault::TypeNameRequired { index, kind } => Self::TypeNameRequired {
                index,
                kind: kind.into(),
            },
            GoImageFault::TypeNameForbidden { index, kind } => Self::TypeNameForbidden {
                index,
                kind: kind.into(),
            },
            GoImageFault::TypeDirection { index, found } => Self::TypeDirection { index, found },
            GoImageFault::TypeDirectionCell { index, kind } => Self::TypeDirectionCell {
                index,
                kind: kind.into(),
            },
            GoImageFault::TypeVariadicFlag { index, found } => {
                Self::TypeVariadicFlag { index, found }
            }
            GoImageFault::TypeVariadicCell { index, kind } => Self::TypeVariadicCell {
                index,
                kind: kind.into(),
            },
            GoImageFault::ArrayLength { index, length } => Self::ArrayLength { index, length },
            GoImageFault::TypeParamCount {
                index,
                kind,
                param_count,
                child_count,
            } => Self::TypeParamCount {
                index,
                kind: kind.into(),
                param_count,
                child_count,
            },
            GoImageFault::TypeChildRange {
                index,
                start,
                count,
                child_count,
            } => Self::TypeChildRange {
                index,
                start,
                count,
                child_count,
            },
            GoImageFault::TypeChildTarget {
                index,
                target,
                type_count,
            } => Self::TypeChildTarget {
                index,
                target,
                type_count,
            },
            GoImageFault::TypeChildFlags { index, flags } => Self::TypeChildFlags { index, flags },
            GoImageFault::TypeChildTiling { declared, plane } => {
                Self::TypeChildTiling { declared, plane }
            }
            GoImageFault::TypeMemberRange {
                index,
                start,
                count,
                member_count,
            } => Self::TypeMemberRange {
                index,
                start,
                count,
                member_count,
            },
            GoImageFault::MethodOwner {
                index,
                owner,
                declaration_count,
            } => Self::MethodOwner {
                index,
                owner,
                declaration_count,
            },
            GoImageFault::MethodFlag { index, cell, found } => Self::MethodFlag {
                index,
                cell: cell.into(),
                found,
            },
            GoImageFault::MethodTypeRoot {
                index,
                root,
                type_count,
            } => Self::MethodTypeRoot {
                index,
                root,
                type_count,
            },
            GoImageFault::MethodReceiverParams {
                index,
                count,
                blob_bytes,
            } => Self::MethodReceiverParams {
                index,
                count,
                blob_bytes,
            },
            GoImageFault::MethodSort {
                index,
                owner,
                previous,
            } => Self::MethodSort {
                index,
                owner,
                previous,
            },
            GoImageFault::TypeParameterOwner {
                index,
                owner,
                declaration_count,
            } => Self::TypeParameterOwner {
                index,
                owner,
                declaration_count,
            },
            GoImageFault::TypeParameterConstraint {
                index,
                root,
                type_count,
            } => Self::TypeParameterConstraint {
                index,
                root,
                type_count,
            },
            GoImageFault::TypeParameterSort {
                index,
                owner,
                previous,
            } => Self::TypeParameterSort {
                index,
                owner,
                previous,
            },
            GoImageFault::MemberKind { index, found } => Self::MemberKind { index, found },
            GoImageFault::MemberFlag { index, cell, found } => Self::MemberFlag {
                index,
                cell: cell.into(),
                found,
            },
            GoImageFault::MemberEmbedded { index } => Self::MemberEmbedded { index },
            GoImageFault::MemberOwner {
                index,
                owner,
                type_count,
            } => Self::MemberOwner {
                index,
                owner,
                type_count,
            },
            GoImageFault::MemberTypeRoot {
                index,
                root,
                type_count,
            } => Self::MemberTypeRoot {
                index,
                root,
                type_count,
            },
            GoImageFault::MemberSort {
                index,
                owner,
                previous,
            } => Self::MemberSort {
                index,
                owner,
                previous,
            },
            GoImageFault::MemberOwnerRange {
                owner,
                start,
                count,
                actual_start,
                actual_count,
            } => Self::MemberOwnerRange {
                owner,
                start,
                count,
                actual_start,
                actual_count,
            },
            GoImageFault::DocOwnerKind { index, found } => Self::DocOwnerKind { index, found },
            GoImageFault::DocOwner {
                index,
                owner,
                bound,
            } => Self::DocOwner {
                index,
                owner,
                bound,
            },
            GoImageFault::EmptyDoc { index } => Self::EmptyDoc { index },
            GoImageFault::DocSort {
                index,
                owner_kind,
                owner,
                previous_kind,
                previous_owner,
            } => Self::DocSort {
                index,
                owner_kind,
                owner,
                previous_kind,
                previous_owner,
            },
            GoImageFault::ReferenceOwner {
                index,
                owner,
                declaration_count,
            } => Self::ReferenceOwner {
                index,
                owner,
                declaration_count,
            },
            GoImageFault::ReferenceSpan { index, start, end } => {
                Self::ReferenceSpan { index, start, end }
            }
            GoImageFault::ReferenceOwnerUnresolved {
                index,
                owner_bytes,
                function_bytes,
            } => Self::ReferenceOwnerUnresolved {
                index,
                owner_bytes,
                function_bytes,
            },
            GoImageFault::ReferenceOwnerSpan { index } => Self::ReferenceOwnerSpan { index },
            GoImageFault::ReferenceFile { index } => Self::ReferenceFile { index },
            GoImageFault::ReferenceContainment {
                index,
                start,
                end,
                owner_start,
                owner_end,
            } => Self::ReferenceContainment {
                index,
                start,
                end,
                owner_start,
                owner_end,
            },
            GoImageFault::ReferenceSort { index } => Self::ReferenceSort { index },
            GoImageFault::EmptyConstraint { index } => Self::EmptyConstraint { index },
            GoImageFault::ConstraintBlob {
                index,
                count,
                blob_bytes,
            } => Self::ConstraintBlob {
                index,
                count,
                blob_bytes,
            },
            GoImageFault::ConstraintSort { index } => Self::ConstraintSort { index },
            GoImageFault::SatisfactionSubject {
                index,
                subject,
                declaration_count,
            } => Self::SatisfactionSubject {
                index,
                subject,
                declaration_count,
            },
            GoImageFault::SatisfactionSort {
                index,
                subject,
                previous,
            } => Self::SatisfactionSort {
                index,
                subject,
                previous,
            },
            GoImageFault::SatisfactionSubjectKind { index, kind } => {
                Self::SatisfactionSubjectKind {
                    index,
                    kind: kind.into(),
                }
            }
            GoImageFault::ModulePath => Self::ModulePath,
            GoImageFault::PackageFiles {
                index,
                count,
                blob_bytes,
            } => Self::PackageFiles {
                index,
                count,
                blob_bytes,
            },
            GoImageFault::PackageSort { index } => Self::PackageSort { index },
            GoImageFault::DeclarationPackage {
                index,
                package_count,
            } => Self::DeclarationPackage {
                index,
                package_count,
            },
            GoImageFault::SignatureParameterPosition { index } => {
                Self::SignatureParameterPosition { index }
            }
            GoImageFault::MethodSetOwner {
                index,
                owner,
                type_count,
            } => Self::MethodSetOwner {
                index,
                owner,
                type_count,
            },
            GoImageFault::MethodSetOwnerKind { index, kind } => Self::MethodSetOwnerKind {
                index,
                kind: kind.into(),
            },
            GoImageFault::MethodSetTypeRoot {
                index,
                root,
                type_count,
            } => Self::MethodSetTypeRoot {
                index,
                root,
                type_count,
            },
            GoImageFault::MethodSetSort { index } => Self::MethodSetSort { index },
            GoImageFault::SignatureParameterOwner {
                index,
                owner,
                type_count,
            } => Self::SignatureParameterOwner {
                index,
                owner,
                type_count,
            },
            GoImageFault::SignatureParameterOwnerRow {
                index,
                owner,
                expected,
            } => Self::SignatureParameterOwnerRow {
                index,
                owner,
                expected,
            },
            GoImageFault::SignatureParameterOrdinal {
                index,
                ordinal,
                expected,
            } => Self::SignatureParameterOrdinal {
                index,
                ordinal,
                expected,
            },
            GoImageFault::SignatureParameterTiling { declared, plane } => {
                Self::SignatureParameterTiling { declared, plane }
            }
            GoImageFault::EmptyName { plane, index } => Self::EmptyName {
                plane: plane.into(),
                index,
            },
            GoImageFault::AtomRange {
                plane,
                index,
                offset,
                length,
                atom_bytes,
            } => Self::AtomRange {
                plane: plane.into(),
                index,
                offset,
                length,
                atom_bytes,
            },
            GoImageFault::AtomUtf8 { plane, index } => Self::AtomUtf8 {
                plane: plane.into(),
                index,
            },
        }
    }
}

impl From<compiler_vocabulary::GoImageHeaderFault> for GoldenGoImageHeaderFault {
    fn from(fault: compiler_vocabulary::GoImageHeaderFault) -> Self {
        use compiler_vocabulary::GoImageHeaderFault;
        match fault {
            GoImageHeaderFault::Truncated { actual } => Self::Truncated { actual },
            GoImageHeaderFault::Magic { found } => Self::Magic { found },
            GoImageHeaderFault::Version { found } => Self::Version { found },
            GoImageHeaderFault::Length { found } => Self::Length { found },
            GoImageHeaderFault::BodyLength { declared, actual } => {
                Self::BodyLength { declared, actual }
            }
            GoImageHeaderFault::Reserved => Self::Reserved,
            GoImageHeaderFault::ModuleCount { found } => Self::ModuleCount { found },
        }
    }
}

impl_unit_conversion!(GoImagePlane => GoldenGoImagePlane [Declaration, Type, TypeChild, Method, TypeParameter, Member, Doc, Reference, ReferenceTarget, BuildConstraint, Satisfaction, SatisfactionTarget, Package, SignatureParameter, MethodSet, Module]);
impl_unit_conversion!(GoImageFlagCell => GoldenGoImageFlagCell [Exported, PointerReceiver, Promoted, Embedded]);
impl_unit_conversion!(GoImageDeclarationKind => GoldenGoImageDeclarationKind [Type, Alias, Function, Constant, Static]);
impl_unit_conversion!(GoImageTypeKind => GoldenGoImageTypeKind [Basic, Named, Alias, TypeParameter, Pointer, Slice, Array, Map, Channel, Function, Struct, Interface, Union, Tuple, Invalid]);
impl_unit_conversion!(GoProjectionIndexPhase => GoldenGoProjectionIndexPhase [ImageHeader, ImageRow, FactOrdinal, TypeRow, TypeChild, Declaration, Method, TypeParameter, Member, Documentation, Reference, Constraint, Satisfaction, Package, SignatureParameter, MethodSet, Atom, EntityList]);
impl_unit_conversion!(GoProjectionListPhase => GoldenGoProjectionListPhase [Entity, Type, Atom, TypeParameter]);

impl From<compiler_vocabulary::TypeScriptProjectionFault> for GoldenTypeScriptProjectionFault {
    fn from(fault: compiler_vocabulary::TypeScriptProjectionFault) -> Self {
        use compiler_vocabulary::TypeScriptProjectionFault;
        match fault {
            TypeScriptProjectionFault::ForeignKey { start, end, cause } => Self::ForeignKey {
                start,
                end,
                cause: cause.into(),
            },
            TypeScriptProjectionFault::PackageLineage { start, end, cause } => {
                Self::PackageLineage {
                    start,
                    end,
                    cause: cause.into(),
                }
            }
            TypeScriptProjectionFault::CoordinateOverflow { value } => {
                Self::CoordinateOverflow { value }
            }
            TypeScriptProjectionFault::MissingImportBinding { fact } => {
                Self::MissingImportBinding { fact }
            }
        }
    }
}

impl From<compiler_vocabulary::PythonProjectionFault> for GoldenPythonProjectionFault {
    fn from(fault: compiler_vocabulary::PythonProjectionFault) -> Self {
        use compiler_vocabulary::PythonProjectionFault;
        match fault {
            PythonProjectionFault::ForeignSpellingUtf8 { start, end } => {
                Self::ForeignSpellingUtf8 { start, end }
            }
            PythonProjectionFault::ForeignKey { start, end, cause } => Self::ForeignKey {
                start,
                end,
                cause: cause.into(),
            },
            PythonProjectionFault::PackageLineage { start, end, cause } => Self::PackageLineage {
                start,
                end,
                cause: cause.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::CSharpProjectionFault> for GoldenCSharpProjectionFault {
    fn from(fault: compiler_vocabulary::CSharpProjectionFault) -> Self {
        use compiler_vocabulary::CSharpProjectionFault;
        match fault {
            CSharpProjectionFault::Image { cause } => Self::Image {
                cause: cause.into(),
            },
            CSharpProjectionFault::Depth { type_row } => Self::Depth { type_row },
            CSharpProjectionFault::NameSpan { start, end } => Self::NameSpan { start, end },
            CSharpProjectionFault::OwnerOrder {
                owner_start,
                reference_start,
            } => Self::OwnerOrder {
                owner_start,
                reference_start,
            },
            CSharpProjectionFault::Anchor { owner } => Self::Anchor { owner },
            CSharpProjectionFault::Foreign { reference } => Self::Foreign { reference },
            CSharpProjectionFault::AttributeCapacity { spellings } => {
                Self::AttributeCapacity { spellings }
            }
            CSharpProjectionFault::IndexCapacity { phase, observed } => Self::IndexCapacity {
                phase: phase.into(),
                observed,
            },
            CSharpProjectionFault::HeterogeneousArrayRank { first, observed } => {
                Self::HeterogeneousArrayRank { first, observed }
            }
            CSharpProjectionFault::ResultName { type_row } => Self::ResultName { type_row },
            CSharpProjectionFault::Admission {
                fact,
                name_len,
                cause,
            } => Self::Admission {
                fact,
                name_len,
                cause: cause.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::CSharpImageFault> for GoldenCSharpImageFault {
    fn from(fault: compiler_vocabulary::CSharpImageFault) -> Self {
        use compiler_vocabulary::CSharpImageFault;
        match fault {
            CSharpImageFault::Header { cause } => Self::Header {
                cause: cause.into(),
            },
            CSharpImageFault::Digest => Self::Digest,
            CSharpImageFault::DeclarationKind {
                index,
                found,
                plane,
            } => Self::DeclarationKind {
                index,
                found,
                plane: plane.into(),
            },
            CSharpImageFault::DeclarationReserved { index, plane } => Self::DeclarationReserved {
                index,
                plane: plane.into(),
            },
            CSharpImageFault::NameRange {
                index,
                offset,
                length,
                atom_bytes,
            } => Self::NameRange {
                index,
                offset,
                length,
                atom_bytes,
            },
            CSharpImageFault::NameUtf8 { index } => Self::NameUtf8 { index },
            CSharpImageFault::Span { index, start, end } => Self::Span { index, start, end },
            CSharpImageFault::TypeChildCount {
                index,
                kind,
                min,
                max,
                actual,
            } => Self::TypeChildCount {
                index,
                kind: kind.into(),
                min,
                max,
                actual,
            },
        }
    }
}

impl From<compiler_vocabulary::CSharpImageHeaderFault> for GoldenCSharpImageHeaderFault {
    fn from(fault: compiler_vocabulary::CSharpImageHeaderFault) -> Self {
        use compiler_vocabulary::CSharpImageHeaderFault;
        match fault {
            CSharpImageHeaderFault::Truncated { actual } => Self::Truncated { actual },
            CSharpImageHeaderFault::Magic { found } => Self::Magic { found },
            CSharpImageHeaderFault::Version { found } => Self::Version { found },
            CSharpImageHeaderFault::Length { found } => Self::Length { found },
            CSharpImageHeaderFault::SectionCount { found } => Self::SectionCount { found },
            CSharpImageHeaderFault::BodyLength { declared, actual } => {
                Self::BodyLength { declared, actual }
            }
            CSharpImageHeaderFault::Reserved => Self::Reserved,
            CSharpImageHeaderFault::DirectoryTag { expected, found } => {
                Self::DirectoryTag { expected, found }
            }
            CSharpImageHeaderFault::DirectoryRowBytes { expected, found } => {
                Self::DirectoryRowBytes { expected, found }
            }
            CSharpImageHeaderFault::DirectoryByteCount {
                count,
                row_bytes,
                found,
            } => Self::DirectoryByteCount {
                count,
                row_bytes,
                found,
            },
            CSharpImageHeaderFault::DirectoryOffset { expected, found } => {
                Self::DirectoryOffset { expected, found }
            }
            CSharpImageHeaderFault::DirectoryRange {
                offset,
                length,
                image_bytes,
            } => Self::DirectoryRange {
                offset,
                length,
                image_bytes,
            },
        }
    }
}

impl_unit_conversion!(CSharpImageSection => GoldenCSharpImageSection [Atoms, AtomBytes, Declarations, Parameters, TypeParameters, TypeConstraints, Types, TypeChildren, Attributes, Docs, References]);
impl_unit_conversion!(CSharpImageTypeKind => GoldenCSharpImageTypeKind [Named, Array, Pointer, NullableValue, Tuple, FunctionPointer, TypeParameter, Dynamic, Error]);
impl_unit_conversion!(CSharpProjectionIndexPhase => GoldenCSharpProjectionIndexPhase [ImageHeader, FactOrdinal, Signature, Declaration, TypeRow, TypeChild, Attribute, Documentation, Reference, Name, SourceSpan]);
impl_unit_conversion!(ClangProjectionTypeKind => GoldenClangTypeKind [Unknown, Builtin, Named, Pointer, BlockPointer, MemberPointer, LvalueReference, RvalueReference, Array, Function]);

impl From<compiler_vocabulary::ClangProjectionFault> for GoldenClangProjectionFault {
    fn from(fault: compiler_vocabulary::ClangProjectionFault) -> Self {
        use compiler_vocabulary::ClangProjectionFault;
        match fault {
            ClangProjectionFault::Span { start, end } => Self::Span { start, end },
            ClangProjectionFault::Nameless { declaration } => Self::Nameless { declaration },
            ClangProjectionFault::Anchor => Self::Anchor,
            ClangProjectionFault::IndexCapacity => Self::IndexCapacity,
            ClangProjectionFault::ForeignOverride { identity } => {
                Self::ForeignOverride { identity }
            }
            ClangProjectionFault::ForeignReference { identity } => {
                Self::ForeignReference { identity }
            }
            ClangProjectionFault::IllegalQualifierTarget {
                type_id,
                kind,
                qualifiers,
            } => Self::IllegalQualifierTarget {
                type_id,
                kind: kind.into(),
                qualifiers: qualifiers.into(),
            },
            ClangProjectionFault::IllegalMemberPointerOwner {
                pointer,
                owner,
                kind,
                declaration,
            } => Self::IllegalMemberPointerOwner {
                pointer,
                owner,
                kind: kind.into(),
                declaration: declaration.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::ClangProjectionQualifiers> for GoldenClangQualifiers {
    fn from(qualifiers: compiler_vocabulary::ClangProjectionQualifiers) -> Self {
        Self {
            is_const: qualifiers.is_const,
            is_volatile: qualifiers.is_volatile,
            is_restrict: qualifiers.is_restrict,
        }
    }
}

impl From<compiler_vocabulary::ClangProjectionDeclaration> for GoldenClangDeclaration {
    fn from(declaration: compiler_vocabulary::ClangProjectionDeclaration) -> Self {
        match declaration {
            compiler_vocabulary::ClangProjectionDeclaration::Known { identity } => {
                Self::Known { identity }
            }
            compiler_vocabulary::ClangProjectionDeclaration::Unavailable => Self::Unavailable,
        }
    }
}
