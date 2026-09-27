//! Declaration facts read from the same parse the tags query already ran on.
//!
//! The structural lane observes deprecation and obligation for every
//! language it parses. Deprecation comes from the attributes, annotations,
//! and decorators written on the declaration and from the documentation
//! conventions of its language. Obligation comes from the node that
//! declares a member and the contract it sits in: a body or its absence, and
//! the modifiers that say `abstract`, `default`, `virtual`, or `?`. It never
//! comes from how the signature text happens to end.

use crate::facts::{
    DeclarationFacts, Fact, Obligation, deprecation_in_attribute, deprecation_in_documentation,
    is_abstract_method_decorator,
};
use crate::syntax::{SourceLanguage, is_comment, node_text};
use tree_sitter::Node;

/// Parents a declaration's own attributes may hang on instead of the
/// captured node itself: a Python `decorated_definition`, a C++ declaration
/// around its declarator, an exported TypeScript statement.
const ATTRIBUTE_PARENTS: [&str; 8] = [
    "decorated_definition",
    "pointer_declarator",
    "reference_declarator",
    "declaration",
    "field_declaration",
    "function_definition",
    "export_statement",
    "template_declaration",
];

/// Reads every fact this lane observes for one captured declaration.
pub(crate) fn declaration_facts(
    language: SourceLanguage,
    node: Node<'_>,
    source: &str,
    documentation: &str,
) -> DeclarationFacts {
    let decorations = decorations(node, source);
    let written = decorations
        .iter()
        .find_map(|text| deprecation_in_attribute(language, text));
    let documented = deprecation_in_documentation(language, documentation);
    let deprecation = match written {
        Some(notice) => Some(notice.completed_by(documented.as_ref())),
        None => documented,
    };
    DeclarationFacts {
        deprecation: Fact::observed(deprecation),
        obligation: Fact::observed(obligation(language, node, source, &decorations)),
    }
}

/// Returns whether a node is one attribute, annotation, or decorator.
const fn is_attribute(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"attribute_item"
            | b"attribute_list"
            | b"attribute_declaration"
            | b"attribute_specifier"
            | b"ms_declspec_modifier"
            | b"annotation"
            | b"marker_annotation"
            | b"decorator"
    )
}

/// Collects the text of every attribute written on the declaration: its own
/// attribute children (and those inside a Java `modifiers` node), the
/// contiguous attribute run written just above it, and the same for the few
/// parents that carry a declaration's attributes for it.
fn decorations<'a>(node: Node<'_>, source: &'a str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut current = node;
    for _ in 0..3 {
        attributes_within(current, source, &mut found);
        let mut previous = current.prev_named_sibling();
        while let Some(candidate) = previous {
            previous = candidate.prev_named_sibling();
            if is_attribute(candidate.kind()) {
                found.push(node_text(candidate, source));
            } else if !is_comment(candidate.kind()) {
                break;
            }
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if !ATTRIBUTE_PARENTS.contains(&parent.kind()) {
            break;
        }
        current = parent;
    }
    found
}

fn attributes_within<'a>(node: Node<'_>, source: &'a str, found: &mut Vec<&'a str>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if is_attribute(child.kind()) {
            found.push(node_text(child, source));
        } else if child.kind() == "modifiers" {
            let mut inner = child.walk();
            for modifier in child.named_children(&mut inner) {
                if is_attribute(modifier.kind()) {
                    found.push(node_text(modifier, source));
                }
            }
        }
    }
}

fn obligation(
    language: SourceLanguage,
    node: Node<'_>,
    source: &str,
    decorations: &[&str],
) -> Option<Obligation> {
    match language {
        SourceLanguage::Rust => rust_obligation(node),
        SourceLanguage::Java => java_obligation(node, source),
        SourceLanguage::CSharp => csharp_obligation(node, source),
        SourceLanguage::TypeScript => typescript_obligation(node),
        SourceLanguage::Python => python_obligation(node, source, decorations),
        SourceLanguage::Go => (node.kind() == "method_elem").then_some(Obligation::Required),
        SourceLanguage::Clang => clang_obligation(node, source),
    }
}

/// A trait item without a body is required; one with a body is provided.
fn rust_obligation(node: Node<'_>) -> Option<Obligation> {
    let in_trait = node
        .parent()
        .filter(|list| list.kind() == "declaration_list")
        .and_then(|list| list.parent())
        .is_some_and(|owner| owner.kind() == "trait_item");
    if !in_trait {
        return None;
    }
    match node.kind() {
        "function_signature_item" | "associated_type" => Some(Obligation::Required),
        "function_item" => Some(Obligation::Provided),
        "const_item" => Some(if node.child_by_field_name("value").is_some() {
            Obligation::Provided
        } else {
            Obligation::Required
        }),
        _ => None,
    }
}

/// The whitespace-separated words of a node's `modifiers`/`modifier` children.
fn modifier_words<'a>(node: Node<'_>, source: &'a str) -> Vec<&'a str> {
    let mut words = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "modifiers" | "modifier") {
            words.extend(node_text(child, source).split_whitespace());
        }
    }
    words
}

/// Java interface methods are required unless `default` (provided) or
/// `static`/`private` (not part of what an implementor owes). An abstract
/// class owes its `abstract` methods and gives the rest.
fn java_obligation(node: Node<'_>, source: &str) -> Option<Obligation> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let words = modifier_words(node, source);
    let has = |word: &str| words.contains(&word);
    let container = node.parent()?;
    match container.kind() {
        "interface_body" => {
            if has("static") || has("private") {
                None
            } else if has("default") || node.child_by_field_name("body").is_some() {
                Some(Obligation::Provided)
            } else {
                Some(Obligation::Required)
            }
        }
        "class_body" => {
            let class = container.parent()?;
            let abstract_class = modifier_words(class, source).contains(&"abstract");
            if has("abstract") {
                Some(Obligation::Required)
            } else if abstract_class && !has("static") && !has("private") {
                Some(Obligation::Provided)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether a C# member carries its own implementation.
fn csharp_has_body(node: Node<'_>) -> bool {
    if node.child_by_field_name("body").is_some() || node.child_by_field_name("value").is_some() {
        return true;
    }
    let Some(accessors) = node.child_by_field_name("accessors") else {
        return false;
    };
    let mut cursor = accessors.walk();
    accessors
        .named_children(&mut cursor)
        .any(|accessor| accessor.child_by_field_name("body").is_some())
}

/// A C# interface member with a body is a default implementation; one
/// without is required. An abstract class owes its `abstract` members.
fn csharp_obligation(node: Node<'_>, source: &str) -> Option<Obligation> {
    if !matches!(
        node.kind(),
        "method_declaration" | "property_declaration" | "event_declaration" | "indexer_declaration"
    ) {
        return None;
    }
    let words = modifier_words(node, source);
    let has = |word: &str| words.contains(&word);
    let owner = node
        .parent()
        .filter(|list| list.kind() == "declaration_list")?
        .parent()?;
    match owner.kind() {
        "interface_declaration" => {
            if has("static") && !has("abstract") {
                None
            } else if has("abstract") || !csharp_has_body(node) {
                Some(Obligation::Required)
            } else {
                Some(Obligation::Provided)
            }
        }
        "class_declaration" | "record_declaration" => {
            let abstract_class = modifier_words(owner, source).contains(&"abstract");
            if has("abstract") {
                Some(Obligation::Required)
            } else if abstract_class && !has("static") && !has("private") {
                Some(Obligation::Provided)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == kind)
}

/// Interface and object-type members are required, or optional when
/// written with `?`; an abstract class owes its `abstract` members and
/// gives the rest.
fn typescript_obligation(node: Node<'_>) -> Option<Obligation> {
    match node.kind() {
        "method_signature" | "property_signature" => Some(if has_child_kind(node, "?") {
            Obligation::Optional
        } else {
            Obligation::Required
        }),
        "abstract_method_signature" => Some(Obligation::Required),
        "method_definition" | "public_field_definition" => {
            let class = node.parent()?.parent()?;
            if class.kind() != "abstract_class_declaration" {
                return None;
            }
            Some(if has_child_kind(node, "abstract") {
                Obligation::Required
            } else {
                Obligation::Provided
            })
        }
        _ => None,
    }
}

/// Terminal names a Python base list mentions (`abc.ABC` → `ABC`,
/// `Protocol[T]` → `Protocol`, `metaclass=ABCMeta` → `ABCMeta`).
fn base_names(bases: &str) -> impl Iterator<Item = &str> {
    bases
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .filter(|token| !token.is_empty())
        .map(|token| token.rsplit('.').next().unwrap_or(token))
}

/// Whether a Python function body only states its shape: `...`, `pass`,
/// or a docstring.
fn stub_body(node: Node<'_>) -> bool {
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor).all(|statement| {
        statement.kind() == "pass_statement"
            || (statement.kind() == "expression_statement"
                && statement
                    .named_child(0)
                    .is_some_and(|expression| matches!(expression.kind(), "ellipsis" | "string")))
            || is_comment(statement.kind())
    })
}

/// An `@abstractmethod` is required. Inside a `Protocol`, a stub is
/// required and a written body is provided; inside an ABC, a concrete
/// method is provided.
fn python_obligation(node: Node<'_>, source: &str, decorations: &[&str]) -> Option<Obligation> {
    if node.kind() != "function_definition" {
        return None;
    }
    let mut holder = node.parent()?;
    if holder.kind() == "decorated_definition" {
        holder = holder.parent()?;
    }
    if holder.kind() != "block" {
        return None;
    }
    let class = holder.parent().filter(|class| class.kind() == "class_definition")?;
    if decorations
        .iter()
        .any(|decorator| is_abstract_method_decorator(decorator))
    {
        return Some(Obligation::Required);
    }
    let bases = class
        .child_by_field_name("superclasses")
        .map_or("", |bases| node_text(bases, source));
    let mut protocol = false;
    let mut abstract_base = false;
    for name in base_names(bases) {
        protocol |= name == "Protocol";
        abstract_base |= matches!(name, "ABC" | "ABCMeta");
    }
    if protocol {
        Some(if stub_body(node) {
            Obligation::Required
        } else {
            Obligation::Provided
        })
    } else if abstract_base {
        Some(Obligation::Provided)
    } else {
        None
    }
}

/// A pure virtual member function (`= 0`) is required; any other virtual
/// member function is provided.
fn clang_obligation(node: Node<'_>, source: &str) -> Option<Obligation> {
    if node.kind() != "function_declarator" {
        return None;
    }
    let mut owner = node.parent()?;
    while matches!(owner.kind(), "pointer_declarator" | "reference_declarator") {
        owner = owner.parent()?;
    }
    if !matches!(owner.kind(), "field_declaration" | "function_definition" | "declaration") {
        return None;
    }
    let pure = has_child_kind(owner, "pure_virtual_clause")
        || owner
            .child_by_field_name("default_value")
            .is_some_and(|value| node_text(value, source).trim() == "0");
    let head = source
        .get(owner.start_byte()..node.start_byte())
        .unwrap_or_default();
    let is_virtual = head.split_whitespace().any(|word| word == "virtual");
    if pure && is_virtual {
        Some(Obligation::Required)
    } else if is_virtual {
        Some(Obligation::Provided)
    } else {
        None
    }
}
