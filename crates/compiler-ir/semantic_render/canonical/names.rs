//! Closed stable names for semantic type constructors and modifiers.

use crate::{
    AnnotationKind, BuiltinType, ChannelDirection, CxxReferenceCategory, ItemKind, MappedModifier,
    Mutability, NativeCharacterRole, TupleElementKind, UnknownReason, VariadicForm,
};

pub(super) const fn builtin_name(value: BuiltinType) -> &'static str {
    match value {
        BuiltinType::Unit => "unit",
        BuiltinType::Never => "never",
        BuiltinType::Bool => "bool",
        BuiltinType::LegacyChar => "legacy-char",
        BuiltinType::I8 => "i8",
        BuiltinType::I16 => "i16",
        BuiltinType::I32 => "i32",
        BuiltinType::I64 => "i64",
        BuiltinType::I128 => "i128",
        BuiltinType::U8 => "u8",
        BuiltinType::U16 => "u16",
        BuiltinType::U32 => "u32",
        BuiltinType::U64 => "u64",
        BuiltinType::U128 => "u128",
        BuiltinType::F16 => "f16",
        BuiltinType::F32 => "f32",
        BuiltinType::F64 => "f64",
        BuiltinType::String => "string",
        BuiltinType::Bytes => "bytes",
        BuiltinType::Object => "object",
        BuiltinType::Any => "any",
        BuiltinType::Unknown => "unknown",
        BuiltinType::Void => "void",
        BuiltinType::Number => "number",
        BuiltinType::BigInt => "bigint",
        BuiltinType::Symbol => "symbol",
        BuiltinType::UniqueSymbol => "unique-symbol",
        BuiltinType::Null => "null",
        BuiltinType::Undefined => "undefined",
        BuiltinType::None_ => "none",
        BuiltinType::List => "list",
        BuiltinType::Dict => "dict",
        BuiltinType::Set => "set",
        BuiltinType::FrozenSet => "frozen-set",
        BuiltinType::Complex => "complex",
        BuiltinType::Decimal => "decimal",
        BuiltinType::ArbitraryInteger => "arbitrary-integer",
        BuiltinType::NativeSignedInteger => "native-signed-integer",
        BuiltinType::NativeUnsignedInteger => "native-unsigned-integer",
        BuiltinType::PointerAddressInteger => "pointer-address-integer",
    }
}

pub(super) const fn unknown_name(value: UnknownReason) -> &'static str {
    match value {
        UnknownReason::Unannotated => "unannotated",
        UnknownReason::DynamicallyTyped => "dynamic",
        UnknownReason::UnresolvedLocalName => "unresolved-local",
        UnknownReason::UnresolvedExternal => "unresolved-external",
        UnknownReason::TruncatedAtDepthLimit => "truncated-depth",
        UnknownReason::OracleGap => "oracle-gap",
        UnknownReason::NoIrRepresentation => "unrepresented",
        UnknownReason::Error => "error",
    }
}

pub(super) const fn mutability_name(value: Mutability) -> &'static str {
    match value {
        Mutability::Immutable => "immutable",
        Mutability::Mutable => "mutable",
    }
}

pub(super) const fn cxx_reference_name(value: CxxReferenceCategory) -> &'static str {
    match value {
        CxxReferenceCategory::Lvalue => "lvalue",
        CxxReferenceCategory::Rvalue => "rvalue",
    }
}

pub(super) const fn variadic_name(value: VariadicForm) -> &'static str {
    match value {
        VariadicForm::None => "none",
        VariadicForm::TypedLast => "typed-last",
        VariadicForm::CUnbounded => "c-unbounded",
    }
}

pub(super) const fn character_role_name(value: NativeCharacterRole) -> &'static str {
    match value {
        NativeCharacterRole::UnicodeScalar => "unicode-scalar",
        NativeCharacterRole::Utf16CodeUnit => "utf16-code-unit",
        NativeCharacterRole::Utf32CodeUnit => "utf32-code-unit",
        NativeCharacterRole::CPlainSigned => "c-plain-signed",
        NativeCharacterRole::CPlainUnsigned => "c-plain-unsigned",
        NativeCharacterRole::CSigned => "c-signed",
        NativeCharacterRole::CUnsigned => "c-unsigned",
        NativeCharacterRole::CWideSigned => "c-wide-signed",
        NativeCharacterRole::CWideUnsigned => "c-wide-unsigned",
        NativeCharacterRole::CWideSignednessUnavailable => "c-wide-signedness-unavailable",
    }
}

pub(super) const fn annotation_name(value: AnnotationKind) -> &'static str {
    match value {
        AnnotationKind::Readonly => "readonly",
        AnnotationKind::NullableValue => "nullable-value",
        AnnotationKind::NullableReference => "nullable-reference",
        AnnotationKind::NonNullableReference => "nonnullable-reference",
    }
}

pub(super) const fn channel_name(value: ChannelDirection) -> &'static str {
    match value {
        ChannelDirection::Both => "both",
        ChannelDirection::Send => "send",
        ChannelDirection::Receive => "receive",
    }
}

pub(super) const fn modifier_name(value: MappedModifier) -> &'static str {
    match value {
        MappedModifier::Preserve => "preserve",
        MappedModifier::Add => "add",
        MappedModifier::Remove => "remove",
    }
}

pub(super) const fn tuple_kind_name(value: TupleElementKind) -> &'static str {
    match value {
        TupleElementKind::Required => "required",
        TupleElementKind::Optional => "optional",
        TupleElementKind::Rest => "rest",
    }
}

pub(super) const fn item_kind_name(value: ItemKind) -> &'static str {
    match value {
        ItemKind::Module => "module",
        ItemKind::Record => "record",
        ItemKind::Field => "field",
        ItemKind::Parameter => "parameter",
        ItemKind::Variant => "variant",
        ItemKind::Function => "function",
        ItemKind::TypeAlias => "alias",
        ItemKind::Trait => "trait",
        ItemKind::Implementation => "implementation",
        ItemKind::Enum => "enum",
        ItemKind::Constant => "constant",
        ItemKind::Static => "static",
        ItemKind::Reexport => "reexport",
        ItemKind::Macro => "macro",
        ItemKind::Namespace => "namespace",
    }
}
