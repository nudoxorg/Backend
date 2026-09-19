use crate::{DeclarationKind, SourceLanguage};

pub(crate) fn declaration_kind(
    _language: SourceLanguage,
    tag: &str,
    node_kind: &str,
) -> DeclarationKind {
    parser_native_kind(node_kind).unwrap_or_else(|| DeclarationKind::from_name(tag))
}

fn parser_native_kind(node_kind: &str) -> Option<DeclarationKind> {
    match node_kind {
        "struct_item" | "struct_specifier" => Some(DeclarationKind::Struct),
        "enum_item" | "enum_specifier" => Some(DeclarationKind::Enum),
        "trait_item" => Some(DeclarationKind::Trait),
        "union_item" | "union_specifier" => Some(DeclarationKind::Union),
        "type_item" => Some(DeclarationKind::Type),
        _ => None,
    }
}
