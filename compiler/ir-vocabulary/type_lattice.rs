//! The cross-language type-expression lattice as tag-validated records.
//!
//! Ported from the old system's `Type` lattice, which a measured census of
//! 87,101 Python annotation positions froze: 41.3% of annotated positions
//! had collapsed onto one dynamic opcode before the lattice split, and every
//! named [`TypeReason`] below exists because a distinct source fact once
//! shared an encoding. The variant set is exactly the frozen census lattice
//! (24 constructors plus seven named unknown reasons), reshaped into dense,
//! borrowed, allocation-free records whose cells each closed tag owns.
//!
//! Nesting is expressed the canonical way: child coordinates into a
//! caller-owned pooled lane, spans validated per tag, and no owned
//! intermediate tree.

use crate::{EntityId, ExternalEntityRef, ExternalTypeRef, ListSpan, TypeId};

/// Marker for the pooled type-child lane.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypeChildren {}

/// Dense coordinate of one row in a fragment's type-fact lane.
pub type TypeFactId = TypeId;

/// A type coordinate: local to this fragment's type-fact lane, or owned by
/// another fragment authority. Foreign ordinals remain inseparable from
/// their typed fragment authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeRef {
    /// A type row inside the same fragment.
    Local(TypeId),
    /// A type row owned by another fragment authority.
    External(ExternalTypeRef),
}

/// A nominal target: the declared entity a `Nominal` type row names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NominalRef {
    /// An entity row inside the same fragment.
    Local(EntityId),
    /// An entity row owned by another fragment authority.
    External(ExternalEntityRef),
}

/// The closed set of type constructors. Frozen discriminants; never
/// renumber. Order mirrors the frozen census lattice.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticTypeTag {
    /// A receiver/self type such as Rust `Self` or TypeScript `this`.
    SelfType = 0,
    /// A fundamental, language-level built-in type.
    Primitive = 1,
    /// A fixed-length, heterogeneous collection with optional labels.
    Tuple = 2,
    /// A dynamically-sized view into a contiguous sequence.
    Slice = 3,
    /// A fixed-size contiguous sequence; the length cells own the size.
    Array = 4,
    /// An untagged union or sum of types.
    Union = 5,
    /// An intersection or combination of types.
    Intersection = 6,
    /// The bottom type (`!`, `never`, `NoReturn`).
    Never = 7,
    /// The language's genuine top type — upcast-only, with a declaration
    /// and method set (`Object`, `interface{}`, `unknown`).
    Any = 8,
    /// The type is not known, together with the named reason.
    Unknown = 9,
    /// A nominal reference to a declared type entity.
    Nominal = 10,
    /// A generic application of a base type to arguments.
    Apply = 11,
    /// A use of a generic parameter, by name.
    TypeVar = 12,
    /// A use-site wildcard with optional bound.
    Wildcard = 13,
    /// A callable type expressed as a structural signature.
    FunctionPointer = 14,
    /// A type with attached source-level annotation metadata.
    Annotated = 15,
    /// A TypeScript conditional type: `check extends ext ? then : else`.
    Conditional = 16,
    /// A TypeScript mapped type iterating a source type's keys.
    Mapped = 17,
    /// A TypeScript template literal type.
    TemplateLiteral = 18,
    /// An anonymous structural record (Go struct/interface, TS object
    /// literal).
    AnonymousRecord = 19,
    /// Rust `impl Trait`: static-dispatch existential over a bound set.
    ImplTrait = 20,
    /// Rust `dyn Trait`: dynamic dispatch through a fat pointer.
    DynTrait = 21,
    /// A written inference request (`_`, `auto`, `var`).
    Inferred = 22,
    /// A qualified path `<T as Trait>::Assoc` / `Outer<T>.Inner`.
    QualifiedPath = 23,
}

/// Closed staged callable-tail discriminator carried in a function record's
/// low payload bits. It mirrors, but does not depend on, owned IR so the wire
/// grammar can reject a mixed typed-rest/C-ellipsis claim before projection.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionVariadicForm {
    None = 0,
    TypedLast = 1,
    CUnbounded = 2,
}

impl TryFrom<u32> for FunctionVariadicForm {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::TypedLast),
            2 => Ok(Self::CUnbounded),
            _ => Err(()),
        }
    }
}

/// Exact type-tag rejection retaining the observed byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypeTagError {
    /// Rejected tag byte.
    pub actual: u8,
}

impl From<SemanticTypeTag> for u8 {
    /// Encodes the stable wire discriminant.
    fn from(value: SemanticTypeTag) -> Self {
        value as u8
    }
}

impl TryFrom<u8> for SemanticTypeTag {
    type Error = SemanticTypeTagError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u8) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::SelfType),
            1 => Ok(Self::Primitive),
            2 => Ok(Self::Tuple),
            3 => Ok(Self::Slice),
            4 => Ok(Self::Array),
            5 => Ok(Self::Union),
            6 => Ok(Self::Intersection),
            7 => Ok(Self::Never),
            8 => Ok(Self::Any),
            9 => Ok(Self::Unknown),
            10 => Ok(Self::Nominal),
            11 => Ok(Self::Apply),
            12 => Ok(Self::TypeVar),
            13 => Ok(Self::Wildcard),
            14 => Ok(Self::FunctionPointer),
            15 => Ok(Self::Annotated),
            16 => Ok(Self::Conditional),
            17 => Ok(Self::Mapped),
            18 => Ok(Self::TemplateLiteral),
            19 => Ok(Self::AnonymousRecord),
            20 => Ok(Self::ImplTrait),
            21 => Ok(Self::DynTrait),
            22 => Ok(Self::Inferred),
            23 => Ok(Self::QualifiedPath),
            actual => Err(SemanticTypeTagError { actual }),
        }
    }
}

/// Why a type position has no resolved type. Deliberately exhaustive — no
/// catch-all variant, because a wildcard arm anywhere here recreates the
/// one-opcode collapse this lattice exists to undo. Frozen discriminants;
/// never renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypeReason {
    /// The source wrote no type here at all (`def f(x):`).
    Unannotated = 0,
    /// The source explicitly asked for dynamic typing (`typing.Any`, `any`,
    /// `dynamic`) — bidirectionally consistent, check-suppressing.
    DynamicallyTyped = 1,
    /// A short name that matches a fully-qualified id the same package
    /// declares elsewhere; one name-resolution pass closes it. The spelling
    /// is retained in the record's text cell.
    UnresolvedLocalName = 2,
    /// A name that resolves outside this package and no foreign key could
    /// be built for it. The spelling is retained in the text cell.
    UnresolvedExternal = 3,
    /// The producer's own recursion guard fired; raising the limit closes
    /// it with no new information from anywhere.
    TruncatedAtDepthLimit = 4,
    /// The language front-end handed the producer nothing usable at this
    /// position; the upstream tool already reported its own failure.
    OracleGap = 5,
    /// The construct is known and named and this lattice has no slot for
    /// it; the spelling is retained in the text cell so the backlog is
    /// countable.
    NoIrRepresentation = 6,
}

/// Exact reason-code rejection retaining the observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeReasonError {
    /// Rejected reason cell.
    pub actual: u32,
}

impl TypeReason {
    /// The reasons whose records must retain the source spelling in the
    /// text cell.
    #[must_use]
    pub const fn carries_spelling(self) -> bool {
        matches!(
            self,
            Self::UnresolvedLocalName | Self::UnresolvedExternal | Self::NoIrRepresentation
        )
    }

    /// A short, stable, language-neutral tag for this reason.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Unannotated => "unannotated",
            Self::DynamicallyTyped => "dynamically-typed",
            Self::UnresolvedLocalName => "unresolved-local-name",
            Self::UnresolvedExternal => "unresolved-external",
            Self::TruncatedAtDepthLimit => "truncated-at-depth-limit",
            Self::OracleGap => "oracle-gap",
            Self::NoIrRepresentation => "no-ir-representation",
        }
    }
}

impl From<TypeReason> for u32 {
    /// Encodes the stable wire discriminant.
    fn from(value: TypeReason) -> Self {
        match value {
            TypeReason::Unannotated => 0,
            TypeReason::DynamicallyTyped => 1,
            TypeReason::UnresolvedLocalName => 2,
            TypeReason::UnresolvedExternal => 3,
            TypeReason::TruncatedAtDepthLimit => 4,
            TypeReason::OracleGap => 5,
            TypeReason::NoIrRepresentation => 6,
        }
    }
}

impl TryFrom<u32> for TypeReason {
    type Error = TypeReasonError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Unannotated),
            1 => Ok(Self::DynamicallyTyped),
            2 => Ok(Self::UnresolvedLocalName),
            3 => Ok(Self::UnresolvedExternal),
            4 => Ok(Self::TruncatedAtDepthLimit),
            5 => Ok(Self::OracleGap),
            6 => Ok(Self::NoIrRepresentation),
            actual => Err(TypeReasonError { actual }),
        }
    }
}

/// The closed primitive shapes a [`SemanticTypeTag::Primitive`] row can
/// own. Frozen discriminants; never renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PrimitiveShape {
    /// An integer of fixed or architecture width and signedness.
    Integer = 0,
    /// A floating-point width.
    Float = 1,
    /// The boolean type.
    Bool = 2,
    /// The character type.
    Char = 3,
    /// A string-literal type (`str`, `string`).
    Str = 4,
    /// A raw mutable pointer (`*mut T`, `int*`); the pointee is the row's
    /// one child.
    MutPointer = 5,
    /// A raw const pointer (`*const T`); the pointee is the row's one
    /// child.
    ConstPointer = 6,
    /// A managed reference with optional lifetime text and mutability; the
    /// referent is the row's one child.
    Reference = 7,
    /// An arbitrary language builtin spelling; the text cell owns it.
    Builtin = 8,
}

/// Exact primitive-shape rejection retaining the observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrimitiveShapeError {
    /// Rejected shape cell.
    pub actual: u32,
}

impl From<PrimitiveShape> for u32 {
    /// Encodes the stable wire discriminant.
    fn from(value: PrimitiveShape) -> Self {
        match value {
            PrimitiveShape::Integer => 0,
            PrimitiveShape::Float => 1,
            PrimitiveShape::Bool => 2,
            PrimitiveShape::Char => 3,
            PrimitiveShape::Str => 4,
            PrimitiveShape::MutPointer => 5,
            PrimitiveShape::ConstPointer => 6,
            PrimitiveShape::Reference => 7,
            PrimitiveShape::Builtin => 8,
        }
    }
}

impl TryFrom<u32> for PrimitiveShape {
    type Error = PrimitiveShapeError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Integer),
            1 => Ok(Self::Float),
            2 => Ok(Self::Bool),
            3 => Ok(Self::Char),
            4 => Ok(Self::Str),
            5 => Ok(Self::MutPointer),
            6 => Ok(Self::ConstPointer),
            7 => Ok(Self::Reference),
            8 => Ok(Self::Builtin),
            actual => Err(PrimitiveShapeError { actual }),
        }
    }
}

/// Integer and float width: a fixed nonzero bit width or the platform
/// architecture width.
///
/// Cell encoding (17-bit budget so an integer row can pack a signedness bit
/// below a shifted width cell): bits 0..15 own the fixed width, bit 16 is
/// the architecture flag, bits 17..31 are reserved zero.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypeWidth {
    /// A fixed bit width.
    Fixed(u16) = 0,
    /// The target architecture's pointer width.
    Arch = 1,
}

/// Exact width rejection retaining the complete observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeWidthError {
    /// The cell sets a reserved bit, names an unknown tag, or declares a
    /// fixed width of zero; the complete rejected cell travels.
    Malformed {
        /// Complete rejected cell.
        actual: u32,
    },
}

impl TypeWidth {
    /// Architecture flag inside the 17-bit cell budget.
    pub const ARCH_FLAG: u32 = 1 << 16;
    /// Fixed-width lane mask (bits 0..15).
    pub const WIDTH_MASK: u32 = 0xffff;
    /// Every reserved bit above the cell budget.
    const RESERVED: u32 = 0xffff_0000 & !Self::ARCH_FLAG;

    /// Encodes this width into one record cell.
    #[must_use]
    pub const fn to_cell(self) -> u32 {
        match self {
            Self::Fixed(width) => width as u32,
            Self::Arch => Self::ARCH_FLAG,
        }
    }

    /// Decodes one record cell, rejecting reserved bits, unknown tags, and
    /// zero widths with the complete observed cell.
    pub fn try_from_cell(cell: u32) -> Result<Self, TypeWidthError> {
        if cell & Self::RESERVED != 0 {
            return Err(TypeWidthError::Malformed { actual: cell });
        }
        if cell & Self::ARCH_FLAG != 0 {
            if cell & Self::WIDTH_MASK != 0 {
                return Err(TypeWidthError::Malformed { actual: cell });
            }
            return Ok(Self::Arch);
        }
        let width = cell & Self::WIDTH_MASK;
        if width == 0 {
            return Err(TypeWidthError::Malformed { actual: cell });
        }
        #[expect(
            clippy::as_conversions,
            reason = "the masked cell is exactly the u16 width lane"
        )]
        Ok(Self::Fixed(width as u16))
    }
}

/// Variance of a wildcard or projection. Frozen discriminants; never
/// renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Variance {
    /// Invariant use.
    Invariant = 0,
    /// Covariant use (`out T`).
    Covariant = 1,
    /// Contravariant use (`in T`).
    Contravariant = 2,
}

/// Exact variance rejection retaining the observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VarianceError {
    /// Rejected variance cell.
    pub actual: u32,
}

impl From<Variance> for u32 {
    /// Encodes the stable wire discriminant.
    fn from(value: Variance) -> Self {
        match value {
            Variance::Invariant => 0,
            Variance::Covariant => 1,
            Variance::Contravariant => 2,
        }
    }
}

impl TryFrom<u32> for Variance {
    type Error = VarianceError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Invariant),
            1 => Ok(Self::Covariant),
            2 => Ok(Self::Contravariant),
            actual => Err(VarianceError { actual }),
        }
    }
}

/// A tri-state modifier on a mapped type's `readonly`/`optional` position.
/// A bare bool cannot represent the three states: `Add` and `Absent` would
/// collapse, losing either the removal or the inheritance case. Frozen
/// discriminants; never renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MappedModifier {
    /// Explicitly added (`readonly`, `?`).
    Add = 0,
    /// Explicitly removed (`-readonly`, `-?`).
    Remove = 1,
    /// Not mentioned; inherited from the source type.
    Absent = 2,
}

/// Exact mapped-modifier rejection retaining the observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappedModifierError {
    /// Rejected modifier cell.
    pub actual: u32,
}

impl From<MappedModifier> for u32 {
    /// Encodes the stable wire discriminant.
    fn from(value: MappedModifier) -> Self {
        match value {
            MappedModifier::Add => 0,
            MappedModifier::Remove => 1,
            MappedModifier::Absent => 2,
        }
    }
}

impl TryFrom<u32> for MappedModifier {
    type Error = MappedModifierError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Add),
            1 => Ok(Self::Remove),
            2 => Ok(Self::Absent),
            actual => Err(MappedModifierError { actual }),
        }
    }
}

/// The form of an anonymous structural record. Frozen discriminants; never
/// renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AnonRecordForm {
    /// An anonymous struct (`struct { X int }`).
    Struct = 0,
    /// An anonymous interface (Go method sets, TS object literals).
    Interface = 1,
}

/// Exact anonymous-record-form rejection retaining the observed cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnonRecordFormError {
    /// Rejected form cell.
    pub actual: u32,
}

impl From<AnonRecordForm> for u32 {
    /// Encodes the stable wire discriminant.
    fn from(value: AnonRecordForm) -> Self {
        match value {
            AnonRecordForm::Struct => 0,
            AnonRecordForm::Interface => 1,
        }
    }
}

impl TryFrom<u32> for AnonRecordForm {
    type Error = AnonRecordFormError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Struct),
            1 => Ok(Self::Interface),
            actual => Err(AnonRecordFormError { actual }),
        }
    }
}

/// One nested position under a type row: a nested type coordinate, or a
/// literal text part of a template literal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeChildTarget {
    /// A nested type row.
    Type(TypeRef),
    /// A fixed literal text part (template literals).
    Text,
}

/// One pooled child of a type row.
///
/// `name` is valid only where the parent tag demands it (labeled tuple
/// elements, anonymous-record members); `flags` bits are valid only under
/// anonymous-record members. Tag validation rejects tag-foreign names and
/// flags instead of letting consumers guess.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypeChild<'bytes> {
    /// The nested target of this position.
    pub target: TypeChildTarget,
    /// Member or label spelling, where the parent tag demands one.
    pub name: Option<&'bytes [u8]>,
    /// Tag-owned child modifiers. Bit 0 is optional (tuple, function, or
    /// object member), bit 1 is readonly (object member), bit 2 is rest
    /// (tuple or function parameter).
    pub flags: u8,
}

impl SemanticTypeChild<'_> {
    /// Optional tuple/function/object-member flag.
    pub const FLAG_OPTIONAL: u8 = 1 << 0;
    /// Anonymous-record member readonly flag.
    pub const FLAG_READONLY: u8 = 1 << 1;
    /// Rest tuple/function-parameter flag.
    pub const FLAG_REST: u8 = 1 << 2;
    /// Every flag bit the closed grammar defines.
    pub const FLAG_ALL: u8 = Self::FLAG_OPTIONAL | Self::FLAG_READONLY | Self::FLAG_REST;
}

/// Which record cell carried a tag-foreign value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeCell {
    /// The first tag-owned payload cell.
    Payload0,
    /// The second tag-owned payload cell.
    Payload1,
    /// The required-or-forbidden text cell.
    Text,
    /// The second optional text cell (annotation argument).
    Text2,
    /// The nominal-target cell.
    Nominal,
}

/// The presence law one tag imposes on one cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CellLaw {
    /// The tag requires the cell.
    Required,
    /// The tag may carry the cell or not.
    Optional,
    /// The tag forbids the cell.
    Forbidden,
}

/// The exact child-count law one tag imposes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildCountLaw {
    /// Minimum legal child count.
    pub min: u32,
    /// Maximum legal child count.
    pub max: u32,
}

/// Exact type-record rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticTypeFault {
    /// The observed tag is outside the closed lattice.
    Tag {
        /// Rejected tag value.
        actual: u8,
    },
    /// A cell reserved for another tag carries a value.
    ReservedCell {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// The offending cell.
        cell: TypeCell,
        /// Observed cell value (1 for a present-but-forbidden text cell).
        actual: u32,
    },
    /// A cell the tag requires is missing.
    MissingCell {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// The missing cell.
        cell: TypeCell,
    },
    /// The unknown-reason cell is outside the closed reason set.
    Reason {
        /// Rejected reason value.
        actual: u32,
    },
    /// The primitive-shape cell is outside the closed shape set.
    PrimitiveShape {
        /// Rejected shape value.
        actual: u32,
    },
    /// A width cell is malformed; the complete observed cell travels.
    Width {
        /// Complete rejected width cell.
        actual: u32,
    },
    /// The child count violates the tag's closed law.
    ChildCount {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// The tag's exact child-count law.
        law: ChildCountLaw,
        /// Observed child count.
        actual: u32,
    },
    /// A child carries a name where the parent tag forbids one.
    ChildNameForbidden {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// Child position.
        position: u32,
    },
    /// A child must carry a name and does not.
    ChildNameRequired {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// Child position.
        position: u32,
    },
    /// A child carries flag bits the parent tag forbids.
    ChildFlagsForbidden {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// Child position.
        position: u32,
        /// Observed flags.
        actual: u8,
    },
    /// A function row claims a variadic final parameter but its parameter
    /// range does not carry the required rest child modifier.
    VariadicParameter {
        /// The final parameter position before the trailing result range.
        position: u32,
        /// Flags observed at that position.
        actual: u8,
    },
    /// A child targets text where the parent tag forbids it.
    ChildTextForbidden {
        /// Owning tag.
        tag: SemanticTypeTag,
        /// Child position.
        position: u32,
    },
}

/// One type-lattice record.
///
/// The record is a fixed set of cells whose meaning each closed tag owns:
/// tag validation rejects tag-foreign cells, missing required cells, and
/// child-count/child-shape violations with exact operands — the same
/// discipline the semantic product constructors apply.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypeRecord<'bytes> {
    /// Closed structural tag.
    pub tag: SemanticTypeTag,
    /// First tag-owned payload cell.
    pub payload0: u32,
    /// Second tag-owned payload cell.
    pub payload1: u32,
    /// The tag's required or optional text cell (names, spellings, ABI).
    pub text: Option<&'bytes [u8]>,
    /// The tag's second optional text cell (annotation argument).
    pub text2: Option<&'bytes [u8]>,
    /// The nominal-target cell; valid only under `Nominal`.
    pub nominal: Option<NominalRef>,
    /// The row's child range in the caller-owned pooled child lane.
    pub children: ListSpan<TypeChildren>,
}

impl SemanticTypeRecord<'_> {
    /// A leaf row with no children and no cells.
    #[must_use]
    pub const fn leaf(tag: SemanticTypeTag) -> Self {
        Self {
            tag,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 0),
        }
    }

    /// Exact trailing result count for a one-result function-pointer row.
    ///
    /// The child sequence is `[parameters..., results...]`; zero therefore
    /// denotes a no-result callable, and every positive payload1 value is an
    /// unambiguous trailing result count.
    pub const FUNCTION_RESULT_COUNT_ONE: u32 = 1;
    /// Schema-2 result-presence bit accepted only when reopening historical
    /// fragments. New writers must use [`Self::FUNCTION_RESULT_COUNT_ONE`] or
    /// a larger direct count for multi-result signatures.
    pub const LEGACY_RESULT_FLAG: u32 = 1 << 31;
    /// Function-pointer payload0: exactly the final parameter is a typed
    /// variadic/rest element (Go, TypeScript, Python).
    pub const FUNCTION_TYPED_VARIADIC_FLAG: u32 = 1;
    /// Function-pointer payload0: an unnamed C-family `...` follows the
    /// parameter range. It is distinct from a typed rest parameter.
    pub const FUNCTION_C_VARIADIC_FLAG: u32 = 1 << 1;
    /// Function-pointer payload0 mask for [`FunctionVariadicForm`].
    pub const FUNCTION_VARIADIC_MASK: u32 = Self::FUNCTION_TYPED_VARIADIC_FLAG
        | Self::FUNCTION_C_VARIADIC_FLAG;
    /// Function-pointer payload0: the callable is unsafe.
    pub const FUNCTION_UNSAFE_FLAG: u32 = 1 << 2;
    /// All closed function-pointer payload0 modifier bits.
    pub const FUNCTION_FLAGS: u32 = Self::FUNCTION_VARIADIC_MASK | Self::FUNCTION_UNSAFE_FLAG;

    /// Decodes the closed staged variadic form, rejecting the unused mask
    /// state `3` instead of treating a mixed typed/C tail as two tails.
    #[must_use]
    pub const fn function_variadic_form(&self) -> Option<FunctionVariadicForm> {
        match self.payload0 & Self::FUNCTION_VARIADIC_MASK {
            0 => Some(FunctionVariadicForm::None),
            Self::FUNCTION_TYPED_VARIADIC_FLAG => Some(FunctionVariadicForm::TypedLast),
            Self::FUNCTION_C_VARIADIC_FLAG => Some(FunctionVariadicForm::CUnbounded),
            _ => None,
        }
    }

    /// Decodes the exact result count carried by one function row. The old
    /// high-bit presence form remains reopenable as one result, while any
    /// other high-bit value is a malformed legacy payload.
    #[must_use]
    pub const fn function_result_count(&self) -> Option<u32> {
        if self.payload1 == Self::LEGACY_RESULT_FLAG {
            Some(1)
        } else if self.payload1 & Self::LEGACY_RESULT_FLAG == 0 {
            Some(self.payload1)
        } else {
            None
        }
    }

    /// The integer signedness bit (payload1 bit 0); the width cell occupies
    /// the bits above it.
    pub const INTEGER_SIGNED_FLAG: u32 = 1;
    /// Bit offset of the integer width cell above the signedness bit.
    const INTEGER_WIDTH_SHIFT: u32 = 1;

    /// The closed child-count law of one tag.
    #[must_use]
    pub const fn child_law(tag: SemanticTypeTag) -> ChildCountLaw {
        match tag {
            SemanticTypeTag::Slice | SemanticTypeTag::Array | SemanticTypeTag::Annotated => {
                ChildCountLaw { min: 1, max: 1 }
            }
            SemanticTypeTag::Conditional => ChildCountLaw { min: 4, max: 4 },
            // constraint, optional key-remap (`as`), value
            SemanticTypeTag::Mapped => ChildCountLaw { min: 2, max: 3 },
            SemanticTypeTag::Apply => ChildCountLaw {
                min: 1,
                max: u32::MAX,
            },
            SemanticTypeTag::QualifiedPath => ChildCountLaw { min: 1, max: 2 },
            SemanticTypeTag::Wildcard => ChildCountLaw { min: 0, max: 1 },
            SemanticTypeTag::Tuple
            | SemanticTypeTag::Union
            | SemanticTypeTag::Intersection
            | SemanticTypeTag::FunctionPointer
            | SemanticTypeTag::TemplateLiteral
            | SemanticTypeTag::AnonymousRecord
            | SemanticTypeTag::ImplTrait
            | SemanticTypeTag::DynTrait => ChildCountLaw {
                min: 0,
                max: u32::MAX,
            },
            SemanticTypeTag::Never
            | SemanticTypeTag::Any
            | SemanticTypeTag::Unknown
            | SemanticTypeTag::Nominal
            | SemanticTypeTag::TypeVar
            | SemanticTypeTag::Inferred => ChildCountLaw { min: 0, max: 0 },
            // The primitive tag-level law stays permissive: pointer and
            // reference shapes own exactly one child, every other shape
            // owns none, and `validate_primitive` proves the exact law
            // from the shape cell.
            SemanticTypeTag::Primitive => ChildCountLaw {
                min: 0,
                max: u32::MAX,
            },
        }
    }

    /// Proves one record's cells against its tag: payload ownership, text
    /// presence, nominal presence, and the child-count law across
    /// `child_count` pooled positions. Per-child name/flags/text laws are
    /// proven by [`SemanticTypeRecord::validate_child`].
    pub fn validate(&self, child_count: u32) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        let law = Self::child_law(tag);
        if child_count < law.min || child_count > law.max {
            return Err(SemanticTypeFault::ChildCount {
                tag,
                law,
                actual: child_count,
            });
        }
        match tag {
            SemanticTypeTag::Never
            | SemanticTypeTag::Any
            | SemanticTypeTag::Inferred
            | SemanticTypeTag::Tuple
            | SemanticTypeTag::Slice
            | SemanticTypeTag::Union
            | SemanticTypeTag::Intersection
            | SemanticTypeTag::ImplTrait
            | SemanticTypeTag::DynTrait
            | SemanticTypeTag::TemplateLiteral => {
                self.require_no_cells()?;
            }
            // `Self` needs no spelling, but TypeScript's distinct `this`
            // type must survive the common row so language policy can render
            // it without pretending it is Rust `Self`.
            SemanticTypeTag::SelfType => {
                if self.payload0 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                if self.payload1 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
                self.check_cell(TypeCell::Text, CellLaw::Optional, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(TypeCell::Nominal, CellLaw::Forbidden, self.nominal.is_some())?;
            }
            SemanticTypeTag::Primitive => self.validate_primitive(child_count)?,
            SemanticTypeTag::Unknown => {
                let reason = TypeReason::try_from(self.payload0).map_err(|error| {
                    SemanticTypeFault::Reason {
                        actual: error.actual,
                    }
                })?;
                let text_law = if reason.carries_spelling() {
                    CellLaw::Required
                } else {
                    CellLaw::Forbidden
                };
                self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Nominal => {
                // Local nominals are resolved solely by their typed entity
                // coordinate.  Foreign nominals additionally carry the
                // authority-provided module-qualified display/path spelling;
                // legacy fragments without it remain reopenable but cannot
                // be rendered as an invented `foreign` name.
                let text_law = match self.nominal {
                    Some(NominalRef::External(_)) => CellLaw::Optional,
                    Some(NominalRef::Local(_)) | None => CellLaw::Forbidden,
                };
                self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(TypeCell::Nominal, CellLaw::Required, self.nominal.is_some())?;
            }
            SemanticTypeTag::TypeVar => {
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Array | SemanticTypeTag::QualifiedPath => {
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Wildcard => {
                if self.payload0 > u32::from(Variance::Contravariant) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_no_text()?;
            }
            SemanticTypeTag::FunctionPointer => {
                let Some(results) = self.function_result_count() else {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                };
                if results > child_count {
                    return Err(SemanticTypeFault::ChildCount {
                        tag,
                        law: ChildCountLaw {
                            min: results,
                            max: u32::MAX,
                        },
                        actual: child_count,
                    });
                }
                if self.payload0 & !Self::FUNCTION_FLAGS != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                let Some(variadic) = self.function_variadic_form() else {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                };
                if variadic == FunctionVariadicForm::TypedLast {
                    let minimum_variadic_children = results.checked_add(1).ok_or(
                        SemanticTypeFault::ReservedCell {
                            tag,
                            cell: TypeCell::Payload1,
                            actual: self.payload1,
                        },
                    )?;
                    if child_count < minimum_variadic_children {
                        return Err(SemanticTypeFault::ChildCount {
                            tag,
                            law: ChildCountLaw {
                                min: minimum_variadic_children,
                                max: u32::MAX,
                            },
                            actual: child_count,
                        });
                    }
                }
                self.check_cell(TypeCell::Text, CellLaw::Optional, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Annotated => {
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Optional, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Conditional | SemanticTypeTag::Apply => {
                self.require_no_cells()?;
            }
            SemanticTypeTag::Mapped => {
                if self.payload0 > u32::from(MappedModifier::Absent) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                if self.payload1 > u32::from(MappedModifier::Absent) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::AnonymousRecord => {
                if self.payload0 > u32::from(AnonRecordForm::Interface) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_no_text()?;
            }
        }
        Ok(())
    }

    /// Proves one pooled child against this row's tag at `position`.
    ///
    /// The tag owns every child fact: names only where the grammar demands
    /// them, modifiers only under their matching structural parents, and text
    /// targets only under template literals.
    pub fn validate_child(
        &self,
        position: u32,
        child: &SemanticTypeChild<'_>,
    ) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        let name_allowed = matches!(
            tag,
            SemanticTypeTag::Tuple | SemanticTypeTag::AnonymousRecord
        );
        let name_required = tag == SemanticTypeTag::AnonymousRecord;
        if !name_allowed && child.name.is_some() {
            return Err(SemanticTypeFault::ChildNameForbidden { tag, position });
        }
        if name_required && child.name.is_none() {
            return Err(SemanticTypeFault::ChildNameRequired { tag, position });
        }
        let allowed_flags = match tag {
            SemanticTypeTag::AnonymousRecord => {
                SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_READONLY
            }
            SemanticTypeTag::Tuple | SemanticTypeTag::FunctionPointer => {
                SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST
            }
            _ => 0,
        };
        if child.flags & !allowed_flags != 0 {
            return Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual: child.flags,
            });
        }
        if child.flags & !SemanticTypeChild::FLAG_ALL != 0 {
            return Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual: child.flags,
            });
        }
        let text_allowed = tag == SemanticTypeTag::TemplateLiteral;
        if !text_allowed && matches!(child.target, TypeChildTarget::Text) {
            return Err(SemanticTypeFault::ChildTextForbidden { tag, position });
        }
        Ok(())
    }

    /// Validates one child together with row-wide constraints that depend on
    /// the result range. Callers admitting a full row use this instead of
    /// [`Self::validate_child`] so a variadic marker cannot point at a result
    /// or an unmarked parameter.
    pub fn validate_child_in_row(
        &self,
        position: u32,
        child_count: u32,
        child: &SemanticTypeChild<'_>,
    ) -> Result<(), SemanticTypeFault> {
        self.validate_child(position, child)?;
        if self.tag == SemanticTypeTag::FunctionPointer
            && self.function_variadic_form() == Some(FunctionVariadicForm::TypedLast)
        {
            let results = self.function_result_count().ok_or(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload1,
                actual: self.payload1,
            })?;
            let minimum_variadic_children = results.checked_add(1).ok_or(
                SemanticTypeFault::ReservedCell {
                    tag: self.tag,
                    cell: TypeCell::Payload1,
                    actual: self.payload1,
                },
            )?;
            let parameters = child_count.checked_sub(results).ok_or(
                SemanticTypeFault::ChildCount {
                    tag: self.tag,
                    law: ChildCountLaw {
                        min: minimum_variadic_children,
                        max: u32::MAX,
                    },
                    actual: child_count,
                },
            )?;
            let final_parameter = parameters.checked_sub(1).ok_or(SemanticTypeFault::ChildCount {
                tag: self.tag,
                law: ChildCountLaw {
                    min: minimum_variadic_children,
                    max: u32::MAX,
                },
                actual: child_count,
            })?;
            if position == final_parameter && child.flags & SemanticTypeChild::FLAG_REST == 0 {
                return Err(SemanticTypeFault::VariadicParameter {
                    position,
                    actual: child.flags,
                });
            }
            if position != final_parameter && child.flags & SemanticTypeChild::FLAG_REST != 0 {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
        }
        if self.tag == SemanticTypeTag::FunctionPointer {
            let variadic = self.function_variadic_form().ok_or(
                SemanticTypeFault::ReservedCell {
                    tag: self.tag,
                    cell: TypeCell::Payload0,
                    actual: self.payload0,
                },
            )?;
            let results = self.function_result_count().ok_or(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload1,
                actual: self.payload1,
            })?;
            let parameters = child_count.checked_sub(results).ok_or(
                SemanticTypeFault::ChildCount {
                    tag: self.tag,
                    law: ChildCountLaw {
                        min: results,
                        max: u32::MAX,
                    },
                    actual: child_count,
                },
            )?;
            if position >= parameters
                && child.flags & (SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST)
                    != 0
            {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
            if variadic != FunctionVariadicForm::TypedLast
                && child.flags & SemanticTypeChild::FLAG_REST != 0
            {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
        }
        Ok(())
    }

    fn validate_primitive(&self, child_count: u32) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        let shape = PrimitiveShape::try_from(self.payload0).map_err(|error| {
            SemanticTypeFault::PrimitiveShape {
                actual: error.actual,
            }
        })?;
        match shape {
            PrimitiveShape::Integer => {
                if TypeWidth::try_from_cell(self.payload1 >> Self::INTEGER_WIDTH_SHIFT).is_err() {
                    return Err(SemanticTypeFault::Width {
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Float => {
                if TypeWidth::try_from_cell(self.payload1).is_err() {
                    return Err(SemanticTypeFault::Width {
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Reference => {
                if self.payload1 & !Self::INTEGER_SIGNED_FLAG != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Bool
            | PrimitiveShape::Char
            | PrimitiveShape::Str
            | PrimitiveShape::MutPointer
            | PrimitiveShape::ConstPointer => {
                if self.payload1 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Builtin => {}
        }
        let text_law = match shape {
            PrimitiveShape::Builtin => CellLaw::Required,
            PrimitiveShape::Reference => CellLaw::Optional,
            _ => CellLaw::Forbidden,
        };
        self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
        self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
        self.check_cell(
            TypeCell::Nominal,
            CellLaw::Forbidden,
            self.nominal.is_some(),
        )?;
        let owns_child = matches!(
            shape,
            PrimitiveShape::MutPointer | PrimitiveShape::ConstPointer | PrimitiveShape::Reference
        );
        if owns_child != (child_count == 1) {
            let law = if owns_child {
                ChildCountLaw { min: 1, max: 1 }
            } else {
                ChildCountLaw { min: 0, max: 0 }
            };
            return Err(SemanticTypeFault::ChildCount {
                tag,
                law,
                actual: child_count,
            });
        }
        Ok(())
    }

    fn check_cell(
        &self,
        cell: TypeCell,
        law: CellLaw,
        present: bool,
    ) -> Result<(), SemanticTypeFault> {
        match (present, law) {
            (false, CellLaw::Required) => Err(SemanticTypeFault::MissingCell {
                tag: self.tag,
                cell,
            }),
            (true, CellLaw::Forbidden) => Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell,
                actual: 1,
            }),
            _ => Ok(()),
        }
    }

    fn require_no_cells(&self) -> Result<(), SemanticTypeFault> {
        if self.payload0 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload0,
                actual: self.payload0,
            });
        }
        if self.payload1 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload1,
                actual: self.payload1,
            });
        }
        self.require_no_text()
    }

    fn require_no_text(&self) -> Result<(), SemanticTypeFault> {
        self.check_cell(TypeCell::Text, CellLaw::Forbidden, self.text.is_some())?;
        self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
        self.check_cell(
            TypeCell::Nominal,
            CellLaw::Forbidden,
            self.nominal.is_some(),
        )
    }
}
