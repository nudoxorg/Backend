use super::ids::{
    AtomListId, EntityListId, ExternalId, FreePredicateListId, ObjectMemberListId,
    TemplatePartListId, TupleElementListId, TypeListId, TypeParameterBoundListId,
    TypeParameterListId,
};
use super::type_model::{
    BuiltinType, ComputedState, ConcreteState, CxxReferenceCategory, GuardedType, Mutability,
    NativeCharacterRole, TypeColumns, TypeExpr, TypeHeader, TypePairPayload, TypeQuadPayload,
    TypeState, TypeTag, TypeTriplePayload, TypedTypeId, UnknownReason, UnknownState, UnknownType,
};
use crate::ir::{
    AnnotationKind, AtomId, CapacityError, ChannelDirection, CvQualifiers, DenseId, EntityId,
    TypeId, VariantFingerprint,
    interner::{HashIndex, hash},
};
use alloc::vec::Vec;
use core::{fmt, hash::Hash, num::NonZeroU16};

mod decode;

#[derive(Default)]
pub(super) struct PackedTypes {
    headers: Vec<TypeHeader>,
    pairs: Vec<TypePairPayload>,
    triples: Vec<TypeTriplePayload>,
    quads: Vec<TypeQuadPayload>,
}

impl PackedTypes {
    pub(super) fn reserve(&mut self, additional: usize) {
        self.headers.reserve(additional);
    }

    pub(super) fn pair(&mut self, values: [u32; 2]) -> u32 {
        let ordinal = self.pairs.len() as u32;
        self.pairs.push(TypePairPayload(values));
        ordinal
    }

    pub(super) fn triple(&mut self, values: [u32; 3]) -> u32 {
        let ordinal = self.triples.len() as u32;
        self.triples.push(TypeTriplePayload(values));
        ordinal
    }

    pub(super) fn quad(&mut self, values: [u32; 4]) -> u32 {
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
                ConcreteType::CxxReference { target, category } => {
                    header(TypeTag::CxxReference, category as u8, 0, target.raw)
                }
                ConcreteType::CPointer { target } => header(TypeTag::CPointer, 0, 0, target.raw),
                ConcreteType::CxxMemberPointer { owner, member } => header(
                    TypeTag::CxxMemberPointer,
                    0,
                    0,
                    self.pair([owner.raw, member.raw]),
                ),
                ConcreteType::CQualified { target, qualifiers } => {
                    header(TypeTag::CQualified, u8::from(qualifiers), 0, target.raw)
                }
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
                    WildcardBound::Extends(bound) => header(TypeTag::Wildcard, 1, 0, bound.raw),
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

    pub(super) fn get(&self, id: TypeId) -> Option<TypeExpr> {
        let value = *self.headers.get(id.index())?;
        self.decode(value)
    }

    pub(super) fn columns(&self) -> TypeColumns<'_> {
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
pub(super) struct TypeInterner {
    packed: PackedTypes,
    index: HashIndex,
}

impl TypeInterner {
    pub(super) fn reserve(&mut self, additional: usize) {
        self.packed.reserve(additional);
        self.index.reserve(additional);
    }

    pub(super) fn intern(&mut self, ty: TypeExpr) -> Result<TypeId, CapacityError> {
        let value_hash = hash(&ty);
        if let Some(ordinal) = self.index.find(value_hash, |ordinal| {
            self.packed.get(TypeId::new(ordinal)) == Some(ty)
        }) {
            return Ok(TypeId::new(ordinal));
        }
        let id = TypeId::try_from_index(self.packed.headers.len()).map_err(|_| CapacityError {
            space: crate::ir::CapacitySpace::Value,
            actual: self.packed.headers.len(),
        })?;
        self.packed.push(ty);
        self.index.insert(value_hash, id.raw);
        Ok(id)
    }

    pub(super) fn get(&self, id: TypeId) -> Option<TypeExpr> {
        self.packed.get(id)
    }

    pub(super) fn len(&self) -> usize {
        self.packed.headers.len()
    }

    pub(super) fn freeze(self) -> PackedTypes {
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
                qualifiers: crate::ir::CvQualifiers::new(true, false, false),
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
            TypeExpr::Concrete(ConcreteType::Wildcard(WildcardBound::Extends(TypeId::new(
                25,
            )))),
            TypeExpr::Concrete(ConcreteType::Wildcard(WildcardBound::Super(TypeId::new(
                26,
            )))),
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

impl TypeExpr {
    /// Returns the closed directory class of this semantic type.
    ///
    /// The class is coordinate-free and therefore safe to carry in search
    /// projections that retain a [`TypeId`] only as a route back into the
    /// image that owns it.  Rendering and structural traversal must still use
    /// the owning [`crate::ir::SemanticReader`].
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
