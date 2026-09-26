//! Packed type-header decode.
//!
//! Encoding stays on [`super::PackedTypes`]. This module rebuilds a type
//! expression from the eight-byte header and the pair, triple, and quad lanes.

use super::*;

impl PackedTypes {
    /// Rebuilds one semantic type from its packed header and operand lanes.
    pub(super) fn decode(&self, value: TypeHeader) -> Option<TypeExpr> {
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

fn cxx_qualifiers_from(value: u8) -> Option<crate::ir::CvQualifiers> {
    crate::ir::CvQualifiers::try_from(u32::from(value)).ok()
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
