use crate::{DeclarationKind, SourceLanguage};

pub(crate) fn declaration_kind(
    _language: SourceLanguage,
    tag: &str,
    node_kind: &str,
) -> DeclarationKind {
    parser_native_kind(node_kind).unwrap_or_else(|| DeclarationKind::from_name(tag))
}

/// Returns the kind a grammar's own node name settles, whatever it was tagged.
///
/// A stock tags query answers what a *reference* may point at, so several
/// grammars tag an enum `definition.type` and a record `definition.class`.
/// The node name is the grammar's own authority on what was declared, and a
/// product that groups by kind needs that answer rather than the tag's.
fn parser_native_kind(node_kind: &str) -> Option<DeclarationKind> {
    match node_kind {
        "struct_item" | "struct_specifier" | "struct_declaration" | "record_declaration" => {
            Some(DeclarationKind::Struct)
        }
        "enum_item" | "enum_specifier" | "enum_declaration" => Some(DeclarationKind::Enum),
        "enum_variant" | "enumerator" | "enum_constant" | "enum_member_declaration"
        | "enum_assignment" => Some(DeclarationKind::Variant),
        "trait_item" => Some(DeclarationKind::Trait),
        "union_item" | "union_specifier" => Some(DeclarationKind::Union),
        "type_item" => Some(DeclarationKind::Type),
        "namespace_definition" => Some(DeclarationKind::Module),
        _ => None,
    }
}
