//! Comment and docstring prose attached to a structural declaration.
//!
//! A tags capture is often the declarator, not the statement a reader wrote
//! the comment above. This walk steps over decorators, climbs parents that
//! own no prose of their own, and reads Python docstrings from inside the body.

use super::{bounded, node_text};
use tree_sitter::Node;

pub(super) fn declaration_documentation(mut node: Node<'_>, source: &str) -> String {
    for _ in 0..3 {
        let comments = preceding_comments(node, source);
        if !comments.is_empty() {
            return bounded(&xml_documentation_text(&comments.join("\n")));
        }
        if let Some(docstring) = enclosed_docstring(node, source) {
            return bounded(&docstring);
        }
        let Some(parent) = node.parent() else {
            break;
        };
        if !documentation_parent(parent.kind()) {
            break;
        }
        node = parent;
    }
    String::new()
}

/// Collects the contiguous comment run written immediately above a node.
///
/// An attribute, annotation, or decorator sits between a declaration and its
/// documentation, so the scan steps over one instead of concluding the
/// declaration is undocumented.
fn preceding_comments(node: Node<'_>, source: &str) -> Vec<String> {
    let mut comments = Vec::new();
    let mut previous = node.prev_named_sibling();
    while let Some(candidate) = previous {
        previous = candidate.prev_named_sibling();
        if decorates_a_declaration(candidate.kind()) {
            continue;
        }
        if !is_comment(candidate.kind()) {
            break;
        }
        comments.push(clean_comment(node_text(candidate, source)));
    }
    comments.reverse();
    comments
}

/// Returns a Python definition's docstring, when its body opens with one.
///
/// Python documents a definition from the inside rather than above it, so a
/// class or function with a docstring and no comment would otherwise arrive
/// with no prose at all.
fn enclosed_docstring(node: Node<'_>, source: &str) -> Option<String> {
    if !matches!(node.kind(), "function_definition" | "class_definition") {
        return None;
    }
    let statement = node.child_by_field_name("body")?.named_child(0)?;
    if statement.kind() != "expression_statement" {
        return None;
    }
    let literal = statement.named_child(0)?;
    if literal.kind() != "string" {
        return None;
    }
    let text = node_text(literal, source)
        .trim()
        .trim_start_matches(['r', 'b', 'u', 'f', 'R', 'B', 'U', 'F'])
        .trim_matches(['"', '\''])
        .trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Returns whether a node is a comment some grammar attaches prose to.
///
/// Grammars disagree about the name: Rust and Java say `line_comment` and
/// `block_comment`, everyone else says `comment`. Reading only `comment` is
/// why every Rust and Java declaration arrived undocumented.
const fn is_comment(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"comment" | b"line_comment" | b"block_comment" | b"doc_comment"
    )
}

/// Returns whether a node decorates the declaration that follows it.
const fn decorates_a_declaration(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"attribute_item"
            | b"attribute_list"
            | b"attribute_declaration"
            | b"attribute_specifier"
            | b"annotation"
            | b"marker_annotation"
            | b"decorator"
            | b"modifiers"
    )
}

/// Returns whether documentation written above a parent belongs to its child.
///
/// A capture is often a declarator or an expression rather than the statement
/// a reader wrote the comment above, and each of these parents adds no
/// declaration of its own for the prose to belong to instead.
const fn documentation_parent(kind: &str) -> bool {
    matches!(
        kind.as_bytes(),
        b"function_definition"
            | b"declaration"
            | b"export_statement"
            | b"decorated_definition"
            | b"template_declaration"
            | b"expression_statement"
            | b"const_declaration"
            | b"var_declaration"
            | b"type_declaration"
            | b"field_declaration"
    )
}

fn clean_comment(comment: &str) -> String {
    comment
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches('/')
                .trim_start_matches('!')
                .trim_start_matches('*')
                .trim_start_matches('#')
                .trim_end_matches("*/")
                .trim()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads the prose out of a C# XML documentation comment.
///
/// `/// <summary>Builds a beacon.</summary>` is the documentation convention
/// C# tooling reads, and its prose is the element text, not the markup: shown
/// verbatim, every C# summary rendered as `<summary>Builds a beacon.</summary>`.
/// The `<summary>` body is the declaration's summary; without one, element
/// markup is dropped and a reference element (`<see cref="Beacon"/>`,
/// `<paramref name="level"/>`) keeps the name it points at. Text without an
/// XML documentation element, such as a Rust comment naming `Vec<T>`, is
/// returned unchanged.
fn xml_documentation_text(text: &str) -> String {
    const ELEMENTS: [&str; 6] = [
        "<summary",
        "<param ",
        "<returns",
        "<remarks",
        "<see ",
        "<inheritdoc",
    ];
    if !ELEMENTS.iter().any(|element| text.contains(element)) {
        return text.to_owned();
    }
    let body = text
        .split_once("<summary>")
        .and_then(|(_, rest)| rest.split_once("</summary>"))
        .map_or(text, |(summary, _)| summary);
    let mut output = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        output.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            output.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        if let Some(reference) = ["cref=\"", "name=\"", "langword=\""]
            .iter()
            .find_map(|attribute| tag.split_once(attribute))
            .and_then(|(_, value)| value.split_once('"'))
            .map(|(value, _)| value)
        {
            output.push_str(reference.rsplit(':').next().unwrap_or(reference));
        }
        rest = &rest[open + close + 1..];
    }
    output.push_str(rest);
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
