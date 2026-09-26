//! The cross-language type-expression lattice as tag-validated records.
//!
//! Ported from the old system's `Type` lattice, which a measured census of
//! 87,101 Python annotation positions froze: 41.3% of annotated positions
//! had collapsed onto one dynamic opcode before the lattice split, and every
//! named [`TypeReason`] below exists because a distinct source fact once
//! shared an encoding. The variant set extends the frozen census lattice
//! with exact C-family qualifier placement (32 constructors plus seven named
//! unknown reasons), reshaped into dense,
//! borrowed, allocation-free records whose cells each closed tag owns.
//!
//! Nesting is expressed the canonical way: child coordinates into a
//! caller-owned pooled lane, spans validated per tag, and no owned
//! intermediate tree.

use crate::ir_vocabulary::{
    EntityId, ExternalEntityRef, ExternalTypeRef, ListSpan, StableRef, TypeId,
};

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
    /// A declaration-identified external target: a known fragment authority
    /// plus that fragment's exact composite declaration identity. Unlike
    /// [`NominalRef::External`] it carries no unverified ordinal and, unlike
    /// [`NominalRef::Local`], it is resolved by the target fragment's own
    /// declaration endpoint rather than this fragment's entity lane.
    Stable(StableRef),
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
    /// A Go associative map with a key and value child.
    Map = 24,
    /// A Go channel with one element child and a closed direction cell.
    Channel = 25,
    /// A sequence array (`T[]`, `T...`, Java `T[]`) with no stored extent.
    ArraySequence = 26,
    /// A rectangular array with a nonzero rank (C# `T[,]`).
    ArrayRectangular = 27,
    /// A fixed numeric extent (`[N]T`, `T[N]`).
    ArrayFixed = 28,
    /// A source expression extent (`[T; N + 1]`) retained as text.
    ArrayConstExpression = 29,
    /// An incomplete/dependent C-family array with no known extent.
    ArrayIncomplete = 30,
    /// A direct C-family cv/restrict qualification wrapper around exactly one
    /// child. It keeps a pointee's qualifiers separate from a pointer's own
    /// qualifiers.
    CQualified = 31,
}

/// Closed staged callable-tail discriminator carried in a function record's
/// low payload bits. It mirrors, but does not depend on, owned IR so the wire
/// grammar can reject a mixed typed-rest/C-ellipsis claim before projection.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

/// The only directional channel forms admitted by the common wire grammar.
/// `Both` is `chan T`, `Send` is `chan<- T`, and `Receive` is `<-chan T`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChannelDirection {
    Both = 0,
    Send = 1,
    Receive = 2,
}

impl TryFrom<u32> for ChannelDirection {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Both),
            1 => Ok(Self::Send),
            2 => Ok(Self::Receive),
            _ => Err(()),
        }
    }
}

/// Closed source-level annotation forms whose inner type remains a structural
/// child. Arbitrary annotation text is not a type escape hatch.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnnotationKind {
    /// TypeScript's `readonly T` structural annotation.
    Readonly = 0,
    /// C# value-nullable `T?` (`Nullable<T>`).
    NullableValue = 1,
    /// C# nullable-reference annotation on this exact recursive type node.
    NullableReference = 2,
    /// C# explicitly non-null reference annotation on this exact recursive
    /// type node.  Oblivious nullability is represented by no wrapper.
    NonNullableReference = 3,
}

impl TryFrom<u32> for AnnotationKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Readonly),
            1 => Ok(Self::NullableValue),
            2 => Ok(Self::NullableReference),
            3 => Ok(Self::NonNullableReference),
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
            24 => Ok(Self::Map),
            25 => Ok(Self::Channel),
            26 => Ok(Self::ArraySequence),
            27 => Ok(Self::ArrayRectangular),
            28 => Ok(Self::ArrayFixed),
            29 => Ok(Self::ArrayConstExpression),
            30 => Ok(Self::ArrayIncomplete),
            31 => Ok(Self::CQualified),
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
    /// A legacy or fixed-width integer with signedness. Fresh producers use
    /// the dedicated native-word shapes instead of the architecture width.
    Integer = 0,
    /// A floating-point width.
    Float = 1,
    /// The boolean type.
    Bool = 2,
    /// A historical, under-specified character row. New producers must use
    /// one of the precise character roles appended below; this discriminant
    /// exists only so legacy fragments remain decodable.
    LegacyChar = 3,
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
    /// A C/C++ raw pointer with one pointee child. Direct qualifiers use the
    /// single [`SemanticTypeTag::CQualified`] wrapper, so `const T * const`
    /// has exactly one canonical nesting shape.
    CPointer = 9,
    /// A C++ lvalue reference. This is deliberately distinct from the Rust
    /// borrow shape above: neither mutability nor a Rust lifetime can be
    /// inferred from `T&`.
    CxxLvalueReference = 10,
    /// A C++ rvalue reference (`T&&`), never a mutable Rust borrow.
    CxxRvalueReference = 11,
    /// A C++ member pointer with ordered `(owner, member)` children.
    CxxMemberPointer = 12,
    /// An arbitrary-precision integer (`Python int`), not a target word.
    ArbitraryInteger = 13,
    /// A signed native machine word (`isize`, Go `int`, C# `nint`).
    NativeSignedInteger = 14,
    /// An unsigned native machine word (`usize`, Go `uint`).
    NativeUnsignedInteger = 15,
    /// An unsigned pointer-address integer (`uintptr`, C# `nuint`).
    PointerAddressInteger = 16,
    /// Rust's Unicode scalar value (`char`), not a C code unit.
    UnicodeScalar = 17,
    /// A UTF-16 code unit (`char` in Java/C#, `char16_t` in C++).
    Utf16CodeUnit = 18,
    /// A UTF-32 code unit (`char32_t` in C++), distinct from a Unicode
    /// scalar because the source language chooses its operations and range.
    Utf32CodeUnit = 19,
    /// A plain C/C++ `char` whose authority reports signed representation.
    CPlainSignedChar = 20,
    /// A plain C/C++ `char` whose authority reports unsigned representation.
    CPlainUnsignedChar = 21,
    /// An explicitly spelled C/C++ `signed char`.
    CSignedChar = 22,
    /// An explicitly spelled C/C++ `unsigned char`.
    CUnsignedChar = 23,
    /// The implementation-defined C/C++ `wchar_t` role when the native
    /// authority could not establish signedness.
    CWideChar = 24,
    /// An Objective-C block pointer with one signature/pointee child. It is
    /// not a C raw pointer and must survive for a declarator dialect.
    CBlockPointer = 25,
    /// `wchar_t` with authority-proven signed representation.
    CWideSignedChar = 26,
    /// `wchar_t` with authority-proven unsigned representation.
    CWideUnsignedChar = 27,
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
            PrimitiveShape::LegacyChar => 3,
            PrimitiveShape::Str => 4,
            PrimitiveShape::MutPointer => 5,
            PrimitiveShape::ConstPointer => 6,
            PrimitiveShape::Reference => 7,
            PrimitiveShape::Builtin => 8,
            PrimitiveShape::CPointer => 9,
            PrimitiveShape::CxxLvalueReference => 10,
            PrimitiveShape::CxxRvalueReference => 11,
            PrimitiveShape::CxxMemberPointer => 12,
            PrimitiveShape::ArbitraryInteger => 13,
            PrimitiveShape::NativeSignedInteger => 14,
            PrimitiveShape::NativeUnsignedInteger => 15,
            PrimitiveShape::PointerAddressInteger => 16,
            PrimitiveShape::UnicodeScalar => 17,
            PrimitiveShape::Utf16CodeUnit => 18,
            PrimitiveShape::Utf32CodeUnit => 19,
            PrimitiveShape::CPlainSignedChar => 20,
            PrimitiveShape::CPlainUnsignedChar => 21,
            PrimitiveShape::CSignedChar => 22,
            PrimitiveShape::CUnsignedChar => 23,
            PrimitiveShape::CWideChar => 24,
            PrimitiveShape::CBlockPointer => 25,
            PrimitiveShape::CWideSignedChar => 26,
            PrimitiveShape::CWideUnsignedChar => 27,
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
            3 => Ok(Self::LegacyChar),
            4 => Ok(Self::Str),
            5 => Ok(Self::MutPointer),
            6 => Ok(Self::ConstPointer),
            7 => Ok(Self::Reference),
            8 => Ok(Self::Builtin),
            9 => Ok(Self::CPointer),
            10 => Ok(Self::CxxLvalueReference),
            11 => Ok(Self::CxxRvalueReference),
            12 => Ok(Self::CxxMemberPointer),
            13 => Ok(Self::ArbitraryInteger),
            14 => Ok(Self::NativeSignedInteger),
            15 => Ok(Self::NativeUnsignedInteger),
            16 => Ok(Self::PointerAddressInteger),
            17 => Ok(Self::UnicodeScalar),
            18 => Ok(Self::Utf16CodeUnit),
            19 => Ok(Self::Utf32CodeUnit),
            20 => Ok(Self::CPlainSignedChar),
            21 => Ok(Self::CPlainUnsignedChar),
            22 => Ok(Self::CSignedChar),
            23 => Ok(Self::CUnsignedChar),
            24 => Ok(Self::CWideChar),
            25 => Ok(Self::CBlockPointer),
            26 => Ok(Self::CWideSignedChar),
            27 => Ok(Self::CWideUnsignedChar),
            actual => Err(PrimitiveShapeError { actual }),
        }
    }
}

/// Closed C-family cv/restrict qualifier set. The set is a property of one
/// exact type node; pointer qualifiers and pointee qualifiers therefore
/// occupy different structural nodes instead of sharing a mutability bit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CvQualifiers {
    /// `const` applies directly to this node.
    pub const_: bool,
    /// `volatile` applies directly to this node.
    pub volatile: bool,
    /// `restrict` applies directly to this node.
    pub restrict: bool,
}

/// Exact qualifier-bit rejection retaining the unmasked source cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CvQualifiersError {
    /// Complete rejected qualifier cell.
    pub actual: u32,
}

impl CvQualifiers {
    /// `const` on the directly represented node.
    pub const CONST: u8 = 1;
    /// `volatile` on the directly represented node.
    pub const VOLATILE: u8 = 1 << 1;
    /// `restrict` on the directly represented node.
    pub const RESTRICT: u8 = 1 << 2;
    /// Every admitted qualifier bit.
    pub const ALL: u8 = Self::CONST | Self::VOLATILE | Self::RESTRICT;

    /// No qualifiers.
    pub const NONE: Self = Self {
        const_: false,
        volatile: false,
        restrict: false,
    };

    /// Builds one closed qualifier set from individual direct facts.
    #[must_use]
    pub const fn new(const_: bool, volatile: bool, restrict: bool) -> Self {
        Self {
            const_,
            volatile,
            restrict,
        }
    }

    /// True when no direct qualifier is present.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.const_ && !self.volatile && !self.restrict
    }
}

impl From<CvQualifiers> for u8 {
    fn from(value: CvQualifiers) -> Self {
        (if value.const_ { CvQualifiers::CONST } else { 0 })
            | (if value.volatile {
                CvQualifiers::VOLATILE
            } else {
                0
            })
            | (if value.restrict {
                CvQualifiers::RESTRICT
            } else {
                0
            })
    }
}

impl TryFrom<u32> for CvQualifiers {
    type Error = CvQualifiersError;

    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        if actual & !u32::from(Self::ALL) != 0 {
            return Err(CvQualifiersError { actual });
        }
        Ok(Self {
            const_: actual & u32::from(Self::CONST) != 0,
            volatile: actual & u32::from(Self::VOLATILE) != 0,
            restrict: actual & u32::from(Self::RESTRICT) != 0,
        })
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
    /// A C-family qualifier cell names bits outside the closed set, or
    /// claims an empty wrapper that has no structural meaning.
    CvQualifiers {
        /// Complete rejected qualifier cell.
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

mod admit;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_family_forms_preserve_closed_arity_and_qualifier_placement() {
        let mut pointer = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        pointer.payload0 = u32::from(PrimitiveShape::CPointer);
        assert_eq!(pointer.validate(1), Ok(()));
        assert!(matches!(
            pointer.validate(0),
            Err(SemanticTypeFault::ChildCount { .. })
        ));

        let mut member = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        member.payload0 = u32::from(PrimitiveShape::CxxMemberPointer);
        assert_eq!(member.validate(2), Ok(()));
        assert!(matches!(
            member.validate(1),
            Err(SemanticTypeFault::ChildCount { .. })
        ));

        let mut qualified = SemanticTypeRecord::leaf(SemanticTypeTag::CQualified);
        qualified.payload0 = u32::from(u8::from(CvQualifiers::new(true, false, false)));
        assert_eq!(qualified.validate(1), Ok(()));
        qualified.payload0 = 0;
        assert!(matches!(
            qualified.validate(1),
            Err(SemanticTypeFault::CvQualifiers { actual: 0 })
        ));

        // Falsifier: width and signedness are part of a native character
        // fact, so a C `char` cannot reopen as the same row as Java `char`
        // or legacy character payloads.
        let mut native_char = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        native_char.payload0 = u32::from(PrimitiveShape::CPlainUnsignedChar);
        native_char.payload1 = TypeWidth::Fixed(8).to_cell();
        assert_eq!(native_char.validate(0), Ok(()));
        native_char.payload1 = TypeWidth::ARCH_FLAG;
        assert!(matches!(
            native_char.validate(0),
            Err(SemanticTypeFault::Width { .. })
        ));

        let mut block = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        block.payload0 = u32::from(PrimitiveShape::CBlockPointer);
        assert_eq!(block.validate(1), Ok(()));
        assert!(matches!(
            block.validate(0),
            Err(SemanticTypeFault::ChildCount { .. })
        ));
    }
}
