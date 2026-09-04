//! Exhaustive finalized-IR row to typed dependency edge projection.

use super::*;
use crate::Ir;

pub(super) fn for_each_edge(
    ir: &Ir,
    node: TypedPlanNode,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    macro_rules! emit {
        ($role:expr, $target:expr) => {
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
            emit!(TypedEdgeRole::TypeField($field), TypedPlanTarget::Scalar($value))
        };
    }

    match node {
        TypedPlanNode::Type(id) => {
            let expression = ir.ty(id).ok_or(missing_node(ir, node)?)?;
            match expression {
                crate::TypeExpr::Concrete(value) => match value {
                    crate::ConcreteType::Builtin(value) => {
                        type_tag!(1);
                        scalar!(0, builtin_type(value));
                    }
                    crate::ConcreteType::Literal(value) => {
                        type_tag!(2);
                        match value {
                            crate::LiteralType::String(atom) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::LiteralType::Number(atom) => {
                                scalar!(0, 1);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::LiteralType::BigInt(atom) => {
                                scalar!(0, 2);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(atom));
                            }
                            crate::LiteralType::Boolean(value) => {
                                scalar!(0, 3);
                                scalar!(1, u64::from(value));
                            }
                            crate::LiteralType::Null => scalar!(0, 4),
                            crate::LiteralType::Undefined => scalar!(0, 5),
                        }
                    }
                    crate::ConcreteType::Nominal(entity) => {
                        type_tag!(3);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Entity(entity));
                    }
                    crate::ConcreteType::External(external) => {
                        type_tag!(4);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::External(external));
                    }
                    crate::ConcreteType::Parameter(atom) => {
                        type_tag!(5);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(atom));
                    }
                    crate::ConcreteType::Applied { constructor, arguments } => {
                        type_tag!(6);
                        type_child!(0, constructor);
                        type_list!(1, arguments);
                    }
                    crate::ConcreteType::Tuple(elements) => {
                        type_tag!(7);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TupleElements(elements)),
                        );
                    }
                    crate::ConcreteType::Object(members) => {
                        type_tag!(8);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::ObjectMembers(members)),
                        );
                    }
                    crate::ConcreteType::Function {
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
                    crate::ConcreteType::Reference { target, mutability, lifetime } => {
                        type_tag!(10);
                        type_child!(0, target);
                        scalar!(1, mutability(mutability));
                        scalar!(2, u64::from(lifetime.is_some()));
                        if let Some(lifetime) = lifetime {
                            emit!(TypedEdgeRole::TypeField(3), TypedPlanTarget::Atom(lifetime));
                        }
                    }
                    crate::ConcreteType::CxxReference { target, category } => {
                        type_tag!(11);
                        type_child!(0, target);
                        scalar!(1, cxx_reference_category(category));
                    }
                    crate::ConcreteType::CPointer { target } => {
                        type_tag!(12);
                        type_child!(0, target);
                    }
                    crate::ConcreteType::CxxMemberPointer { owner, member } => {
                        type_tag!(13);
                        type_child!(0, owner);
                        type_child!(1, member);
                    }
                    crate::ConcreteType::CQualified { target, qualifiers } => {
                        type_tag!(14);
                        type_child!(0, target);
                        scalar!(1, u64::from(u8::from(qualifiers)));
                    }
                    crate::ConcreteType::CBlockPointer { target } => {
                        type_tag!(15);
                        type_child!(0, target);
                    }
                    crate::ConcreteType::NativeCharacter { role, width } => {
                        type_tag!(16);
                        scalar!(0, native_character_role(role));
                        scalar!(1, u64::from(width.get()));
                    }
                    crate::ConcreteType::Pointer { target, mutability } => {
                        type_tag!(17);
                        type_child!(0, target);
                        scalar!(1, mutability(mutability));
                    }
                    crate::ConcreteType::Slice(target) => {
                        type_tag!(18);
                        type_child!(0, target);
                    }
                    crate::ConcreteType::Array { element, shape } => {
                        type_tag!(19);
                        type_child!(0, element);
                        match shape {
                            crate::ArrayShape::Sequence => scalar!(1, 0),
                            crate::ArrayShape::Rectangular { rank } => {
                                scalar!(1, 1);
                                scalar!(2, u64::from(rank.get()));
                            }
                            crate::ArrayShape::FixedValue { length } => {
                                scalar!(1, 2);
                                scalar!(2, length);
                            }
                            crate::ArrayShape::ConstExpression(atom) => {
                                scalar!(1, 3);
                                emit!(TypedEdgeRole::TypeField(2), TypedPlanTarget::Atom(atom));
                            }
                            crate::ArrayShape::Incomplete => scalar!(1, 4),
                        }
                    }
                    crate::ConcreteType::Optional(target) => {
                        type_tag!(20);
                        type_child!(0, target);
                    }
                    crate::ConcreteType::Union(items) => {
                        type_tag!(21);
                        type_list!(0, items);
                    }
                    crate::ConcreteType::Intersection(items) => {
                        type_tag!(22);
                        type_list!(0, items);
                    }
                    crate::ConcreteType::ImplTrait(items) => {
                        type_tag!(23);
                        type_list!(0, items);
                    }
                    crate::ConcreteType::DynTrait(items) => {
                        type_tag!(24);
                        type_list!(0, items);
                    }
                    crate::ConcreteType::Wildcard(bound) => {
                        type_tag!(25);
                        match bound {
                            crate::WildcardBound::Unbounded => scalar!(0, 0),
                            crate::WildcardBound::Extends(target) => {
                                scalar!(0, 1);
                                type_child!(1, target);
                            }
                            crate::WildcardBound::Super(target) => {
                                scalar!(0, 2);
                                type_child!(1, target);
                            }
                        }
                    }
                    crate::ConcreteType::Annotated { kind, target } => {
                        type_tag!(26);
                        scalar!(0, annotation_kind(kind));
                        type_child!(1, target);
                    }
                    crate::ConcreteType::Inferred(spelling) => {
                        type_tag!(27);
                        scalar!(0, u64::from(spelling.is_some()));
                        if let Some(spelling) = spelling {
                            emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Atom(spelling));
                        }
                    }
                    crate::ConcreteType::QualifiedPath {
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
                            crate::QualifiedSegments::Captured(segments) => {
                                scalar!(3, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(4),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(segments)),
                                );
                            }
                            crate::QualifiedSegments::Unavailable => scalar!(3, 0),
                        }
                        emit!(TypedEdgeRole::TypeField(5), TypedPlanTarget::Atom(spelling));
                    }
                    crate::ConcreteType::Map { key, value } => {
                        type_tag!(29);
                        type_child!(0, key);
                        type_child!(1, value);
                    }
                    crate::ConcreteType::Channel { direction, element } => {
                        type_tag!(30);
                        scalar!(0, channel_direction(direction));
                        type_child!(1, element);
                    }
                },
                crate::TypeExpr::Computed(value) => match value {
                    crate::ComputedType::KeyOf(target) => {
                        type_tag!(40);
                        type_child!(0, target);
                    }
                    crate::ComputedType::TypeOf(query) => {
                        type_tag!(41);
                        match query {
                            crate::TypeQuery::Entity(entity) => {
                                scalar!(0, 0);
                                emit!(TypedEdgeRole::TypeField(1), TypedPlanTarget::Entity(entity));
                            }
                            crate::TypeQuery::Path(path) => {
                                scalar!(0, 1);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::Node(TypedPlanNode::AtomList(path)),
                                );
                            }
                            crate::TypeQuery::External(external) => {
                                scalar!(0, 2);
                                emit!(
                                    TypedEdgeRole::TypeField(1),
                                    TypedPlanTarget::External(external),
                                );
                            }
                        }
                    }
                    crate::ComputedType::IndexedAccess { object, index } => {
                        type_tag!(42);
                        type_child!(0, object);
                        type_child!(1, index);
                    }
                    crate::ComputedType::Conditional {
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
                    crate::ComputedType::Mapped {
                        parameter,
                        constraint,
                        name_as,
                        value,
                        readonly,
                        optional,
                    } => {
                        type_tag!(44);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(parameter));
                        type_child!(1, constraint);
                        scalar!(2, u64::from(name_as.is_some()));
                        if let Some(name_as) = name_as {
                            type_child!(3, name_as);
                        }
                        type_child!(4, value);
                        scalar!(5, mapped_modifier(readonly));
                        scalar!(6, mapped_modifier(optional));
                    }
                    crate::ComputedType::Infer { parameter, constraint } => {
                        type_tag!(45);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(parameter));
                        scalar!(1, u64::from(constraint.is_some()));
                        if let Some(constraint) = constraint {
                            type_child!(2, constraint);
                        }
                    }
                    crate::ComputedType::TemplateLiteral(parts) => {
                        type_tag!(46);
                        emit!(
                            TypedEdgeRole::TypeField(0),
                            TypedPlanTarget::Node(TypedPlanNode::TemplateParts(parts)),
                        );
                    }
                    crate::ComputedType::Import {
                        specifier,
                        qualifier,
                        arguments,
                    } => {
                        type_tag!(47);
                        emit!(TypedEdgeRole::TypeField(0), TypedPlanTarget::Atom(specifier));
                        emit!(
                            TypedEdgeRole::TypeField(1),
                            TypedPlanTarget::Node(TypedPlanNode::AtomList(qualifier)),
                        );
                        type_list!(2, arguments);
                    }
                    crate::ComputedType::Awaited(target) => {
                        type_tag!(48);
                        type_child!(0, target);
                    }
                    crate::ComputedType::This => type_tag!(49),
                },
                crate::TypeExpr::Unknown(value) => {
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
                    emit!(TypedEdgeRole::TupleLabel(index), TypedPlanTarget::Atom(label));
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
                    crate::ObjectMember::Property { key, ty, optional, readonly } => {
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
                    crate::ObjectMember::Method { key, signature, optional } => {
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
                    crate::ObjectMember::Index { parameter, key, value, readonly } => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(2));
                        emit!(TypedEdgeRole::ObjectParameter(index), TypedPlanTarget::Atom(parameter));
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
                    crate::ObjectMember::Call(signature) => {
                        emit!(TypedEdgeRole::ObjectKind(index), TypedPlanTarget::Scalar(3));
                        emit!(
                            TypedEdgeRole::ObjectType(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(signature)),
                        );
                    }
                    crate::ObjectMember::Construct(signature) => {
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
                    crate::TemplatePart::Bytes(atom) => {
                        emit!(TypedEdgeRole::TemplatePart(index), TypedPlanTarget::Scalar(0));
                        emit!(TypedEdgeRole::TemplatePart(index), TypedPlanTarget::Atom(atom));
                    }
                    crate::TemplatePart::Placeholder(ty) => {
                        emit!(TypedEdgeRole::TemplatePart(index), TypedPlanTarget::Scalar(1));
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
                emit!(TypedEdgeRole::ParameterName(index), TypedPlanTarget::Atom(value.name));
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
                    crate::TypeParameterKind::Type { inference } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(0),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(type_parameter_inference(inference)),
                        );
                    }
                    crate::TypeParameterKind::ConstValue { value_type } => {
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Scalar(1),
                        );
                        emit!(
                            TypedEdgeRole::ParameterKind(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(value_type)),
                        );
                    }
                    crate::TypeParameterKind::Lifetime => emit!(
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
            let values = ir.type_parameter_bounds(id).ok_or(missing_node(ir, node)?)?;
            for (index, value) in values.iter().copied().enumerate() {
                let index = list_index(node, index)?;
                match value {
                    crate::TypeParameterBound::Type(ty) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(0));
                        emit!(
                            TypedEdgeRole::BoundValue(index),
                            TypedPlanTarget::Node(TypedPlanNode::Type(ty)),
                        );
                    }
                    crate::TypeParameterBound::Lifetime(atom) => {
                        emit!(TypedEdgeRole::BoundKind(index), TypedPlanTarget::Scalar(1));
                        emit!(TypedEdgeRole::BoundValue(index), TypedPlanTarget::Atom(atom));
                    }
                }
            }
        }
    }
    Ok(())
}

fn for_each_property_key(
    key: crate::PropertyKey,
    index: u32,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    let (tag, target) = match key {
        crate::PropertyKey::Named(atom) => (0, TypedPlanTarget::Atom(atom)),
        crate::PropertyKey::Private(atom) => (1, TypedPlanTarget::Atom(atom)),
        crate::PropertyKey::Numeric(atom) => (2, TypedPlanTarget::Atom(atom)),
        crate::PropertyKey::Computed(ty) => {
            (3, TypedPlanTarget::Node(TypedPlanNode::Type(ty)))
        }
    };
    sink(TypedPlanEdge {
        role: TypedEdgeRole::ObjectKey(index),
        target: TypedPlanTarget::Scalar(tag),
    })?;
    sink(TypedPlanEdge {
        role: TypedEdgeRole::ObjectKey(index),
        target,
    })
}

fn primary_requirement(value: crate::TypeParameterPrimaryRequirement) -> u64 {
    match value {
        crate::TypeParameterPrimaryRequirement::None => 0,
        crate::TypeParameterPrimaryRequirement::Reference { nullable: false } => 1,
        crate::TypeParameterPrimaryRequirement::Reference { nullable: true } => 2,
        crate::TypeParameterPrimaryRequirement::Value => 3,
        crate::TypeParameterPrimaryRequirement::Unmanaged => 4,
        crate::TypeParameterPrimaryRequirement::NotNull => 5,
        crate::TypeParameterPrimaryRequirement::Default => 6,
    }
}

fn tuple_element_kind(value: crate::TupleElementKind) -> u64 {
    match value {
        crate::TupleElementKind::Required => 0,
        crate::TupleElementKind::Optional => 1,
        crate::TupleElementKind::Rest => 2,
    }
}

fn variance(value: crate::Variance) -> u64 {
    match value {
        crate::Variance::Invariant => 0,
        crate::Variance::Covariant => 1,
        crate::Variance::Contravariant => 2,
        crate::Variance::Bivariant => 3,
    }
}

fn type_parameter_inference(value: crate::TypeParameterInference) -> u64 {
    match value {
        crate::TypeParameterInference::Ordinary => 0,
        crate::TypeParameterInference::Const => 1,
    }
}

fn list_index(node: TypedPlanNode, index: usize) -> Result<u32, TypedPlanError> {
    u32::try_from(index)
        .map_err(|_| TypedPlanFault::ListIndexOverflow { node, index }.into())
}

fn missing_node(ir: &Ir, node: TypedPlanNode) -> Result<TypedPlanFault, TypedPlanError> {
    let count = TypedPlanCounts::from_ir(ir)?.at(node.domain());
    Ok(TypedPlanFault::MissingNode { node, count })
}

fn builtin_type(value: crate::BuiltinType) -> u64 {
    match value {
        crate::BuiltinType::Unit => 0,
        crate::BuiltinType::Never => 1,
        crate::BuiltinType::Bool => 2,
        crate::BuiltinType::LegacyChar => 3,
        crate::BuiltinType::I8 => 4,
        crate::BuiltinType::I16 => 5,
        crate::BuiltinType::I32 => 6,
        crate::BuiltinType::I64 => 7,
        crate::BuiltinType::I128 => 8,
        crate::BuiltinType::U8 => 9,
        crate::BuiltinType::U16 => 10,
        crate::BuiltinType::U32 => 11,
        crate::BuiltinType::U64 => 12,
        crate::BuiltinType::U128 => 13,
        crate::BuiltinType::F16 => 14,
        crate::BuiltinType::F32 => 15,
        crate::BuiltinType::F64 => 16,
        crate::BuiltinType::String => 17,
        crate::BuiltinType::Bytes => 18,
        crate::BuiltinType::Object => 19,
        crate::BuiltinType::Any => 20,
        crate::BuiltinType::Unknown => 21,
        crate::BuiltinType::Void => 22,
        crate::BuiltinType::Number => 23,
        crate::BuiltinType::BigInt => 24,
        crate::BuiltinType::Symbol => 25,
        crate::BuiltinType::UniqueSymbol => 26,
        crate::BuiltinType::Null => 27,
        crate::BuiltinType::Undefined => 28,
        crate::BuiltinType::None_ => 30,
        crate::BuiltinType::List => 31,
        crate::BuiltinType::Dict => 32,
        crate::BuiltinType::Set => 33,
        crate::BuiltinType::FrozenSet => 34,
        crate::BuiltinType::Complex => 36,
        crate::BuiltinType::Decimal => 37,
        crate::BuiltinType::ArbitraryInteger => 38,
        crate::BuiltinType::NativeSignedInteger => 39,
        crate::BuiltinType::NativeUnsignedInteger => 40,
        crate::BuiltinType::PointerAddressInteger => 41,
    }
}

fn variadic_form(value: crate::VariadicForm) -> u64 {
    match value {
        crate::FunctionVariadicForm::None => 0,
        crate::FunctionVariadicForm::TypedLast => 1,
        crate::FunctionVariadicForm::CUnbounded => 2,
    }
}

fn mutability(value: crate::Mutability) -> u64 {
    match value {
        crate::Mutability::Immutable => 0,
        crate::Mutability::Mutable => 1,
    }
}

fn cxx_reference_category(value: crate::CxxReferenceCategory) -> u64 {
    match value {
        crate::CxxReferenceCategory::Lvalue => 0,
        crate::CxxReferenceCategory::Rvalue => 1,
    }
}

fn native_character_role(value: crate::NativeCharacterRole) -> u64 {
    match value {
        crate::NativeCharacterRole::UnicodeScalar => 0,
        crate::NativeCharacterRole::Utf16CodeUnit => 1,
        crate::NativeCharacterRole::Utf32CodeUnit => 2,
        crate::NativeCharacterRole::CPlainSigned => 3,
        crate::NativeCharacterRole::CPlainUnsigned => 4,
        crate::NativeCharacterRole::CSigned => 5,
        crate::NativeCharacterRole::CUnsigned => 6,
        crate::NativeCharacterRole::CWideSigned => 7,
        crate::NativeCharacterRole::CWideUnsigned => 8,
        crate::NativeCharacterRole::CWideSignednessUnavailable => 9,
    }
}

fn annotation_kind(value: crate::AnnotationKind) -> u64 {
    match value {
        crate::AnnotationKind::Readonly => 0,
        crate::AnnotationKind::NullableValue => 1,
        crate::AnnotationKind::NullableReference => 2,
        crate::AnnotationKind::NonNullableReference => 3,
    }
}

fn channel_direction(value: crate::ChannelDirection) -> u64 {
    match value {
        crate::ChannelDirection::Both => 0,
        crate::ChannelDirection::Send => 1,
        crate::ChannelDirection::Receive => 2,
    }
}

fn mapped_modifier(value: crate::MappedModifier) -> u64 {
    match value {
        crate::MappedModifier::Preserve => 0,
        crate::MappedModifier::Add => 1,
        crate::MappedModifier::Remove => 2,
    }
}

fn unknown_reason(value: crate::UnknownReason) -> u64 {
    match value {
        crate::UnknownReason::Unannotated => 0,
        crate::UnknownReason::DynamicallyTyped => 1,
        crate::UnknownReason::UnresolvedLocalName => 2,
        crate::UnknownReason::UnresolvedExternal => 3,
        crate::UnknownReason::TruncatedAtDepthLimit => 4,
        crate::UnknownReason::OracleGap => 5,
        crate::UnknownReason::NoIrRepresentation => 6,
        crate::UnknownReason::Error => 7,
    }
}
