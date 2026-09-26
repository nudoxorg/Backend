//! Inverse grammar for the explicit typed dependency directories.
//!
//! The full image never stores Rust `TypeExpr` memory.  This module decodes
//! the closed, role-bearing rows emitted by `typed::graph` after structural
//! validation.  The same small decoder is used by admission and later by the
//! borrowed reader, which keeps the two interpretations from drifting.

use core::num::NonZeroU16;

use crate::ir::{
    AnnotationKind, ArrayShape, AtomId, AtomListId, BuiltinType, ChannelDirection, ComputedType,
    ConcreteType, CvQualifiers, CxxReferenceCategory, FreePredicate,
    FreePredicateListId, LiteralType, MappedModifier, Mutability, NativeCharacterRole, ObjectMember,
    ObjectMemberListId, PropertyKey,
    QualifiedSegments, TemplatePart, TemplatePartListId, TupleElement, TupleElementKind,
    TupleElementListId, TypeExpr, TypeId, TypeListId, TypeParameter, TypeParameterBound,
    TypeParameterBoundListId, TypeParameterInference, TypeParameterKind, TypeParameterListId,
    TypeParameterPrimaryRequirement, TypeParameterRequirements, TypeQuery, UnknownReason,
    UnknownType, VariadicForm, Variance, WildcardBound,
};

use super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    validate::TypedLayout,
    wire::FullImageLayout,
};

mod cursor;

pub(in crate::ir::semantic_image::full_wire) use cursor::Edges;
pub(crate) use cursor::logical_count;
use cursor::{count, node, shape};

const TYPE: u8 = 0;
const TYPE_LIST: u8 = 1;
const TUPLE_ELEMENTS: u8 = 2;
const OBJECT_MEMBERS: u8 = 3;
const TEMPLATE_PARTS: u8 = 4;
const ATOM_LIST: u8 = 5;
const TYPE_PARAMETERS: u8 = 6;
const TYPE_PARAMETER_BOUNDS: u8 = 7;
const FREE_PREDICATES: u8 = 8;

/// Proves every type/list node has exactly one closed semantic
/// interpretation.  It intentionally performs no recursive descent: all
/// child coordinates were already bounds-checked by structural admission.
pub(crate) fn validate_semantic_nodes(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
) -> Result<(), FullSemanticImageFault> {
    for id in 0..count(typed, TYPE)? {
        let _ = ty(bytes, layout, typed, TypeId::new(id))?;
    }
    for id in 0..count(typed, TYPE_LIST)? {
        validate_type_list(bytes, layout, typed, TypeListId::new(id))?;
    }
    for id in 0..count(typed, TUPLE_ELEMENTS)? {
        validate_tuple_elements(bytes, layout, typed, TupleElementListId::new(id))?;
    }
    for id in 0..count(typed, OBJECT_MEMBERS)? {
        validate_object_members(bytes, layout, typed, ObjectMemberListId::new(id))?;
    }
    for id in 0..count(typed, TEMPLATE_PARTS)? {
        validate_template_parts(bytes, layout, typed, TemplatePartListId::new(id))?;
    }
    for id in 0..count(typed, ATOM_LIST)? {
        validate_atom_list(bytes, layout, typed, AtomListId::new(id))?;
    }
    for id in 0..count(typed, TYPE_PARAMETERS)? {
        validate_type_parameters(bytes, layout, typed, TypeParameterListId::new(id))?;
    }
    for id in 0..count(typed, TYPE_PARAMETER_BOUNDS)? {
        validate_type_parameter_bounds(bytes, layout, typed, TypeParameterBoundListId::new(id))?;
    }
    for id in 0..count(typed, FREE_PREDICATES)? {
        validate_free_predicates(bytes, layout, typed, FreePredicateListId::new(id))?;
    }
    Ok(())
}

pub(crate) fn ty(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TypeId,
) -> Result<TypeExpr, FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TYPE, id.raw)?;
    let tag = edges.scalar(0, 0)?;
    let value = match tag {
        1 => TypeExpr::Concrete(ConcreteType::Builtin(builtin(
            edges.scalar(1, 0)?,
            edges.node,
        )?)),
        2 => TypeExpr::Concrete(ConcreteType::Literal(literal(&mut edges)?)),
        3 => TypeExpr::Concrete(ConcreteType::Nominal(edges.entity(1, 0)?)),
        4 => TypeExpr::Concrete(ConcreteType::External(edges.external(1, 0)?)),
        5 => TypeExpr::Concrete(ConcreteType::Parameter(edges.atom(1, 0)?)),
        6 => TypeExpr::Concrete(ConcreteType::Applied {
            constructor: edges.type_node(1, 0)?,
            arguments: edges.type_list(1, 1)?,
        }),
        7 => TypeExpr::Concrete(ConcreteType::Tuple(edges.tuple_elements(1, 0)?)),
        8 => TypeExpr::Concrete(ConcreteType::Object(edges.object_members(1, 0)?)),
        9 => {
            let parameters = edges.tuple_elements(1, 0)?;
            let results = edges.tuple_elements(1, 1)?;
            let abi = option_atom(&mut edges, 2, 3)?;
            let variadic = variadic(edges.scalar(1, 4)?, edges.node)?;
            let unsafe_ = boolean(edges.scalar(1, 5)?, edges.node)?;
            TypeExpr::Concrete(ConcreteType::Function {
                parameters,
                results,
                abi,
                variadic,
                unsafe_,
            })
        }
        10 => {
            let target = edges.type_node(1, 0)?;
            let mutability = mutability(edges.scalar(1, 1)?, edges.node)?;
            let lifetime = option_atom(&mut edges, 2, 3)?;
            TypeExpr::Concrete(ConcreteType::Reference {
                target,
                mutability,
                lifetime,
            })
        }
        11 => TypeExpr::Concrete(ConcreteType::CxxReference {
            target: edges.type_node(1, 0)?,
            category: cxx_reference(edges.scalar(1, 1)?, edges.node)?,
        }),
        12 => TypeExpr::Concrete(ConcreteType::CPointer {
            target: edges.type_node(1, 0)?,
        }),
        13 => TypeExpr::Concrete(ConcreteType::CxxMemberPointer {
            owner: edges.type_node(1, 0)?,
            member: edges.type_node(1, 1)?,
        }),
        14 => {
            let target = edges.type_node(1, 0)?;
            let raw = narrow_u32(edges.scalar(1, 1)?, edges.node)?;
            let qualifiers = CvQualifiers::try_from(raw).map_err(|_| shape(edges.node))?;
            TypeExpr::Concrete(ConcreteType::CQualified { target, qualifiers })
        }
        15 => TypeExpr::Concrete(ConcreteType::CBlockPointer {
            target: edges.type_node(1, 0)?,
        }),
        16 => TypeExpr::Concrete(ConcreteType::NativeCharacter {
            role: native_character(edges.scalar(1, 0)?, edges.node)?,
            width: nonzero_u16(edges.scalar(1, 1)?, edges.node)?,
        }),
        17 => TypeExpr::Concrete(ConcreteType::Pointer {
            target: edges.type_node(1, 0)?,
            mutability: mutability(edges.scalar(1, 1)?, edges.node)?,
        }),
        18 => TypeExpr::Concrete(ConcreteType::Slice(edges.type_node(1, 0)?)),
        19 => {
            let element = edges.type_node(1, 0)?;
            let shape = match edges.scalar(1, 1)? {
                0 => ArrayShape::Sequence,
                1 => ArrayShape::Rectangular {
                    rank: nonzero_u16(edges.scalar(1, 2)?, edges.node)?,
                },
                2 => ArrayShape::FixedValue {
                    length: edges.scalar(1, 2)?,
                },
                3 => ArrayShape::ConstExpression(edges.atom(1, 2)?),
                4 => ArrayShape::Incomplete,
                _ => return Err(shape(edges.node)),
            };
            TypeExpr::Concrete(ConcreteType::Array { element, shape })
        }
        20 => TypeExpr::Concrete(ConcreteType::Optional(edges.type_node(1, 0)?)),
        21 => TypeExpr::Concrete(ConcreteType::Union(edges.type_list(1, 0)?)),
        22 => TypeExpr::Concrete(ConcreteType::Intersection(edges.type_list(1, 0)?)),
        23 => TypeExpr::Concrete(ConcreteType::ImplTrait(edges.type_list(1, 0)?)),
        24 => TypeExpr::Concrete(ConcreteType::DynTrait(edges.type_list(1, 0)?)),
        25 => {
            let bound = match edges.scalar(1, 0)? {
                0 => WildcardBound::Unbounded,
                1 => WildcardBound::Extends(edges.type_node(1, 1)?),
                2 => WildcardBound::Super(edges.type_node(1, 1)?),
                _ => return Err(shape(edges.node)),
            };
            TypeExpr::Concrete(ConcreteType::Wildcard(bound))
        }
        26 => TypeExpr::Concrete(ConcreteType::Annotated {
            kind: annotation(edges.scalar(1, 0)?, edges.node)?,
            target: edges.type_node(1, 1)?,
        }),
        27 => TypeExpr::Concrete(ConcreteType::Inferred(option_atom(&mut edges, 0, 1)?)),
        28 => {
            let self_type = edges.type_node(1, 0)?;
            let trait_type = option_type(&mut edges, 1, 2)?;
            let segments = match edges.scalar(1, 3)? {
                0 => QualifiedSegments::Unavailable,
                1 => QualifiedSegments::Captured(edges.atom_list(1, 4)?),
                _ => return Err(shape(edges.node)),
            };
            let spelling = edges.atom(1, 5)?;
            TypeExpr::Concrete(ConcreteType::QualifiedPath {
                self_type,
                trait_type,
                segments,
                spelling,
            })
        }
        29 => TypeExpr::Concrete(ConcreteType::Map {
            key: edges.type_node(1, 0)?,
            value: edges.type_node(1, 1)?,
        }),
        30 => TypeExpr::Concrete(ConcreteType::Channel {
            direction: channel(edges.scalar(1, 0)?, edges.node)?,
            element: edges.type_node(1, 1)?,
        }),
        40 => TypeExpr::Computed(ComputedType::KeyOf(edges.type_node(1, 0)?)),
        41 => {
            let query = match edges.scalar(1, 0)? {
                0 => TypeQuery::Entity(edges.entity(1, 1)?),
                1 => TypeQuery::Path(edges.atom_list(1, 1)?),
                2 => TypeQuery::External(edges.external(1, 1)?),
                _ => return Err(shape(edges.node)),
            };
            TypeExpr::Computed(ComputedType::TypeOf(query))
        }
        42 => TypeExpr::Computed(ComputedType::IndexedAccess {
            object: edges.type_node(1, 0)?,
            index: edges.type_node(1, 1)?,
        }),
        43 => TypeExpr::Computed(ComputedType::Conditional {
            check: edges.type_node(1, 0)?,
            extends: edges.type_node(1, 1)?,
            then_type: edges.type_node(1, 2)?,
            else_type: edges.type_node(1, 3)?,
            distributive: boolean(edges.scalar(1, 4)?, edges.node)?,
        }),
        44 => {
            let parameter = edges.atom(1, 0)?;
            let constraint = edges.type_node(1, 1)?;
            let name_as = option_type(&mut edges, 2, 3)?;
            let value = edges.type_node(1, 4)?;
            let readonly = mapped(edges.scalar(1, 5)?, edges.node)?;
            let optional = mapped(edges.scalar(1, 6)?, edges.node)?;
            TypeExpr::Computed(ComputedType::Mapped {
                parameter,
                constraint,
                name_as,
                value,
                readonly,
                optional,
            })
        }
        45 => TypeExpr::Computed(ComputedType::Infer {
            parameter: edges.atom(1, 0)?,
            constraint: option_type(&mut edges, 1, 2)?,
        }),
        46 => TypeExpr::Computed(ComputedType::TemplateLiteral(edges.template_parts(1, 0)?)),
        47 => TypeExpr::Computed(ComputedType::Import {
            specifier: edges.atom(1, 0)?,
            qualifier: edges.atom_list(1, 1)?,
            arguments: edges.type_list(1, 2)?,
        }),
        48 => TypeExpr::Computed(ComputedType::Awaited(edges.type_node(1, 0)?)),
        49 => TypeExpr::Computed(ComputedType::This),
        60 => TypeExpr::Unknown(UnknownType {
            reason: unknown_reason(edges.scalar(1, 0)?, edges.node)?,
            spelling: option_atom(&mut edges, 1, 2)?,
        }),
        _ => return Err(shape(edges.node)),
    };
    edges.finish()?;
    Ok(value)
}

pub(crate) fn type_list_item(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<TypeId, FullSemanticImageFault> {
    edges.node_at(2, index, TYPE)
}

pub(crate) fn atom_list_item(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<AtomId, FullSemanticImageFault> {
    edges.atom_at(2, index)
}

pub(crate) fn tuple_element(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<TupleElement, FullSemanticImageFault> {
    let label_present = boolean(edges.scalar_at(3, index)?, edges.node)?;
    let label = if label_present {
        Some(edges.atom_at_n(3, index, 1)?)
    } else {
        None
    };
    let ty = edges.node_at(4, index, TYPE)?;
    let kind = tuple_kind(edges.scalar_at(5, index)?, edges.node)?;
    Ok(TupleElement { label, ty, kind })
}

pub(crate) fn object_member(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<ObjectMember, FullSemanticImageFault> {
    let kind = edges.scalar_at(6, index)?;
    let value = match kind {
        0 => ObjectMember::Property {
            key: property_key(edges, index)?,
            ty: edges.node_at(8, index, TYPE)?,
            optional: boolean(edges.scalar_at(9, index)?, edges.node)?,
            readonly: boolean(edges.scalar_at(10, index)?, edges.node)?,
        },
        1 => ObjectMember::Method {
            key: property_key(edges, index)?,
            signature: edges.node_at(8, index, TYPE)?,
            optional: boolean(edges.scalar_at(9, index)?, edges.node)?,
        },
        2 => ObjectMember::Index {
            parameter: edges.atom_at(11, index)?,
            key: edges.node_at(7, index, TYPE)?,
            value: edges.node_at(12, index, TYPE)?,
            readonly: boolean(edges.scalar_at(10, index)?, edges.node)?,
        },
        3 => ObjectMember::Call(edges.node_at(8, index, TYPE)?),
        4 => ObjectMember::Construct(edges.node_at(8, index, TYPE)?),
        _ => return Err(shape(edges.node)),
    };
    Ok(value)
}

pub(crate) fn template_part(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<TemplatePart, FullSemanticImageFault> {
    match edges.scalar_at(13, index)? {
        0 => Ok(TemplatePart::Bytes(edges.atom_at_n(13, index, 1)?)),
        1 => Ok(TemplatePart::Placeholder(edges.node_at_n(
            13, index, 1, TYPE,
        )?)),
        _ => Err(shape(edges.node)),
    }
}

pub(crate) fn type_parameter(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<TypeParameter, FullSemanticImageFault> {
    let name = edges.atom_at(14, index)?;
    let bounds =
        TypeParameterBoundListId::new(edges.node_at(15, index, TYPE_PARAMETER_BOUNDS)?.raw);
    let default = option_type_at(edges, 16, index)?;
    let variance = variance(edges.scalar_at(17, index)?, edges.node)?;
    let kind_tag = edges.scalar_at(18, index)?;
    let kind = match kind_tag {
        0 => TypeParameterKind::Type {
            inference: inference(edges.scalar_at_n(18, index, 1)?, edges.node)?,
        },
        1 => TypeParameterKind::ConstValue {
            value_type: edges.node_at_n(18, index, 1, TYPE)?,
        },
        2 => TypeParameterKind::Lifetime,
        _ => return Err(shape(edges.node)),
    };
    let primary = primary_requirement(edges.scalar_at(19, index)?, edges.node)?;
    let constructor = boolean(edges.scalar_at(20, index)?, edges.node)?;
    let allows_ref_like = boolean(edges.scalar_at(21, index)?, edges.node)?;
    let requirements = TypeParameterRequirements {
        primary,
        constructor,
        allows_ref_like,
    };
    if !requirements.is_valid() {
        return Err(shape(edges.node));
    }
    Ok(TypeParameter {
        name,
        bounds,
        default,
        variance,
        kind,
        requirements,
    })
}

pub(crate) fn type_parameter_bound(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<TypeParameterBound, FullSemanticImageFault> {
    match edges.scalar_at(22, index)? {
        0 => Ok(TypeParameterBound::Type(edges.node_at(23, index, TYPE)?)),
        1 => Ok(TypeParameterBound::Lifetime(edges.atom_at(23, index)?)),
        _ => Err(shape(edges.node)),
    }
}

pub(crate) fn free_predicate(
    edges: &mut Edges<'_>,
    index: u32,
) -> Result<FreePredicate, FullSemanticImageFault> {
    Ok(FreePredicate {
        subject: edges.node_at(24, index, TYPE)?,
        bounds: TypeParameterBoundListId::new(
            edges.node_at(25, index, TYPE_PARAMETER_BOUNDS)?.raw,
        ),
    })
}


fn validate_type_list(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TypeListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TYPE_LIST, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        let _ = edges.type_node(2, index)?;
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_atom_list(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: AtomListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, ATOM_LIST, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        let _ = edges.atom(2, index)?;
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_tuple_elements(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TupleElementListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TUPLE_ELEMENTS, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        if boolean(edges.scalar(3, index)?, edges.node)? {
            let _ = edges.atom(3, index)?;
        }
        let _ = edges.type_node(4, index)?;
        let _ = tuple_kind(edges.scalar(5, index)?, edges.node)?;
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_object_members(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: ObjectMemberListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, OBJECT_MEMBERS, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        match edges.scalar(6, index)? {
            0 => {
                validate_property_key(&mut edges, index)?;
                let _ = edges.type_node(8, index)?;
                let _ = boolean(edges.scalar(9, index)?, edges.node)?;
                let _ = boolean(edges.scalar(10, index)?, edges.node)?;
            }
            1 => {
                validate_property_key(&mut edges, index)?;
                let _ = edges.type_node(8, index)?;
                let _ = boolean(edges.scalar(9, index)?, edges.node)?;
            }
            2 => {
                let _ = edges.atom(11, index)?;
                let _ = edges.type_node(7, index)?;
                let _ = edges.type_node(12, index)?;
                let _ = boolean(edges.scalar(10, index)?, edges.node)?;
            }
            3 | 4 => {
                let _ = edges.type_node(8, index)?;
            }
            _ => return Err(shape(edges.node)),
        }
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_template_parts(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TemplatePartListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TEMPLATE_PARTS, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        match edges.scalar(13, index)? {
            0 => {
                let _ = edges.atom(13, index)?;
            }
            1 => {
                let _ = edges.type_node(13, index)?;
            }
            _ => return Err(shape(edges.node)),
        }
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_type_parameters(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TypeParameterListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TYPE_PARAMETERS, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        let _ = edges.atom(14, index)?;
        let _ = node(edges.take(15, index)?, TYPE_PARAMETER_BOUNDS, edges.node)?;
        let default = boolean(edges.scalar(16, index)?, edges.node)?;
        if default {
            let _ = edges.type_node(16, index)?;
        }
        let _ = variance(edges.scalar(17, index)?, edges.node)?;
        match edges.scalar(18, index)? {
            0 => {
                let _ = inference(edges.scalar(18, index)?, edges.node)?;
            }
            1 => {
                let _ = edges.type_node(18, index)?;
            }
            2 => {}
            _ => return Err(shape(edges.node)),
        }
        let requirements = TypeParameterRequirements {
            primary: primary_requirement(edges.scalar(19, index)?, edges.node)?,
            constructor: boolean(edges.scalar(20, index)?, edges.node)?,
            allows_ref_like: boolean(edges.scalar(21, index)?, edges.node)?,
        };
        if !requirements.is_valid() {
            return Err(shape(edges.node));
        }
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}
fn validate_type_parameter_bounds(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: TypeParameterBoundListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, TYPE_PARAMETER_BOUNDS, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        match edges.scalar(22, index)? {
            0 => {
                let _ = edges.type_node(23, index)?;
            }
            1 => {
                let _ = edges.atom(23, index)?;
            }
            _ => return Err(shape(edges.node)),
        }
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}

/// Raw edge-run count for one typed list node; used by structural admission.
pub(crate) fn list_count(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    domain: u8,
    coordinate: u32,
) -> Result<u32, FullSemanticImageFault> {
    Edges::for_node(bytes, layout, typed, domain, coordinate).map(|edges| edges.count)
}

fn validate_free_predicates(
    bytes: &[u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    id: FreePredicateListId,
) -> Result<(), FullSemanticImageFault> {
    let mut edges = Edges::for_node(bytes, layout, typed, FREE_PREDICATES, id.raw)?;
    let mut index = 0_u32;
    while edges.next < edges.count {
        let _ = edges.type_node(24, index)?;
        let _ = node(edges.take(25, index)?, TYPE_PARAMETER_BOUNDS, edges.node)?;
        index = index
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
    }
    edges.finish()
}

fn property_key(edges: &mut Edges<'_>, index: u32) -> Result<PropertyKey, FullSemanticImageFault> {
    match edges.scalar_at(7, index)? {
        0 => Ok(PropertyKey::Named(edges.atom_at_n(7, index, 1)?)),
        1 => Ok(PropertyKey::Private(edges.atom_at_n(7, index, 1)?)),
        2 => Ok(PropertyKey::Numeric(edges.atom_at_n(7, index, 1)?)),
        3 => Ok(PropertyKey::Computed(edges.node_at_n(7, index, 1, TYPE)?)),
        _ => Err(shape(edges.node)),
    }
}

fn validate_property_key(edges: &mut Edges<'_>, index: u32) -> Result<(), FullSemanticImageFault> {
    match edges.scalar(7, index)? {
        0..=2 => {
            let _ = edges.atom(7, index)?;
        }
        3 => {
            let _ = edges.type_node(7, index)?;
        }
        _ => return Err(shape(edges.node)),
    }
    Ok(())
}

fn literal(edges: &mut Edges<'_>) -> Result<LiteralType, FullSemanticImageFault> {
    match edges.scalar(1, 0)? {
        0 => Ok(LiteralType::String(edges.atom(1, 1)?)),
        1 => Ok(LiteralType::Number(edges.atom(1, 1)?)),
        2 => Ok(LiteralType::BigInt(edges.atom(1, 1)?)),
        3 => Ok(LiteralType::Boolean(boolean(
            edges.scalar(1, 1)?,
            edges.node,
        )?)),
        4 => Ok(LiteralType::Null),
        5 => Ok(LiteralType::Undefined),
        _ => Err(shape(edges.node)),
    }
}

fn option_atom(
    edges: &mut Edges<'_>,
    present_field: u32,
    value_field: u32,
) -> Result<Option<AtomId>, FullSemanticImageFault> {
    if boolean(edges.scalar(1, present_field)?, edges.node)? {
        Ok(Some(edges.atom(1, value_field)?))
    } else {
        Ok(None)
    }
}
fn option_type(
    edges: &mut Edges<'_>,
    present_field: u32,
    value_field: u32,
) -> Result<Option<TypeId>, FullSemanticImageFault> {
    if boolean(edges.scalar(1, present_field)?, edges.node)? {
        Ok(Some(edges.type_node(1, value_field)?))
    } else {
        Ok(None)
    }
}
fn option_type_at(
    edges: &mut Edges<'_>,
    role: u8,
    index: u32,
) -> Result<Option<TypeId>, FullSemanticImageFault> {
    let present = boolean(edges.scalar_at(role, index)?, edges.node)?;
    if present {
        Ok(Some(edges.node_at_n(role, index, 1, TYPE)?))
    } else {
        Ok(None)
    }
}


fn boolean(value: u64, node: u32) -> Result<bool, FullSemanticImageFault> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(shape(node)),
    }
}
fn narrow_u32(value: u64, node: u32) -> Result<u32, FullSemanticImageFault> {
    u32::try_from(value).map_err(|_| shape(node))
}
fn nonzero_u16(value: u64, node: u32) -> Result<NonZeroU16, FullSemanticImageFault> {
    let value = u16::try_from(value).map_err(|_| shape(node))?;
    NonZeroU16::new(value).ok_or_else(|| shape(node))
}

fn builtin(value: u64, node: u32) -> Result<BuiltinType, FullSemanticImageFault> {
    Ok(match value {
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
        _ => return Err(shape(node)),
    })
}
fn mutability(value: u64, node: u32) -> Result<Mutability, FullSemanticImageFault> {
    match value {
        0 => Ok(Mutability::Immutable),
        1 => Ok(Mutability::Mutable),
        _ => Err(shape(node)),
    }
}
fn cxx_reference(value: u64, node: u32) -> Result<CxxReferenceCategory, FullSemanticImageFault> {
    match value {
        0 => Ok(CxxReferenceCategory::Lvalue),
        1 => Ok(CxxReferenceCategory::Rvalue),
        _ => Err(shape(node)),
    }
}
fn native_character(value: u64, node: u32) -> Result<NativeCharacterRole, FullSemanticImageFault> {
    Ok(match value {
        0 => NativeCharacterRole::UnicodeScalar,
        1 => NativeCharacterRole::Utf16CodeUnit,
        2 => NativeCharacterRole::Utf32CodeUnit,
        3 => NativeCharacterRole::CPlainSigned,
        4 => NativeCharacterRole::CPlainUnsigned,
        5 => NativeCharacterRole::CSigned,
        6 => NativeCharacterRole::CUnsigned,
        7 => NativeCharacterRole::CWideSigned,
        8 => NativeCharacterRole::CWideUnsigned,
        9 => NativeCharacterRole::CWideSignednessUnavailable,
        _ => return Err(shape(node)),
    })
}
fn variadic(value: u64, node: u32) -> Result<VariadicForm, FullSemanticImageFault> {
    match value {
        0 => Ok(VariadicForm::None),
        1 => Ok(VariadicForm::TypedLast),
        2 => Ok(VariadicForm::CUnbounded),
        _ => Err(shape(node)),
    }
}
fn annotation(value: u64, node: u32) -> Result<AnnotationKind, FullSemanticImageFault> {
    match value {
        0 => Ok(AnnotationKind::Readonly),
        1 => Ok(AnnotationKind::NullableValue),
        2 => Ok(AnnotationKind::NullableReference),
        3 => Ok(AnnotationKind::NonNullableReference),
        _ => Err(shape(node)),
    }
}
fn channel(value: u64, node: u32) -> Result<ChannelDirection, FullSemanticImageFault> {
    match value {
        0 => Ok(ChannelDirection::Both),
        1 => Ok(ChannelDirection::Send),
        2 => Ok(ChannelDirection::Receive),
        _ => Err(shape(node)),
    }
}
fn mapped(value: u64, node: u32) -> Result<MappedModifier, FullSemanticImageFault> {
    match value {
        0 => Ok(MappedModifier::Preserve),
        1 => Ok(MappedModifier::Add),
        2 => Ok(MappedModifier::Remove),
        _ => Err(shape(node)),
    }
}
fn unknown_reason(value: u64, node: u32) -> Result<UnknownReason, FullSemanticImageFault> {
    match value {
        0 => Ok(UnknownReason::Unannotated),
        1 => Ok(UnknownReason::DynamicallyTyped),
        2 => Ok(UnknownReason::UnresolvedLocalName),
        3 => Ok(UnknownReason::UnresolvedExternal),
        4 => Ok(UnknownReason::TruncatedAtDepthLimit),
        5 => Ok(UnknownReason::OracleGap),
        6 => Ok(UnknownReason::NoIrRepresentation),
        7 => Ok(UnknownReason::Error),
        _ => Err(shape(node)),
    }
}
fn tuple_kind(value: u64, node: u32) -> Result<TupleElementKind, FullSemanticImageFault> {
    match value {
        0 => Ok(TupleElementKind::Required),
        1 => Ok(TupleElementKind::Optional),
        2 => Ok(TupleElementKind::Rest),
        _ => Err(shape(node)),
    }
}
fn variance(value: u64, node: u32) -> Result<Variance, FullSemanticImageFault> {
    match value {
        0 => Ok(Variance::Invariant),
        1 => Ok(Variance::Covariant),
        2 => Ok(Variance::Contravariant),
        3 => Ok(Variance::Bivariant),
        _ => Err(shape(node)),
    }
}
fn inference(value: u64, node: u32) -> Result<TypeParameterInference, FullSemanticImageFault> {
    match value {
        0 => Ok(TypeParameterInference::Ordinary),
        1 => Ok(TypeParameterInference::Const),
        _ => Err(shape(node)),
    }
}
fn primary_requirement(
    value: u64,
    node: u32,
) -> Result<TypeParameterPrimaryRequirement, FullSemanticImageFault> {
    match value {
        0 => Ok(TypeParameterPrimaryRequirement::None),
        1 => Ok(TypeParameterPrimaryRequirement::Reference { nullable: false }),
        2 => Ok(TypeParameterPrimaryRequirement::Reference { nullable: true }),
        3 => Ok(TypeParameterPrimaryRequirement::Value),
        4 => Ok(TypeParameterPrimaryRequirement::Unmanaged),
        5 => Ok(TypeParameterPrimaryRequirement::NotNull),
        6 => Ok(TypeParameterPrimaryRequirement::Default),
        _ => Err(shape(node)),
    }
}
