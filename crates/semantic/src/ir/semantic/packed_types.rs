use super::ids::{
    AtomListId, ExternalId, ObjectMemberListId, TemplatePartListId, TupleElementListId, TypeListId,
};
use super::type_model::{
    BuiltinType, CxxReferenceCategory, Mutability, NativeCharacterRole, TypeColumns, TypeExpr,
    TypeHeader, TypePairPayload, TypeQuadPayload, TypeTag, TypeTriplePayload, UnknownReason,
    UnknownType,
};
use crate::ir::{
    AnnotationKind, AtomId, CapacityError, ChannelDirection, DenseId, EntityId, TypeId,
    interner::{HashIndex, hash},
};
use alloc::vec::Vec;

mod shapes;

pub use shapes::{
    ArrayShape, CallableElementRole, ComputedType, ConcreteType, FreePredicate, LiteralType,
    MappedModifier, ObjectMember, PropertyKey, QualifiedSegments, TemplatePart, TupleElement,
    TupleElementKind, TypeParameter, TypeParameterBound, TypeParameterInference, TypeParameterKind,
    TypeParameterPrimaryRequirement, TypeParameterRequirements, TypeQuery, VariadicForm, Variance,
    WildcardBound,
};

use core::num::NonZeroU16;

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
