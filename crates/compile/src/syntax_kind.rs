use crate::{DeclarationKind, SourceLanguage};
use tree_sitter::Node;

pub(crate) fn declaration_kind(
    _language: SourceLanguage,
    tag: &str,
    node_kind: &str,
) -> DeclarationKind {
    parser_native_kind(node_kind).unwrap_or_else(|| DeclarationKind::from_name(tag))
}

/// Returns the kind of one captured definition, read from its parse node.
///
/// [`declaration_kind`] settles what a grammar's node *name* says. Four
/// languages say too little there, so the shape under the node decides:
///
/// * Go spells every named type `type_spec`; the type it names (`struct_type`,
///   `interface_type`) is what makes it a struct or an interface, and a reader
///   comparing it with the compiler-backed row must find the same word.
/// * Python has no constants, and the stock query tags every module-level
///   assignment one. A name written in the screaming-case convention is a
///   constant; any other binding is a variable, which is also what the
///   compiler-backed lane calls it.
/// * A TypeScript `let` binding is a variable, not a constant.
/// * C# uses `record_declaration` for both record classes and record structs;
///   only the latter have a `struct` keyword before the declaration name.
pub(crate) fn declaration_kind_at(
    language: SourceLanguage,
    tag: &str,
    node: Node<'_>,
    name: &str,
) -> DeclarationKind {
    refined_kind(language, tag, node, name)
        .unwrap_or_else(|| declaration_kind(language, tag, node.kind()))
}

fn refined_kind(
    language: SourceLanguage,
    tag: &str,
    node: Node<'_>,
    name: &str,
) -> Option<DeclarationKind> {
    match (language, node.kind()) {
        (SourceLanguage::CSharp, "record_declaration") => {
            let name = node.child_by_field_name("name")?;
            let mut cursor = node.walk();
            let is_struct = node
                .children(&mut cursor)
                .take_while(|child| child.start_byte() < name.start_byte())
                .any(|child| child.kind() == "struct");
            Some(if is_struct {
                DeclarationKind::Struct
            } else {
                DeclarationKind::Class
            })
        }
        (SourceLanguage::Go, "type_spec") => match node.child_by_field_name("type")?.kind() {
            "struct_type" => Some(DeclarationKind::Struct),
            "interface_type" => Some(DeclarationKind::Interface),
            _ => None,
        },
        (SourceLanguage::Python, "assignment") if tag == "constant" => {
            Some(python_binding_kind(name))
        }
        (SourceLanguage::TypeScript, "lexical_declaration") if tag == "constant" => {
            let keyword = node.child(0)?;
            matches!(keyword.kind(), "let").then_some(DeclarationKind::Variable)
        }
        _ => None,
    }
}

/// Returns what a module-level Python binding is called: a name written in
/// the screaming-case convention is a constant, any other name a variable.
pub fn python_binding_kind(name: &str) -> DeclarationKind {
    if is_constant_name(name) {
        DeclarationKind::Constant
    } else {
        DeclarationKind::Variable
    }
}

/// Returns whether a name is spelled like a constant: at least one letter,
/// and every letter upper-case (`LIMIT`, `_CACHE_SIZE`, `HTTP2_PORT`).
pub(crate) fn is_constant_name(name: &str) -> bool {
    let mut letters = false;
    for character in name.chars() {
        if character.is_alphabetic() {
            if !character.is_uppercase() {
                return false;
            }
            letters = true;
        } else if !(character.is_ascii_digit() || character == '_') {
            return false;
        }
    }
    letters
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
        "enum_variant"
        | "enumerator"
        | "enum_constant"
        | "enum_member_declaration"
        | "enum_assignment" => Some(DeclarationKind::Variant),
        "trait_item" => Some(DeclarationKind::Trait),
        "union_item" | "union_specifier" => Some(DeclarationKind::Union),
        "type_item" => Some(DeclarationKind::Type),
        "namespace_definition" => Some(DeclarationKind::Module),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_constant_name, python_binding_kind};
    use crate::DeclarationKind;

    #[test]
    fn a_name_in_screaming_case_is_a_constant_and_any_other_is_a_variable() {
        for constant in ["LIMIT", "_CACHE_SIZE", "HTTP2_PORT", "__ALL__", "X"] {
            assert!(is_constant_name(constant), "{constant}");
            assert_eq!(python_binding_kind(constant), DeclarationKind::Constant);
        }
        for variable in [
            "counter",
            "__version__",
            "Registry",
            "_",
            "__",
            "a1",
            "",
            "ÄB1c",
        ] {
            assert!(!is_constant_name(variable), "{variable}");
            assert_eq!(python_binding_kind(variable), DeclarationKind::Variable);
        }
    }
}
