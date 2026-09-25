use super::packed_types::{ComputedType, ConcreteType};
use crate::ir::{AtomId, DenseId, TypeId};
use core::{fmt, hash::Hash, marker::PhantomData};

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
    pub(super) node: State::Node,
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
    pub(in crate::ir::semantic) const fn proven(erased: TypeId) -> Self {
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
