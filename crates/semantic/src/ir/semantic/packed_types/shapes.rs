//! Closed semantic type shapes.
//!
//! Concrete rows are already known. Computed rows are type-level programs
//! that have not been evaluated into one of those shapes.

use core::num::NonZeroU16;

use crate::ir::{AnnotationKind, AtomId, ChannelDirection, EntityId, TypeId};

use super::super::ids::{
    AtomListId, ExternalId, ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeListId,
    TypeParameterBoundListId,
};
use super::super::type_model::{
    BuiltinType, CxxReferenceCategory, Mutability, NativeCharacterRole, TypeTag,
};

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
    CPointer {
        target: TypeId,
    },
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
        qualifiers: crate::ir::CvQualifiers,
    },
    /// An Objective-C block pointer, whose callable/signature target is
    /// structurally distinct from a C pointer. A C declarator dialect owns
    /// its exact `^` placement.
    CBlockPointer {
        target: TypeId,
    },
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
pub type VariadicForm = crate::ir::FunctionVariadicForm;

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

/// One free generic predicate whose subject is not a declared parameter.
///
/// Rust admits predicates on arbitrary written types (`Vec<T>: Clone`,
/// `<T as Trait>::Item: Clone`) and on the implicit `Self` type.  These rows
/// keep the exact subject type and its ordered bound run without inventing a
/// declared parameter for it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FreePredicate {
    /// The predicate subject type.
    pub subject: TypeId,
    /// Ordered bounds in the shared bound lane.
    pub bounds: TypeParameterBoundListId,
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
