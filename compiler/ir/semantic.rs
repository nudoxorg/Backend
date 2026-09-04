//! Rich, compact semantic IR and its borrowed compiler-ingestion path.
//!
//! The immutable result is a set of flat arenas. Variable-length data is held
//! in typed interned list pools, text and binary symbols share one atom pool,
//! and graph adjacency is compressed into forward/reverse CSR indices. Nothing
//! in an entity, type, document, or link owns a box or string.

use alloc::{vec, vec::Vec};
use compiler_vocabulary::{CompileRecipeFact, Language, LanguageProfile};
use core::{fmt, hash::Hash, marker::PhantomData, num::NonZeroU16};
use heart_identity::{ContentId, SemanticScopeDomain};

use crate::{
    AnnotationKind, AtomId, AtomInterner, AtomTable, AtomTableView, AuthorityFactFault,
    AuthorityFactPlane, CapacityError, ChannelDirection, DenseId, EntityAuthorityColumns,
    EntityAuthorityFacts,
    DeclarationFamilyId, DeclarationIdentity, DeclarationKey, EntityId,
    ExternalDeclarationIdentity, ExternalEntityRef, FactAvailability, ImageProvenance, ImageProvenanceClaim,
    Interner, ListId, ListInterner,
    ListTable, ListTableView, OccurrenceAuthorityColumns,
    OccurrenceAuthorityFacts, PackageLineage, ParentageAuthority, PreimageOverflow, SemanticScopeClaim, SemanticScopeFacts,
    SourceIdentity, StableRef, TextId, Type, TypeId,
    VariantFingerprint,
    authority::{AuthorityColumns, OccurrenceAuthorityColumn},
    columnar::{RawColumn, Slab, SlabPlan},
    interner::{HashIndex, hash},
};

/// Marker for a cross-package graph target.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum External {}
/// Marker for a graph-link row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkSpace {}
/// Marker for one observed occurrence of a canonical graph relation.
///
/// A relation is unique by `(from, target, kind)`; an occurrence is not.
/// Multiple written reference sites can prove the same relation with distinct
/// spans and confidence, and remain independently queryable through this ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkOccurrenceSpace {}
/// Marker for an entity coordinate local to one borrowed tree submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TreeEntity {}

/// Dense ID of an external symbol descriptor.
pub type ExternalId = DenseId<External>;
/// Dense ID of a graph link.
pub type LinkId = DenseId<LinkSpace>;
/// Dense ID of one authority-observed graph occurrence site.
pub type LinkOccurrenceId = DenseId<LinkOccurrenceSpace>;
/// Entity coordinate local to a [`BorrowedTree`].
pub type TreeEntityId = DenseId<TreeEntity>;
/// Interned sequence of semantic types.
pub type TypeListId = ListId<TypeId>;
/// Interned sequence of child entities.
pub type EntityListId = ListId<EntityId>;
/// Interned sequence of atoms.
pub type AtomListId = ListId<AtomId>;
/// Interned sequence of documentation fragments.
pub type DocId = ListId<DocFragment>;
/// Interned sequence of TypeScript/Rust tuple elements with labels and modifiers.
pub type TupleElementListId = ListId<TupleElement>;
/// Interned sequence of structural object members.
pub type ObjectMemberListId = ListId<ObjectMember>;
/// Interned sequence of template-literal pieces.
pub type TemplatePartListId = ListId<TemplatePart>;
/// Interned sequence of generic parameter declarations.
pub type TypeParameterListId = ListId<TypeParameter>;
/// Interned source-ordered bounds of one generic parameter.
///
/// A bound is deliberately not just a type ID: Rust lifetime bounds occupy
/// the same written sequence as trait bounds, and a renderer/discovery view
/// must never recover their order from language-specific side tables.
pub type TypeParameterBoundListId = ListId<TypeParameterBound>;

/// Compatibility spelling for the one frozen declaration-kind vocabulary.
///
/// The underlying type and every discriminant come from
/// `compiler-ir-vocabulary::EntityKind`; `TypeAlias` remains only as that
/// type's narrow associated compatibility constant.
pub type ItemKind = compiler_ir_vocabulary::EntityKind;

/// Visibility independent of any one language's spelling.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Visibility {
    /// The admitting authority did not provide a visibility fact.
    Unknown,
    Private,
    Restricted,
    Package,
    Public,
}

/// Mutability carried by references, pointers, fields, and bindings.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Mutability {
    Immutable,
    Mutable,
}

/// C++ reference category. This is intentionally separate from Rust borrow
/// mutability and lifetimes: `T&&` is not `&mut T`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CxxReferenceCategory {
    Lvalue,
    Rvalue,
}

/// Cross-language primitive vocabulary. Language-specific spellings remain atoms.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BuiltinType {
    Unit,
    Never,
    Bool,
    /// Historical under-specified character role retained only for fragment
    /// compatibility. Fresh producers use [`NativeCharacterRole`].
    LegacyChar,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    F16,
    F32,
    F64,
    String,
    Bytes,
    Object,
    /// TypeScript's intentionally unchecked top type.
    Any,
    /// TypeScript's checked top type (distinct from an unresolved IR node).
    Unknown,
    Void,
    Number,
    BigInt,
    Symbol,
    UniqueSymbol,
    Null,
    Undefined,
    /// Python's singleton none type; the suffix avoids colliding with `Option`-style names.
    None_ = 30,
    /// Python's heterogeneous growable sequence.
    List,
    /// Python's associative mapping from keys to values.
    Dict,
    /// Python's unordered mutable collection of distinct values.
    Set,
    /// Python's immutable set.
    FrozenSet,
    /// Python's exact complex builtin.
    Complex = 36,
    /// C# decimal fixed-point builtin.
    Decimal = 37,
    /// Arbitrary-precision signed integer (`Python int`).
    ArbitraryInteger = 38,
    /// Signed target machine word (`isize`, Go `int`, C# `nint`).
    NativeSignedInteger = 39,
    /// Unsigned target machine word (`usize`, Go `uint`).
    NativeUnsignedInteger = 40,
    /// Unsigned pointer-address integer (`uintptr`, C# `nuint`).
    PointerAddressInteger = 41,
}

/// Closed semantic role of a character scalar/code-unit node. The concrete
/// node also carries an exact measured width, so C target facts never
/// collapse into Rust/Java/C# spelling coincidences.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeCharacterRole {
    /// Rust's Unicode scalar value (`char`).
    UnicodeScalar,
    /// Java/C# `char` and C++ `char16_t`.
    Utf16CodeUnit,
    /// C++ `char32_t`.
    Utf32CodeUnit,
    /// Plain C/C++ `char` where the native authority reports signed form.
    CPlainSigned,
    /// Plain C/C++ `char` where the native authority reports unsigned form.
    CPlainUnsigned,
    /// Explicit C/C++ `signed char`.
    CSigned,
    /// Explicit C/C++ `unsigned char`.
    CUnsigned,
    /// `wchar_t` with signed representation verified by native authority.
    CWideSigned,
    /// `wchar_t` with unsigned representation verified by native authority.
    CWideUnsigned,
    /// `wchar_t` where libclang retained no signedness authority. This is a
    /// real unavailable fact, never an invented signed integer.
    CWideSignednessUnavailable,
}

/// Closed cause for an explicitly unknown semantic type.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UnknownReason {
    Unannotated,
    DynamicallyTyped,
    UnresolvedLocalName,
    UnresolvedExternal,
    TruncatedAtDepthLimit,
    OracleGap,
    NoIrRepresentation,
    Error,
}

/// Why a frontend could not produce a more precise type, together with the
/// exact optional source spelling the authority supplied.  Unknown is a real
/// semantic state, not a missing `TypeId`; a spelling is an atom so it
/// participates in owned IR, render, and durable comparison without a side
/// channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnknownType {
    pub reason: UnknownReason,
    pub spelling: Option<AtomId>,
}

impl UnknownType {
    #[must_use]
    pub const fn new(reason: UnknownReason) -> Self {
        Self {
            reason,
            spelling: None,
        }
    }

    #[must_use]
    pub const fn with_spelling(self, spelling: AtomId) -> Self {
        Self {
            reason: self.reason,
            spelling: Some(spelling),
        }
    }

}

/// One hash-consed semantic type node.
///
/// Concrete types are directly renderable shapes. Computed types are explicit
/// type-level programs whose result depends on another type/environment. The
/// split makes it impossible for a resolver, VCS classifier, or Trustfall
/// adapter to accidentally report an unevaluated conditional/mapped type as a
/// resolved concrete result.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeExpr {
    Concrete(ConcreteType),
    Computed(ComputedType),
    Unknown(UnknownType),
}

/// Discriminator-free state-indexed type term.
///
/// This is the stable representation of the useful GADT property: the state
/// selects exactly one associated node type, and the private field makes every
/// other case unconstructible. `repr(transparent)` guarantees that a concrete,
/// computed, or unknown term is exactly the size and ABI of that selected node.
/// The literal never-guarded enum formulation still retains a discriminator on
/// stable Rust and is therefore intentionally confined to compile-time design,
/// not stored or passed through the lowering hot path.
#[repr(transparent)]
pub struct GuardedType<State: TypeState> {
    node: State::Node,
}

impl GuardedType<ConcreteState> {
    /// Constructs the only inhabited concrete-state variant.
    #[must_use]
    pub const fn concrete(node: ConcreteType) -> Self {
        Self { node }
    }
}

impl GuardedType<ComputedState> {
    /// Constructs the only inhabited computed-state variant.
    #[must_use]
    pub const fn computed(node: ComputedType) -> Self {
        Self { node }
    }
}

impl GuardedType<UnknownState> {
    /// Constructs the only inhabited unknown-state variant.
    #[must_use]
    pub const fn unknown(node: UnknownType) -> Self {
        Self { node }
    }
}

mod type_state_sealed {
    pub trait Sealed {}
}

/// Marker for a statically proven concrete semantic type.
pub enum ConcreteState {}
/// Marker for a statically proven computed semantic type.
pub enum ComputedState {}
/// Marker for a statically proven unknown semantic type.
pub enum UnknownState {}

impl type_state_sealed::Sealed for ConcreteState {}
impl type_state_sealed::Sealed for ComputedState {}
impl type_state_sealed::Sealed for UnknownState {}

/// Sealed state-index mapping from a type marker to its exact node type.
pub trait TypeState: type_state_sealed::Sealed {
    /// Node guaranteed by a [`TypedTypeId`] carrying this state.
    type Node;
    #[doc(hidden)]
    fn project(expression: TypeExpr) -> Option<Self::Node>;
    #[doc(hidden)]
    fn inject(node: Self::Node) -> TypeExpr;
}

impl TypeState for ConcreteState {
    type Node = ConcreteType;

    fn project(expression: TypeExpr) -> Option<Self::Node> {
        match expression {
            TypeExpr::Concrete(node) => Some(node),
            TypeExpr::Computed(_) | TypeExpr::Unknown(_) => None,
        }
    }

    fn inject(node: Self::Node) -> TypeExpr {
        TypeExpr::Concrete(node)
    }
}

impl TypeState for ComputedState {
    type Node = ComputedType;

    fn project(expression: TypeExpr) -> Option<Self::Node> {
        match expression {
            TypeExpr::Computed(node) => Some(node),
            TypeExpr::Concrete(_) | TypeExpr::Unknown(_) => None,
        }
    }

    fn inject(node: Self::Node) -> TypeExpr {
        TypeExpr::Computed(node)
    }
}

impl TypeState for UnknownState {
    type Node = UnknownType;

    fn project(expression: TypeExpr) -> Option<Self::Node> {
        match expression {
            TypeExpr::Unknown(node) => Some(node),
            TypeExpr::Concrete(_) | TypeExpr::Computed(_) => None,
        }
    }

    fn inject(node: Self::Node) -> TypeExpr {
        TypeExpr::Unknown(node)
    }
}

/// Four-byte semantic type coordinate carrying a compile-time node-state proof.
///
/// The constructor is intentionally private: only the matching interning path
/// can manufacture a concrete, computed, or unknown proof. `erase()` is free
/// when a heterogeneous type edge is required.
#[repr(transparent)]
pub struct TypedTypeId<State> {
    erased: TypeId,
    state: PhantomData<fn() -> State>,
}

impl<State> TypedTypeId<State> {
    const fn proven(erased: TypeId) -> Self {
        Self {
            erased,
            state: PhantomData,
        }
    }

    /// Erases only the static state while preserving the exact coordinate.
    #[must_use]
    pub const fn erase(self) -> TypeId {
        self.erased
    }
}

pub(crate) const fn reopened_computed_type(id: TypeId) -> ComputedTypeId {
    TypedTypeId::proven(id)
}

impl<State> Copy for TypedTypeId<State> {}
impl<State> Clone for TypedTypeId<State> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<State> PartialEq for TypedTypeId<State> {
    fn eq(&self, other: &Self) -> bool {
        self.erased == other.erased
    }
}
impl<State> Eq for TypedTypeId<State> {}
impl<State> Hash for TypedTypeId<State> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.erased.hash(state);
    }
}
impl<State> fmt::Debug for TypedTypeId<State> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.erased.fmt(formatter)
    }
}

/// Coordinate proven to reference [`ConcreteType`].
pub type ConcreteTypeId = TypedTypeId<ConcreteState>;
/// Coordinate proven to reference [`ComputedType`].
pub type ComputedTypeId = TypedTypeId<ComputedState>;
/// Coordinate proven to reference [`UnknownType`].
pub type UnknownTypeId = TypedTypeId<UnknownState>;

/// Eight-byte final type-directory row. Single-operand nodes store their
/// operand inline; multi-operand nodes index an exact-arity cold lane.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeTag {
    Builtin,
    Literal,
    Nominal,
    External,
    Parameter,
    Applied,
    Tuple,
    Object,
    Function,
    Reference,
    Pointer,
    Slice,
    Array,
    Optional,
    Union,
    Intersection,
    KeyOf,
    TypeOf,
    IndexedAccess,
    Conditional,
    Mapped,
    Infer,
    TemplateLiteral,
    Import,
    Awaited,
    This,
    Unknown,
    ImplTrait,
    DynTrait,
    Wildcard,
    Annotated,
    Inferred,
    QualifiedPath,
    Map,
    Channel,
    CxxReference,
    CPointer,
    CxxMemberPointer,
    CQualified,
    CBlockPointer,
    NativeCharacter,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeHeader {
    /// Closed internal node tag.
    pub tag: TypeTag,
    /// Boolean/small-enum flags specific to the tag.
    pub flags: u8,
    /// Small inline discriminant or modifier bits.
    pub auxiliary: u16,
    /// Inline operand or dense exact-arity cold-lane ordinal.
    pub payload: u32,
}

/// Two cold operands with no padding for unused generic payload slots.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypePairPayload(pub [u32; 2]);

/// Three cold operands with no padding for unused generic payload slots.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeTriplePayload(pub [u32; 3]);

/// Four cold operands with no padding for unused generic payload slots.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeQuadPayload(pub [u32; 4]);

/// Borrowed storage columns for the compact final type arena.
#[derive(Clone, Copy, Debug)]
pub struct TypeColumns<'ir> {
    /// `TypeId`-indexed eight-byte directory.
    pub headers: &'ir [TypeHeader],
    /// Dense two-operand lane referenced only by matching headers.
    pub pairs: &'ir [TypePairPayload],
    /// Dense three-operand lane referenced only by matching headers.
    pub triples: &'ir [TypeTriplePayload],
    /// Dense four-operand lane referenced only by matching headers.
    pub quads: &'ir [TypeQuadPayload],
}

#[derive(Default)]
struct PackedTypes {
    headers: Vec<TypeHeader>,
    pairs: Vec<TypePairPayload>,
    triples: Vec<TypeTriplePayload>,
    quads: Vec<TypeQuadPayload>,
}

impl PackedTypes {
    fn reserve(&mut self, additional: usize) {
        self.headers.reserve(additional);
    }

    fn pair(&mut self, values: [u32; 2]) -> u32 {
        let ordinal = self.pairs.len() as u32;
        self.pairs.push(TypePairPayload(values));
        ordinal
    }

    fn triple(&mut self, values: [u32; 3]) -> u32 {
        let ordinal = self.triples.len() as u32;
        self.triples.push(TypeTriplePayload(values));
        ordinal
    }

    fn quad(&mut self, values: [u32; 4]) -> u32 {
        let ordinal = self.quads.len() as u32;
        self.quads.push(TypeQuadPayload(values));
        ordinal
    }

    fn push(&mut self, ty: TypeExpr) {
        let header = match ty {
            TypeExpr::Concrete(ty) => match ty {
                ConcreteType::Builtin(value) => header(TypeTag::Builtin, 0, value as u16, 0),
                ConcreteType::Literal(value) => match value {
                    LiteralType::String(value) => header(TypeTag::Literal, 0, 0, value.raw),
                    LiteralType::Number(value) => header(TypeTag::Literal, 1, 0, value.raw),
                    LiteralType::BigInt(value) => header(TypeTag::Literal, 2, 0, value.raw),
                    LiteralType::Boolean(value) => header(TypeTag::Literal, 3, u16::from(value), 0),
                    LiteralType::Null => header(TypeTag::Literal, 4, 0, 0),
                    LiteralType::Undefined => header(TypeTag::Literal, 5, 0, 0),
                },
                ConcreteType::Nominal(value) => header(TypeTag::Nominal, 0, 0, value.raw),
                ConcreteType::External(value) => header(TypeTag::External, 0, 0, value.raw),
                ConcreteType::Parameter(value) => header(TypeTag::Parameter, 0, 0, value.raw),
                ConcreteType::Applied {
                    constructor,
                    arguments,
                } => header(
                    TypeTag::Applied,
                    0,
                    0,
                    self.pair([constructor.raw, arguments.raw]),
                ),
                ConcreteType::Tuple(value) => header(TypeTag::Tuple, 0, 0, value.raw),
                ConcreteType::Object(value) => header(TypeTag::Object, 0, 0, value.raw),
                ConcreteType::Function {
                    parameters,
                    results,
                    abi,
                    variadic,
                    unsafe_,
                } => header(
                    TypeTag::Function,
                    variadic as u8 | (u8::from(unsafe_) << 2),
                    0,
                    self.triple([parameters.raw, results.raw, option_raw(abi)]),
                ),
                ConcreteType::Reference {
                    target,
                    mutability,
                    lifetime,
                } => header(
                    TypeTag::Reference,
                    mutability as u8,
                    0,
                    self.pair([target.raw, option_raw(lifetime)]),
                ),
                ConcreteType::CxxReference { target, category } => header(
                    TypeTag::CxxReference,
                    category as u8,
                    0,
                    target.raw,
                ),
                ConcreteType::CPointer { target } => {
                    header(TypeTag::CPointer, 0, 0, target.raw)
                }
                ConcreteType::CxxMemberPointer {
                    owner,
                    member,
                } => header(
                    TypeTag::CxxMemberPointer,
                    0,
                    0,
                    self.pair([owner.raw, member.raw]),
                ),
                ConcreteType::CQualified { target, qualifiers } => header(
                    TypeTag::CQualified,
                    u8::from(qualifiers),
                    0,
                    target.raw,
                ),
                ConcreteType::CBlockPointer { target } => {
                    header(TypeTag::CBlockPointer, 0, 0, target.raw)
                }
                ConcreteType::NativeCharacter { role, width } => {
                    header(TypeTag::NativeCharacter, role as u8, width.get(), 0)
                }
                ConcreteType::Pointer { target, mutability } => {
                    header(TypeTag::Pointer, mutability as u8, 0, target.raw)
                }
                ConcreteType::Slice(value) => header(TypeTag::Slice, 0, 0, value.raw),
                ConcreteType::Array { element, shape } => match shape {
                    ArrayShape::Sequence => header(TypeTag::Array, 0, 0, element.raw),
                    ArrayShape::Rectangular { rank } => {
                        header(TypeTag::Array, 1, rank.get(), element.raw)
                    }
                    ArrayShape::FixedValue { length } => {
                        let bytes = length.to_le_bytes();
                        header(
                            TypeTag::Array,
                            2,
                            0,
                            self.triple([
                                element.raw,
                                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                                u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                            ]),
                        )
                    }
                    ArrayShape::ConstExpression(expression) => header(
                        TypeTag::Array,
                        3,
                        0,
                        self.pair([element.raw, expression.raw]),
                    ),
                    ArrayShape::Incomplete => header(TypeTag::Array, 4, 0, element.raw),
                },
                ConcreteType::Optional(value) => header(TypeTag::Optional, 0, 0, value.raw),
                ConcreteType::Union(value) => header(TypeTag::Union, 0, 0, value.raw),
                ConcreteType::Intersection(value) => header(TypeTag::Intersection, 0, 0, value.raw),
                ConcreteType::ImplTrait(value) => header(TypeTag::ImplTrait, 0, 0, value.raw),
                ConcreteType::DynTrait(value) => header(TypeTag::DynTrait, 0, 0, value.raw),
                ConcreteType::Wildcard(bound) => match bound {
                    WildcardBound::Unbounded => header(TypeTag::Wildcard, 0, 0, 0),
                    WildcardBound::Extends(bound) => {
                        header(TypeTag::Wildcard, 1, 0, bound.raw)
                    }
                    WildcardBound::Super(bound) => header(TypeTag::Wildcard, 2, 0, bound.raw),
                },
                ConcreteType::Annotated { kind, target } => {
                    header(TypeTag::Annotated, kind as u8, 0, target.raw)
                }
                ConcreteType::Inferred(spelling) => {
                    header(TypeTag::Inferred, 0, 0, option_raw(spelling))
                }
                ConcreteType::QualifiedPath {
                    self_type,
                    trait_type,
                    segments,
                    spelling,
                } => header(
                    TypeTag::QualifiedPath,
                    match segments {
                        QualifiedSegments::Captured(_) => 1,
                        QualifiedSegments::Unavailable => 0,
                    },
                    0,
                    self.quad([
                        self_type.raw,
                        option_raw(trait_type),
                        match segments {
                            QualifiedSegments::Captured(segments) => segments.raw,
                            QualifiedSegments::Unavailable => 0,
                        },
                        spelling.raw,
                    ]),
                ),
                ConcreteType::Map { key, value } => {
                    header(TypeTag::Map, 0, 0, self.pair([key.raw, value.raw]))
                }
                ConcreteType::Channel { direction, element } => {
                    header(TypeTag::Channel, direction as u8, 0, element.raw)
                }
            },
            TypeExpr::Computed(ty) => match ty {
                ComputedType::KeyOf(value) => header(TypeTag::KeyOf, 0, 0, value.raw),
                ComputedType::TypeOf(query) => match query {
                    TypeQuery::Entity(value) => header(TypeTag::TypeOf, 0, 0, value.raw),
                    TypeQuery::Path(value) => header(TypeTag::TypeOf, 1, 0, value.raw),
                    TypeQuery::External(value) => header(TypeTag::TypeOf, 2, 0, value.raw),
                },
                ComputedType::IndexedAccess { object, index } => header(
                    TypeTag::IndexedAccess,
                    0,
                    0,
                    self.pair([object.raw, index.raw]),
                ),
                ComputedType::Conditional {
                    check,
                    extends,
                    then_type,
                    else_type,
                    distributive,
                } => header(
                    TypeTag::Conditional,
                    u8::from(distributive),
                    0,
                    self.quad([check.raw, extends.raw, then_type.raw, else_type.raw]),
                ),
                ComputedType::Mapped {
                    parameter,
                    constraint,
                    name_as,
                    value,
                    readonly,
                    optional,
                } => header(
                    TypeTag::Mapped,
                    0,
                    u16::from(readonly as u8) | (u16::from(optional as u8) << 8),
                    self.quad([
                        parameter.raw,
                        constraint.raw,
                        option_raw(name_as),
                        value.raw,
                    ]),
                ),
                ComputedType::Infer {
                    parameter,
                    constraint,
                } => header(
                    TypeTag::Infer,
                    0,
                    0,
                    self.pair([parameter.raw, option_raw(constraint)]),
                ),
                ComputedType::TemplateLiteral(value) => {
                    header(TypeTag::TemplateLiteral, 0, 0, value.raw)
                }
                ComputedType::Import {
                    specifier,
                    qualifier,
                    arguments,
                } => header(
                    TypeTag::Import,
                    0,
                    0,
                    self.triple([specifier.raw, qualifier.raw, arguments.raw]),
                ),
                ComputedType::Awaited(value) => header(TypeTag::Awaited, 0, 0, value.raw),
                ComputedType::This => header(TypeTag::This, 0, 0, 0),
            },
            TypeExpr::Unknown(value) => header(
                TypeTag::Unknown,
                0,
                value.reason as u16,
                option_raw(value.spelling),
            ),
        };
        self.headers.push(header);
    }

    fn get(&self, id: TypeId) -> Option<TypeExpr> {
        let value = *self.headers.get(id.index())?;
        self.decode(value)
    }

    fn decode(&self, value: TypeHeader) -> Option<TypeExpr> {
        let pair = |ordinal: u32| self.pairs.get(ordinal as usize).map(|value| value.0);
        let triple = |ordinal: u32| self.triples.get(ordinal as usize).map(|value| value.0);
        let quad = |ordinal: u32| self.quads.get(ordinal as usize).map(|value| value.0);
        Some(match value.tag {
            TypeTag::Builtin => {
                TypeExpr::Concrete(ConcreteType::Builtin(builtin_from(value.auxiliary)?))
            }
            TypeTag::Literal => TypeExpr::Concrete(ConcreteType::Literal(match value.flags {
                0 => LiteralType::String(AtomId::new(value.payload)),
                1 => LiteralType::Number(AtomId::new(value.payload)),
                2 => LiteralType::BigInt(AtomId::new(value.payload)),
                3 => LiteralType::Boolean(value.auxiliary != 0),
                4 => LiteralType::Null,
                5 => LiteralType::Undefined,
                _ => return None,
            })),
            TypeTag::Nominal => {
                TypeExpr::Concrete(ConcreteType::Nominal(EntityId::new(value.payload)))
            }
            TypeTag::External => {
                TypeExpr::Concrete(ConcreteType::External(ExternalId::new(value.payload)))
            }
            TypeTag::Parameter => {
                TypeExpr::Concrete(ConcreteType::Parameter(AtomId::new(value.payload)))
            }
            TypeTag::Applied => {
                let data = pair(value.payload)?;
                TypeExpr::Concrete(ConcreteType::Applied {
                    constructor: TypeId::new(data[0]),
                    arguments: TypeListId::new(data[1]),
                })
            }
            TypeTag::Tuple => {
                TypeExpr::Concrete(ConcreteType::Tuple(TupleElementListId::new(value.payload)))
            }
            TypeTag::Object => {
                TypeExpr::Concrete(ConcreteType::Object(ObjectMemberListId::new(value.payload)))
            }
            TypeTag::Function => {
                let data = triple(value.payload)?;
                let variadic = match value.flags & 0b11 {
                    0 => VariadicForm::None,
                    1 => VariadicForm::TypedLast,
                    2 => VariadicForm::CUnbounded,
                    _ => return None,
                };
                if value.flags & !0b111 != 0 {
                    return None;
                }
                TypeExpr::Concrete(ConcreteType::Function {
                    parameters: TupleElementListId::new(data[0]),
                    results: TupleElementListId::new(data[1]),
                    abi: raw_option(data[2]).map(AtomId::new),
                    variadic,
                    unsafe_: value.flags & 4 != 0,
                })
            }
            TypeTag::Reference => {
                let data = pair(value.payload)?;
                TypeExpr::Concrete(ConcreteType::Reference {
                    target: TypeId::new(data[0]),
                    mutability: mutability_from(value.flags)?,
                    lifetime: raw_option(data[1]).map(AtomId::new),
                })
            }
            TypeTag::CxxReference if value.auxiliary == 0 => {
                TypeExpr::Concrete(ConcreteType::CxxReference {
                    target: TypeId::new(value.payload),
                    category: cxx_reference_category_from(value.flags)?,
                })
            }
            TypeTag::CPointer if value.flags == 0 && value.auxiliary == 0 => {
                TypeExpr::Concrete(ConcreteType::CPointer {
                    target: TypeId::new(value.payload),
                })
            }
            TypeTag::CxxMemberPointer => {
                if value.flags != 0 || value.auxiliary != 0 {
                    return None;
                }
                let data = pair(value.payload)?;
                TypeExpr::Concrete(ConcreteType::CxxMemberPointer {
                    owner: TypeId::new(data[0]),
                    member: TypeId::new(data[1]),
                })
            }
            TypeTag::CQualified if value.auxiliary == 0 => {
                let qualifiers = cxx_qualifiers_from(value.flags)?;
                if qualifiers.is_empty() {
                    return None;
                }
                TypeExpr::Concrete(ConcreteType::CQualified {
                    target: TypeId::new(value.payload),
                    qualifiers,
                })
            }
            TypeTag::CBlockPointer if value.flags == 0 && value.auxiliary == 0 => {
                TypeExpr::Concrete(ConcreteType::CBlockPointer {
                    target: TypeId::new(value.payload),
                })
            }
            TypeTag::NativeCharacter if value.payload == 0 => {
                TypeExpr::Concrete(ConcreteType::NativeCharacter {
                    role: native_character_role_from(value.flags)?,
                    width: NonZeroU16::new(value.auxiliary)?,
                })
            }
            TypeTag::CxxReference
            | TypeTag::CPointer
            | TypeTag::CQualified
            | TypeTag::CBlockPointer
            | TypeTag::NativeCharacter => return None,
            TypeTag::Pointer => TypeExpr::Concrete(ConcreteType::Pointer {
                target: TypeId::new(value.payload),
                mutability: mutability_from(value.flags)?,
            }),
            TypeTag::Slice => TypeExpr::Concrete(ConcreteType::Slice(TypeId::new(value.payload))),
            TypeTag::Array => TypeExpr::Concrete(ConcreteType::Array {
                element: match value.flags {
                    0 | 1 | 4 => TypeId::new(value.payload),
                    2 => TypeId::new(triple(value.payload)?[0]),
                    3 => TypeId::new(pair(value.payload)?[0]),
                    _ => return None,
                },
                shape: match value.flags {
                    0 if value.auxiliary == 0 => ArrayShape::Sequence,
                    1 => ArrayShape::Rectangular {
                        rank: NonZeroU16::new(value.auxiliary)?,
                    },
                    2 if value.auxiliary == 0 => {
                        let data = triple(value.payload)?;
                        ArrayShape::FixedValue {
                            length: u64::from(data[1]) | (u64::from(data[2]) << 32),
                        }
                    }
                    3 if value.auxiliary == 0 => {
                        ArrayShape::ConstExpression(AtomId::new(pair(value.payload)?[1]))
                    }
                    4 if value.auxiliary == 0 => ArrayShape::Incomplete,
                    _ => return None,
                },
            }),
            TypeTag::Optional => {
                TypeExpr::Concrete(ConcreteType::Optional(TypeId::new(value.payload)))
            }
            TypeTag::Union => {
                TypeExpr::Concrete(ConcreteType::Union(TypeListId::new(value.payload)))
            }
            TypeTag::Intersection => {
                TypeExpr::Concrete(ConcreteType::Intersection(TypeListId::new(value.payload)))
            }
            TypeTag::ImplTrait => {
                TypeExpr::Concrete(ConcreteType::ImplTrait(TypeListId::new(value.payload)))
            }
            TypeTag::DynTrait => {
                TypeExpr::Concrete(ConcreteType::DynTrait(TypeListId::new(value.payload)))
            }
            TypeTag::Wildcard => TypeExpr::Concrete(ConcreteType::Wildcard(match value.flags {
                0 if value.payload == 0 => WildcardBound::Unbounded,
                1 => WildcardBound::Extends(TypeId::new(value.payload)),
                2 => WildcardBound::Super(TypeId::new(value.payload)),
                _ => return None,
            })),
            TypeTag::Annotated => TypeExpr::Concrete(ConcreteType::Annotated {
                kind: annotation_kind_from(value.flags)?,
                target: TypeId::new(value.payload),
            }),
            TypeTag::Inferred => TypeExpr::Concrete(ConcreteType::Inferred(
                raw_option(value.payload).map(AtomId::new),
            )),
            TypeTag::QualifiedPath => {
                let data = quad(value.payload)?;
                TypeExpr::Concrete(ConcreteType::QualifiedPath {
                    self_type: TypeId::new(data[0]),
                    trait_type: raw_option(data[1]).map(TypeId::new),
                    segments: match value.flags {
                        0 => QualifiedSegments::Unavailable,
                        1 => QualifiedSegments::Captured(AtomListId::new(data[2])),
                        _ => return None,
                    },
                    spelling: AtomId::new(data[3]),
                })
            }
            TypeTag::Map => {
                let data = pair(value.payload)?;
                TypeExpr::Concrete(ConcreteType::Map {
                    key: TypeId::new(data[0]),
                    value: TypeId::new(data[1]),
                })
            }
            TypeTag::Channel => TypeExpr::Concrete(ConcreteType::Channel {
                direction: channel_direction_from(value.flags)?,
                element: TypeId::new(value.payload),
            }),
            TypeTag::KeyOf => TypeExpr::Computed(ComputedType::KeyOf(TypeId::new(value.payload))),
            TypeTag::TypeOf => TypeExpr::Computed(ComputedType::TypeOf(match value.flags {
                0 => TypeQuery::Entity(EntityId::new(value.payload)),
                1 => TypeQuery::Path(AtomListId::new(value.payload)),
                2 => TypeQuery::External(ExternalId::new(value.payload)),
                _ => return None,
            })),
            TypeTag::IndexedAccess => {
                let data = pair(value.payload)?;
                TypeExpr::Computed(ComputedType::IndexedAccess {
                    object: TypeId::new(data[0]),
                    index: TypeId::new(data[1]),
                })
            }
            TypeTag::Conditional => {
                let data = quad(value.payload)?;
                TypeExpr::Computed(ComputedType::Conditional {
                    check: TypeId::new(data[0]),
                    extends: TypeId::new(data[1]),
                    then_type: TypeId::new(data[2]),
                    else_type: TypeId::new(data[3]),
                    distributive: value.flags != 0,
                })
            }
            TypeTag::Mapped => {
                let data = quad(value.payload)?;
                TypeExpr::Computed(ComputedType::Mapped {
                    parameter: AtomId::new(data[0]),
                    constraint: TypeId::new(data[1]),
                    name_as: raw_option(data[2]).map(TypeId::new),
                    value: TypeId::new(data[3]),
                    readonly: modifier_from(value.auxiliary as u8)?,
                    optional: modifier_from((value.auxiliary >> 8) as u8)?,
                })
            }
            TypeTag::Infer => {
                let data = pair(value.payload)?;
                TypeExpr::Computed(ComputedType::Infer {
                    parameter: AtomId::new(data[0]),
                    constraint: raw_option(data[1]).map(TypeId::new),
                })
            }
            TypeTag::TemplateLiteral => TypeExpr::Computed(ComputedType::TemplateLiteral(
                TemplatePartListId::new(value.payload),
            )),
            TypeTag::Import => {
                let data = triple(value.payload)?;
                TypeExpr::Computed(ComputedType::Import {
                    specifier: AtomId::new(data[0]),
                    qualifier: AtomListId::new(data[1]),
                    arguments: TypeListId::new(data[2]),
                })
            }
            TypeTag::Awaited => {
                TypeExpr::Computed(ComputedType::Awaited(TypeId::new(value.payload)))
            }
            TypeTag::This => TypeExpr::Computed(ComputedType::This),
            TypeTag::Unknown => TypeExpr::Unknown(UnknownType {
                reason: unknown_from(value.auxiliary)?,
                spelling: raw_option(value.payload).map(AtomId::new),
            }),
        })
    }

    fn columns(&self) -> TypeColumns<'_> {
        TypeColumns {
            headers: &self.headers,
            pairs: &self.pairs,
            triples: &self.triples,
            quads: &self.quads,
        }
    }
}

/// Builder-side hash-consing performed directly over the final packed lanes.
///
/// Equality reconstructs a candidate only when a 32-bit hash slot matches.
/// This removes the former `Vec<TypeExpr>` and its finish-time transposition:
/// the eight-byte header written during interning is the header retained by
/// renderers, VCS, Trustfall, storage, and vector adapters.
#[derive(Default)]
struct TypeInterner {
    packed: PackedTypes,
    index: HashIndex,
}

impl TypeInterner {
    fn reserve(&mut self, additional: usize) {
        self.packed.reserve(additional);
        self.index.reserve(additional);
    }

    fn intern(&mut self, ty: TypeExpr) -> Result<TypeId, CapacityError> {
        let value_hash = hash(&ty);
        if let Some(ordinal) = self.index.find(value_hash, |ordinal| {
            self.packed.get(TypeId::new(ordinal)) == Some(ty)
        }) {
            return Ok(TypeId::new(ordinal));
        }
        let id = TypeId::try_from_index(self.packed.headers.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: self.packed.headers.len(),
        })?;
        self.packed.push(ty);
        self.index.insert(value_hash, id.raw);
        Ok(id)
    }

    fn get(&self, id: TypeId) -> Option<TypeExpr> {
        self.packed.get(id)
    }

    fn len(&self) -> usize {
        self.packed.headers.len()
    }

    fn freeze(self) -> PackedTypes {
        self.packed
    }
}

#[cfg(test)]
mod packed_type_tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn every_packed_tag_and_flag_round_trips_exactly() {
        let mut types = vec![
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::String(AtomId::new(1)))),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::Number(AtomId::new(2)))),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::BigInt(AtomId::new(3)))),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::Boolean(false))),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::Boolean(true))),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::Null)),
            TypeExpr::Concrete(ConcreteType::Literal(LiteralType::Undefined)),
            TypeExpr::Concrete(ConcreteType::Nominal(EntityId::new(4))),
            TypeExpr::Concrete(ConcreteType::External(ExternalId::new(5))),
            TypeExpr::Concrete(ConcreteType::Parameter(AtomId::new(6))),
            TypeExpr::Concrete(ConcreteType::Applied {
                constructor: TypeId::new(7),
                arguments: TypeListId::new(8),
            }),
            TypeExpr::Concrete(ConcreteType::Tuple(TupleElementListId::new(9))),
            TypeExpr::Concrete(ConcreteType::Object(ObjectMemberListId::new(10))),
            TypeExpr::Concrete(ConcreteType::Function {
                parameters: TupleElementListId::new(11),
                results: TupleElementListId::new(12),
                abi: Some(AtomId::new(13)),
                variadic: VariadicForm::TypedLast,
                unsafe_: true,
            }),
            TypeExpr::Concrete(ConcreteType::Reference {
                target: TypeId::new(14),
                mutability: Mutability::Mutable,
                lifetime: Some(AtomId::new(15)),
            }),
            TypeExpr::Concrete(ConcreteType::Pointer {
                target: TypeId::new(16),
                mutability: Mutability::Immutable,
            }),
            TypeExpr::Concrete(ConcreteType::CxxReference {
                target: TypeId::new(17),
                category: CxxReferenceCategory::Rvalue,
            }),
            TypeExpr::Concrete(ConcreteType::CPointer {
                target: TypeId::new(18),
            }),
            TypeExpr::Concrete(ConcreteType::CxxMemberPointer {
                owner: TypeId::new(19),
                member: TypeId::new(20),
            }),
            TypeExpr::Concrete(ConcreteType::CQualified {
                target: TypeId::new(21),
                qualifiers: crate::CvQualifiers::new(true, false, false),
            }),
            TypeExpr::Concrete(ConcreteType::Slice(TypeId::new(17))),
            TypeExpr::Concrete(ConcreteType::Array {
                element: TypeId::new(18),
                shape: ArrayShape::ConstExpression(AtomId::new(19)),
            }),
            TypeExpr::Concrete(ConcreteType::Optional(TypeId::new(20))),
            TypeExpr::Concrete(ConcreteType::Union(TypeListId::new(21))),
            TypeExpr::Concrete(ConcreteType::Intersection(TypeListId::new(22))),
            TypeExpr::Concrete(ConcreteType::ImplTrait(TypeListId::new(23))),
            TypeExpr::Concrete(ConcreteType::DynTrait(TypeListId::new(24))),
            TypeExpr::Concrete(ConcreteType::Wildcard(WildcardBound::Unbounded)),
            TypeExpr::Concrete(ConcreteType::Wildcard(WildcardBound::Extends(TypeId::new(25)))),
            TypeExpr::Concrete(ConcreteType::Wildcard(WildcardBound::Super(TypeId::new(26)))),
            TypeExpr::Concrete(ConcreteType::Annotated {
                kind: AnnotationKind::Readonly,
                target: TypeId::new(27),
            }),
            TypeExpr::Concrete(ConcreteType::Annotated {
                kind: AnnotationKind::NullableValue,
                target: TypeId::new(28),
            }),
            TypeExpr::Concrete(ConcreteType::Inferred(Some(AtomId::new(29)))),
            TypeExpr::Concrete(ConcreteType::QualifiedPath {
                self_type: TypeId::new(30),
                trait_type: Some(TypeId::new(31)),
                segments: QualifiedSegments::Captured(AtomListId::new(32)),
                spelling: AtomId::new(33),
            }),
            TypeExpr::Concrete(ConcreteType::Map {
                key: TypeId::new(34),
                value: TypeId::new(35),
            }),
            TypeExpr::Concrete(ConcreteType::Channel {
                direction: ChannelDirection::Receive,
                element: TypeId::new(36),
            }),
            TypeExpr::Concrete(ConcreteType::Array {
                element: TypeId::new(37),
                shape: ArrayShape::Sequence,
            }),
            TypeExpr::Concrete(ConcreteType::Array {
                element: TypeId::new(38),
                shape: ArrayShape::Rectangular {
                    rank: NonZeroU16::new(2).expect("nonzero rank"),
                },
            }),
            TypeExpr::Concrete(ConcreteType::Array {
                element: TypeId::new(39),
                shape: ArrayShape::FixedValue { length: u64::MAX },
            }),
            TypeExpr::Concrete(ConcreteType::Array {
                element: TypeId::new(40),
                shape: ArrayShape::Incomplete,
            }),
            TypeExpr::Computed(ComputedType::KeyOf(TypeId::new(23))),
            TypeExpr::Computed(ComputedType::TypeOf(TypeQuery::Entity(EntityId::new(24)))),
            TypeExpr::Computed(ComputedType::TypeOf(TypeQuery::Path(AtomListId::new(25)))),
            TypeExpr::Computed(ComputedType::TypeOf(TypeQuery::External(ExternalId::new(
                26,
            )))),
            TypeExpr::Computed(ComputedType::IndexedAccess {
                object: TypeId::new(27),
                index: TypeId::new(28),
            }),
            TypeExpr::Computed(ComputedType::Conditional {
                check: TypeId::new(29),
                extends: TypeId::new(30),
                then_type: TypeId::new(31),
                else_type: TypeId::new(32),
                distributive: false,
            }),
            TypeExpr::Computed(ComputedType::Mapped {
                parameter: AtomId::new(33),
                constraint: TypeId::new(34),
                name_as: Some(TypeId::new(35)),
                value: TypeId::new(36),
                readonly: MappedModifier::Add,
                optional: MappedModifier::Remove,
            }),
            TypeExpr::Computed(ComputedType::Infer {
                parameter: AtomId::new(37),
                constraint: Some(TypeId::new(38)),
            }),
            TypeExpr::Computed(ComputedType::TemplateLiteral(TemplatePartListId::new(39))),
            TypeExpr::Computed(ComputedType::Import {
                specifier: AtomId::new(40),
                qualifier: AtomListId::new(41),
                arguments: TypeListId::new(42),
            }),
            TypeExpr::Computed(ComputedType::Awaited(TypeId::new(43))),
            TypeExpr::Computed(ComputedType::This),
        ];
        for builtin in [
            BuiltinType::Unit,
            BuiltinType::Never,
            BuiltinType::Bool,
            BuiltinType::LegacyChar,
            BuiltinType::I8,
            BuiltinType::I16,
            BuiltinType::I32,
            BuiltinType::I64,
            BuiltinType::I128,
            BuiltinType::U8,
            BuiltinType::U16,
            BuiltinType::U32,
            BuiltinType::U64,
            BuiltinType::U128,
            BuiltinType::F16,
            BuiltinType::F32,
            BuiltinType::F64,
            BuiltinType::String,
            BuiltinType::Bytes,
            BuiltinType::Object,
            BuiltinType::Any,
            BuiltinType::Unknown,
            BuiltinType::None_,
            BuiltinType::List,
            BuiltinType::Dict,
            BuiltinType::Set,
            BuiltinType::FrozenSet,
            BuiltinType::Complex,
            BuiltinType::Decimal,
            BuiltinType::Void,
            BuiltinType::Number,
            BuiltinType::BigInt,
            BuiltinType::Symbol,
            BuiltinType::UniqueSymbol,
            BuiltinType::Null,
            BuiltinType::Undefined,
            BuiltinType::ArbitraryInteger,
            BuiltinType::NativeSignedInteger,
            BuiltinType::NativeUnsignedInteger,
            BuiltinType::PointerAddressInteger,
        ] {
            types.push(TypeExpr::Concrete(ConcreteType::Builtin(builtin)));
        }
        for role in [
            NativeCharacterRole::UnicodeScalar,
            NativeCharacterRole::Utf16CodeUnit,
            NativeCharacterRole::Utf32CodeUnit,
            NativeCharacterRole::CPlainSigned,
            NativeCharacterRole::CPlainUnsigned,
            NativeCharacterRole::CSigned,
            NativeCharacterRole::CUnsigned,
            NativeCharacterRole::CWideSigned,
            NativeCharacterRole::CWideUnsigned,
            NativeCharacterRole::CWideSignednessUnavailable,
        ] {
            types.push(TypeExpr::Concrete(ConcreteType::NativeCharacter {
                role,
                width: NonZeroU16::new(16).expect("fixed character width"),
            }));
        }
        for reason in [
            UnknownType::new(UnknownReason::Unannotated),
            UnknownType::new(UnknownReason::DynamicallyTyped),
            UnknownType::new(UnknownReason::UnresolvedLocalName),
            UnknownType::new(UnknownReason::UnresolvedExternal),
            UnknownType::new(UnknownReason::TruncatedAtDepthLimit),
            UnknownType::new(UnknownReason::OracleGap),
            UnknownType::new(UnknownReason::NoIrRepresentation),
            UnknownType::new(UnknownReason::Error),
        ] {
            types.push(TypeExpr::Unknown(reason));
        }

        let mut packed = PackedTypes::default();
        for ty in types.iter().copied() {
            packed.push(ty);
        }
        assert_eq!(packed.headers.len(), types.len());
        assert_eq!(packed.pairs.len(), 7);
        assert_eq!(packed.triples.len(), 3);
        assert_eq!(packed.quads.len(), 3);
        let cold_bytes = core::mem::size_of_val(packed.pairs.as_slice())
            + core::mem::size_of_val(packed.triples.as_slice())
            + core::mem::size_of_val(packed.quads.as_slice());
        assert_eq!(cold_bytes, 140);
        assert!(cold_bytes < 9 * 20);
        for (index, expected) in types.iter().copied().enumerate() {
            let id = TypeId::new(u32::try_from(index).expect("bounded test index"));
            assert_eq!(packed.get(id), Some(expected), "type row {index}");
        }
    }
}

const fn header(tag: TypeTag, flags: u8, auxiliary: u16, payload: u32) -> TypeHeader {
    TypeHeader {
        tag,
        flags,
        auxiliary,
        payload,
    }
}

const fn option_raw<T>(value: Option<DenseId<T>>) -> u32 {
    match value {
        Some(value) => value.raw,
        None => u32::MAX,
    }
}

const fn raw_option(value: u32) -> Option<u32> {
    if value == u32::MAX { None } else { Some(value) }
}

const fn builtin_from(value: u16) -> Option<BuiltinType> {
    Some(match value {
        0 => BuiltinType::Unit,
        1 => BuiltinType::Never,
        2 => BuiltinType::Bool,
        3 => BuiltinType::LegacyChar,
        4 => BuiltinType::I8,
        5 => BuiltinType::I16,
        6 => BuiltinType::I32,
        7 => BuiltinType::I64,
        8 => BuiltinType::I128,
        9 => BuiltinType::U8,
        10 => BuiltinType::U16,
        11 => BuiltinType::U32,
        12 => BuiltinType::U64,
        13 => BuiltinType::U128,
        14 => BuiltinType::F16,
        15 => BuiltinType::F32,
        16 => BuiltinType::F64,
        17 => BuiltinType::String,
        18 => BuiltinType::Bytes,
        19 => BuiltinType::Object,
        20 => BuiltinType::Any,
        21 => BuiltinType::Unknown,
        22 => BuiltinType::Void,
        23 => BuiltinType::Number,
        24 => BuiltinType::BigInt,
        25 => BuiltinType::Symbol,
        26 => BuiltinType::UniqueSymbol,
        27 => BuiltinType::Null,
        28 => BuiltinType::Undefined,
        30 => BuiltinType::None_,
        31 => BuiltinType::List,
        32 => BuiltinType::Dict,
        33 => BuiltinType::Set,
        34 => BuiltinType::FrozenSet,
        36 => BuiltinType::Complex,
        37 => BuiltinType::Decimal,
        38 => BuiltinType::ArbitraryInteger,
        39 => BuiltinType::NativeSignedInteger,
        40 => BuiltinType::NativeUnsignedInteger,
        41 => BuiltinType::PointerAddressInteger,
        _ => return None,
    })
}

const fn mutability_from(value: u8) -> Option<Mutability> {
    match value {
        0 => Some(Mutability::Immutable),
        1 => Some(Mutability::Mutable),
        _ => None,
    }
}

const fn cxx_reference_category_from(value: u8) -> Option<CxxReferenceCategory> {
    match value {
        0 => Some(CxxReferenceCategory::Lvalue),
        1 => Some(CxxReferenceCategory::Rvalue),
        _ => None,
    }
}

const fn native_character_role_from(value: u8) -> Option<NativeCharacterRole> {
    match value {
        0 => Some(NativeCharacterRole::UnicodeScalar),
        1 => Some(NativeCharacterRole::Utf16CodeUnit),
        2 => Some(NativeCharacterRole::Utf32CodeUnit),
        3 => Some(NativeCharacterRole::CPlainSigned),
        4 => Some(NativeCharacterRole::CPlainUnsigned),
        5 => Some(NativeCharacterRole::CSigned),
        6 => Some(NativeCharacterRole::CUnsigned),
        7 => Some(NativeCharacterRole::CWideSigned),
        8 => Some(NativeCharacterRole::CWideUnsigned),
        9 => Some(NativeCharacterRole::CWideSignednessUnavailable),
        _ => None,
    }
}

fn cxx_qualifiers_from(value: u8) -> Option<crate::CvQualifiers> {
    crate::CvQualifiers::try_from(u32::from(value)).ok()
}

const fn modifier_from(value: u8) -> Option<MappedModifier> {
    match value {
        0 => Some(MappedModifier::Preserve),
        1 => Some(MappedModifier::Add),
        2 => Some(MappedModifier::Remove),
        _ => None,
    }
}

const fn annotation_kind_from(value: u8) -> Option<AnnotationKind> {
    match value {
        0 => Some(AnnotationKind::Readonly),
        1 => Some(AnnotationKind::NullableValue),
        2 => Some(AnnotationKind::NullableReference),
        3 => Some(AnnotationKind::NonNullableReference),
        _ => None,
    }
}

const fn channel_direction_from(value: u8) -> Option<ChannelDirection> {
    match value {
        0 => Some(ChannelDirection::Both),
        1 => Some(ChannelDirection::Send),
        2 => Some(ChannelDirection::Receive),
        _ => None,
    }
}

const fn unknown_from(value: u16) -> Option<UnknownReason> {
    match value {
        0 => Some(UnknownReason::Unannotated),
        1 => Some(UnknownReason::DynamicallyTyped),
        2 => Some(UnknownReason::UnresolvedLocalName),
        3 => Some(UnknownReason::UnresolvedExternal),
        4 => Some(UnknownReason::TruncatedAtDepthLimit),
        5 => Some(UnknownReason::OracleGap),
        6 => Some(UnknownReason::NoIrRepresentation),
        7 => Some(UnknownReason::Error),
        _ => None,
    }
}

impl TypeExpr {
    /// Returns the closed directory class of this semantic type.
    ///
    /// The class is coordinate-free and therefore safe to carry in search
    /// projections that retain a [`TypeId`] only as a route back into the
    /// image that owns it.  Rendering and structural traversal must still use
    /// the owning [`crate::SemanticReader`].
    #[must_use]
    pub const fn tag(self) -> TypeTag {
        match self {
            Self::Concrete(ty) => ty.tag(),
            Self::Computed(ty) => ty.tag(),
            Self::Unknown(_) => TypeTag::Unknown,
        }
    }

    #[must_use]
    pub const fn is_computed(self) -> bool {
        matches!(self, Self::Computed(_))
    }
    #[must_use]
    pub const fn concrete(self) -> Option<ConcreteType> {
        match self {
            Self::Concrete(ty) => Some(ty),
            Self::Computed(_) | Self::Unknown(_) => None,
        }
    }
    #[must_use]
    pub const fn computed(self) -> Option<ComputedType> {
        match self {
            Self::Computed(ty) => Some(ty),
            Self::Concrete(_) | Self::Unknown(_) => None,
        }
    }
}

/// A type whose shape is already known and can be rendered without evaluation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConcreteType {
    Builtin(BuiltinType),
    Literal(LiteralType),
    Nominal(EntityId),
    External(ExternalId),
    Parameter(AtomId),
    Applied {
        constructor: TypeId,
        arguments: TypeListId,
    },
    Tuple(TupleElementListId),
    Object(ObjectMemberListId),
    Function {
        /// Parameter rows retain names, optionality, and rest position. This is
        /// equally useful for docs rendering and exact TypeScript signatures.
        parameters: TupleElementListId,
        /// Results are an independent ordered range. Most languages have zero
        /// or one; Go has several, and retaining role-bearing elements keeps
        /// result labels and modifiers with the type rather than recovering
        /// them from a language extension.
        results: TupleElementListId,
        abi: Option<AtomId>,
        variadic: VariadicForm,
        unsafe_: bool,
    },
    Reference {
        target: TypeId,
        mutability: Mutability,
        lifetime: Option<AtomId>,
    },
    /// A C++ reference category. The target may itself be [`Self::CQualified`]
    /// so direct cv qualification stays attached to the referent instead of
    /// becoming Rust mutability.
    CxxReference {
        target: TypeId,
        category: CxxReferenceCategory,
    },
    /// A C/C++ raw pointer. Direct qualifier placement is represented only
    /// by the enclosing [`Self::CQualified`] node.
    CPointer { target: TypeId },
    /// A C++ member pointer. The owning class and member type are distinct
    /// operands and cannot be reconstructed from a display spelling.
    CxxMemberPointer {
        owner: TypeId,
        member: TypeId,
    },
    /// One direct C-family cv/restrict wrapper. Empty qualifier sets never
    /// construct this variant.
    CQualified {
        target: TypeId,
        qualifiers: crate::CvQualifiers,
    },
    /// An Objective-C block pointer, whose callable/signature target is
    /// structurally distinct from a C pointer. A C declarator dialect owns
    /// its exact `^` placement.
    CBlockPointer { target: TypeId },
    /// A source-level character role plus its exact measured code-unit or
    /// scalar width. This is deliberately not `BuiltinType`: `char`,
    /// `wchar_t`, and Java/C# `char` have incompatible semantics.
    NativeCharacter {
        role: NativeCharacterRole,
        width: NonZeroU16,
    },
    Pointer {
        target: TypeId,
        mutability: Mutability,
    },
    Slice(TypeId),
    Array {
        element: TypeId,
        shape: ArrayShape,
    },
    Optional(TypeId),
    Union(TypeListId),
    Intersection(TypeListId),
    /// Rust's static-dispatch existential bound set (`impl Trait`).
    ImplTrait(TypeListId),
    /// Rust's dynamic-dispatch trait-object bound set (`dyn Trait`).
    DynTrait(TypeListId),
    /// Java/C# wildcard with one of its three legal bound states.
    Wildcard(WildcardBound),
    /// A closed source-level annotation that preserves its inner type.
    Annotated {
        kind: AnnotationKind,
        target: TypeId,
    },
    /// A written inference request (`_`, `auto`, `var`), with an optional
    /// authority spelling when the source language distinguishes them.
    Inferred(Option<AtomId>),
    /// A qualified path with source-authoritative named segments.  Segments
    /// are atoms rather than fabricated type nodes, so `<Self as Trait>::Assoc`
    /// and `Outer<T>.Inner` retain their actual path grammar.
    QualifiedPath {
        self_type: TypeId,
        trait_type: Option<TypeId>,
        segments: QualifiedSegments,
        spelling: AtomId,
    },
    /// Go's structural map, never an application of a fabricated `map` base.
    Map {
        key: TypeId,
        value: TypeId,
    },
    /// Go's directional channel, never an application of a fabricated `chan` base.
    Channel {
        direction: ChannelDirection,
        element: TypeId,
    },
}

impl ConcreteType {
    /// Returns the closed directory class of this concrete type.
    #[must_use]
    pub const fn tag(self) -> TypeTag {
        match self {
            Self::Builtin(_) => TypeTag::Builtin,
            Self::Literal(_) => TypeTag::Literal,
            Self::Nominal(_) => TypeTag::Nominal,
            Self::External(_) => TypeTag::External,
            Self::Parameter(_) => TypeTag::Parameter,
            Self::Applied { .. } => TypeTag::Applied,
            Self::Tuple(_) => TypeTag::Tuple,
            Self::Object(_) => TypeTag::Object,
            Self::Function { .. } => TypeTag::Function,
            Self::Reference { .. } => TypeTag::Reference,
            Self::Pointer { .. } => TypeTag::Pointer,
            Self::Slice(_) => TypeTag::Slice,
            Self::Array { .. } => TypeTag::Array,
            Self::Optional(_) => TypeTag::Optional,
            Self::Union(_) => TypeTag::Union,
            Self::Intersection(_) => TypeTag::Intersection,
            Self::ImplTrait(_) => TypeTag::ImplTrait,
            Self::DynTrait(_) => TypeTag::DynTrait,
            Self::Wildcard(_) => TypeTag::Wildcard,
            Self::Annotated { .. } => TypeTag::Annotated,
            Self::Inferred(_) => TypeTag::Inferred,
            Self::QualifiedPath { .. } => TypeTag::QualifiedPath,
            Self::Map { .. } => TypeTag::Map,
            Self::Channel { .. } => TypeTag::Channel,
            Self::CxxReference { .. } => TypeTag::CxxReference,
            Self::CPointer { .. } => TypeTag::CPointer,
            Self::CxxMemberPointer { .. } => TypeTag::CxxMemberPointer,
            Self::CQualified { .. } => TypeTag::CQualified,
            Self::CBlockPointer { .. } => TypeTag::CBlockPointer,
            Self::NativeCharacter { .. } => TypeTag::NativeCharacter,
        }
    }
}

/// Closed array extent semantics. Rendering belongs to a language dialect;
/// it may never guess an extent from a surface spelling shared by languages.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArrayShape {
    Sequence,
    Rectangular { rank: NonZeroU16 },
    FixedValue { length: u64 },
    ConstExpression(AtomId),
    Incomplete,
}

/// Legal Java/C# wildcard states. A bound is inseparable from `extends` or
/// `super`; `?` cannot accidentally carry a hidden target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WildcardBound {
    Unbounded,
    Extends(TypeId),
    Super(TypeId),
}

/// Availability of source-authoritative qualified-path segments. Legacy rows
/// can retain a complete spelling without falsely claiming it was one parsed
/// component; only a captured nonempty atom list permits structural traversal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QualifiedSegments {
    Captured(AtomListId),
    Unavailable,
}

/// Literal type payload. Numeric spelling remains raw to preserve `-0`, bigint,
/// separators, and frontend-specific precision without parsing through `f64`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LiteralType {
    String(AtomId),
    Number(AtomId),
    BigInt(AtomId),
    Boolean(bool),
    Null,
    Undefined,
}

/// A type-level operation that has not been evaluated into a concrete shape.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ComputedType {
    KeyOf(TypeId),
    TypeOf(TypeQuery),
    IndexedAccess {
        object: TypeId,
        index: TypeId,
    },
    Conditional {
        check: TypeId,
        extends: TypeId,
        then_type: TypeId,
        else_type: TypeId,
        distributive: bool,
    },
    Mapped {
        parameter: AtomId,
        constraint: TypeId,
        name_as: Option<TypeId>,
        value: TypeId,
        readonly: MappedModifier,
        optional: MappedModifier,
    },
    Infer {
        parameter: AtomId,
        constraint: Option<TypeId>,
    },
    TemplateLiteral(TemplatePartListId),
    Import {
        specifier: AtomId,
        qualifier: AtomListId,
        arguments: TypeListId,
    },
    Awaited(TypeId),
    This,
}

impl ComputedType {
    /// Returns the closed directory class of this unevaluated type program.
    #[must_use]
    pub const fn tag(self) -> TypeTag {
        match self {
            Self::KeyOf(_) => TypeTag::KeyOf,
            Self::TypeOf(_) => TypeTag::TypeOf,
            Self::IndexedAccess { .. } => TypeTag::IndexedAccess,
            Self::Conditional { .. } => TypeTag::Conditional,
            Self::Mapped { .. } => TypeTag::Mapped,
            Self::Infer { .. } => TypeTag::Infer,
            Self::TemplateLiteral(_) => TypeTag::TemplateLiteral,
            Self::Import { .. } => TypeTag::Import,
            Self::Awaited(_) => TypeTag::Awaited,
            Self::This => TypeTag::This,
        }
    }
}

/// Operand of TypeScript's `typeof` type query.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeQuery {
    Entity(EntityId),
    Path(AtomListId),
    External(ExternalId),
}

/// `+`, `-`, or no override on a mapped type's optional/readonly modifier.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MappedModifier {
    Preserve,
    Add,
    Remove,
}

/// A tuple element retains TypeScript labels, optionality, and rest position.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TupleElement {
    pub label: Option<AtomId>,
    pub ty: TypeId,
    pub kind: TupleElementKind,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TupleElementKind {
    Required,
    Optional,
    Rest,
}

/// Owned IR uses the same closed callable-tail grammar as staged records.
/// Keeping this as an alias eliminates a conversion that could otherwise
/// silently reinterpret a future wire discriminant.
pub type VariadicForm = crate::FunctionVariadicForm;

/// The role of one callable element rejected by owned-IR validation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CallableElementRole {
    Parameter,
    Result,
}

/// Strong property-key split. A computed key is never confused with its
/// rendered spelling or with a statically named property.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PropertyKey {
    Named(AtomId),
    Private(AtomId),
    Numeric(AtomId),
    Computed(TypeId),
}

/// Structural object member, including TypeScript index/call/construct lanes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectMember {
    Property {
        key: PropertyKey,
        ty: TypeId,
        optional: bool,
        readonly: bool,
    },
    Method {
        key: PropertyKey,
        signature: TypeId,
        optional: bool,
    },
    Index {
        parameter: AtomId,
        key: TypeId,
        value: TypeId,
        readonly: bool,
    },
    Call(TypeId),
    Construct(TypeId),
}

/// One template-literal type segment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TemplatePart {
    Bytes(AtomId),
    Placeholder(TypeId),
}

/// One source-ordered generic bound.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeParameterBound {
    /// A semantic type bound such as `T: Display` or `T extends Base`.
    Type(TypeId),
    /// A lifetime bound such as `T: 'scope`.
    Lifetime(AtomId),
}

/// The declaration role of a generic parameter.
///
/// A Rust `const N: usize` is a value parameter with a declared value type;
/// it is not a synthetic `Const` bound on a type parameter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeParameterKind {
    /// An ordinary type parameter, optionally carrying TypeScript's `const`
    /// inference modifier. This is distinct from a Rust const-value parameter.
    Type { inference: TypeParameterInference },
    /// A Rust-style const value parameter and its declared value type.
    ConstValue { value_type: TypeId },
    /// A Rust lifetime parameter. Its ordered lifetime/type bounds remain in
    /// the shared bound list rather than a parallel Rust-only side table.
    Lifetime,
}

/// The inference mode of a type parameter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeParameterInference {
    Ordinary,
    Const,
}

/// The one primary C# generic requirement. These source facts are mutually
/// exclusive and therefore cannot be represented by independent booleans.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TypeParameterPrimaryRequirement {
    None,
    Reference { nullable: bool },
    Value,
    Unmanaged,
    NotNull,
    Default,
}

/// Closed special requirements which are orthogonal to ordered bounds.
///
/// The primary requirement retains C#'s distinct `class`, `class?`, `struct`,
/// `unmanaged`, `notnull`, and `default` facts. Constructor and
/// `allows ref struct` remain orthogonal rather than becoming a stringly
/// constraint or lossy boolean collection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeParameterRequirements {
    pub primary: TypeParameterPrimaryRequirement,
    pub constructor: bool,
    pub allows_ref_like: bool,
}

impl TypeParameterRequirements {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            primary: TypeParameterPrimaryRequirement::None,
            constructor: false,
            allows_ref_like: false,
        }
    }

    #[must_use]
    pub const fn is_valid(self) -> bool {
        !(self.constructor
            && matches!(
                self.primary,
                TypeParameterPrimaryRequirement::Value
                    | TypeParameterPrimaryRequirement::Unmanaged
                    | TypeParameterPrimaryRequirement::Default
            ))
            && !(self.allows_ref_like
                && matches!(
                    self.primary,
                    TypeParameterPrimaryRequirement::Reference { .. }
                ))
    }
}

/// Generic declaration retained separately from a parameter reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeParameter {
    pub name: AtomId,
    /// Source-ordered type and lifetime bounds.
    pub bounds: TypeParameterBoundListId,
    pub default: Option<TypeId>,
    pub variance: Variance,
    pub kind: TypeParameterKind,
    pub requirements: TypeParameterRequirements,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Variance {
    Invariant,
    Covariant,
    Contravariant,
    Bivariant,
}

/// TypeScript declaration-specific facts kept out of generic entity fields.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeScriptFacts {
    pub type_parameters: TypeParameterListId,
    pub declared: Option<TypeId>,
    /// Exact checker-observed type.  This is deliberately a general
    /// [`TypeId`]: an observation may be a concrete, computed, or unknown
    /// type.  Narrowing it to `ComputedTypeId` used to force lowerers to mint
    /// a false `typeof owner` node merely to satisfy the static marker.
    pub observed: Option<TypeId>,
}

/// Marker for TypeScript-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeScriptExtension {}
/// Marker for C#-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CSharpExtension {}
/// Marker for Go-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GoExtension {}
/// Marker for Rust-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RustExtension {}
/// Marker for Python-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PythonExtension {}
/// Marker for Java-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JavaExtension {}
/// Marker for Clang-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ClangExtension {}

/// C# nullable-reference interpretation retained independently of type spelling.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpNullability {
    Oblivious,
    NonNullable,
    Nullable,
}

/// C# parameter and return reference convention.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpReferenceKind {
    Value,
    In,
    Ref,
    Out,
}

/// Independent C# member effects; the three flags may occur together.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpMemberEffects {
    pub is_async: bool,
    pub is_iterator: bool,
    pub is_extension: bool,
}

/// Whether a C# declaration is one half of a partial declaration.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpPartialRole {
    None,
    Definition,
    Implementation,
}

/// C# facts not shared by the language-neutral declaration row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpFacts {
    pub nullability: CSharpNullability,
    pub reference_kind: CSharpReferenceKind,
    pub constraints: TypeParameterListId,
    pub effects: CSharpMemberEffects,
    pub attributes: AtomListId,
    pub partial: CSharpPartialRole,
    pub xml_provenance: Option<SourceSpan>,
}

/// Signature lists carried by a Go declaration. Both lists are shared type arenas.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoSignature {
    pub parameters: TypeListId,
    pub results: TypeListId,
    pub variadic: bool,
}

/// Go facts that cannot be inferred from generic declarations and links.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoFacts {
    pub signature: GoSignature,
    pub type_parameters: TypeParameterListId,
    pub fields: EntityListId,
    pub method_set: EntityListId,
    pub build_constraints: AtomListId,
    pub constant_value: AtomListId,
    pub constant_group: i64,
    pub constant_flags: u32,
}

/// Rust ownership fact attached to one declaration or parameter.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RustOwnership {
    Value,
    SharedBorrow,
    MutableBorrow,
    Moved,
}

/// Rust-only declaration facts; text is interned in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RustFacts {
    pub ownership: RustOwnership,
    pub lifetimes: AtomListId,
    pub where_clauses: TypeParameterListId,
    pub macros: AtomListId,
}

/// Python parameter convention, retained instead of erasing it into an atom.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PythonParameterKind {
    PositionalOnly,
    PositionalOrKeyword,
    VariadicPositional,
    KeywordOnly,
    VariadicKeyword,
}

/// Python-only source facts. Dynamic confidence is the existing quality lattice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PythonFacts {
    pub decorators: AtomListId,
    pub parameter_kind: PythonParameterKind,
    pub dynamic_confidence: Confidence,
}

/// Java-specific declaration facts for checked exceptions and record structure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JavaFacts {
    pub throws: TypeListId,
    pub annotations: AtomListId,
    pub overloads: EntityListId,
    pub record_components: EntityListId,
}

/// Clang type qualifier bits kept separate from the generic type graph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
}

/// Clang storage-class fact.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClangStorageClass {
    None,
    Auto,
    Static,
    Extern,
    Register,
    ThreadLocal,
}

/// Optional layout facts measured in bits; `None` means the frontend did not own a layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangLayout {
    pub size_bits: Option<u32>,
    pub align_bits: Option<u32>,
}

/// Clang-only semantic facts. Include spelling is held in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangFacts {
    pub qualifiers: ClangQualifiers,
    pub storage: ClangStorageClass,
    pub layout: ClangLayout,
    pub templates: TypeParameterListId,
    pub includes: AtomListId,
}

/// Authority bound into one semantic image before language facts are admitted.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SemanticImageAuthority {
    /// A common-only image; language extensions are explicitly rejected.
    #[default]
    Shared,
    /// A closed canonical source profile, which proves its language family.
    Language(LanguageProfile),
}

impl SemanticImageAuthority {
    fn language(self) -> Option<Language> {
        match self {
            Self::Shared => None,
            Self::Language(profile) => Some(Language::from(profile)),
        }
    }
}

/// Exactly one language-specific extension submitted for one entity row.
///
/// This tagged input exists only during ingestion. The condensed IR routes it
/// into seven named sparse planes, so scans and reopened views never carry an
/// erased union or the union's maximum payload width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionInput<'facts> {
    TypeScript(&'facts TypeScriptFacts),
    CSharp(&'facts CSharpFacts),
    Go(&'facts GoFacts),
    Rust(&'facts RustFacts),
    Python(&'facts PythonFacts),
    Java(&'facts JavaFacts),
    Clang(&'facts ClangFacts),
}

impl LanguageExtensionInput<'_> {
    /// Closed source-language authority for this extension input.
    #[must_use]
    pub const fn language(self) -> Language {
        match self {
            Self::TypeScript(_) => Language::TypeScript,
            Self::CSharp(_) => Language::CSharp,
            Self::Go(_) => Language::Go,
            Self::Rust(_) => Language::Rust,
            Self::Python(_) => Language::Python,
            Self::Java(_) => Language::Java,
            Self::Clang(_) => Language::Clang,
        }
    }
}

/// A graph target, local or self-describing across a package boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LinkTarget {
    Local(EntityId),
    External(ExternalId),
}

/// Exact retained origin facts for an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForeignTargetOrigin {
    Package { ecosystem: AtomId, package: AtomId },
    Namespace { ecosystem: AtomId, namespace: AtomId },
    Universe { ecosystem: AtomId },
    /// A producer supplied an ecosystem/path but no stronger foreign-origin
    /// classification. This is not silently promoted to a package.
    Unspecified { ecosystem: AtomId },
}

/// Everything a producer knows about an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ForeignExternalTarget {
    /// Separately branded foreign key plus honest variant availability.
    pub identity: ExternalDeclarationIdentity,
    /// Exact native/package/namespace authority supplied by the producer.
    pub origin: ForeignTargetOrigin,
    /// Canonical remote path.
    pub path: AtomId,
    /// Source display spelling.
    pub display: AtomId,
    /// Expected declaration kind, when known.
    pub kind: Option<ItemKind>,
}

/// Cross-fragment graph target. Resolved declaration endpoints are never
/// represented as an unresolved foreign path and an unresolved key can never
/// manufacture a fragment/declaration pair.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternalTarget {
    /// Exact resolved endpoint in a known external fragment.
    Stable { target: StableRef },
    /// Self-describing unresolved foreign authority and path facts.
    Foreign(ForeignExternalTarget),
    /// Legacy external fragment ordinal retained from type facts that do not
    /// yet supply a declaration identity. It remains distinct from both a
    /// resolved [`StableRef`] and an unresolved foreign key.
    FragmentEntity { target: ExternalEntityRef, display: AtomId },
}

/// Documentation is UTF-8 by construction; only its IDs carry that promise.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocFragment {
    Text(TextId),
    Code(TextId),
    Link { label: TextId, target: LinkTarget },
    SoftBreak,
    HardBreak,
}

/// Borrowed documentation accepted from a frontend without an intermediate string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocInput<'source> {
    Text(&'source str),
    Code(&'source str),
    Link {
        label: &'source str,
        target: TreeLinkTarget,
    },
    SoftBreak,
    HardBreak,
}

/// Half-open source byte range. The file itself is universally interned bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceSpan {
    file: AtomId,
    start: u32,
    end: u32,
}

impl SourceSpan {
    /// Creates a valid half-open source range.
    #[must_use]
    pub const fn new(file: AtomId, start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { file, start, end })
        } else {
            None
        }
    }
    #[must_use]
    pub const fn file(self) -> AtomId {
        self.file
    }
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Exact identity used to order a graph endpoint. Local endpoints always
/// retain both family and variant; foreign endpoints retain unavailable
/// variant knowledge explicitly rather than borrowing a local convention.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeclarationLinkTarget {
    Local(DeclarationIdentity),
    Stable(StableRef),
    Foreign(ExternalDeclarationIdentity),
    FragmentEntity(ExternalEntityRef),
}

/// Whether one semantic plane participates in [`CorePayloadHash`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CorePayloadPlane {
    /// Included in the current canonical core-payload contract.
    Included,
    /// Deliberately excluded until the plane has one durable authority
    /// contract; absence here must never be reported as semantic parity.
    ExcludedPendingAuthority,
}

/// Honest coverage of the current core declaration payload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CorePayloadCoverage {
    pub declaration_shape: CorePayloadPlane,
    pub type_structure: CorePayloadPlane,
    pub ordered_product_children: CorePayloadPlane,
    pub ordered_local_members: CorePayloadPlane,
    pub documentation: CorePayloadPlane,
    pub visibility: CorePayloadPlane,
    pub language_extension: CorePayloadPlane,
    pub source_provenance: CorePayloadPlane,
    pub occurrences: CorePayloadPlane,
    pub opaque_parentage: CorePayloadPlane,
}

/// Canonical hash of the currently admitted core semantic declaration
/// payload. It is intentionally not an authority-complete payload hash.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CorePayloadHash([u8; 16]);

impl CorePayloadHash {
    /// Width of one canonical compact payload digest.
    pub const BYTES: usize = core::mem::size_of::<Self>();

    /// Exact plane coverage of every value minted by this type.
    pub const COVERAGE: CorePayloadCoverage = CorePayloadCoverage {
        declaration_shape: CorePayloadPlane::Included,
        type_structure: CorePayloadPlane::Included,
        ordered_product_children: CorePayloadPlane::Included,
        ordered_local_members: CorePayloadPlane::Included,
        documentation: CorePayloadPlane::ExcludedPendingAuthority,
        visibility: CorePayloadPlane::ExcludedPendingAuthority,
        language_extension: CorePayloadPlane::ExcludedPendingAuthority,
        source_provenance: CorePayloadPlane::ExcludedPendingAuthority,
        occurrences: CorePayloadPlane::ExcludedPendingAuthority,
        opaque_parentage: CorePayloadPlane::ExcludedPendingAuthority,
    };

    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let hash = blake3::hash(bytes);
        let bytes = hash.as_bytes();
        Self([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    }
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Cold VCS columns aligned with one hot entity row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntityVersion {
    /// Stable declaration family.  VCS can match a singleton family across a
    /// signature edit without pretending that an overload group is singular.
    pub family: DeclarationFamilyId,
    /// Structural fingerprint distinguishing current instances in one family.
    pub variant: VariantFingerprint,
    /// Current core-only semantic payload hash; see [`CorePayloadHash::COVERAGE`].
    pub core_payload: CorePayloadHash,
}

impl EntityVersion {
    /// The exact local declaration instance key. A family alone is a range,
    /// never a singular graph or index key.
    #[must_use]
    pub const fn identity(self) -> DeclarationIdentity {
        DeclarationIdentity {
            family: self.family,
            variant: self.variant,
        }
    }
}

/// One compact entity row. All variable-size data is an interned typed-list ID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Item {
    pub name: AtomId,
    pub kind: ItemKind,
    pub visibility: Visibility,
    pub parent: Option<EntityId>,
    pub semantic_type: Option<TypeId>,
    pub members: EntityListId,
    pub docs: DocId,
    pub attributes: AtomListId,
    pub source: Option<SourceSpan>,
}

/// Four-byte niche-packed optional dense coordinate used inside column families.
///
/// `u32::MAX` is reserved for absence. In practice an IR cannot materialize
/// that many rows in one address space, and the sentinel halves each optional
/// coordinate column compared with Rust's general `Option<DenseId<_>>` layout.
#[repr(transparent)]
pub struct OptionalId<Owner> {
    raw: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> Copy for OptionalId<Owner> {}

impl<Owner> Clone for OptionalId<Owner> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Owner> OptionalId<Owner> {
    const NONE: u32 = u32::MAX;

    /// Packs an optional coordinate into its aligned four-byte lane.
    #[must_use]
    pub const fn new(value: Option<DenseId<Owner>>) -> Self {
        Self {
            raw: match value {
                Some(id) => id.raw,
                None => Self::NONE,
            },
            owner: PhantomData,
        }
    }

    /// Expands the sentinel niche into the strongly typed coordinate.
    #[must_use]
    pub const fn get(self) -> Option<DenseId<Owner>> {
        if self.raw == Self::NONE {
            None
        } else {
            Some(DenseId::new(self.raw))
        }
    }
}

impl<Owner> fmt::Debug for OptionalId<Owner> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

/// Entity-aligned constant-time index into a dense optional extension arena.
///
/// Each entity costs exactly four bytes regardless of `T`. Present values are
/// stored densely once, so large language or model-specific facts never widen
/// the hot entity families.
struct SparseColumn<T> {
    ordinals: Vec<u32>,
    values: Vec<T>,
    rows: usize,
}

impl<T> Default for SparseColumn<T> {
    fn default() -> Self {
        Self {
            ordinals: Vec::new(),
            values: Vec::new(),
            rows: 0,
        }
    }
}

impl<T> SparseColumn<T> {
    fn reserve(&mut self, rows: usize, values: usize) {
        if values != 0 {
            self.ordinals.reserve(rows);
            self.values.reserve(values);
        }
    }

    fn push(&mut self, value: Option<T>) -> Result<(), CapacityError> {
        let ordinal = match value {
            Some(value) => {
                if self.ordinals.is_empty() && self.rows != 0 {
                    self.ordinals.resize(self.rows, u32::MAX);
                }
                let ordinal = u32::try_from(self.values.len()).map_err(|_| CapacityError {
                    space: crate::CapacitySpace::Value,
                    actual: self.values.len(),
                })?;
                self.values.push(value);
                ordinal
            }
            None => u32::MAX,
        };
        if ordinal != u32::MAX || !self.ordinals.is_empty() {
            self.ordinals.push(ordinal);
        }
        self.rows = self.rows.saturating_add(1);
        Ok(())
    }

    fn get(&self, entity: EntityId) -> Option<&T> {
        self.get_index(entity.index())
    }

    fn set(&mut self, index: usize, value: Option<T>) -> Result<(), CapacityError> {
        if index >= self.rows {
            return Err(CapacityError {
                space: crate::CapacitySpace::Value,
                actual: index,
            });
        }
        match value {
            Some(value) => {
                if self.ordinals.is_empty() {
                    self.ordinals.resize(self.rows, u32::MAX);
                }
                let known = self.ordinals[index];
                if known == u32::MAX {
                    let ordinal = u32::try_from(self.values.len()).map_err(|_| CapacityError {
                        space: crate::CapacitySpace::Value,
                        actual: self.values.len(),
                    })?;
                    self.values.push(value);
                    self.ordinals[index] = ordinal;
                } else if let Some(slot) = self.values.get_mut(known as usize) {
                    *slot = value;
                }
            }
            None if !self.ordinals.is_empty() => self.ordinals[index] = u32::MAX,
            None => {}
        }
        Ok(())
    }

    fn get_index(&self, index: usize) -> Option<&T> {
        let ordinal = *self.ordinals.get(index)?;
        (ordinal != u32::MAX)
            .then(|| self.values.get(ordinal as usize))
            .flatten()
    }

    fn view(&self) -> SparseColumnView<'_, T> {
        SparseColumnView {
            ordinals: &self.ordinals,
            values: &self.values,
            rows: self.rows,
        }
    }
}

/// Borrowed pointer-and-length view of an optional entity extension column.
#[derive(Debug)]
pub struct SparseColumnView<'ir, T> {
    ordinals: &'ir [u32],
    values: &'ir [T],
    rows: usize,
}

impl<T> Copy for SparseColumnView<'_, T> {}

impl<T> Clone for SparseColumnView<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'ir, T> SparseColumnView<'ir, T> {
    /// Looks up one extension fact through its aligned entity ordinal.
    #[must_use]
    pub fn get(self, entity: EntityId) -> Option<&'ir T> {
        let ordinal = *self.ordinals.get(entity.index())?;
        (ordinal != u32::MAX)
            .then(|| self.values.get(ordinal as usize))
            .flatten()
    }

    /// Returns the dense present-value arena without scanning absent entities.
    #[must_use]
    pub const fn values(self) -> &'ir [T] {
        self.values
    }

    /// Returns the entity-aligned ordinal lane used for direct joins.
    ///
    /// An empty lane with nonzero [`Self::row_count`] is the canonical encoding
    /// for a universally absent extension, avoiding four zero-information bytes
    /// per entity. Once one value exists, the slice is fully row-aligned.
    #[must_use]
    pub const fn ordinals(self) -> &'ir [u32] {
        self.ordinals
    }

    /// Number of logical entity rows represented, including universal absence.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.rows
    }
}

/// One language's compact extension plane: entity rows point to a deduplicated
/// typed fact pool rather than carrying language-width facts in the hot row.
struct LanguageExtensionPlane<Facts, Space> {
    ids: SparseColumn<DenseId<Space>>,
    facts: Interner<Facts, Space>,
}

impl<Facts, Space> Default for LanguageExtensionPlane<Facts, Space> {
    fn default() -> Self {
        Self {
            ids: SparseColumn::default(),
            facts: Interner::default(),
        }
    }
}

impl<Facts: Eq + Hash, Space> LanguageExtensionPlane<Facts, Space> {
    fn reserve(&mut self, rows: usize, values: usize) {
        self.ids.reserve(rows, values);
        self.facts.reserve(values);
    }

    fn push(&mut self, facts: Option<Facts>) -> Result<(), CapacityError> {
        let id = facts.map(|facts| self.facts.intern(facts)).transpose()?;
        self.ids.push(id)
    }

    fn get(&self, entity: EntityId) -> Option<&Facts> {
        self.ids
            .get(entity)
            .and_then(|id| self.facts.get(DenseId::new(id.raw)))
    }

    fn view(&self) -> LanguageExtensionColumnView<'_, Facts, Space> {
        LanguageExtensionColumnView {
            ids: self.ids.view(),
            facts: self.facts.as_slice(),
        }
    }
}

/// Borrowed per-language plane: aligned IDs and the matching typed fact pool.
#[derive(Debug)]
pub struct LanguageExtensionColumnView<'ir, Facts, Space> {
    /// Entity-aligned compact IDs; universal absence retains an empty lane.
    pub ids: SparseColumnView<'ir, DenseId<Space>>,
    /// Canonical dense fact pool addressed only by [`Self::ids`].
    pub facts: &'ir [Facts],
}

impl<Facts, Space> Copy for LanguageExtensionColumnView<'_, Facts, Space> {}

impl<Facts, Space> Clone for LanguageExtensionColumnView<'_, Facts, Space> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'ir, Facts, Space> LanguageExtensionColumnView<'ir, Facts, Space> {
    /// Resolves one entity's typed language fact without allocating or scanning.
    #[must_use]
    pub fn get(self, entity: EntityId) -> Option<&'ir Facts> {
        self.ids
            .get(entity)
            .and_then(|id| self.facts.get(DenseId::<Space>::new(id.raw).index()))
    }
}

/// All closed language-extension planes owned by one semantic IR.
///
/// There is deliberately no erased extension map or tag/payload union. Every
/// language has a distinct sparse directory kind and a fact pool with its own
/// coordinate type, making cross-language substitution unrepresentable in the
/// in-memory model.
#[derive(Default)]
struct LanguageExtensions {
    typescript: LanguageExtensionPlane<TypeScriptFacts, TypeScriptExtension>,
    csharp: LanguageExtensionPlane<CSharpFacts, CSharpExtension>,
    go: LanguageExtensionPlane<GoFacts, GoExtension>,
    rust: LanguageExtensionPlane<RustFacts, RustExtension>,
    python: LanguageExtensionPlane<PythonFacts, PythonExtension>,
    java: LanguageExtensionPlane<JavaFacts, JavaExtension>,
    clang: LanguageExtensionPlane<ClangFacts, ClangExtension>,
}

impl LanguageExtensions {
    fn reserve(&mut self, rows: usize, values: LanguageExtensionCounts) {
        self.typescript.reserve(rows, values.typescript);
        self.csharp.reserve(rows, values.csharp);
        self.go.reserve(rows, values.go);
        self.rust.reserve(rows, values.rust);
        self.python.reserve(rows, values.python);
        self.java.reserve(rows, values.java);
        self.clang.reserve(rows, values.clang);
    }

    fn push(&mut self, input: Option<LanguageExtensionInput<'_>>) -> Result<(), CapacityError> {
        match input {
            Some(LanguageExtensionInput::TypeScript(facts)) => {
                self.typescript.push(Some(*facts))?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::CSharp(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(Some(*facts))?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Go(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(Some(*facts))?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Rust(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(Some(*facts))?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Python(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(Some(*facts))?;
                self.java.push(None)?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Java(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(Some(*facts))?;
                self.clang.push(None)
            }
            Some(LanguageExtensionInput::Clang(facts)) => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(Some(*facts))
            }
            None => {
                self.typescript.push(None)?;
                self.csharp.push(None)?;
                self.go.push(None)?;
                self.rust.push(None)?;
                self.python.push(None)?;
                self.java.push(None)?;
                self.clang.push(None)
            }
        }
    }

    fn view(&self, authority: SemanticImageAuthority) -> LanguageExtensionsView<'_> {
        LanguageExtensionsView {
            authority,
            typescript: self.typescript.view(),
            csharp: self.csharp.view(),
            go: self.go.view(),
            rust: self.rust.view(),
            python: self.python.view(),
            java: self.java.view(),
            clang: self.clang.view(),
        }
    }

    fn validate_entity(&self, builder: &IrBuilder, entity: EntityId) -> Result<(), BuildError> {
        if let Some(facts) = self.typescript.get(entity).copied() {
            validate_type_parameters(builder, facts.type_parameters)?;
            optional_id(facts.declared, builder.types.len(), SemanticSpace::Type)?;
            optional_id(facts.observed, builder.types.len(), SemanticSpace::Type)?;
        }
        if let Some(facts) = self.csharp.get(entity).copied() {
            validate_type_parameters(builder, facts.constraints)?;
            validate_atom_list(builder, facts.attributes)?;
            if let Some(span) = facts.xml_provenance {
                atom(builder, span.file())?;
            }
        }
        if let Some(facts) = self.go.get(entity).copied() {
            validate_type_list(builder, facts.signature.parameters)?;
            validate_type_list(builder, facts.signature.results)?;
            validate_type_parameters(builder, facts.type_parameters)?;
            validate_entity_list(builder, facts.fields)?;
            validate_entity_list(builder, facts.method_set)?;
            validate_atom_list(builder, facts.build_constraints)?;
            validate_atom_list(builder, facts.constant_value)?;
        }
        if let Some(facts) = self.rust.get(entity).copied() {
            validate_atom_list(builder, facts.lifetimes)?;
            validate_type_parameters(builder, facts.where_clauses)?;
            validate_atom_list(builder, facts.macros)?;
        }
        if let Some(facts) = self.python.get(entity).copied() {
            validate_atom_list(builder, facts.decorators)?;
        }
        if let Some(facts) = self.java.get(entity).copied() {
            validate_type_list(builder, facts.throws)?;
            validate_atom_list(builder, facts.annotations)?;
            validate_entity_list(builder, facts.overloads)?;
            validate_entity_list(builder, facts.record_components)?;
        }
        if let Some(facts) = self.clang.get(entity).copied() {
            if facts.layout.align_bits == Some(0) {
                return Err(BuildError::LanguageExtension {
                    language: Language::Clang,
                    entity,
                    violation: LanguageExtensionViolation::ZeroLayoutAlignment,
                });
            }
            validate_type_parameters(builder, facts.templates)?;
            validate_atom_list(builder, facts.includes)?;
        }
        Ok(())
    }

    fn has_entity(&self, entity: EntityId) -> bool {
        self.typescript.get(entity).is_some()
            || self.csharp.get(entity).is_some()
            || self.go.get(entity).is_some()
            || self.rust.get(entity).is_some()
            || self.python.get(entity).is_some()
            || self.java.get(entity).is_some()
            || self.clang.get(entity).is_some()
    }
}

fn validate_type_parameters(
    builder: &IrBuilder,
    parameters: TypeParameterListId,
) -> Result<(), BuildError> {
    for (position, parameter) in list_or_dangling(
        &builder.type_parameters,
        parameters,
        SemanticSpace::TypeParameters,
    )?
    .iter()
    .enumerate()
    {
        atom(builder, parameter.name)?;
        for bound in list_or_dangling(
            &builder.type_parameter_bounds,
            parameter.bounds,
            SemanticSpace::TypeParameterBounds,
        )? {
            match bound {
                TypeParameterBound::Type(ty) => {
                    id(*ty, builder.types.len(), SemanticSpace::Type)?;
                }
                TypeParameterBound::Lifetime(name) => atom(builder, *name)?,
            }
        }
        if let TypeParameterKind::ConstValue { value_type } = parameter.kind {
            id(value_type, builder.types.len(), SemanticSpace::Type)?;
        }
        if !parameter.requirements.is_valid() {
            return Err(BuildError::TypeParameterRequirements {
                list: parameters.raw,
                position: u32::try_from(position).unwrap_or(u32::MAX),
                requirements: parameter.requirements,
            });
        }
        optional_id(parameter.default, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

fn validate_atom_list(builder: &IrBuilder, atoms: AtomListId) -> Result<(), BuildError> {
    for atom_id in list_or_dangling(&builder.atom_lists, atoms, SemanticSpace::AtomList)? {
        atom(builder, *atom_id)?;
    }
    Ok(())
}

fn validate_entity_list(builder: &IrBuilder, entities: EntityListId) -> Result<(), BuildError> {
    for entity_id in list_or_dangling(&builder.entity_lists, entities, SemanticSpace::EntityList)? {
        id(*entity_id, builder.items.len(), SemanticSpace::Entity)?;
    }
    Ok(())
}

/// Borrowed complete language-extension directory from an [`Ir`].
#[derive(Clone, Copy, Debug)]
pub struct LanguageExtensionsView<'ir> {
    /// The profile authority that selected every nonempty plane.
    pub authority: SemanticImageAuthority,
    pub typescript: LanguageExtensionColumnView<'ir, TypeScriptFacts, TypeScriptExtension>,
    pub csharp: LanguageExtensionColumnView<'ir, CSharpFacts, CSharpExtension>,
    pub go: LanguageExtensionColumnView<'ir, GoFacts, GoExtension>,
    pub rust: LanguageExtensionColumnView<'ir, RustFacts, RustExtension>,
    pub python: LanguageExtensionColumnView<'ir, PythonFacts, PythonExtension>,
    pub java: LanguageExtensionColumnView<'ir, JavaFacts, JavaExtension>,
    pub clang: LanguageExtensionColumnView<'ir, ClangFacts, ClangExtension>,
}

/// Present-value counts measured before a borrowed frontend stream is lowered.
#[derive(Clone, Copy, Default)]
struct LanguageExtensionCounts {
    typescript: usize,
    csharp: usize,
    go: usize,
    rust: usize,
    python: usize,
    java: usize,
    clang: usize,
}

impl LanguageExtensionCounts {
    fn observe(&mut self, input: Option<LanguageExtensionInput<'_>>) {
        match input {
            Some(LanguageExtensionInput::TypeScript(_)) => self.typescript += 1,
            Some(LanguageExtensionInput::CSharp(_)) => self.csharp += 1,
            Some(LanguageExtensionInput::Go(_)) => self.go += 1,
            Some(LanguageExtensionInput::Rust(_)) => self.rust += 1,
            Some(LanguageExtensionInput::Python(_)) => self.python += 1,
            Some(LanguageExtensionInput::Java(_)) => self.java += 1,
            Some(LanguageExtensionInput::Clang(_)) => self.clang += 1,
            None => {}
        }
    }
}

/// Hot entity columns. Scans touch only the lanes required by a query.
struct ItemColumns {
    _slab: Slab,
    names: RawColumn<AtomId>,
    kinds: RawColumn<ItemKind>,
    visibility: RawColumn<Visibility>,
    parents: RawColumn<OptionalId<crate::Entity>>,
    semantic_types: RawColumn<OptionalId<Type>>,
    members: RawColumn<EntityListId>,
    docs: RawColumn<DocId>,
    attributes: RawColumn<AtomListId>,
    versions: RawColumn<EntityVersion>,
}

impl Default for ItemColumns {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl ItemColumns {
    fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let names = plan.column::<AtomId>(capacity);
        let kinds = plan.column::<ItemKind>(capacity);
        let visibility = plan.column::<Visibility>(capacity);
        let parents = plan.column::<OptionalId<crate::Entity>>(capacity);
        let semantic_types = plan.column::<OptionalId<Type>>(capacity);
        let members = plan.column::<EntityListId>(capacity);
        let docs = plan.column::<DocId>(capacity);
        let attributes = plan.column::<AtomListId>(capacity);
        let versions = plan.column::<EntityVersion>(capacity);
        let slab = plan.allocate();
        Self {
            names: slab.bind(names),
            kinds: slab.bind(kinds),
            visibility: slab.bind(visibility),
            parents: slab.bind(parents),
            semantic_types: slab.bind(semantic_types),
            members: slab.bind(members),
            docs: slab.bind(docs),
            attributes: slab.bind(attributes),
            versions: slab.bind(versions),
            _slab: slab,
        }
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    fn push(&mut self, version: EntityVersion, item: Item) {
        self.names.push(item.name);
        self.kinds.push(item.kind);
        self.visibility.push(item.visibility);
        self.parents.push(OptionalId::new(item.parent));
        self.semantic_types
            .push(OptionalId::new(item.semantic_type));
        self.members.push(item.members);
        self.docs.push(item.docs);
        self.attributes.push(item.attributes);
        self.versions.push(version);
    }

    fn reserve_exact(&mut self, additional: usize) {
        let required = self.len().saturating_add(additional);
        if required <= self.names.capacity() {
            return;
        }
        let mut next = Self::with_capacity(required);
        next.names.extend_from_slice(&self.names);
        next.kinds.extend_from_slice(&self.kinds);
        next.visibility.extend_from_slice(&self.visibility);
        next.parents.extend_from_slice(&self.parents);
        next.semantic_types.extend_from_slice(&self.semantic_types);
        next.members.extend_from_slice(&self.members);
        next.docs.extend_from_slice(&self.docs);
        next.attributes.extend_from_slice(&self.attributes);
        next.versions.extend_from_slice(&self.versions);
        *self = next;
    }

    fn reserve_one(&mut self) {
        if self.len() < self.names.capacity() {
            return;
        }
        let target = self.names.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()));
    }
}

/// Cold source-location columns, loaded only by diagnostics or source links.
#[derive(Default)]
struct SourceColumns {
    files: Vec<OptionalId<crate::AtomSpace>>,
    starts: Vec<u32>,
    ends: Vec<u32>,
    rows: usize,
}

impl SourceColumns {
    fn reserve(&mut self, rows: usize, values: usize) {
        if values != 0 {
            self.files.reserve(rows);
            self.starts.reserve(rows);
            self.ends.reserve(rows);
        }
    }

    fn push(&mut self, source: Option<SourceSpan>) {
        if source.is_some() && self.files.is_empty() && self.rows != 0 {
            self.files
                .resize(self.rows, OptionalId::<crate::AtomSpace>::new(None));
            self.starts.resize(self.rows, 0);
            self.ends.resize(self.rows, 0);
        }
        if source.is_some() || !self.files.is_empty() {
            self.files
                .push(OptionalId::new(source.map(SourceSpan::file)));
            self.starts.push(source.map_or(0, SourceSpan::start));
            self.ends.push(source.map_or(0, SourceSpan::end));
        }
        self.rows = self.rows.saturating_add(1);
    }

    fn get(&self, index: usize) -> Option<SourceSpan> {
        SourceSpan::new(
            self.files.get(index).copied()?.get()?,
            *self.starts.get(index)?,
            *self.ends.get(index)?,
        )
    }
}

/// Kind of an extrinsic graph edge.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LinkKind {
    Calls,
    MethodCall,
    TypeReference,
    Reads,
    Writes,
    Imports,
    Implements,
    Overrides,
    Reexports,
    Inherits,
    Documents,
}

/// Resolution confidence forms a monotonic quality lattice.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    Syntactic,
    Heuristic,
    Indexed,
    Imported,
    Compiler,
}

/// One directed graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Link {
    pub from: EntityId,
    pub target: LinkTarget,
    pub kind: LinkKind,
    /// Strongest confidence observed for this canonical relation.
    pub confidence: Confidence,
    /// Optional representative source for compatibility queries.
    ///
    /// This is never the exhaustive evidence set. Consumers that need source
    /// truth must use [`LinkOccurrence`] rows, which retain every site.
    pub source: Option<SourceSpan>,
}

/// One authority-observed use site of a canonical [`Link`].
///
/// `link` names the deduplicated semantic relation. `confidence` and
/// `source` are intentionally site-local evidence: replacing them with the
/// relation's strongest observation would lose repeated references such as a
/// parameter and result both naming the same symbol.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LinkOccurrence {
    pub link: LinkId,
    pub confidence: Confidence,
    pub source: Option<SourceSpan>,
}

/// Evidence-independent identity used to intern exactly one row per logical link.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LinkKey {
    from: EntityId,
    target: LinkTarget,
    kind: LinkKind,
}

/// Selects the deterministic compatibility evidence stored beside one
/// canonical relation. Captured sites outrank an uncaptured representative;
/// equal-confidence captured sites sort by `(file, start, end)`. The complete
/// site set is deliberately held in [`LinkOccurrence`], never inferred here.
fn canonical_relation_evidence_precedes(candidate: Link, known: Link) -> bool {
    match candidate.confidence.cmp(&known.confidence) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => source_evidence_key(candidate.source) < source_evidence_key(known.source),
    }
}

fn source_evidence_key(source: Option<SourceSpan>) -> (u8, u32, u32, u32) {
    match source {
        // A captured coordinate is more specific than universal absence.
        Some(span) => (0, span.file().raw, span.start(), span.end()),
        None => (1, u32::MAX, u32::MAX, u32::MAX),
    }
}

/// Slice-backed entity input. No field owns frontend memory.
#[derive(Clone, Copy)]
pub struct TreeItemInput<'source> {
    pub name: &'source [u8],
    pub kind: ItemKind,
    pub visibility: Visibility,
    /// Exact authority availability and containment truth for this row.
    pub authority: EntityAuthorityFacts,
    pub parent: Option<TreeEntityId>,
    pub semantic_type: Option<TypeId>,
    pub members: &'source [TreeEntityId],
    pub docs: &'source [DocInput<'source>],
    pub attributes: &'source [&'source [u8]],
    pub source: Option<SourceSpan>,
    pub extension: Option<LanguageExtensionInput<'source>>,
}

/// Slice-backed authority-observed link occurrence input.
///
/// Every input becomes exactly one [`LinkOccurrence`]. The owning builder
/// deduplicates only its canonical relation, never these source sites.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeLinkInput {
    pub from: TreeEntityId,
    pub target: TreeLinkTarget,
    pub kind: LinkKind,
    pub confidence: Confidence,
    /// Exact authority availability for this occurrence's source site.
    pub authority: OccurrenceAuthorityFacts,
    pub source: Option<SourceSpan>,
}

/// Target used by a local borrowed tree before its entity coordinates are rebased.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeLinkTarget {
    Local(TreeEntityId),
    External(ExternalId),
}

/// An entire frontend tree submitted as borrowed slices in one call.
pub struct BorrowedTree<'source> {
    pub versions: &'source [EntityVersion],
    pub items: &'source [TreeItemInput<'source>],
    pub links: &'source [TreeLinkInput],
}

/// Statically dispatched frontend view over an existing compiler tree.
///
/// An adapter may synthesize each [`TreeItemInput`] directly from its AST while
/// iterating. It never has to allocate the 120-byte compatibility rows as an
/// intermediate array, and monomorphization removes the adapter itself.
pub trait FrontendTree {
    /// Stable versions in the same order as [`Self::items`].
    fn versions(&self) -> &[EntityVersion];

    /// Re-iterable borrowed entity stream. A second pass is used only to make
    /// exact arena reservations before canonicalization.
    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>>;

    /// Re-iterable local link stream.
    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput>;
}

impl FrontendTree for BorrowedTree<'_> {
    fn versions(&self) -> &[EntityVersion] {
        self.versions
    }

    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>> {
        self.items.iter().map(|item| TreeItemInput {
            name: item.name,
            kind: item.kind,
            visibility: item.visibility,
            authority: item.authority,
            parent: item.parent,
            semantic_type: item.semantic_type,
            members: item.members,
            docs: item.docs,
            attributes: item.attributes,
            source: item.source,
            extension: item.extension,
        })
    }

    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput> {
        self.links.iter().copied()
    }
}

/// Range assigned to one borrowed tree in the condensed entity arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRange {
    start: EntityId,
    len: u32,
}

impl EntityRange {
    /// Maps a tree-local ID to its final condensed ID.
    #[must_use]
    pub fn get(self, local: TreeEntityId) -> Option<EntityId> {
        (local.raw < self.len).then(|| EntityId::new(self.start.raw + local.raw))
    }
    #[must_use]
    pub const fn start(self) -> EntityId {
        self.start
    }
    #[must_use]
    pub const fn len(self) -> u32 {
        self.len
    }
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Coordinate class used in structural validation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSpace {
    Atom,
    Text,
    Type,
    Entity,
    External,
    Link,
    LinkOccurrence,
    TypeList,
    EntityList,
    AtomList,
    Docs,
    TupleElements,
    ObjectMembers,
    TemplateParts,
    TypeParameterBounds,
    TypeParameters,
}

/// Closed semantic fault in a language-owned extension row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionViolation {
    /// A TypeScript computed ID did not point to a computed type state.
    ComputedType,
    /// A Clang layout alignment claimed an impossible zero-bit alignment.
    ZeroLayoutAlignment,
    /// A transaction-local sparse binding escaped the exact typed fact pool
    /// measured for its one language.  Both coordinates are retained so an
    /// internal projection defect cannot be disguised as an absent fact.
    MissingPoolFact { fact: usize, count: usize },
}

/// Failure while condensing or validating frontend IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildError {
    Capacity(CapacityError),
    InvalidTreeEntity {
        raw: u32,
        count: u32,
    },
    Dangling {
        space: SemanticSpace,
        raw: u32,
    },
    /// A recursive compound type reached a row still under projection.  A
    /// recursive nominal is representable as its terminal nominal row; a
    /// compound cycle needs a dedicated recursive handle and must never
    /// recurse on the process stack while that handle is absent.
    RecursiveType { raw: u32 },
    /// A callable element used a modifier illegal for its semantic role or
    /// variadic form. Results are always required; a typed variadic tail is
    /// exactly one final parameter.
    CallableElement {
        role: CallableElementRole,
        position: usize,
        kind: TupleElementKind,
    },
    /// A callable claimed a typed variadic tail but had no final `Rest`
    /// parameter element to own it.
    MissingTypedVariadicParameter { parameter_count: usize },
    /// A qualified type path had no named segments after its self/trait base.
    EmptyQualifiedPath,
    /// A publicly constructible generic requirement set combined mutually
    /// exclusive C# primary constraints with `new()`.
    TypeParameterRequirements {
        list: u32,
        position: u32,
        requirements: TypeParameterRequirements,
    },
    /// A direct C-family qualifier wrapper was constructed with no qualifier.
    /// Empty qualification has no source-semantic node and must be omitted.
    EmptyCxxQualification,
    /// A direct C-family qualifier wrapper named a structural target on
    /// which its exact native qualifiers are illegal. The wrapper is never
    /// silently moved to a pointee or referent.
    IllegalCQualifierTarget {
        target: TypeId,
        qualifiers: crate::CvQualifiers,
    },
    /// A C++ member pointer's first operand was not a record/class nominal
    /// (or an application of one). Such a pair cannot be rendered truthfully
    /// as `Member Owner::*`.
    IllegalCxxMemberPointerOwner { owner: TypeId },
    /// A durable documentation fact was not valid UTF-8, so it cannot enter
    /// the owned text arena without loss.  Callers must retain it in the
    /// compact fragment or surface this exact terminal; silently dropping it
    /// would split render truth from durable truth.
    InvalidDocumentationUtf8 { bytes: usize },
    /// A relative occurrence span escaped its authority-captured owner span.
    InvalidOccurrenceSpan {
        owner: EntityId,
        start: u32,
        end: u32,
    },
    /// Parentage formed a cycle, so no stable qualified ownership key exists.
    ParentCycle { entity: EntityId },
    /// One source declaration could not form its validated package/path/kind
    /// identity key.  The entity coordinate and original vocabulary fault are
    /// retained instead of being reclassified as a dangling reference.
    DeclarationKey {
        entity: EntityId,
        cause: crate::DeclarationKeyFault,
    },
    /// The central scoped declaration-key writer rejected its exact framed
    /// preimage.  This preserves profile/parentage/collision-width causes.
    ScopedDeclarationPreimage {
        entity: EntityId,
        cause: crate::PreimageOverflow,
    },
    /// A foreign-key preimage could not be measured or written. The owner
    /// and compact foreign-path digest retain the exact failing endpoint.
    ForeignKeyPreimage {
        owner: EntityId,
        target: VariantFingerprint,
        cause: crate::PreimageOverflow,
    },
    DuplicateDeclarationIdentity {
        identity: DeclarationIdentity,
    },
    TreeVersionCount {
        versions: usize,
        items: usize,
    },
    /// The cold semantic-authority lanes no longer matched the owned entity
    /// row count. This is an internal admission invariant, retained as an
    /// exact terminal instead of truncating or padding authority truth.
    AuthorityRowCount {
        entities: usize,
        authority_rows: usize,
    },
    /// The cold occurrence-authority lane no longer matched observed graph
    /// occurrence rows.
    OccurrenceAuthorityRowCount {
        occurrences: usize,
        authority_rows: usize,
    },
    /// One authority row contradicted its corresponding owned entity facts.
    AuthorityFacts {
        entity: EntityId,
        cause: AuthorityFactFault,
    },
    /// One authority row contradicted its corresponding graph occurrence.
    OccurrenceAuthorityFacts {
        occurrence: LinkOccurrenceId,
        cause: AuthorityFactFault,
    },
    /// An image provenance scope could not form the same validated
    /// package/path declaration key required by every entity family.
    ImageProvenanceScope {
        cause: crate::DeclarationKeyFault,
    },
    /// The validated image scope could not allocate or write its canonical
    /// declaration-key claim.
    ImageProvenanceScopePreimage {
        cause: PreimageOverflow,
    },
    /// The image header's recipe was not derived from its exact source and
    /// advertised recipe facts.
    ImageProvenanceRecipe {
        source: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// A second image provenance claim differed from the already-bound
    /// header. Equal claims are idempotent; neither source nor recipe truth
    /// is silently overwritten.
    ImageProvenanceRebind {
        existing: ImageProvenanceClaim,
        requested: ImageProvenanceClaim,
    },
    LanguageExtension {
        language: Language,
        entity: EntityId,
        violation: LanguageExtensionViolation,
    },
    LanguageProfileMismatch {
        authority: SemanticImageAuthority,
        extension: Language,
    },
    LanguageProfileRebind {
        existing: SemanticImageAuthority,
        requested: LanguageProfile,
    },
}

impl From<CapacityError> for BuildError {
    fn from(value: CapacityError) -> Self {
        Self::Capacity(value)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity(error) => error.fmt(formatter),
            Self::InvalidTreeEntity { raw, count } => {
                write!(formatter, "tree entity {raw} is outside tree size {count}")
            }
            Self::Dangling { space, raw } => {
                write!(formatter, "{space:?} coordinate {raw} is dangling")
            }
            Self::RecursiveType { raw } => {
                write!(formatter, "compound type coordinate {raw} is recursively projected")
            }
            Self::CallableElement {
                role,
                position,
                kind,
            } => write!(
                formatter,
                "callable {role:?} element at position {position} has illegal {kind:?} form"
            ),
            Self::MissingTypedVariadicParameter { parameter_count } => write!(
                formatter,
                "typed variadic callable has {parameter_count} parameters but no final rest parameter"
            ),
            Self::TypeParameterRequirements {
                list,
                position,
                requirements,
            } => write!(
                formatter,
                "type-parameter list {list} position {position} has inconsistent requirements {requirements:?}"
            ),
            Self::EmptyQualifiedPath => formatter.write_str("qualified type path has no segments"),
            Self::EmptyCxxQualification => {
                formatter.write_str("C-family qualifier wrapper is empty")
            }
            Self::IllegalCQualifierTarget { target, qualifiers } => write!(
                formatter,
                "C-family qualifiers {qualifiers:?} are illegal on type {}",
                target.raw
            ),
            Self::IllegalCxxMemberPointerOwner { owner } => write!(
                formatter,
                "C++ member pointer owner type {} is not a record nominal",
                owner.raw
            ),
            Self::InvalidDocumentationUtf8 { bytes } => {
                write!(formatter, "documentation fact has {bytes} invalid UTF-8 bytes")
            }
            Self::InvalidOccurrenceSpan { owner, start, end } => {
                write!(formatter, "occurrence span {start}..{end} escapes entity {}", owner.raw)
            }
            Self::ParentCycle { entity } => {
                write!(formatter, "entity {} participates in a parent cycle", entity.raw)
            }
            Self::DeclarationKey { entity, cause } => {
                write!(formatter, "entity {} has an invalid declaration key: {cause:?}", entity.raw)
            }
            Self::ScopedDeclarationPreimage { entity, cause } => write!(
                formatter,
                "entity {} has an invalid scoped declaration preimage: {cause:?}",
                entity.raw
            ),
            Self::ForeignKeyPreimage { owner, target, cause } => write!(
                formatter,
                "entity {} has an invalid foreign key preimage for {:02x?}: {cause:?}",
                owner.raw,
                target.as_bytes()
            ),
            Self::DuplicateDeclarationIdentity { identity } => {
                write!(
                    formatter,
                    "declaration family {:02x?} variant {:02x?} is duplicated",
                    identity.family.as_bytes(),
                    identity.variant.as_bytes()
                )
            }
            Self::TreeVersionCount { versions, items } => {
                write!(
                    formatter,
                    "borrowed tree has {versions} versions for {items} items"
                )
            }
            Self::AuthorityRowCount {
                entities,
                authority_rows,
            } => write!(
                formatter,
                "semantic authority has {authority_rows} rows for {entities} entities"
            ),
            Self::OccurrenceAuthorityRowCount {
                occurrences,
                authority_rows,
            } => write!(
                formatter,
                "occurrence authority has {authority_rows} rows for {occurrences} occurrences"
            ),
            Self::AuthorityFacts { entity, cause } => write!(
                formatter,
                "semantic authority for entity {} is inconsistent: {cause:?}",
                entity.raw
            ),
            Self::OccurrenceAuthorityFacts { occurrence, cause } => write!(
                formatter,
                "semantic authority for occurrence {} is inconsistent: {cause:?}",
                occurrence.raw
            ),
            Self::ImageProvenanceScope { cause } => {
                write!(formatter, "semantic image provenance scope is invalid: {cause:?}")
            }
            Self::ImageProvenanceScopePreimage { cause } => write!(
                formatter,
                "semantic image provenance scope preimage is invalid: {cause:?}"
            ),
            Self::ImageProvenanceRecipe { source, recipe } => write!(
                formatter,
                "semantic image recipe {recipe:?} does not bind source {source:?}"
            ),
            Self::ImageProvenanceRebind { existing, requested } => write!(
                formatter,
                "semantic image provenance {requested:?} cannot replace {existing:?}"
            ),
            Self::LanguageExtension {
                language,
                entity,
                violation,
            } => {
                write!(
                    formatter,
                    "{language:?} extension for entity {} violates {violation:?}",
                    entity.raw
                )
            }
            Self::LanguageProfileMismatch {
                authority,
                extension,
            } => write!(
                formatter,
                "{extension:?} extension conflicts with image authority {authority:?}"
            ),
            Self::LanguageProfileRebind {
                existing,
                requested,
            } => write!(
                formatter,
                "language profile {requested:?} cannot replace {existing:?}"
            ),
        }
    }
}

impl core::error::Error for BuildError {}

/// Allocation-amortized builder for the condensed semantic IR.
///
/// Frontends should prefer [`Self::add_borrowed_tree`]: it consumes their
/// existing slice-backed tree without manufacturing owned nodes or strings.
#[derive(Default)]
pub struct IrBuilder {
    authority: SemanticImageAuthority,
    provenance: ImageProvenance,
    atoms: AtomInterner,
    types: TypeInterner,
    externals: Interner<ExternalTarget, External>,
    type_lists: ListInterner<TypeId>,
    entity_lists: ListInterner<EntityId>,
    atom_lists: ListInterner<AtomId>,
    docs: ListInterner<DocFragment>,
    tuple_elements: ListInterner<TupleElement>,
    object_members: ListInterner<ObjectMember>,
    template_parts: ListInterner<TemplatePart>,
    type_parameter_bounds: ListInterner<TypeParameterBound>,
    type_parameters: ListInterner<TypeParameter>,
    items: ItemColumns,
    authority_facts: AuthorityColumns,
    sources: SourceColumns,
    extensions: LanguageExtensions,
    link_index: HashIndex,
    links: PackedLinks,
    link_occurrences: PackedLinkOccurrences,
    occurrence_authority: OccurrenceAuthorityColumn,
    entity_scratch: Vec<EntityId>,
    atom_scratch: Vec<AtomId>,
    doc_scratch: Vec<DocFragment>,
}

impl IrBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Binds this image to one language before language-specific facts arrive.
    pub fn set_language_profile(&mut self, profile: LanguageProfile) -> Result<(), BuildError> {
        let requested = SemanticImageAuthority::Language(profile);
        if self.authority != SemanticImageAuthority::Shared && self.authority != requested {
            return Err(BuildError::LanguageProfileRebind {
                existing: self.authority,
                requested: profile,
            });
        }
        self.authority = requested;
        Ok(())
    }
    /// Binds one owned compile provenance header before entity materialization.
    ///
    /// The header is unavailable for manually assembled images. A compiled
    /// image retains the exact source, recipe, and validated package/file
    /// scope in the same arena as its semantic facts, so driver scratch never
    /// remains the only source of reader-visible authority.
    pub fn set_image_provenance(
        &mut self,
        source: SourceIdentity,
        recipe: CompileRecipeFact,
        lineage: PackageLineage<'_>,
        path: &str,
    ) -> Result<(), BuildError> {
        let expected = CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            source.identity,
            recipe.toolchain,
        );
        if expected != recipe {
            return Err(BuildError::ImageProvenanceRecipe { source, recipe });
        }
        let scope_key = DeclarationKey::new(lineage, path, ItemKind::Module, b"_")
            .map_err(|cause| BuildError::ImageProvenanceScope { cause })?;
        let scope_preimage_len = scope_key
            .preimage_len()
            .map_err(|cause| BuildError::ImageProvenanceScopePreimage { cause })?;
        let mut scope_preimage = vec![0_u8; scope_preimage_len];
        let requested = ImageProvenanceClaim {
            source,
            recipe,
            scope: SemanticScopeClaim {
                identity: {
                    let written = scope_key
                        .write_preimage(&mut scope_preimage)
                        .map_err(|cause| BuildError::ImageProvenanceScopePreimage { cause })?;
                    ContentId::<SemanticScopeDomain>::from_canonical_bytes(
                        &scope_preimage[..written],
                    )
                },
            },
        };
        if let ImageProvenance::Captured {
            source: existing_source,
            recipe: existing_recipe,
            claim: existing_scope,
            ..
        } = self.provenance
        {
            let existing = ImageProvenanceClaim {
                source: existing_source,
                recipe: existing_recipe,
                scope: existing_scope,
            };
            if existing == requested {
                return Ok(());
            }
            return Err(BuildError::ImageProvenanceRebind {
                existing,
                requested,
            });
        }
        self.set_language_profile(recipe.profile)?;
        let scope = SemanticScopeFacts {
            ecosystem: self.intern_atom(lineage.ecosystem.as_bytes())?,
            package: self.intern_atom(lineage.name.as_bytes())?,
            path: self.intern_atom(path.as_bytes())?,
        };
        self.provenance = ImageProvenance::Captured {
            source,
            recipe,
            claim: requested.scope,
            scope,
        };
        Ok(())
    }
    pub fn intern_atom(&mut self, bytes: &[u8]) -> Result<AtomId, BuildError> {
        self.atoms.intern(bytes).map_err(Into::into)
    }
    pub fn intern_text(&mut self, text: &str) -> Result<TextId, BuildError> {
        self.atoms.intern_text(text).map_err(Into::into)
    }
    /// Reserves final packed headers and hash slots for a known lowering batch.
    pub fn reserve_types(&mut self, additional: usize) {
        self.types.reserve(additional);
    }
    pub fn intern_type(&mut self, ty: TypeExpr) -> Result<TypeId, BuildError> {
        self.types.intern(ty).map_err(Into::into)
    }
    /// Interns a never-guarded state-indexed term and retains its proof in the ID.
    pub fn intern_guarded<State: TypeState>(
        &mut self,
        ty: GuardedType<State>,
    ) -> Result<TypedTypeId<State>, BuildError> {
        self.intern_type(State::inject(ty.node))
            .map(TypedTypeId::proven)
    }
    pub fn intern_concrete(&mut self, ty: ConcreteType) -> Result<ConcreteTypeId, BuildError> {
        self.intern_guarded(GuardedType::concrete(ty))
    }
    pub fn intern_computed(&mut self, ty: ComputedType) -> Result<ComputedTypeId, BuildError> {
        self.intern_guarded(GuardedType::computed(ty))
    }
    pub fn intern_unknown(&mut self, ty: UnknownType) -> Result<UnknownTypeId, BuildError> {
        self.intern_guarded(GuardedType::unknown(ty))
    }
    pub fn intern_types(&mut self, types: &[TypeId]) -> Result<TypeListId, BuildError> {
        self.type_lists.intern(types).map_err(Into::into)
    }
    pub fn intern_external(&mut self, target: ExternalTarget) -> Result<ExternalId, BuildError> {
        self.externals.intern(target).map_err(Into::into)
    }
    pub fn intern_docs(&mut self, docs: &[DocFragment]) -> Result<DocId, BuildError> {
        self.docs.intern(docs).map_err(Into::into)
    }
    pub fn intern_members(&mut self, members: &[EntityId]) -> Result<EntityListId, BuildError> {
        self.entity_lists.intern(members).map_err(Into::into)
    }
    pub fn intern_attributes(&mut self, attributes: &[AtomId]) -> Result<AtomListId, BuildError> {
        self.atom_lists.intern(attributes).map_err(Into::into)
    }
    pub fn intern_tuple_elements(
        &mut self,
        elements: &[TupleElement],
    ) -> Result<TupleElementListId, BuildError> {
        self.tuple_elements.intern(elements).map_err(Into::into)
    }
    pub fn intern_object_members(
        &mut self,
        members: &[ObjectMember],
    ) -> Result<ObjectMemberListId, BuildError> {
        self.object_members.intern(members).map_err(Into::into)
    }
    pub fn intern_template_parts(
        &mut self,
        parts: &[TemplatePart],
    ) -> Result<TemplatePartListId, BuildError> {
        self.template_parts.intern(parts).map_err(Into::into)
    }
    pub fn intern_type_parameter_bounds(
        &mut self,
        bounds: &[TypeParameterBound],
    ) -> Result<TypeParameterBoundListId, BuildError> {
        self.type_parameter_bounds.intern(bounds).map_err(Into::into)
    }
    pub fn intern_type_parameters(
        &mut self,
        parameters: &[TypeParameter],
    ) -> Result<TypeParameterListId, BuildError> {
        self.type_parameters.intern(parameters).map_err(Into::into)
    }
    pub fn add_item(
        &mut self,
        version: EntityVersion,
        item: Item,
        extension: Option<LanguageExtensionInput<'_>>,
        authority_facts: EntityAuthorityFacts,
    ) -> Result<EntityId, BuildError> {
        if let Some(extension) = extension
            && self.authority.language() != Some(extension.language())
        {
            return Err(BuildError::LanguageProfileMismatch {
                authority: self.authority,
                extension: extension.language(),
            });
        }
        let id = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        self.items.reserve_one();
        self.authority_facts.reserve(1);
        self.sources.push(item.source);
        self.items.push(version, item);
        self.authority_facts.push(authority_facts);
        self.extensions.push(extension)?;
        Ok(id)
    }
    pub fn add_link(&mut self, link: Link) -> Result<LinkId, BuildError> {
        let key = LinkKey {
            from: link.from,
            target: link.target,
            kind: link.kind,
        };
        let value_hash = hash(&key);
        if let Some(ordinal) = self.link_index.find(value_hash, |ordinal| {
            self.links.get(LinkId::new(ordinal)).is_some_and(|known| {
                known.from == key.from && known.target == key.target && known.kind == key.kind
            })
        }) {
            let id = LinkId::new(ordinal);
            let Some(known) = self.links.get(id) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Entity,
                    raw: ordinal,
                });
            };
            // Confidence is a monotonic quality lattice. Equal-confidence
            // observations join by one total source order, so the legacy
            // representative column never depends on authority emission
            // order. Exhaustive evidence remains in LinkOccurrence rows.
            if canonical_relation_evidence_precedes(link, known) {
                self.links.replace(id, link)?;
            }
            return Ok(id);
        }
        let id = LinkId::try_from_index(self.links.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: self.links.len(),
        })?;
        self.links.reserve_one(usize::from(link.source.is_some()));
        self.links.push(link)?;
        self.link_index.insert(value_hash, id.raw);
        Ok(id)
    }

    /// Records one authority-observed source site and returns its typed
    /// occurrence coordinate. The relation remains canonical and deduped,
    /// while this method preserves every observation's span and confidence.
    pub fn add_link_occurrence(
        &mut self,
        link: Link,
        authority: OccurrenceAuthorityFacts,
    ) -> Result<LinkOccurrenceId, BuildError> {
        let id = LinkOccurrenceId::try_from_index(self.link_occurrences.len()).map_err(|_| {
            CapacityError {
                space: crate::CapacitySpace::Value,
                actual: self.link_occurrences.len(),
            }
        })?;
        self.link_occurrences
            .reserve_one(usize::from(link.source.is_some()));
        self.occurrence_authority.reserve(1);
        let occurrence = LinkOccurrence {
            link: self.add_link(link)?,
            confidence: link.confidence,
            source: link.source,
        };
        self.link_occurrences.push(occurrence)?;
        self.occurrence_authority.push(authority);
        Ok(id)
    }

    /// Reserves a stable entity range before type construction.
    ///
    /// The returned exclusive builder exposes the final entity coordinates, so
    /// recursive nominal types and forward graph links can name nodes before
    /// any row is copied into the condensed arenas.
    pub fn reserve_tree<'builder, 'source>(
        &'builder mut self,
        versions: &'source [EntityVersion],
    ) -> Result<TreeBuilder<'builder, 'source>, BuildError> {
        let start = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        let len = u32::try_from(versions.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: versions.len(),
        })?;
        Ok(TreeBuilder {
            builder: self,
            versions,
            range: EntityRange { start, len },
        })
    }

    /// Condenses a whole borrowed compiler tree with one copy of each distinct atom/list.
    pub fn add_borrowed_tree(&mut self, tree: BorrowedTree<'_>) -> Result<EntityRange, BuildError> {
        self.add_frontend_tree(&tree)
    }

    /// Condenses any compiler-owned tree through a zero-allocation static
    /// adapter. Frontends yield stack rows directly from their native AST.
    pub fn add_frontend_tree<Tree: FrontendTree + ?Sized>(
        &mut self,
        tree: &Tree,
    ) -> Result<EntityRange, BuildError> {
        let item_count = tree.items().len();
        if tree.versions().len() != item_count {
            return Err(BuildError::TreeVersionCount {
                versions: tree.versions().len(),
                items: item_count,
            });
        }
        self.reserve_frontend_tree(tree);
        let start = EntityId::try_from_index(self.items.len()).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: self.items.len(),
        })?;
        let count = u32::try_from(item_count).map_err(|_| CapacityError {
            space: crate::CapacitySpace::Value,
            actual: item_count,
        })?;
        let range = EntityRange { start, len: count };

        for (input, version) in tree.items().zip(tree.versions()) {
            let name = self.intern_atom(input.name)?;
            let parent = transpose_tree_id(range, input.parent)?;

            self.entity_scratch.clear();
            for member in input.members {
                self.entity_scratch.push(tree_id(range, *member)?);
            }
            let members = self.entity_lists.intern(&self.entity_scratch)?;

            self.atom_scratch.clear();
            for attribute in input.attributes {
                self.atom_scratch.push(self.atoms.intern(attribute)?);
            }
            let attributes = self.atom_lists.intern(&self.atom_scratch)?;

            self.doc_scratch.clear();
            for fragment in input.docs {
                let fragment = match *fragment {
                    DocInput::Text(text) => DocFragment::Text(self.atoms.intern_text(text)?),
                    DocInput::Code(text) => DocFragment::Code(self.atoms.intern_text(text)?),
                    DocInput::Link { label, target } => DocFragment::Link {
                        label: self.atoms.intern_text(label)?,
                        target: match target {
                            TreeLinkTarget::Local(local) => {
                                LinkTarget::Local(tree_id(range, local)?)
                            }
                            TreeLinkTarget::External(external) => LinkTarget::External(external),
                        },
                    },
                    DocInput::SoftBreak => DocFragment::SoftBreak,
                    DocInput::HardBreak => DocFragment::HardBreak,
                };
                self.doc_scratch.push(fragment);
            }
            let docs = self.docs.intern(&self.doc_scratch)?;
            self.add_item(
                *version,
                Item {
                    name,
                    kind: input.kind,
                    visibility: input.visibility,
                    parent,
                    semantic_type: input.semantic_type,
                    members,
                    docs,
                    attributes,
                    source: input.source,
                },
                input.extension,
                input.authority,
            )?;
        }

        for input in tree.links() {
            let target = match input.target {
                TreeLinkTarget::Local(local) => LinkTarget::Local(tree_id(range, local)?),
                TreeLinkTarget::External(external) => LinkTarget::External(external),
            };
            self.add_link_occurrence(Link {
                from: tree_id(range, input.from)?,
                target,
                kind: input.kind,
                confidence: input.confidence,
                source: input.source,
            }, input.authority)?;
        }
        Ok(range)
    }

    fn reserve_frontend_tree<Tree: FrontendTree + ?Sized>(&mut self, tree: &Tree) {
        let capacity = BorrowedTreeCapacity::measure(tree);
        let item_count = tree.items().len();
        let link_count = tree.links().len();
        self.items.reserve_exact(item_count);
        self.authority_facts.reserve(item_count);
        self.sources.reserve(item_count, capacity.source_values);
        self.extensions.reserve(item_count, capacity.extensions);
        let link_source_values = tree.links().filter(|link| link.source.is_some()).count();
        self.links.reserve_exact(link_count, link_source_values);
        self.link_occurrences
            .reserve_exact(link_count, link_source_values);
        self.occurrence_authority.reserve(link_count);
        self.link_index.reserve(link_count);
        self.atoms
            .reserve(capacity.atom_values, capacity.atom_bytes);
        self.entity_lists
            .reserve(capacity.member_lists, capacity.member_values);
        self.atom_lists
            .reserve(capacity.attribute_lists, capacity.attribute_values);
        self.docs.reserve(capacity.doc_lists, capacity.doc_values);
        self.entity_scratch.reserve(capacity.max_members);
        self.atom_scratch.reserve(capacity.max_attributes);
        self.doc_scratch.reserve(capacity.max_docs);
    }

    /// Validates all caller-constructible IDs and freezes compact query indices.
    pub fn finish(self) -> Result<Ir, BuildError> {
        self.validate()?;
        let links = self.links;
        let link_occurrences = self.link_occurrences;
        let (indices, kind_offsets) = IrIndices::build(
            &self.items,
            &self.items.versions,
            &self.atoms,
            &links,
            &link_occurrences,
            self.externals.as_slice(),
        )?;
        Ok(Ir {
            authority: self.authority,
            provenance: self.provenance,
            atoms: self.atoms.freeze(),
            types: self.types.freeze(),
            externals: self.externals.into_values(),
            type_lists: self.type_lists.freeze(),
            entity_lists: self.entity_lists.freeze(),
            atom_lists: self.atom_lists.freeze(),
            docs: self.docs.freeze(),
            tuple_elements: self.tuple_elements.freeze(),
            object_members: self.object_members.freeze(),
            template_parts: self.template_parts.freeze(),
            type_parameter_bounds: self.type_parameter_bounds.freeze(),
            type_parameters: self.type_parameters.freeze(),
            items: self.items,
            authority_facts: self.authority_facts,
            sources: self.sources,
            extensions: self.extensions,
            indices,
            kind_offsets,
            links,
            link_occurrences,
            occurrence_authority: self.occurrence_authority,
        })
    }

    fn validate(&self) -> Result<(), BuildError> {
        if !self.authority_facts.aligned(self.items.len()) {
            return Err(BuildError::AuthorityRowCount {
                entities: self.items.len(),
                authority_rows: self.authority_facts.len(),
            });
        }
        if self.occurrence_authority.len() != self.link_occurrences.len() {
            return Err(BuildError::OccurrenceAuthorityRowCount {
                occurrences: self.link_occurrences.len(),
                authority_rows: self.occurrence_authority.len(),
            });
        }
        if let ImageProvenance::Captured {
            source,
            recipe,
            scope,
            ..
        } = self.provenance
        {
            let expected = CompileRecipeFact::derive(
                recipe.profile,
                recipe.stage,
                recipe.tool,
                source.identity,
                recipe.toolchain,
            );
            if expected != recipe {
                return Err(BuildError::ImageProvenanceRecipe { source, recipe });
            }
            atom(self, scope.ecosystem)?;
            atom(self, scope.package)?;
            atom(self, scope.path)?;
            if self.authority != SemanticImageAuthority::Language(recipe.profile) {
                return Err(BuildError::LanguageProfileRebind {
                    existing: self.authority,
                    requested: recipe.profile,
                });
            }
        }
        for raw in 0..self.types.len() {
            let Some(ty) = self.types.get(TypeId::new(raw as u32)) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Type,
                    raw: raw as u32,
                });
            };
            validate_type(self, ty)?;
        }
        for target in self.externals.as_slice() {
            match target {
                ExternalTarget::Stable { target } => {
                    // A resolved endpoint is entirely typed identity; it has
                    // no invented foreign spelling to validate.
                    let _ = target;
                }
                ExternalTarget::Foreign(target) => {
                    atom(self, target.path)?;
                    atom(self, target.display)?;
                    match target.origin {
                        ForeignTargetOrigin::Package { ecosystem, package } => {
                            atom(self, ecosystem)?;
                            atom(self, package)?;
                        }
                        ForeignTargetOrigin::Namespace { ecosystem, namespace } => {
                            atom(self, ecosystem)?;
                            atom(self, namespace)?;
                        }
                        ForeignTargetOrigin::Universe { ecosystem }
                        | ForeignTargetOrigin::Unspecified { ecosystem } => atom(self, ecosystem)?,
                    }
                }
                ExternalTarget::FragmentEntity { display, .. } => atom(self, *display)?,
            }
        }
        for index in 0..self.items.len() {
            let entity = EntityId::new(index as u32);
            atom(self, self.items.names[index])?;
            optional_id(
                self.items.parents[index].get(),
                self.items.len(),
                SemanticSpace::Entity,
            )?;
            optional_id(
                self.items.semantic_types[index].get(),
                self.types.len(),
                SemanticSpace::Type,
            )?;
            if let Some(source) = self.sources.get(index) {
                atom(self, source.file())?;
            }
            self.extensions.validate_entity(self, entity)?;
            let members = list_or_dangling(
                &self.entity_lists,
                self.items.members[index],
                SemanticSpace::EntityList,
            )?;
            for member in members {
                id(*member, self.items.len(), SemanticSpace::Entity)?;
            }
            let attributes = list_or_dangling(
                &self.atom_lists,
                self.items.attributes[index],
                SemanticSpace::AtomList,
            )?;
            for attribute in attributes {
                atom(self, *attribute)?;
            }
            let docs = list_or_dangling(&self.docs, self.items.docs[index], SemanticSpace::Docs)?;
            for fragment in docs {
                validate_doc(self, *fragment)?;
            }
            let facts = self.authority_facts.facts(index).ok_or(
                BuildError::AuthorityRowCount {
                    entities: self.items.len(),
                    authority_rows: self.authority_facts.len(),
                },
            )?;
            let local_parent = self.items.parents[index]
                .get()
                .and_then(|parent| self.items.versions.get(parent.index()).copied())
                .map(EntityVersion::identity);
            validate_entity_authority(
                entity,
                facts,
                local_parent,
                self.sources.get(index).is_some(),
                !members.is_empty(),
                self.items.semantic_types[index].get().is_some(),
                !docs.is_empty(),
                self.items.visibility[index] != Visibility::Unknown,
                !attributes.is_empty(),
                self.extensions.has_entity(entity),
            )?;
        }
        for raw in 0..self.links.len() {
            let Some(link) = self.links.get(LinkId::new(raw as u32)) else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::Entity,
                    raw: raw as u32,
                });
            };
            id(link.from, self.items.len(), SemanticSpace::Entity)?;
            validate_target(self, link.target)?;
            if let Some(source) = link.source {
                atom(self, source.file())?;
            }
        }
        for raw in 0..self.link_occurrences.len() {
            let Some(occurrence) = self
                .link_occurrences
                .get(LinkOccurrenceId::new(raw as u32))
            else {
                return Err(BuildError::Dangling {
                    space: SemanticSpace::LinkOccurrence,
                    raw: raw as u32,
                });
            };
            id(occurrence.link, self.links.len(), SemanticSpace::Link)?;
            if let Some(source) = occurrence.source {
                atom(self, source.file())?;
            }
            let authority = self.occurrence_authority.get(raw).ok_or(
                BuildError::OccurrenceAuthorityRowCount {
                    occurrences: self.link_occurrences.len(),
                    authority_rows: self.occurrence_authority.len(),
                },
            )?;
            let present = occurrence.source.is_some();
            if matches!(authority.source, FactAvailability::Captured) != present {
                return Err(BuildError::OccurrenceAuthorityFacts {
                    occurrence: LinkOccurrenceId::new(raw as u32),
                    cause: AuthorityFactFault::Availability {
                        plane: AuthorityFactPlane::OccurrenceSource,
                        claimed: authority.source,
                        present,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Validates one cold authority row against the corresponding owned entity
/// facts. List capture is intentionally not inferred from list cardinality:
/// captured-empty and unavailable-empty are distinct authority observations.
fn validate_entity_authority(
    entity: EntityId,
    facts: EntityAuthorityFacts,
    local_parent: Option<DeclarationIdentity>,
    has_source: bool,
    has_members: bool,
    has_semantic_type: bool,
    has_documentation: bool,
    has_visibility: bool,
    has_attributes: bool,
    has_extension: bool,
) -> Result<(), BuildError> {
    let parentage_matches = match facts.parentage {
        ParentageAuthority::Root => local_parent.is_none(),
        ParentageAuthority::Bound(parent) => local_parent == Some(parent),
        ParentageAuthority::UnrepresentedAuthorityOwner(_) | ParentageAuthority::Unavailable => {
            local_parent.is_none()
        }
    };
    if !parentage_matches {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::Parentage {
                claimed: facts.parentage,
                local_parent,
            },
        });
    }
    validate_authority_availability(entity, AuthorityFactPlane::Source, facts.source, has_source)?;
    if facts.source != facts.source_file {
        return Err(BuildError::AuthorityFacts {
            entity,
            cause: AuthorityFactFault::SourceFileWithoutSource {
                source: facts.source,
                source_file: facts.source_file,
            },
        });
    }
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SourceFile,
        facts.source_file,
        has_source,
    )?;
    validate_authority_availability(
        entity,
        AuthorityFactPlane::SemanticType,
        facts.semantic_type,
        has_semantic_type,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Members,
        facts.members,
        has_members,
    )?;
    // Documentation and attributes can be Captured with empty lists. A
    // nonempty value, however, cannot claim an unavailable authority plane.
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Documentation,
        facts.documentation,
        has_documentation,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Visibility,
        facts.visibility,
        has_visibility,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::Attributes,
        facts.attributes,
        has_attributes,
    )?;
    validate_list_authority_availability(
        entity,
        AuthorityFactPlane::LanguageExtension,
        facts.language_extension,
        has_extension,
    )
}

fn validate_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    let matches = matches!(claimed, FactAvailability::Captured) == present;
    if matches {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

fn validate_list_authority_availability(
    entity: EntityId,
    plane: AuthorityFactPlane,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), BuildError> {
    if !present || claimed == FactAvailability::Captured {
        return Ok(());
    }
    Err(BuildError::AuthorityFacts {
        entity,
        cause: AuthorityFactFault::Availability {
            plane,
            claimed,
            present,
        },
    })
}

#[derive(Default)]
struct BorrowedTreeCapacity {
    atom_values: usize,
    atom_bytes: usize,
    member_lists: usize,
    member_values: usize,
    attribute_lists: usize,
    attribute_values: usize,
    doc_lists: usize,
    doc_values: usize,
    extensions: LanguageExtensionCounts,
    source_values: usize,
    max_members: usize,
    max_attributes: usize,
    max_docs: usize,
}

impl BorrowedTreeCapacity {
    fn measure<Tree: FrontendTree + ?Sized>(tree: &Tree) -> Self {
        let mut capacity = Self::default();
        let has_items = usize::from(tree.items().len() != 0);
        capacity.member_lists = has_items;
        capacity.attribute_lists = has_items;
        capacity.doc_lists = has_items;
        for item in tree.items() {
            capacity.atom_values = capacity.atom_values.saturating_add(1);
            capacity.atom_bytes = capacity.atom_bytes.saturating_add(item.name.len());
            capacity.member_values = capacity.member_values.saturating_add(item.members.len());
            capacity.attribute_values = capacity
                .attribute_values
                .saturating_add(item.attributes.len());
            capacity.doc_values = capacity.doc_values.saturating_add(item.docs.len());
            capacity.member_lists = capacity
                .member_lists
                .saturating_add(usize::from(!item.members.is_empty()));
            capacity.attribute_lists = capacity
                .attribute_lists
                .saturating_add(usize::from(!item.attributes.is_empty()));
            capacity.doc_lists = capacity
                .doc_lists
                .saturating_add(usize::from(!item.docs.is_empty()));
            capacity.extensions.observe(item.extension);
            capacity.source_values = capacity
                .source_values
                .saturating_add(usize::from(item.source.is_some()));
            capacity.max_members = capacity.max_members.max(item.members.len());
            capacity.max_attributes = capacity.max_attributes.max(item.attributes.len());
            capacity.max_docs = capacity.max_docs.max(item.docs.len());
            for attribute in item.attributes {
                capacity.atom_values = capacity.atom_values.saturating_add(1);
                capacity.atom_bytes = capacity.atom_bytes.saturating_add(attribute.len());
            }
            for doc in item.docs {
                let text = match doc {
                    DocInput::Text(text) | DocInput::Code(text) => Some(*text),
                    DocInput::Link { label, .. } => Some(*label),
                    DocInput::SoftBreak | DocInput::HardBreak => None,
                };
                if let Some(text) = text {
                    capacity.atom_values = capacity.atom_values.saturating_add(1);
                    capacity.atom_bytes = capacity.atom_bytes.saturating_add(text.len());
                }
            }
        }
        capacity
    }
}

/// Exclusive typestate for a borrowed tree whose final entity range is known.
///
/// Frontends may intern arbitrarily rich types through this handle using the
/// range's final IDs, then commit their original slice-backed rows in one pass.
pub struct TreeBuilder<'builder, 'source> {
    builder: &'builder mut IrBuilder,
    versions: &'source [EntityVersion],
    range: EntityRange,
}

impl TreeBuilder<'_, '_> {
    #[must_use]
    pub const fn entities(&self) -> EntityRange {
        self.range
    }
    pub fn intern_atom(&mut self, bytes: &[u8]) -> Result<AtomId, BuildError> {
        self.builder.intern_atom(bytes)
    }
    pub fn intern_text(&mut self, text: &str) -> Result<TextId, BuildError> {
        self.builder.intern_text(text)
    }
    pub fn reserve_types(&mut self, additional: usize) {
        self.builder.reserve_types(additional);
    }
    pub fn intern_type(&mut self, ty: TypeExpr) -> Result<TypeId, BuildError> {
        self.builder.intern_type(ty)
    }
    pub fn intern_guarded<State: TypeState>(
        &mut self,
        ty: GuardedType<State>,
    ) -> Result<TypedTypeId<State>, BuildError> {
        self.builder.intern_guarded(ty)
    }
    pub fn intern_concrete(&mut self, ty: ConcreteType) -> Result<ConcreteTypeId, BuildError> {
        self.builder.intern_concrete(ty)
    }
    pub fn intern_computed(&mut self, ty: ComputedType) -> Result<ComputedTypeId, BuildError> {
        self.builder.intern_computed(ty)
    }
    pub fn intern_unknown(&mut self, ty: UnknownType) -> Result<UnknownTypeId, BuildError> {
        self.builder.intern_unknown(ty)
    }
    pub fn intern_types(&mut self, types: &[TypeId]) -> Result<TypeListId, BuildError> {
        self.builder.intern_types(types)
    }
    pub fn intern_tuple_elements(
        &mut self,
        elements: &[TupleElement],
    ) -> Result<TupleElementListId, BuildError> {
        self.builder.intern_tuple_elements(elements)
    }
    pub fn intern_object_members(
        &mut self,
        members: &[ObjectMember],
    ) -> Result<ObjectMemberListId, BuildError> {
        self.builder.intern_object_members(members)
    }
    pub fn intern_template_parts(
        &mut self,
        parts: &[TemplatePart],
    ) -> Result<TemplatePartListId, BuildError> {
        self.builder.intern_template_parts(parts)
    }
    pub fn intern_type_parameter_bounds(
        &mut self,
        bounds: &[TypeParameterBound],
    ) -> Result<TypeParameterBoundListId, BuildError> {
        self.builder.intern_type_parameter_bounds(bounds)
    }
    pub fn intern_type_parameters(
        &mut self,
        parameters: &[TypeParameter],
    ) -> Result<TypeParameterListId, BuildError> {
        self.builder.intern_type_parameters(parameters)
    }
    /// Interns an entity list while the reserved tree range keeps local
    /// entity identities branded to this one transaction.
    pub fn intern_members(&mut self, members: &[EntityId]) -> Result<EntityListId, BuildError> {
        self.builder.intern_members(members)
    }
    /// Interns an atom list while the reserved tree range owns its semantic
    /// extension projection.
    pub fn intern_attributes(
        &mut self,
        attributes: &[AtomId],
    ) -> Result<AtomListId, BuildError> {
        self.builder.intern_attributes(attributes)
    }
    pub fn intern_external(&mut self, target: ExternalTarget) -> Result<ExternalId, BuildError> {
        self.builder.intern_external(target)
    }
    pub fn commit(
        self,
        items: &[TreeItemInput<'_>],
        links: &[TreeLinkInput],
    ) -> Result<EntityRange, BuildError> {
        self.builder.add_borrowed_tree(BorrowedTree {
            versions: self.versions,
            items,
            links,
        })
    }
}

fn transpose_tree_id(
    range: EntityRange,
    id: Option<TreeEntityId>,
) -> Result<Option<EntityId>, BuildError> {
    id.map(|id| tree_id(range, id)).transpose()
}

fn tree_id(range: EntityRange, id: TreeEntityId) -> Result<EntityId, BuildError> {
    range.get(id).ok_or(BuildError::InvalidTreeEntity {
        raw: id.raw,
        count: range.len,
    })
}

fn validate_type(builder: &IrBuilder, ty: TypeExpr) -> Result<(), BuildError> {
    match ty {
        TypeExpr::Concrete(concrete) => validate_concrete_type(builder, concrete),
        TypeExpr::Computed(computed) => validate_computed_type(builder, computed),
        TypeExpr::Unknown(unknown) => unknown
            .spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
    }
}

fn validate_concrete_type(builder: &IrBuilder, ty: ConcreteType) -> Result<(), BuildError> {
    match ty {
        ConcreteType::Builtin(_) => Ok(()),
        ConcreteType::Literal(literal) => match literal {
            LiteralType::String(value)
            | LiteralType::Number(value)
            | LiteralType::BigInt(value) => atom(builder, value),
            LiteralType::Boolean(_) | LiteralType::Null | LiteralType::Undefined => Ok(()),
        },
        ConcreteType::Nominal(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        ConcreteType::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
        ConcreteType::Parameter(atom_id) => atom(builder, atom_id),
        ConcreteType::Applied {
            constructor,
            arguments,
        } => {
            id(constructor, builder.types.len(), SemanticSpace::Type)?;
            validate_type_list(builder, arguments)
        }
        ConcreteType::Tuple(list) => {
            for element in
                list_or_dangling(&builder.tuple_elements, list, SemanticSpace::TupleElements)?
            {
                if let Some(label) = element.label {
                    atom(builder, label)?;
                }
                id(element.ty, builder.types.len(), SemanticSpace::Type)?;
            }
            Ok(())
        }
        ConcreteType::Object(list) => {
            for member in
                list_or_dangling(&builder.object_members, list, SemanticSpace::ObjectMembers)?
            {
                validate_object_member(builder, *member)?;
            }
            Ok(())
        }
        ConcreteType::Union(list)
        | ConcreteType::Intersection(list)
        | ConcreteType::ImplTrait(list)
        | ConcreteType::DynTrait(list) => {
            validate_type_list(builder, list)
        }
        ConcreteType::Function {
            parameters,
            results,
            abi,
            variadic,
            ..
        } => {
            let parameters = list_or_dangling(
                &builder.tuple_elements,
                parameters,
                SemanticSpace::TupleElements,
            )?;
            let mut typed_rest = false;
            for (position, parameter) in parameters.iter().copied().enumerate() {
                if let Some(label) = parameter.label {
                    atom(builder, label)?;
                }
                id(parameter.ty, builder.types.len(), SemanticSpace::Type)?;
                if parameter.kind == TupleElementKind::Rest {
                    let is_final = position + 1 == parameters.len();
                    if variadic != VariadicForm::TypedLast || !is_final {
                        return Err(BuildError::CallableElement {
                            role: CallableElementRole::Parameter,
                            position,
                            kind: parameter.kind,
                        });
                    }
                    typed_rest = true;
                }
            }
            if variadic == VariadicForm::TypedLast && !typed_rest {
                return Err(BuildError::MissingTypedVariadicParameter {
                    parameter_count: parameters.len(),
                });
            }
            for (position, result) in list_or_dangling(
                &builder.tuple_elements,
                results,
                SemanticSpace::TupleElements,
            )?
            .iter()
            .copied()
            .enumerate()
            {
                if let Some(label) = result.label {
                    atom(builder, label)?;
                }
                id(result.ty, builder.types.len(), SemanticSpace::Type)?;
                if result.kind != TupleElementKind::Required {
                    return Err(BuildError::CallableElement {
                        role: CallableElementRole::Result,
                        position,
                        kind: result.kind,
                    });
                }
            }
            if let Some(abi) = abi {
                atom(builder, abi)?;
            }
            Ok(())
        }
        ConcreteType::Reference {
            target, lifetime, ..
        } => {
            id(target, builder.types.len(), SemanticSpace::Type)?;
            if let Some(lifetime) = lifetime {
                atom(builder, lifetime)?;
            }
            Ok(())
        }
        ConcreteType::CxxReference { target, .. }
        | ConcreteType::CPointer { target }
        | ConcreteType::CBlockPointer { target } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::NativeCharacter { .. } => Ok(()),
        ConcreteType::CxxMemberPointer { owner, member } => {
            id(owner, builder.types.len(), SemanticSpace::Type)?;
            id(member, builder.types.len(), SemanticSpace::Type)
                ?;
            if !is_cxx_record_owner(builder, owner) {
                return Err(BuildError::IllegalCxxMemberPointerOwner { owner });
            }
            Ok(())
        }
        ConcreteType::CQualified { target, qualifiers } => {
            if qualifiers.is_empty() {
                return Err(BuildError::EmptyCxxQualification);
            }
            id(target, builder.types.len(), SemanticSpace::Type)?;
            let target_type = builder.types.get(target).ok_or(BuildError::Dangling {
                space: SemanticSpace::Type,
                raw: target.raw,
            })?;
            let legal = match target_type.concrete() {
                Some(ConcreteType::CPointer { .. }) => true,
                Some(ConcreteType::Function { .. })
                | Some(ConcreteType::CxxReference { .. }) => {
                    !qualifiers.const_ && !qualifiers.volatile && !qualifiers.restrict
                }
                _ => !qualifiers.restrict,
            };
            if !legal {
                return Err(BuildError::IllegalCQualifierTarget { target, qualifiers });
            }
            Ok(())
        }
        ConcreteType::Pointer { target, .. }
        | ConcreteType::Slice(target)
        | ConcreteType::Optional(target) => id(target, builder.types.len(), SemanticSpace::Type),
        ConcreteType::Array { element, shape } => {
            id(element, builder.types.len(), SemanticSpace::Type)?;
            if let ArrayShape::ConstExpression(expression) = shape {
                atom(builder, expression)?;
            }
            Ok(())
        }
        ConcreteType::Wildcard(WildcardBound::Unbounded) => Ok(()),
        ConcreteType::Wildcard(WildcardBound::Extends(bound))
        | ConcreteType::Wildcard(WildcardBound::Super(bound)) => {
            id(bound, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Annotated { target, .. } => {
            id(target, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Inferred(spelling) => spelling
            .map(|spelling| atom(builder, spelling))
            .transpose()
            .map(|_| ()),
        ConcreteType::QualifiedPath {
            self_type,
            trait_type,
            segments,
            spelling,
        } => {
            id(self_type, builder.types.len(), SemanticSpace::Type)?;
            optional_id(trait_type, builder.types.len(), SemanticSpace::Type)?;
            if let QualifiedSegments::Captured(segments) = segments {
                let segments = list_or_dangling(
                    &builder.atom_lists,
                    segments,
                    SemanticSpace::AtomList,
                )?;
                if segments.is_empty() {
                    return Err(BuildError::EmptyQualifiedPath);
                }
                for segment in segments {
                    atom(builder, *segment)?;
                }
            }
            atom(builder, spelling)
        }
        ConcreteType::Map { key, value } => {
            id(key, builder.types.len(), SemanticSpace::Type)?;
            id(value, builder.types.len(), SemanticSpace::Type)
        }
        ConcreteType::Channel { element, .. } => {
            id(element, builder.types.len(), SemanticSpace::Type)
        }
    }
}

/// A C++ member-pointer owner is a class/record nominal or one generic
/// application whose constructor is such a nominal. This deliberately does
/// not accept a source spelling, enum, pointer, or unrelated type variable.
fn is_cxx_record_owner(builder: &IrBuilder, owner: TypeId) -> bool {
    let Some(TypeExpr::Concrete(owner)) = builder.types.get(owner) else {
        return false;
    };
    let nominal = match owner {
        ConcreteType::Nominal(entity) => Some(entity),
        ConcreteType::Applied { constructor, .. } => match builder.types.get(constructor) {
            Some(TypeExpr::Concrete(ConcreteType::Nominal(entity))) => Some(entity),
            _ => None,
        },
        _ => None,
    };
    nominal.is_some_and(|entity| {
        builder
            .items
            .kinds
            .get(entity.index())
            .is_some_and(|kind| *kind == ItemKind::Record)
    })
}

fn validate_computed_type(builder: &IrBuilder, ty: ComputedType) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match ty {
        ComputedType::KeyOf(ty) | ComputedType::Awaited(ty) => {
            id(ty, type_len, SemanticSpace::Type)
        }
        ComputedType::TypeOf(query) => match query {
            TypeQuery::Entity(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
            TypeQuery::Path(path) => {
                for component in
                    list_or_dangling(&builder.atom_lists, path, SemanticSpace::AtomList)?
                {
                    atom(builder, *component)?;
                }
                Ok(())
            }
            TypeQuery::External(external) => id(
                external,
                builder.externals.as_slice().len(),
                SemanticSpace::External,
            ),
        },
        ComputedType::IndexedAccess { object, index } => {
            id(object, type_len, SemanticSpace::Type)?;
            id(index, type_len, SemanticSpace::Type)
        }
        ComputedType::Conditional {
            check,
            extends,
            then_type,
            else_type,
            ..
        } => {
            for ty in [check, extends, then_type, else_type] {
                id(ty, type_len, SemanticSpace::Type)?;
            }
            Ok(())
        }
        ComputedType::Mapped {
            parameter,
            constraint,
            name_as,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(constraint, type_len, SemanticSpace::Type)?;
            optional_id(name_as, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ComputedType::Infer {
            parameter,
            constraint,
        } => {
            atom(builder, parameter)?;
            optional_id(constraint, type_len, SemanticSpace::Type)
        }
        ComputedType::TemplateLiteral(parts) => {
            for part in
                list_or_dangling(&builder.template_parts, parts, SemanticSpace::TemplateParts)?
            {
                match *part {
                    TemplatePart::Bytes(bytes) => atom(builder, bytes)?,
                    TemplatePart::Placeholder(ty) => id(ty, type_len, SemanticSpace::Type)?,
                }
            }
            Ok(())
        }
        ComputedType::Import {
            specifier,
            qualifier,
            arguments,
        } => {
            atom(builder, specifier)?;
            for component in
                list_or_dangling(&builder.atom_lists, qualifier, SemanticSpace::AtomList)?
            {
                atom(builder, *component)?;
            }
            validate_type_list(builder, arguments)
        }
        ComputedType::This => Ok(()),
    }
}

fn validate_object_member(builder: &IrBuilder, member: ObjectMember) -> Result<(), BuildError> {
    let type_len = builder.types.len();
    match member {
        ObjectMember::Property { key, ty, .. } => {
            validate_property_key(builder, key)?;
            id(ty, type_len, SemanticSpace::Type)
        }
        ObjectMember::Method { key, signature, .. } => {
            validate_property_key(builder, key)?;
            id(signature, type_len, SemanticSpace::Type)
        }
        ObjectMember::Index {
            parameter,
            key,
            value,
            ..
        } => {
            atom(builder, parameter)?;
            id(key, type_len, SemanticSpace::Type)?;
            id(value, type_len, SemanticSpace::Type)
        }
        ObjectMember::Call(signature) | ObjectMember::Construct(signature) => {
            id(signature, type_len, SemanticSpace::Type)
        }
    }
}

fn validate_property_key(builder: &IrBuilder, key: PropertyKey) -> Result<(), BuildError> {
    match key {
        PropertyKey::Named(atom_id)
        | PropertyKey::Private(atom_id)
        | PropertyKey::Numeric(atom_id) => atom(builder, atom_id),
        PropertyKey::Computed(ty) => id(ty, builder.types.len(), SemanticSpace::Type),
    }
}

fn validate_type_list(builder: &IrBuilder, list: TypeListId) -> Result<(), BuildError> {
    for ty in list_or_dangling(&builder.type_lists, list, SemanticSpace::TypeList)? {
        id(*ty, builder.types.len(), SemanticSpace::Type)?;
    }
    Ok(())
}

fn validate_doc(builder: &IrBuilder, doc: DocFragment) -> Result<(), BuildError> {
    match doc {
        DocFragment::Text(text) | DocFragment::Code(text) => text_id(builder, text),
        DocFragment::Link { label, target } => {
            text_id(builder, label)?;
            validate_target(builder, target)
        }
        DocFragment::SoftBreak | DocFragment::HardBreak => Ok(()),
    }
}

fn validate_target(builder: &IrBuilder, target: LinkTarget) -> Result<(), BuildError> {
    match target {
        LinkTarget::Local(entity) => id(entity, builder.items.len(), SemanticSpace::Entity),
        LinkTarget::External(external) => id(
            external,
            builder.externals.as_slice().len(),
            SemanticSpace::External,
        ),
    }
}

fn atom(builder: &IrBuilder, atom: AtomId) -> Result<(), BuildError> {
    builder
        .atoms
        .get(atom)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Atom,
            raw: atom.raw,
        })
}

fn text_id(builder: &IrBuilder, text: TextId) -> Result<(), BuildError> {
    builder
        .atoms
        .text(text)
        .map(|_| ())
        .ok_or(BuildError::Dangling {
            space: SemanticSpace::Text,
            raw: text.raw,
        })
}

fn id<T: Copy>(id: DenseId<T>, len: usize, space: SemanticSpace) -> Result<(), BuildError> {
    (id.index() < len)
        .then_some(())
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}

fn optional_id<T: Copy>(
    id_: Option<DenseId<T>>,
    len: usize,
    space: SemanticSpace,
) -> Result<(), BuildError> {
    id_.map_or(Ok(()), |id_| id(id_, len, space))
}

fn list_or_dangling<T: Copy + Eq + Hash>(
    lists: &ListInterner<T>,
    id: ListId<T>,
    space: SemanticSpace,
) -> Result<&[T], BuildError> {
    lists
        .get(id)
        .ok_or(BuildError::Dangling { space, raw: id.raw })
}

struct IrIndices {
    _slab: Slab,
    instances: RawColumn<EntityId>,
    kind: RawColumn<EntityId>,
    name: RawColumn<EntityId>,
    canonical_links: RawColumn<LinkId>,
    outgoing: RawColumn<LinkId>,
    outgoing_offsets: RawColumn<u32>,
    incoming: RawColumn<LinkId>,
    incoming_offsets: RawColumn<u32>,
    occurrence_outgoing: RawColumn<LinkOccurrenceId>,
    occurrence_outgoing_offsets: RawColumn<u32>,
}

impl IrIndices {
    fn build(
        items: &ItemColumns,
        versions: &[EntityVersion],
        atoms: &AtomInterner,
        links: &PackedLinks,
        link_occurrences: &PackedLinkOccurrences,
        externals: &[ExternalTarget],
    ) -> Result<(Self, [u32; 16]), BuildError> {
        let entity_count = items.len();
        let link_count = links.len();
        let occurrence_count = link_occurrences.len();
        let local_links = (0..link_count)
            .filter(|raw| {
                links
                    .get(LinkId::new(*raw as u32))
                    .is_some_and(|link| matches!(link.target, LinkTarget::Local(_)))
            })
            .count();
        let mut plan = SlabPlan::default();
        let instances = plan.column::<EntityId>(entity_count);
        let kind = plan.column::<EntityId>(entity_count);
        let name = plan.column::<EntityId>(entity_count);
        let canonical_links = plan.column::<LinkId>(link_count);
        let outgoing = plan.column::<LinkId>(link_count);
        let outgoing_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let incoming = plan.column::<LinkId>(local_links);
        let incoming_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let occurrence_outgoing = plan.column::<LinkOccurrenceId>(occurrence_count);
        let occurrence_outgoing_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let slab = plan.allocate();
        let mut indices = Self {
            instances: slab.bind(instances),
            kind: slab.bind(kind),
            name: slab.bind(name),
            canonical_links: slab.bind(canonical_links),
            outgoing: slab.bind(outgoing),
            outgoing_offsets: slab.bind(outgoing_offsets),
            incoming: slab.bind(incoming),
            incoming_offsets: slab.bind(incoming_offsets),
            occurrence_outgoing: slab.bind(occurrence_outgoing),
            occurrence_outgoing_offsets: slab.bind(occurrence_outgoing_offsets),
            _slab: slab,
        };
        for raw in 0..entity_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::CapacitySpace::Value,
                actual: raw,
            })?;
            let id = EntityId::new(raw);
            indices.instances.push(id);
            indices.kind.push(id);
            indices.name.push(id);
        }
        for raw in 0..link_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::CapacitySpace::Value,
                actual: raw,
            })?;
            let id = LinkId::new(raw);
            indices.canonical_links.push(id);
            indices.outgoing.push(id);
            if links
                .get(id)
                .is_some_and(|link| matches!(link.target, LinkTarget::Local(_)))
            {
                indices.incoming.push(id);
            }
        }
        for raw in 0..occurrence_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::CapacitySpace::Value,
                actual: raw,
            })?;
            indices.occurrence_outgoing.push(LinkOccurrenceId::new(raw));
        }
        for _ in 0..=entity_count {
            indices.outgoing_offsets.push(0);
            indices.incoming_offsets.push(0);
            indices.occurrence_outgoing_offsets.push(0);
        }

        indices
            .instances
            .as_mut_slice()
            .sort_unstable_by_key(|id| versions[id.index()].identity());
        for pair in indices.instances.windows(2) {
            if versions[pair[0].index()].identity() == versions[pair[1].index()].identity() {
                return Err(BuildError::DuplicateDeclarationIdentity {
                    identity: versions[pair[0].index()].identity(),
                });
            }
        }

        indices
            .kind
            .as_mut_slice()
            .sort_unstable_by_key(|id| (items.kinds[id.index()], versions[id.index()].identity()));
        let mut kind_offsets = [0_u32; 16];
        for kind in items.kinds.iter() {
            kind_offsets[*kind as usize + 1] += 1;
        }
        prefix_sum(&mut kind_offsets);

        indices.name.as_mut_slice().sort_unstable_by(|left, right| {
            let left_name = atoms.get(items.names[left.index()]).unwrap_or(&[]);
            let right_name = atoms.get(items.names[right.index()]).unwrap_or(&[]);
            (left_name, versions[left.index()].identity())
                .cmp(&(right_name, versions[right.index()].identity()))
        });
        for id in indices.canonical_links.iter() {
            let link = links
                .get(*id)
                .expect("canonical IDs originate from packed links");
            if let LinkTarget::External(external) = link.target {
                externals
                    .get(external.index())
                    .ok_or(BuildError::Dangling {
                        space: SemanticSpace::External,
                        raw: external.raw,
                    })?;
            }
        }
        indices.canonical_links.as_mut_slice().sort_unstable_by_key(|id| {
                let link = links
                    .get(*id)
                    .expect("canonical IDs originate from packed links");
                let from = versions[link.from.index()].identity();
                let target = match link.target {
                    LinkTarget::Local(entity) => {
                        DeclarationLinkTarget::Local(versions[entity.index()].identity())
                    }
                    LinkTarget::External(external) => declaration_link_target(
                        *externals
                            .get(external.index())
                            .expect("external coordinate was validated before canonical sorting"),
                    ),
                };
                (from, target, link.kind)
            });
        sort_adjacency(
            links,
            indices.outgoing.as_mut_slice(),
            indices.outgoing_offsets.as_mut_slice(),
            false,
        );
        sort_adjacency(
            links,
            indices.incoming.as_mut_slice(),
            indices.incoming_offsets.as_mut_slice(),
            true,
        );
        sort_occurrence_adjacency(
            links,
            link_occurrences,
            indices.occurrence_outgoing.as_mut_slice(),
            indices.occurrence_outgoing_offsets.as_mut_slice(),
        );
        Ok((indices, kind_offsets))
    }
}

fn declaration_link_target(target: ExternalTarget) -> DeclarationLinkTarget {
    match target {
        ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
        ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
        ExternalTarget::FragmentEntity { target, .. } => DeclarationLinkTarget::FragmentEntity(target),
    }
}

fn prefix_sum(values: &mut [u32]) {
    for index in 1..values.len() {
        values[index] += values[index - 1];
    }
}

fn sort_adjacency(links: &PackedLinks, order: &mut [LinkId], offsets: &mut [u32], reverse: bool) {
    order.sort_unstable_by_key(|link_id| {
        let link = links
            .get(*link_id)
            .expect("adjacency IDs originate from packed links");
        if reverse {
            let target = match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => u32::MAX,
            };
            (target, 0, link.from.raw, link.kind, link_id.raw)
        } else {
            let (external, target) = match link.target {
                LinkTarget::Local(target) => (0, target.raw),
                LinkTarget::External(target) => (1, target.raw),
            };
            (link.from.raw, external, target, link.kind, link_id.raw)
        }
    });
    for link_id in order.iter().copied() {
        let link = links
            .get(link_id)
            .expect("adjacency IDs originate from packed links");
        let raw = if reverse {
            match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => continue,
            }
        } else {
            link.from.raw
        };
        offsets[raw as usize + 1] += 1;
    }
    prefix_sum(offsets);
}

/// Sorts every authority-observed occurrence by owner and canonical relation
/// without collapsing sites. Fully equal sites are intentionally an unordered
/// multiset: no emission ordinal enters a canonical storage key.
fn sort_occurrence_adjacency(
    links: &PackedLinks,
    occurrences: &PackedLinkOccurrences,
    order: &mut [LinkOccurrenceId],
    offsets: &mut [u32],
) {
    order.sort_unstable_by_key(|occurrence_id| {
        let occurrence = occurrences
            .get(*occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        let (external, target) = match relation.target {
            LinkTarget::Local(target) => (0, target.raw),
            LinkTarget::External(target) => (1, target.raw),
        };
        (
            relation.from.raw,
            external,
            target,
            relation.kind,
            occurrence.source.map_or(u32::MAX, |span| span.file().raw),
            occurrence.source.map_or(u32::MAX, SourceSpan::start),
            occurrence.source.map_or(u32::MAX, SourceSpan::end),
        )
    });
    for occurrence_id in order.iter().copied() {
        let occurrence = occurrences
            .get(occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        offsets[relation.from.index() + 1] += 1;
    }
    prefix_sum(offsets);
}

/// Dense entity column family. Every slice has the same length and uses
/// [`EntityId`] as its direct array coordinate.
#[derive(Clone, Copy, Debug)]
pub struct EntityColumns<'ir> {
    pub names: &'ir [AtomId],
    pub kinds: &'ir [ItemKind],
    pub visibility: &'ir [Visibility],
    pub parents: &'ir [OptionalId<crate::Entity>],
    pub semantic_types: &'ir [OptionalId<Type>],
    pub members: &'ir [EntityListId],
    pub docs: &'ir [DocId],
    pub attributes: &'ir [AtomListId],
}

/// Cold source column family aligned one-for-one with [`EntityColumns`].
#[derive(Clone, Copy, Debug)]
pub struct SourceColumnsView<'ir> {
    /// File lane, empty when every row has no source.
    pub files: &'ir [OptionalId<crate::AtomSpace>],
    /// Start lane, empty when every row has no source.
    pub starts: &'ir [u32],
    /// End lane, empty when every row has no source.
    pub ends: &'ir [u32],
    rows: usize,
}

impl SourceColumnsView<'_> {
    /// Number of logical entity rows, including universal absence.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.rows
    }
}

/// Precomputed graph columns. Consumers traverse CSR slices directly; no
/// projection, sorting, or adjacency construction occurs when this view is made.
#[derive(Clone, Copy, Debug)]
pub struct GraphColumns<'ir> {
    /// Canonical relation rows, unique by `(from, target, kind)`.
    pub from: &'ir [EntityId],
    pub targets: &'ir [LinkTarget],
    pub kinds: &'ir [LinkKind],
    pub confidence: &'ir [Confidence],
    pub sources: SparseColumnView<'ir, SourceSpan>,
    pub outgoing: &'ir [LinkId],
    pub outgoing_offsets: &'ir [u32],
    pub incoming: &'ir [LinkId],
    pub incoming_offsets: &'ir [u32],
    /// Every observed source occurrence, including repeated sites for one
    /// canonical relation. Its CSR index is keyed by the relation's `from`.
    pub occurrences: LinkOccurrenceColumns<'ir>,
}

/// Typed dense columns for authority-observed graph source sites.
#[derive(Clone, Copy, Debug)]
pub struct LinkOccurrenceColumns<'ir> {
    pub links: &'ir [LinkId],
    pub confidence: &'ir [Confidence],
    pub sources: SparseColumnView<'ir, SourceSpan>,
    pub outgoing: &'ir [LinkOccurrenceId],
    pub outgoing_offsets: &'ir [u32],
}

struct PackedLinks {
    _slab: Slab,
    from: RawColumn<EntityId>,
    targets: RawColumn<LinkTarget>,
    kinds: RawColumn<LinkKind>,
    confidence: RawColumn<Confidence>,
    sources: SparseColumn<SourceSpan>,
}

/// Compact site-evidence plane parallel to canonical graph links.
///
/// This is deliberately not interned: two equal source spans are still two
/// independent authority observations if they were emitted separately.
struct PackedLinkOccurrences {
    _slab: Slab,
    links: RawColumn<LinkId>,
    confidence: RawColumn<Confidence>,
    sources: SparseColumn<SourceSpan>,
}

impl Default for PackedLinkOccurrences {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl PackedLinkOccurrences {
    fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let links = plan.column::<LinkId>(capacity);
        let confidence = plan.column::<Confidence>(capacity);
        let slab = plan.allocate();
        Self {
            links: slab.bind(links),
            confidence: slab.bind(confidence),
            _slab: slab,
            sources: SparseColumn::default(),
        }
    }

    fn len(&self) -> usize {
        self.links.len()
    }

    fn reserve_exact(&mut self, additional: usize, source_values: usize) {
        let required = self.len().saturating_add(additional);
        if required > self.links.capacity() {
            let mut next = Self::with_capacity(required);
            next.links.extend_from_slice(&self.links);
            next.confidence.extend_from_slice(&self.confidence);
            core::mem::swap(&mut next.sources, &mut self.sources);
            *self = next;
        }
        self.sources.reserve(required, source_values);
    }

    fn reserve_one(&mut self, source_values: usize) {
        if self.len() < self.links.capacity() {
            self.sources
                .reserve(self.len().saturating_add(1), source_values);
            return;
        }
        let target = self.links.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()), source_values);
    }

    fn push(&mut self, occurrence: LinkOccurrence) -> Result<(), CapacityError> {
        self.links.push(occurrence.link);
        self.confidence.push(occurrence.confidence);
        self.sources.push(occurrence.source)
    }

    fn get(&self, id: LinkOccurrenceId) -> Option<LinkOccurrence> {
        let index = id.index();
        Some(LinkOccurrence {
            link: *self.links.get(index)?,
            confidence: *self.confidence.get(index)?,
            source: self.sources.get_index(index).copied(),
        })
    }

    fn view(&self) -> LinkOccurrenceColumns<'_> {
        LinkOccurrenceColumns {
            links: &self.links,
            confidence: &self.confidence,
            sources: self.sources.view(),
            outgoing: &[],
            outgoing_offsets: &[],
        }
    }
}

impl Default for PackedLinks {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl PackedLinks {
    fn with_capacity(capacity: usize) -> Self {
        let mut plan = SlabPlan::default();
        let from = plan.column::<EntityId>(capacity);
        let targets = plan.column::<LinkTarget>(capacity);
        let kinds = plan.column::<LinkKind>(capacity);
        let confidence = plan.column::<Confidence>(capacity);
        let slab = plan.allocate();
        Self {
            from: slab.bind(from),
            targets: slab.bind(targets),
            kinds: slab.bind(kinds),
            confidence: slab.bind(confidence),
            _slab: slab,
            sources: SparseColumn::default(),
        }
    }

    fn len(&self) -> usize {
        self.from.len()
    }

    fn reserve_exact(&mut self, additional: usize, source_values: usize) {
        let required = self.len().saturating_add(additional);
        if required > self.from.capacity() {
            let mut next = Self::with_capacity(required);
            next.from.extend_from_slice(&self.from);
            next.targets.extend_from_slice(&self.targets);
            next.kinds.extend_from_slice(&self.kinds);
            next.confidence.extend_from_slice(&self.confidence);
            core::mem::swap(&mut next.sources, &mut self.sources);
            *self = next;
        }
        self.sources.reserve(required, source_values);
    }

    fn reserve_one(&mut self, source_values: usize) {
        if self.len() < self.from.capacity() {
            self.sources
                .reserve(self.len().saturating_add(1), source_values);
            return;
        }
        let target = self.from.capacity().saturating_mul(2).max(8);
        self.reserve_exact(target.saturating_sub(self.len()), source_values);
    }

    fn push(&mut self, link: Link) -> Result<(), CapacityError> {
        self.from.push(link.from);
        self.targets.push(link.target);
        self.kinds.push(link.kind);
        self.confidence.push(link.confidence);
        self.sources.push(link.source)
    }

    fn replace(&mut self, id: LinkId, link: Link) -> Result<(), CapacityError> {
        let index = id.index();
        let Some(from) = self.from.as_mut_slice().get_mut(index) else {
            return Err(CapacityError {
                space: crate::CapacitySpace::Value,
                actual: index,
            });
        };
        *from = link.from;
        self.targets.as_mut_slice()[index] = link.target;
        self.kinds.as_mut_slice()[index] = link.kind;
        self.confidence.as_mut_slice()[index] = link.confidence;
        self.sources.set(index, link.source)
    }

    fn get(&self, id: LinkId) -> Option<Link> {
        let index = id.index();
        Some(Link {
            from: *self.from.get(index)?,
            target: *self.targets.get(index)?,
            kind: *self.kinds.get(index)?,
            confidence: *self.confidence.get(index)?,
            source: self.sources.get_index(index).copied(),
        })
    }

    fn view<'ir>(
        &'ir self,
        occurrences: LinkOccurrenceColumns<'ir>,
    ) -> GraphColumns<'ir> {
        GraphColumns {
            from: &self.from,
            targets: &self.targets,
            kinds: &self.kinds,
            confidence: &self.confidence,
            sources: self.sources.view(),
            outgoing: &[],
            outgoing_offsets: &[],
            incoming: &[],
            incoming_offsets: &[],
            occurrences,
        }
    }
}

/// Precomputed stable-order columns consumed directly by IR-VCS and storage.
#[derive(Clone, Copy, Debug)]
pub struct VcsColumns<'ir> {
    pub versions: &'ir [EntityVersion],
    /// Canonical exact local instance order, by `(family, variant)`.
    pub declaration_instances: &'ir [EntityId],
    pub stable_links: &'ir [LinkId],
}

/// Complete pointer-and-length storage view of the canonical semantic image.
///
/// This is a manifest of the actual compiler-owned columns, not a wire model.
/// In-process caches, pagers, renderers, and derived-index builders can retain
/// or scatter/gather these exact slices without serializing semantic rows.
#[derive(Clone, Copy, Debug)]
pub struct StorageColumns<'ir> {
    pub authority: SemanticImageAuthority,
    /// Image-level source, recipe, and scope authority when compiled.
    pub provenance: ImageProvenance,
    pub atoms: AtomTableView<'ir>,
    pub types: TypeColumns<'ir>,
    pub externals: &'ir [ExternalTarget],
    pub type_lists: ListTableView<'ir, TypeId>,
    pub entity_lists: ListTableView<'ir, EntityId>,
    pub atom_lists: ListTableView<'ir, AtomId>,
    pub docs: ListTableView<'ir, DocFragment>,
    pub tuple_elements: ListTableView<'ir, TupleElement>,
    pub object_members: ListTableView<'ir, ObjectMember>,
    pub template_parts: ListTableView<'ir, TemplatePart>,
    pub type_parameter_bounds: ListTableView<'ir, TypeParameterBound>,
    pub type_parameters: ListTableView<'ir, TypeParameter>,
    pub entities: EntityColumns<'ir>,
    /// Cold authority facts aligned exactly with `entities`.
    pub entity_authority: EntityAuthorityColumns<'ir>,
    /// Cold source availability aligned exactly with graph occurrences.
    pub occurrence_authority: OccurrenceAuthorityColumns<'ir>,
    pub sources: SourceColumnsView<'ir>,
    pub language_extensions: LanguageExtensionsView<'ir>,
    pub graph: GraphColumns<'ir>,
    pub vcs: VcsColumns<'ir>,
    pub kind_entities: &'ir [EntityId],
    pub kind_offsets: &'ir [u32; 16],
    pub name_entities: &'ir [EntityId],
}

impl StorageColumns<'_> {
    /// Exact logical bytes occupied by initialized canonical column elements.
    ///
    /// Vector capacities and allocator metadata are intentionally excluded;
    /// this is the representation payload a pager, capacity benchmark, or
    /// scatter/gather storage owner would retain.
    #[must_use]
    pub fn resident_bytes(self) -> usize {
        let mut total = 0_usize;
        macro_rules! add {
            ($slice:expr) => {
                total = total.saturating_add(core::mem::size_of_val($slice));
            };
        }
        add!(self.atoms.bytes);
        add!(self.atoms.ranges);
        add!(self.types.headers);
        add!(self.types.pairs);
        add!(self.types.triples);
        add!(self.types.quads);
        add!(self.externals);
        add!(self.type_lists.elements);
        add!(self.type_lists.ranges);
        add!(self.entity_lists.elements);
        add!(self.entity_lists.ranges);
        add!(self.atom_lists.elements);
        add!(self.atom_lists.ranges);
        add!(self.docs.elements);
        add!(self.docs.ranges);
        add!(self.tuple_elements.elements);
        add!(self.tuple_elements.ranges);
        add!(self.object_members.elements);
        add!(self.object_members.ranges);
        add!(self.template_parts.elements);
        add!(self.template_parts.ranges);
        add!(self.type_parameter_bounds.elements);
        add!(self.type_parameter_bounds.ranges);
        add!(self.type_parameters.elements);
        add!(self.type_parameters.ranges);
        add!(self.entities.names);
        add!(self.entities.kinds);
        add!(self.entities.visibility);
        add!(self.entities.parents);
        add!(self.entities.semantic_types);
        add!(self.entities.members);
        add!(self.entities.docs);
        add!(self.entities.attributes);
        add!(self.entity_authority.parentage);
        add!(self.entity_authority.source);
        add!(self.entity_authority.source_file);
        add!(self.entity_authority.members);
        add!(self.entity_authority.semantic_type);
        add!(self.entity_authority.documentation);
        add!(self.entity_authority.visibility);
        add!(self.entity_authority.attributes);
        add!(self.entity_authority.language_extension);
        add!(self.occurrence_authority.source);
        add!(self.sources.files);
        add!(self.sources.starts);
        add!(self.sources.ends);
        add!(self.language_extensions.typescript.ids.ordinals());
        add!(self.language_extensions.typescript.ids.values());
        add!(self.language_extensions.typescript.facts);
        add!(self.language_extensions.csharp.ids.ordinals());
        add!(self.language_extensions.csharp.ids.values());
        add!(self.language_extensions.csharp.facts);
        add!(self.language_extensions.go.ids.ordinals());
        add!(self.language_extensions.go.ids.values());
        add!(self.language_extensions.go.facts);
        add!(self.language_extensions.rust.ids.ordinals());
        add!(self.language_extensions.rust.ids.values());
        add!(self.language_extensions.rust.facts);
        add!(self.language_extensions.python.ids.ordinals());
        add!(self.language_extensions.python.ids.values());
        add!(self.language_extensions.python.facts);
        add!(self.language_extensions.java.ids.ordinals());
        add!(self.language_extensions.java.ids.values());
        add!(self.language_extensions.java.facts);
        add!(self.language_extensions.clang.ids.ordinals());
        add!(self.language_extensions.clang.ids.values());
        add!(self.language_extensions.clang.facts);
        add!(self.graph.from);
        add!(self.graph.targets);
        add!(self.graph.kinds);
        add!(self.graph.confidence);
        add!(self.graph.sources.ordinals());
        add!(self.graph.sources.values());
        add!(self.graph.outgoing);
        add!(self.graph.outgoing_offsets);
        add!(self.graph.incoming);
        add!(self.graph.incoming_offsets);
        add!(self.graph.occurrences.links);
        add!(self.graph.occurrences.confidence);
        add!(self.graph.occurrences.sources.ordinals());
        add!(self.graph.occurrences.sources.values());
        add!(self.graph.occurrences.outgoing);
        add!(self.graph.occurrences.outgoing_offsets);
        add!(self.vcs.versions);
        add!(self.vcs.declaration_instances);
        add!(self.vcs.stable_links);
        add!(self.kind_entities);
        add!(self.kind_offsets);
        add!(self.name_entities);
        total
    }
}

/// Immutable condensed semantic IR.
pub struct Ir {
    authority: SemanticImageAuthority,
    provenance: ImageProvenance,
    atoms: AtomTable,
    types: PackedTypes,
    externals: Vec<ExternalTarget>,
    type_lists: ListTable<TypeId>,
    entity_lists: ListTable<EntityId>,
    atom_lists: ListTable<AtomId>,
    docs: ListTable<DocFragment>,
    tuple_elements: ListTable<TupleElement>,
    object_members: ListTable<ObjectMember>,
    template_parts: ListTable<TemplatePart>,
    type_parameter_bounds: ListTable<TypeParameterBound>,
    type_parameters: ListTable<TypeParameter>,
    items: ItemColumns,
    authority_facts: AuthorityColumns,
    sources: SourceColumns,
    extensions: LanguageExtensions,
    indices: IrIndices,
    kind_offsets: [u32; 16],
    links: PackedLinks,
    link_occurrences: PackedLinkOccurrences,
    occurrence_authority: OccurrenceAuthorityColumn,
}

impl Ir {
    /// Number of rows shared by every entity-aligned column family.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.items.len()
    }

    /// Returns the image-level source, recipe, and scope authority when this
    /// image was built by one compiled authority transaction.
    #[must_use]
    pub const fn image_provenance(&self) -> ImageProvenance {
        self.provenance
    }

    /// Borrows cold per-entity authority facts aligned with the entity rows.
    #[must_use]
    pub fn entity_authority_columns(&self) -> EntityAuthorityColumns<'_> {
        self.authority_facts.view()
    }

    /// Borrows cold authority source availability aligned with every observed
    /// graph occurrence. The current two-state lane is exact: `Captured`
    /// means `LinkOccurrence::source` is present, while `Unavailable` means
    /// no source site was supplied.
    #[must_use]
    pub fn occurrence_authority_columns(&self) -> OccurrenceAuthorityColumns<'_> {
        self.occurrence_authority.view()
    }

    /// Borrows every canonical column as typed slices, with no semantic
    /// conversion, hashing, directory construction, or allocation.
    #[must_use]
    pub fn storage_columns(&self) -> StorageColumns<'_> {
        StorageColumns {
            authority: self.authority,
            provenance: self.provenance,
            atoms: self.atoms.view(),
            types: self.types.columns(),
            externals: &self.externals,
            type_lists: self.type_lists.view(),
            entity_lists: self.entity_lists.view(),
            atom_lists: self.atom_lists.view(),
            docs: self.docs.view(),
            tuple_elements: self.tuple_elements.view(),
            object_members: self.object_members.view(),
            template_parts: self.template_parts.view(),
            type_parameter_bounds: self.type_parameter_bounds.view(),
            type_parameters: self.type_parameters.view(),
            entities: self.entity_columns(),
            entity_authority: self.entity_authority_columns(),
            occurrence_authority: self.occurrence_authority_columns(),
            sources: self.source_columns(),
            language_extensions: self.language_extensions(),
            graph: self.graph_columns(),
            vcs: self.vcs_columns(),
            kind_entities: &self.indices.kind,
            kind_offsets: &self.kind_offsets,
            name_entities: &self.indices.name,
        }
    }

    /// Borrows the dense entity SoA with no projection or validation work.
    #[must_use]
    pub fn entity_columns(&self) -> EntityColumns<'_> {
        EntityColumns {
            names: &self.items.names,
            kinds: &self.items.kinds,
            visibility: &self.items.visibility,
            parents: &self.items.parents,
            semantic_types: &self.items.semantic_types,
            members: &self.items.members,
            docs: &self.items.docs,
            attributes: &self.items.attributes,
        }
    }

    /// Borrows cold source lanes without touching them during ordinary scans.
    #[must_use]
    pub fn source_columns(&self) -> SourceColumnsView<'_> {
        SourceColumnsView {
            files: &self.sources.files,
            starts: &self.sources.starts,
            ends: &self.sources.ends,
            rows: self.sources.rows,
        }
    }

    /// Borrows precomputed graph tables and both CSR directions.
    #[must_use]
    pub fn graph_columns(&self) -> GraphColumns<'_> {
        let mut columns = self.links.view(self.link_occurrences.view());
        columns.outgoing = &self.indices.outgoing;
        columns.outgoing_offsets = &self.indices.outgoing_offsets;
        columns.incoming = &self.indices.incoming;
        columns.incoming_offsets = &self.indices.incoming_offsets;
        columns.occurrences.outgoing = &self.indices.occurrence_outgoing;
        columns.occurrences.outgoing_offsets = &self.indices.occurrence_outgoing_offsets;
        columns
    }

    /// Borrows the exact stable-order lanes already used by IR-VCS.
    #[must_use]
    pub fn vcs_columns(&self) -> VcsColumns<'_> {
        VcsColumns {
            versions: &self.items.versions,
            declaration_instances: &self.indices.instances,
            stable_links: &self.indices.canonical_links,
        }
    }

    #[must_use]
    pub fn atom(&self, id: AtomId) -> Option<&[u8]> {
        self.atoms.get(id)
    }
    #[must_use]
    pub fn text(&self, id: TextId) -> Option<&str> {
        self.atoms.text(id)
    }
    #[must_use]
    pub fn ty(&self, id: TypeId) -> Option<TypeExpr> {
        self.types.get(id)
    }
    #[must_use]
    pub fn typed_type<State: TypeState>(&self, id: TypedTypeId<State>) -> Option<State::Node> {
        State::project(self.ty(id.erase())?)
    }
    #[must_use]
    pub fn types(&self, id: TypeListId) -> Option<&[TypeId]> {
        self.type_lists.get(id)
    }
    #[must_use]
    pub fn atom_list(&self, id: AtomListId) -> Option<&[AtomId]> {
        self.atom_lists.get(id)
    }
    /// Borrows one canonical local-member list by its typed pool coordinate.
    #[must_use]
    pub(crate) fn entity_list(&self, id: EntityListId) -> Option<&[EntityId]> {
        self.entity_lists.get(id)
    }
    /// Borrows one canonical documentation list by its typed pool coordinate.
    #[must_use]
    pub(crate) fn documentation(&self, id: DocId) -> Option<&[DocFragment]> {
        self.docs.get(id)
    }
    #[must_use]
    pub fn tuple_elements(&self, id: TupleElementListId) -> Option<&[TupleElement]> {
        self.tuple_elements.get(id)
    }
    #[must_use]
    pub fn object_members(&self, id: ObjectMemberListId) -> Option<&[ObjectMember]> {
        self.object_members.get(id)
    }
    #[must_use]
    pub fn template_parts(&self, id: TemplatePartListId) -> Option<&[TemplatePart]> {
        self.template_parts.get(id)
    }
    #[must_use]
    pub fn type_parameters(&self, id: TypeParameterListId) -> Option<&[TypeParameter]> {
        self.type_parameters.get(id)
    }
    #[must_use]
    pub fn type_parameter_bounds(
        &self,
        id: TypeParameterBoundListId,
    ) -> Option<&[TypeParameterBound]> {
        self.type_parameter_bounds.get(id)
    }
    #[must_use]
    pub fn external(&self, id: ExternalId) -> Option<&ExternalTarget> {
        self.externals.get(id.index())
    }
    #[must_use]
    pub fn version(&self, id: EntityId) -> Option<EntityVersion> {
        self.items.versions.get(id.index()).copied()
    }
    /// Projects every aligned immutable semantic row for one local entity.
    ///
    /// This is the reader boundary's row view: consumers receive the hot
    /// declaration fields, cold authority facts, source truth, and exact
    /// declaration version together rather than joining parallel lanes by
    /// convention.
    #[must_use]
    pub(crate) fn semantic_entity(&self, id: EntityId) -> Option<crate::SemanticEntity> {
        let index = id.index();
        Some(crate::SemanticEntity {
            id,
            name: *self.items.names.get(index)?,
            kind: *self.items.kinds.get(index)?,
            visibility: *self.items.visibility.get(index)?,
            parent: self.items.parents.get(index)?.get(),
            semantic_type: self.items.semantic_types.get(index)?.get(),
            members: *self.items.members.get(index)?,
            docs: *self.items.docs.get(index)?,
            attributes: *self.items.attributes.get(index)?,
            source: self.sources.get(index),
            authority: self.authority_facts.facts(index)?,
            version: *self.items.versions.get(index)?,
        })
    }

    /// Projects only the self-contained portable declaration facts.
    ///
    /// Unlike [`Self::semantic_entity`], this deliberately carries no pooled
    /// coordinate.  A subordinate core image can therefore expose identity,
    /// authority, and source truth without claiming its omitted type/list,
    /// documentation, graph, or extension planes exist.
    pub(crate) fn core_semantic_entity(
        &self,
        id: EntityId,
    ) -> Option<crate::CoreSemanticEntity> {
        let item = self.item(id)?;
        let authority = self.authority_facts.facts(id.index())?;
        Some(crate::CoreSemanticEntity {
            id,
            name: *self.items.names.get(id.index())?,
            kind: item.kind(),
            visibility: item.visibility(),
            parent: item.parent(),
            authority,
            source: item.source(),
            version: item.version(),
        })
    }
    /// Binary-searches the exact canonical declaration-instance index.
    #[must_use]
    pub fn find_declaration(&self, identity: DeclarationIdentity) -> Option<ItemView<'_>> {
        let index = self
            .indices
            .instances
            .binary_search_by_key(&identity, |id| self.items.versions[id.index()].identity())
            .ok()?;
        self.item(self.indices.instances[index])
    }
    /// Iterates every current instance in one declaration family.
    #[must_use]
    pub fn family_items(&self, family: DeclarationFamilyId) -> ItemIdIter<'_> {
        let start = self.indices.instances.partition_point(|id| {
            self.items.versions[id.index()].family < family
        });
        let end = start + self.indices.instances[start..].partition_point(|id| {
            self.items.versions[id.index()].family == family
        });
        ItemIdIter { ir: self, ids: &self.indices.instances[start..end] }
    }
    /// Iterates declarations in canonical exact-instance order.
    #[must_use]
    pub fn canonical_items(&self) -> ItemIdIter<'_> {
        ItemIdIter {
            ir: self,
            ids: &self.indices.instances,
        }
    }
    /// Iterates one declaration kind through its Trustfall-friendly posting index.
    #[must_use]
    pub fn items_of_kind(&self, kind: ItemKind) -> ItemIdIter<'_> {
        let raw = kind as usize;
        let ids = self
            .indices
            .kind
            .get(self.kind_offsets[raw] as usize..self.kind_offsets[raw + 1] as usize)
            .unwrap_or(&[]);
        ItemIdIter { ir: self, ids }
    }
    /// Binary-searches the precomputed raw-byte name index.
    #[must_use]
    pub fn items_named(&self, name: &[u8]) -> ItemIdIter<'_> {
        let start = self
            .indices
            .name
            .partition_point(|id| self.atom(self.items.names[id.index()]).unwrap_or(&[]) < name);
        let end = self.indices.name[start..]
            .partition_point(|id| self.atom(self.items.names[id.index()]).unwrap_or(&[]) == name)
            + start;
        ItemIdIter {
            ir: self,
            ids: &self.indices.name[start..end],
        }
    }
    #[must_use]
    pub fn item(&self, id: EntityId) -> Option<ItemView<'_>> {
        (id.index() < self.items.len()).then_some(ItemView { ir: self, id })
    }
    /// Borrows all typed language-extension planes without widening hot entity rows.
    #[must_use]
    pub fn language_extensions(&self) -> LanguageExtensionsView<'_> {
        self.extensions.view(self.authority)
    }

    /// Produces bounds proved by this validated IR's shared semantic columns.
    #[must_use]
    pub fn language_extension_common_bounds(
        &self,
    ) -> Option<crate::ValidatedLanguageExtensionCommonBounds> {
        let columns = self.storage_columns();
        Some(
            crate::ValidatedLanguageExtensionCommonBounds::from_validated(
                crate::LanguageExtensionCommonBounds {
                    atoms: u32::try_from(columns.atoms.ranges.len()).ok()?,
                    types: u32::try_from(columns.types.headers.len()).ok()?,
                    entities: u32::try_from(columns.entities.names.len()).ok()?,
                    type_lists: u32::try_from(columns.type_lists.ranges.len()).ok()?,
                    entity_lists: u32::try_from(columns.entity_lists.ranges.len()).ok()?,
                    atom_lists: u32::try_from(columns.atom_lists.ranges.len()).ok()?,
                    type_parameters: crate::TypeParameterListBounds::ExactRanges {
                        count: u32::try_from(columns.type_parameters.ranges.len()).ok()?,
                    },
                },
            ),
        )
    }
    #[must_use]
    pub fn items(&self) -> impl ExactSizeIterator<Item = ItemView<'_>> {
        (0..self.items.len()).map(|raw| ItemView {
            ir: self,
            id: EntityId::new(raw as u32),
        })
    }
    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<Link> {
        self.links.get(id)
    }
    /// Returns one authority-observed graph site without widening it into its
    /// deduplicated relation.
    #[must_use]
    pub fn link_occurrence(&self, id: LinkOccurrenceId) -> Option<LinkOccurrence> {
        self.link_occurrences.get(id)
    }
    /// Iterates every source occurrence in authority emission order.
    #[must_use]
    pub fn link_occurrences(&self) -> LinkOccurrenceIter<'_> {
        LinkOccurrenceIter {
            ir: self,
            ids: None,
            next: 0,
            end: self.link_occurrences.len(),
        }
    }
    /// Iterates source occurrences from one entity through the precomputed
    /// occurrence CSR index. This never scans unrelated owners or reparses
    /// the compact occurrence lane.
    #[must_use]
    pub fn link_occurrences_from(&self, entity: EntityId) -> LinkOccurrenceIter<'_> {
        let range = self
            .indices
            .occurrence_outgoing_offsets
            .get(entity.index())
            .zip(self.indices.occurrence_outgoing_offsets.get(entity.index() + 1));
        let ids = range
            .and_then(|(start, end)| {
                self.indices
                    .occurrence_outgoing
                    .get(*start as usize..*end as usize)
            })
            .unwrap_or(&[]);
        LinkOccurrenceIter {
            ir: self,
            ids: Some(ids),
            next: 0,
            end: ids.len(),
        }
    }
    #[must_use]
    pub(crate) fn canonical_link_ids(&self) -> &[LinkId] {
        &self.indices.canonical_links
    }
    #[must_use]
    pub fn links_from(&self, entity: EntityId) -> LinkIter<'_> {
        self.adjacent(entity, false)
    }
    #[must_use]
    pub fn links_to(&self, entity: EntityId) -> LinkIter<'_> {
        self.adjacent(entity, true)
    }
    fn adjacent(&self, entity: EntityId, reverse: bool) -> LinkIter<'_> {
        let (ids, offsets) = if reverse {
            (&self.indices.incoming, &self.indices.incoming_offsets)
        } else {
            (&self.indices.outgoing, &self.indices.outgoing_offsets)
        };
        let range = offsets
            .get(entity.index())
            .zip(offsets.get(entity.index() + 1));
        let ids = range
            .and_then(|(start, end)| ids.get(*start as usize..*end as usize))
            .unwrap_or(&[]);
        LinkIter { ir: self, ids }
    }
}

/// Borrowing entity handle; all child/doc/edge views inherit one IR lifetime.
#[derive(Clone, Copy)]
pub struct ItemView<'ir> {
    ir: &'ir Ir,
    id: EntityId,
}

impl<'ir> ItemView<'ir> {
    #[must_use]
    pub const fn id(self) -> EntityId {
        self.id
    }
    #[must_use]
    pub fn kind(self) -> ItemKind {
        self.ir.items.kinds[self.id.index()]
    }
    #[must_use]
    pub fn visibility(self) -> Visibility {
        self.ir.items.visibility[self.id.index()]
    }
    #[must_use]
    pub fn parent(self) -> Option<EntityId> {
        self.ir.items.parents[self.id.index()].get()
    }
    #[must_use]
    pub fn semantic_type(self) -> Option<TypeId> {
        self.ir.items.semantic_types[self.id.index()].get()
    }
    #[must_use]
    pub fn source(self) -> Option<SourceSpan> {
        self.ir.sources.get(self.id.index())
    }
    #[must_use]
    pub fn version(self) -> EntityVersion {
        self.ir.items.versions[self.id.index()]
    }
    #[must_use]
    pub fn name(self) -> &'ir [u8] {
        self.ir
            .atom(self.ir.items.names[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn members(self) -> &'ir [EntityId] {
        self.ir
            .entity_lists
            .get(self.ir.items.members[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn docs(self) -> &'ir [DocFragment] {
        self.ir
            .docs
            .get(self.ir.items.docs[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn attributes(self) -> &'ir [AtomId] {
        self.ir
            .atom_lists
            .get(self.ir.items.attributes[self.id.index()])
            .unwrap_or(&[])
    }
    #[must_use]
    pub fn links_from(self) -> LinkIter<'ir> {
        self.ir.links_from(self.id)
    }
    /// Iterates every authority-observed outgoing source site, including
    /// repeated uses that share a canonical semantic relation.
    #[must_use]
    pub fn link_occurrences_from(self) -> LinkOccurrenceIter<'ir> {
        self.ir.link_occurrences_from(self.id)
    }
    #[must_use]
    pub fn links_to(self) -> LinkIter<'ir> {
        self.ir.links_to(self.id)
    }
}

/// Exact-size borrowed iterator over graph links.
pub struct LinkIter<'ir> {
    ir: &'ir Ir,
    ids: &'ir [LinkId],
}

impl<'ir> Iterator for LinkIter<'ir> {
    type Item = (LinkId, Link);
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        self.ir.link(*id).map(|link| (*id, link))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for LinkIter<'_> {}
impl core::iter::FusedIterator for LinkIter<'_> {}

/// Exact-size iterator over typed authority-observed graph source sites.
pub struct LinkOccurrenceIter<'ir> {
    ir: &'ir Ir,
    ids: Option<&'ir [LinkOccurrenceId]>,
    next: usize,
    end: usize,
}

impl<'ir> Iterator for LinkOccurrenceIter<'ir> {
    type Item = (LinkOccurrenceId, LinkOccurrence);

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next;
        if index == self.end {
            return None;
        }
        self.next += 1;
        let id = self
            .ids
            .and_then(|ids| ids.get(index).copied())
            .unwrap_or_else(|| LinkOccurrenceId::new(index as u32));
        self.ir.link_occurrence(id).map(|occurrence| (id, occurrence))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for LinkOccurrenceIter<'_> {}
impl core::iter::FusedIterator for LinkOccurrenceIter<'_> {}

/// Exact-size borrowed iterator over entity posting lists.
pub struct ItemIdIter<'ir> {
    ir: &'ir Ir,
    ids: &'ir [EntityId],
}

impl<'ir> Iterator for ItemIdIter<'ir> {
    type Item = ItemView<'ir>;
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        self.ir.item(*id)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for ItemIdIter<'_> {}
impl core::iter::FusedIterator for ItemIdIter<'_> {}
