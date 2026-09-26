//! Exhaustive finalized-IR row to typed dependency edge projection.

use super::*;
use crate::ir::Ir;

mod codes;

use codes::{
    annotation_kind, builtin_type, channel_direction, cxx_reference_category,
    for_each_property_key, list_index, mapped_modifier, missing_node, mutability_code,
    native_character_role, primary_requirement, tuple_element_kind, type_parameter_inference,
    unknown_reason, variance, variadic_form,
};

pub(crate) fn for_each_edge(
    ir: &Ir,
    node: TypedPlanNode,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    macro_rules! emit {
        ($role:expr, $target:expr $(,)?) => {
            sink(TypedPlanEdge {
                role: $role,
                target: $target,
            })?
        };
    }
    macro_rules! type_tag {
        ($tag:expr) => {
            emit!(TypedEdgeRole::TypeTag, TypedPlanTarget::Scalar($tag))
        };
    }
    macro_rules! type_child {
        ($field:expr, $id:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Node(TypedPlanNode::Type($id)),
            )
        };
    }
    macro_rules! type_list {
        ($field:expr, $id:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Node(TypedPlanNode::TypeList($id)),
            )
        };
    }
    macro_rules! scalar {
        ($field:expr, $value:expr) => {
            emit!(
                TypedEdgeRole::TypeField($field),
                TypedPlanTarget::Scalar($value)
            )
        };
    }

    match node {
        TypedPlanNode::Type(id) => {
            let expression = ir.ty(id).ok_or(missing_node(ir, node)?)?;
            match expression {
                crate::ir::TypeExpr::Concrete(value) => match value {
                    crate::ir::ConcreteType::Builtin(value) => {
                        type_tag!(1);
                        scalar!(0, builtin_type(value));
                    }
                    crate::ir::ConcreteType::Literal(value) => {
                        type_tag!(2);
                        match value {
                            crate::ir::LiteralType::String(atom) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::Number(atom) => {
                                scalar!(0, 1);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::BigInt(atom) => {
                                scalar!(0, 2);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::LiteralType::Boolean(value) => {
                                scalar!(0, 3);
                                scalar!(1, u64::from(value));
                            }
                            crate::ir::LiteralType::Null => scalar!(0, 4),
                            crate::ir::LiteralType::Undefined => scalar!(0, 5),
                        }
                    }
                    crate::ir::ConcreteType::Nominal(entity) => {
                        type_tag!(3);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Entity(entity));
                    }
                    crate::ir::ConcreteType::External(external) => {
                        type_tag!(4);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::External(external)
                        );
                    }
                    crate::ir::ConcreteType::Parameter(atom) => {
                        type_tag!(5);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(atom));
                    }
                    crate::ir::ConcreteType::Applied {
                        constructor,
                        arguments,
                    } => {
                        type_tag!(6);
                        type_child!(0, constructor);
                        type_list!(1, arguments);
                    }
                    crate::ir::ConcreteType::Tuple(elements) => {
                        type_tag!(7);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(elements)),
                        );
                    }
                    crate::ir::ConcreteType::Object(members) => {
                        type_tag!(8);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::ObjectMembers(members)),
                        );
                    }
                    crate::ir::ConcreteType::Function {
                        parameters,
                        results,
                        abi,
                        variadic,
                        unsafe_,
                    } => {
                        type_tag!(9);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(parameters)),
                        );
                        emit!(
                            TypedEdgeRole::TypeField(1),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(results)),
                        );
                        scalar!(2, u64::from(abi.is_some()));
                        if let Some(abi) = abi {
                            emit!(TypedEdgeRole::TypeField(3), TypedPlanTarget::Atom(abi));
                        }
                        scalar!(4, variadic_form(variadic));
                        scalar!(5, u64::from(unsafe_));
                    }
                    crate::ir::ConcreteType::Reference {
                        target,
                        mutability,
                        lifetime,
                    } => {
                        type_tag!(10);
                        type_child!(0, target);
                        scalar!(1, mutability_code(mutability));
                        scalar!(2, u64::from(lifetime.is_some()));
                        if let Some(lifetime) = lifetime {
                            emit!(TypedEdgeRole::TypeField(3), TypedPlanTarget::Atom(lifetime));
                        }
                    }
                    crate::ir::ConcreteType::CxxReference { target, category } => {
                        type_tag!(11);
                        type_child!(0, target);
                        scalar!(1, cxx_reference_category(category));
                    }
                    crate::ir::ConcreteType::CPointer { target } => {
                        type_tag!(12);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::CxxMemberPointer { owner, member } => {
                        type_tag!(13);
                        type_child!(0, owner);
                        type_child!(1, member);
                    }
                    crate::ir::ConcreteType::CQualified { target, qualifiers } => {
                        type_tag!(14);
                        type_child!(0, target);
                        scalar!(1, u64::from(u8::from(qualifiers)));
                    }
                    crate::ir::ConcreteType::CBlockPointer { target } => {
                        type_tag!(15);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::NativeCharacter { role, width } => {
                        type_tag!(16);
                        scalar!(0, native_character_role(role));
                        scalar!(1, u64::from(width.get()));
                    }
                    crate::ir::ConcreteType::Pointer { target, mutability } => {
                        type_tag!(17);
                        type_child!(0, target);
                        scalar!(1, mutability_code(mutability));
                    }
                    crate::ir::ConcreteType::Slice(target) => {
                        type_tag!(18);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::Array { element, shape } => {
                        type_tag!(19);
                        type_child!(0, element);
                        match shape {
                            crate::ir::ArrayShape::Sequence => scalar!(1, 0),
                            crate::ir::ArrayShape::Rectangular { rank } => {
                                scalar!(1, 1);
                                scalar!(2, u64::from(rank.get()));
                            }
                            crate::ir::ArrayShape::FixedValue { length } => {
                                scalar!(1, 2);
                                scalar!(2, length);
                            }
                            crate::ir::ArrayShape::ConstExpression(atom) => {
                                scalar!(1, 3);
                                emit!(TypedEdgeRole::TypeField(2), TypedPlanTarget::Atom(atom));
                            }
                            crate::ir::ArrayShape::Incomplete => scalar!(1, 4),
                        }
                    }
                    crate::ir::ConcreteType::Optional(target) => {
                        type_tag!(20);
                        type_child!(0, target);
                    }
                    crate::ir::ConcreteType::Union(items) => {
                        type_tag!(21);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::Intersection(items) => {
                        type_tag!(22);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::ImplTrait(items) => {
                        type_tag!(23);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::DynTrait(items) => {
                        type_tag!(24);
                        type_list!(0, items);
                    }
                    crate::ir::ConcreteType::Wildcard(bound) => {
                        type_tag!(25);
                        match bound {
                            crate::ir::WildcardBound::Unbounded => scalar!(0, 0),
                            crate::ir::WildcardBound::Extends(target) => {
                                scalar!(0, 1);
                                type_child!(1, target);
                            }
                            crate::ir::WildcardBound::Super(target) => {
                                scalar!(0, 2);
                                type_child!(1, target);
                            }
                        }
                    }
                    crate::ir::ConcreteType::Annotated { kind, target } => {
                        type_tag!(26);
                        scalar!(0, annotation_kind(kind));
                        type_child!(1, target);
                    }
                    crate::ir::ConcreteType::Inferred(spelling) => {
                        type_tag!(27);
                        scalar!(0, u64::from(spelling.is_some()));
                        if let Some(spelling) = spelling {
                            emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(spelling));
                        }
                    }
                    crate::ir::ConcreteType::QualifiedPath {
                        self_type,
                        trait_type,
                        segments,
                        spelling,
                    } => {
                        type_tag!(28);
                        type_child!(0, self_type);
                        scalar!(1, u64::from(trait_type.is_some()));
                        if let Some(trait_type) = trait_type {
                            type_child!(2, trait_type);
                        }
                        match segments {
                            crate::ir::QualifiedSegments::Captured(segments) => {
                                scalar!(3, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(4),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(segments)),
                                );
                            }
                            crate::ir::QualifiedSegments::Unavailable => scalar!(3, 0),
                        }
                        emit!(TypedEdgeRole::TypeField(5), TypedPlanTarget::Atom(spelling));
                    }
                    crate::ir::ConcreteType::Map { key, value } => {
                        type_tag!(29);
                        type_child!(0, key);
                        type_child!(1, value);
                    }
                    crate::ir::ConcreteType::Channel { direction, element } => {
                        type_tag!(30);
                        scalar!(0, channel_direction(direction));
                        type_child!(1, element);
                    }
                },
                crate::ir::TypeExpr::Computed(value) => match value {
                    crate::ir::ComputedType::KeyOf(target) => {
                        type_tag!(40);
                        type_child!(0, target);
                    }
                    crate::ir::ComputedType::TypeOf(query) => {
                        type_tag!(41);
                        match query {
                            crate::ir::TypeQuery::Entity(entity) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Entity(entity));
                            }
                            crate::ir::TypeQuery::Path(path) => {
                                scalar!(0, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(path)),
                                );
                            }
                            crate::ir::TypeQuery::External(external) => {
                                scalar!(0, 2);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::External(external),
                                );
                            }
                        }
                    }
                    crate::ir::ComputedType::IndexedAccess { object, index } => {
                        type_tag!(42);
                        type_child!(0, object);
                        type_child!(1, index);
                    }
                    crate::ir::ComputedType::Conditional {
                        check,
                        extends,
                        then_type,
                        else_type,
                        distributive,
                    } => {
                        type_tag!(43);
                        type_child!(0, check);
                        type_child!(1, extends);
                        type_child!(2, then_type);
                        type_child!(3, else_type);
                        scalar!(4, u64::from(distributive));
                    }
                    crate::ir::ComputedType::Mapped {
                        parameter,
                        constraint,
                        name_as,
                        value,
                        readonly,
                        optional,
                    } => {
                        type_tag!(44);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(parameter)
                        );
                        type_child!(1, constraint);
                        scalar!(2, u64::from(name_as.is_some()));
                        if let Some(name_as) = name_as {
                            type_child!(3, name_as);
                        }
                        type_child!(4, value);
                        scalar!(5, mapped_modifier(readonly));
                        scalar!(6, mapped_modifier(optional));
                    }
                    crate::ir::ComputedType::Infer {
                        parameter,
                        constraint,
                    } => {
                        type_tag!(45);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(parameter)
                        );
                        scalar!(1, u64::from(constraint.is_some()));
                        if let Some(constraint) = constraint {
                            type_child!(2, constraint);
                        }
                    }
                    crate::ir::ComputedType::TemplateLiteral(parts) => {
                        type_tag!(46);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TemplateParts(parts)),
                        );
                    }
                    crate::ir::ComputedType::Import {
                        specifier,
                        qualifier,
                        arguments,
                    } => {
                        type_tag!(47);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Atom(specifier)
                        );
                        emit!(
                            TypedEdgeRole::TypeField(1),
                            TypedPlanTarget::Node(TypedPlanNode::AtomList(qualifier)),
                        );
                        type_list!(2, arguments);
                    }
                    crate::ir::ComputedType::Awaited(target) => {
                        type_tag!(48);
                        type_child!(0, target);
                    }
                    crate::ir::ComputedType::This => type_tag!(49),
                },
                crate::ir::TypeExpr::Unknown(value) => {
                    type_tag!(60);
                    scalar!(0, unknown_reason(value.reason));
                    scalar!(1, u64::from(value.spelling.is_some()));
                    if let Some(spelling) = value.spelling {
                        emit!(TypedEdgeRole::TypeField(2), TypedPlanTarget::Atom(spelling));
                    }
                }
            }
        }
        TypedPlanNode::TypeList(id) => {
            let values = ir.types(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::ListElement(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value)),
                );
            }
        }
        TypedPlanNode::AtomList(id) => {
            let values = ir.atom_list(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                emit!(
                    TypedEdgeRole::ListElement(list_index(node, index)?),
                    TypedPlanTarget::Atom(value),
                );
            }
        }
        TypedPlanNode::TupleElements(id) => {
            let values = ir.tuple_elements(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::TupleLabel(index),
                    TypedPlanTarget::Scalar(u64::from(value.label.is_some())),
                );
                if let Some(label) = value.label {
                    emit!(
                        TypedEdgeRole::TupleLabel(index),
                        TypedPlanTarget::Atom(label)
                    );
                }
                emit!(
                    TypedEdgeRole::TupleType(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value.ty)),
                );
                emit!(
                    TypedEdgeRole::TupleKind(index),
                    TypedPlanTarget::Scalar(tuple_element_kind(value.kind)),
                );
            }
        }
        TypedPlanNode::ObjectMembers(id) => {
            let values = ir.object_members(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::ObjectMember::Property {
                        key,
                        ty,
                        optional,
                        readonly,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(0));
                        for_each_property_key(key, index, sink)?;
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectOptional(index),
                            TypedPlanTarget::Scalar(u64::from(optional)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectReadonly(index),
                            TypedPlanTarget::Scalar(u64::from(readonly)),
                        );
                    }
                    crate::ir::ObjectMember::Method {
                        key,
                        signature,
                        optional,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(1));
                        for_each_property_key(key, index, sink)?;
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectOptional(index),
                            TypedPlanTarget::Scalar(u64::from(optional)),
                        );
                    }
                    crate::ir::ObjectMember::Index {
                        parameter,
                        key,
                        value,
                        readonly,
                    } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(2));
                        emit!(
                            TypedEdgeRole::ObjectParameter(index),
                            TypedPlanTarget::Atom(parameter)
                        );
                        emit!(
                            TypedEdgeRole::ObjectKey(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(key)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectValue(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(value)),
                        );
                        emit!(
                            TypedEdgeRole::ObjectReadonly(index),
                            TypedPlanTarget::Scalar(u64::from(readonly)),
                        );
                    }
                    crate::ir::ObjectMember::Call(signature) => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(3));
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                    }
                    crate::ir::ObjectMember::Construct(signature) => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(4));
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                    }
                }
            }
        }
        TypedPlanNode::TemplateParts(id) => {
            let values = ir.template_parts(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::TemplatePart::Bytes(atom) => {
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Scalar(0)
                        );
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Atom(atom)
                        );
                    }
                    crate::ir::TemplatePart::Placeholder(ty) => {
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Scalar(1)
                        );
                        emit!(
                            TypedEdgeRole::TemplatePart(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                    }
                }
            }
        }
        TypedPlanNode::TypeParameters(id) => {
            let values = ir.type_parameters(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::ParameterName(index),
                    TypedPlanTarget::Atom(value.name)
                );
                emit!(
                    TypedEdgeRole::ParameterBound(index),
                    TypedPlanTarget::Node(TypedPlanNode::TypeParameterBounds(value.bounds)),
                );
                emit!(
                    TypedEdgeRole::ParameterDefault(index),
                    TypedPlanTarget::Scalar(u64::from(value.default.is_some())),
                );
                if let Some(default) = value.default {
                    emit!(
                        TypedEdgeRole::ParameterDefault(index),
                        TypedPlanTarget::Node(TypedPlanNode::Type(default)),
                    );
                }
                emit!(
                    TypedEdgeRole::ParameterVariance(index),
                    TypedPlanTarget::Scalar(variance(value.variance)),
                );
                match value.kind {
                    crate::ir::TypeParameterKind::Type { inference } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(0),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(type_parameter_inference(inference)),
                        );
                    }
                    crate::ir::TypeParameterKind::ConstValue { value_type } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(1),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(value_type)),
                        );
                    }
                    crate::ir::TypeParameterKind::Lifetime => emit!(
                        TypedEdgeRole::ParameterKind(index),
                        TypedPlanTarget::Scalar(2),
                    ),
                }
                emit!(
                    TypedEdgeRole::ParameterRequirement(index),
                    TypedPlanTarget::Scalar(primary_requirement(value.requirements.primary)),
                );
                emit!(
                    TypedEdgeRole::ParameterConstructor(index),
                    TypedPlanTarget::Scalar(u64::from(value.requirements.constructor)),
                );
                emit!(
                    TypedEdgeRole::ParameterAllowsRefLike(index),
                    TypedPlanTarget::Scalar(u64::from(value.requirements.allows_ref_like)),
                );
            }
        }
        TypedPlanNode::TypeParameterBounds(id) => {
            let values = ir
                .type_parameter_bounds(id)
                .ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::ir::TypeParameterBound::Type(ty) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(0));
                        emit!(
                            TypedEdgeRole::BoundValue(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                    }
                    crate::ir::TypeParameterBound::Lifetime(atom) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(1));
                        emit!(
                            TypedEdgeRole::BoundValue(index),
                            TypedPlanTarget::Atom(atom)
                        );
                    }
                }
            }
        }
        TypedPlanNode::FreePredicates(id) => {
            let values = ir.free_predicates(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                emit!(
                    TypedEdgeRole::PredicateSubject(index),
                    TypedPlanTarget::Node(TypedPlanNode::Type(value.subject)),
                );
                emit!(
                    TypedEdgeRole::PredicateBound(index),
                    TypedPlanTarget::Node(TypedPlanNode::TypeParameterBounds(value.bounds)),
                );
            }
        }
    }
    Ok(())
}

