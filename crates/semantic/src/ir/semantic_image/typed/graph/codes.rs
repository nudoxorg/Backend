//! Scalar codes for one typed dependency edge.
//!
//! Each closed vocabulary value becomes the same little-endian tag the
//! full-image decoder proves on reopen. The graph walk emits those tags
//! without owning the code tables.

use super::super::{
    TypedEdgeRole, TypedPlanCounts, TypedPlanEdge, TypedPlanError, TypedPlanFault, TypedPlanNode,
    TypedPlanTarget,
};
use crate::ir::Ir;

pub(super) fn for_each_property_key(
    key: crate::ir::PropertyKey,
    index: u32,
    sink: &mut impl FnMut(TypedPlanEdge) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    let (tag, target) = match key {
        crate::ir::PropertyKey::Named(atom) => (0, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Private(atom) => (1, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Numeric(atom) => (2, TypedPlanTarget::Atom(atom)),
        crate::ir::PropertyKey::Computed(ty) => (3, TypedPlanTarget::Node(TypedPlanNode::Type(ty))),
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

pub(super) fn primary_requirement(value: crate::ir::TypeParameterPrimaryRequirement) -> u64 {
    match value {
        crate::ir::TypeParameterPrimaryRequirement::None => 0,
        crate::ir::TypeParameterPrimaryRequirement::Reference { nullable: false } => 1,
        crate::ir::TypeParameterPrimaryRequirement::Reference { nullable: true } => 2,
        crate::ir::TypeParameterPrimaryRequirement::Value => 3,
        crate::ir::TypeParameterPrimaryRequirement::Unmanaged => 4,
        crate::ir::TypeParameterPrimaryRequirement::NotNull => 5,
        crate::ir::TypeParameterPrimaryRequirement::Default => 6,
    }
}

pub(super) fn tuple_element_kind(value: crate::ir::TupleElementKind) -> u64 {
    match value {
        crate::ir::TupleElementKind::Required => 0,
        crate::ir::TupleElementKind::Optional => 1,
        crate::ir::TupleElementKind::Rest => 2,
    }
}

pub(super) fn variance(value: crate::ir::Variance) -> u64 {
    match value {
        crate::ir::Variance::Invariant => 0,
        crate::ir::Variance::Covariant => 1,
        crate::ir::Variance::Contravariant => 2,
        crate::ir::Variance::Bivariant => 3,
    }
}

pub(super) fn type_parameter_inference(value: crate::ir::TypeParameterInference) -> u64 {
    match value {
        crate::ir::TypeParameterInference::Ordinary => 0,
        crate::ir::TypeParameterInference::Const => 1,
    }
}

pub(super) fn list_index(node: TypedPlanNode, index: usize) -> Result<u32, TypedPlanError> {
    u32::try_from(index).map_err(|_| TypedPlanFault::ListIndexOverflow { node, index }.into())
}

pub(super) fn missing_node(ir: &Ir, node: TypedPlanNode) -> Result<TypedPlanFault, TypedPlanError> {
    let count = TypedPlanCounts::from_ir(ir)?.at(node.domain());
    Ok(TypedPlanFault::MissingNode { node, count })
}

pub(super) fn builtin_type(value: crate::ir::BuiltinType) -> u64 {
    match value {
        crate::ir::BuiltinType::Unit => 0,
        crate::ir::BuiltinType::Never => 1,
        crate::ir::BuiltinType::Bool => 2,
        crate::ir::BuiltinType::LegacyChar => 3,
        crate::ir::BuiltinType::I8 => 4,
        crate::ir::BuiltinType::I16 => 5,
        crate::ir::BuiltinType::I32 => 6,
        crate::ir::BuiltinType::I64 => 7,
        crate::ir::BuiltinType::I128 => 8,
        crate::ir::BuiltinType::U8 => 9,
        crate::ir::BuiltinType::U16 => 10,
        crate::ir::BuiltinType::U32 => 11,
        crate::ir::BuiltinType::U64 => 12,
        crate::ir::BuiltinType::U128 => 13,
        crate::ir::BuiltinType::F16 => 14,
        crate::ir::BuiltinType::F32 => 15,
        crate::ir::BuiltinType::F64 => 16,
        crate::ir::BuiltinType::String => 17,
        crate::ir::BuiltinType::Bytes => 18,
        crate::ir::BuiltinType::Object => 19,
        crate::ir::BuiltinType::Any => 20,
        crate::ir::BuiltinType::Unknown => 21,
        crate::ir::BuiltinType::Void => 22,
        crate::ir::BuiltinType::Number => 23,
        crate::ir::BuiltinType::BigInt => 24,
        crate::ir::BuiltinType::Symbol => 25,
        crate::ir::BuiltinType::UniqueSymbol => 26,
        crate::ir::BuiltinType::Null => 27,
        crate::ir::BuiltinType::Undefined => 28,
        crate::ir::BuiltinType::None_ => 30,
        crate::ir::BuiltinType::List => 31,
        crate::ir::BuiltinType::Dict => 32,
        crate::ir::BuiltinType::Set => 33,
        crate::ir::BuiltinType::FrozenSet => 34,
        crate::ir::BuiltinType::Complex => 36,
        crate::ir::BuiltinType::Decimal => 37,
        crate::ir::BuiltinType::ArbitraryInteger => 38,
        crate::ir::BuiltinType::NativeSignedInteger => 39,
        crate::ir::BuiltinType::NativeUnsignedInteger => 40,
        crate::ir::BuiltinType::PointerAddressInteger => 41,
    }
}

pub(super) fn variadic_form(value: crate::ir::VariadicForm) -> u64 {
    match value {
        crate::ir::FunctionVariadicForm::None => 0,
        crate::ir::FunctionVariadicForm::TypedLast => 1,
        crate::ir::FunctionVariadicForm::CUnbounded => 2,
    }
}

pub(super) fn mutability_code(value: crate::ir::Mutability) -> u64 {
    match value {
        crate::ir::Mutability::Immutable => 0,
        crate::ir::Mutability::Mutable => 1,
    }
}

pub(super) fn cxx_reference_category(value: crate::ir::CxxReferenceCategory) -> u64 {
    match value {
        crate::ir::CxxReferenceCategory::Lvalue => 0,
        crate::ir::CxxReferenceCategory::Rvalue => 1,
    }
}

pub(super) fn native_character_role(value: crate::ir::NativeCharacterRole) -> u64 {
    match value {
        crate::ir::NativeCharacterRole::UnicodeScalar => 0,
        crate::ir::NativeCharacterRole::Utf16CodeUnit => 1,
        crate::ir::NativeCharacterRole::Utf32CodeUnit => 2,
        crate::ir::NativeCharacterRole::CPlainSigned => 3,
        crate::ir::NativeCharacterRole::CPlainUnsigned => 4,
        crate::ir::NativeCharacterRole::CSigned => 5,
        crate::ir::NativeCharacterRole::CUnsigned => 6,
        crate::ir::NativeCharacterRole::CWideSigned => 7,
        crate::ir::NativeCharacterRole::CWideUnsigned => 8,
        crate::ir::NativeCharacterRole::CWideSignednessUnavailable => 9,
    }
}

pub(super) fn annotation_kind(value: crate::ir::AnnotationKind) -> u64 {
    match value {
        crate::ir::AnnotationKind::Readonly => 0,
        crate::ir::AnnotationKind::NullableValue => 1,
        crate::ir::AnnotationKind::NullableReference => 2,
        crate::ir::AnnotationKind::NonNullableReference => 3,
    }
}

pub(super) fn channel_direction(value: crate::ir::ChannelDirection) -> u64 {
    match value {
        crate::ir::ChannelDirection::Both => 0,
        crate::ir::ChannelDirection::Send => 1,
        crate::ir::ChannelDirection::Receive => 2,
    }
}

pub(super) fn mapped_modifier(value: crate::ir::MappedModifier) -> u64 {
    match value {
        crate::ir::MappedModifier::Preserve => 0,
        crate::ir::MappedModifier::Add => 1,
        crate::ir::MappedModifier::Remove => 2,
    }
}

pub(super) fn unknown_reason(value: crate::ir::UnknownReason) -> u64 {
    match value {
        crate::ir::UnknownReason::Unannotated => 0,
        crate::ir::UnknownReason::DynamicallyTyped => 1,
        crate::ir::UnknownReason::UnresolvedLocalName => 2,
        crate::ir::UnknownReason::UnresolvedExternal => 3,
        crate::ir::UnknownReason::TruncatedAtDepthLimit => 4,
        crate::ir::UnknownReason::OracleGap => 5,
        crate::ir::UnknownReason::NoIrRepresentation => 6,
        crate::ir::UnknownReason::Error => 7,
    }
}
