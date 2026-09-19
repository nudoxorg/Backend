//! Where a captured definition sits, derived once from the parse tree.
//!
//! A grammar's tags query answers *what* a definition is and never *where* it
//! sits, so every extracted declaration used to arrive at a product with no
//! containment at all: an outline could only parent every declaration in a
//! file to that file's module, which is why a struct page had no fields and a
//! method never reached its type.  Containment is therefore derived here, in
//! one place, from the same parse the tags query already ran against.
//!
//! The decision is deliberately structural rather than semantic: the nearest
//! *captured* enclosing definition wins, because only a captured definition
//! has a row to be a parent of; an intervening `impl` block, method receiver,
//! or out-of-line qualified name instead names a type to attach to, because
//! the type is declared elsewhere in the file (or in the project) and can only
//! be resolved by name; anything else sits in the file module.  These rules
//! live next to the tag vocabulary in `syntax_kind.rs` because both are read
//! by the single extraction loop and must agree about one grammar's nodes.

use crate::syntax::{Container, node_text};
use std::{collections::BTreeMap, num::NonZeroU32};
use tree_sitter::Node;

/// Exact byte span a parse-tree node occupies.
type ByteRange = (usize, usize);

/// How a declaration is addressed by whatever contains it.
type DefinitionIdentity = (String, NonZeroU32);

/// Maximum ancestors walked before a definition is treated as file-level.
const MAX_CONTAINER_DEPTH: usize = 256;

/// Every captured definition's name and start line, keyed by exact byte range.
///
/// The byte range is the identity a parse tree can be asked about: an ancestor
/// node is the same definition as a capture exactly when their ranges are
/// equal, which keeps the lookup independent of node kinds and of the order
/// the tags query produced its matches in.
pub(crate) struct DefinitionIndex {
    by_range: BTreeMap<ByteRange, DefinitionIdentity>,
    tags: BTreeMap<DefinitionIdentity, String>,
}

impl DefinitionIndex {
    /// Creates an empty index.
    pub(crate) const fn new() -> Self {
        Self {
            by_range: BTreeMap::new(),
            tags: BTreeMap::new(),
        }
    }

    /// Retains one captured definition, keeping the first name for a range.
    ///
    /// Two tags patterns routinely select the same node - a Rust `impl`
    /// method is both `definition.method` and `definition.function` - and they
    /// always agree about its name, so the first capture is authoritative.
    pub(crate) fn insert(&mut self, node: Node<'_>, name: &str, line: NonZeroU32, tag: &str) {
        self.by_range
            .entry((node.start_byte(), node.end_byte()))
            .or_insert_with(|| (name.to_owned(), line));
        self.tags
            .entry((name.to_owned(), line))
            .or_insert_with(|| tag.to_owned());
    }

    /// Returns whether a container is a type that can own members.
    ///
    /// A callable is a method because of what holds it, not because of the
    /// node shape that selected it: an `impl` block or a receiver attaches to
    /// a type by name, and a class, struct, trait, or interface body holds one
    /// lexically, while a module body holds plain functions.
    pub(crate) fn contained_by_a_type(&self, container: &Container) -> bool {
        match container {
            Container::Module => false,
            Container::Attached { .. } => true,
            Container::Enclosing { name, line } => self
                .tags
                .get(&(name.clone(), *line))
                .is_some_and(|tag| owns_members(tag)),
        }
    }

    /// Returns the captured definition that *strictly* contains `node`.
    ///
    /// A parent may span exactly the bytes of its child - a Python
    /// `expression_statement` and its assignment do - and a range-keyed
    /// lookup would then answer with the child itself and parent a
    /// declaration to itself. Containment is therefore asked of a range that
    /// is a proper superset of the contained one.
    fn strictly_containing(
        &self,
        candidate: Node<'_>,
        contained: ByteRange,
    ) -> Option<&DefinitionIdentity> {
        let range = (candidate.start_byte(), candidate.end_byte());
        if range == contained {
            return None;
        }
        self.by_range.get(&range)
    }
}

/// Returns where one captured definition node sits among its file's captures.
pub(crate) fn container_of(
    node: Node<'_>,
    definitions: &DefinitionIndex,
    source: &str,
) -> Container {
    if let Some(type_name) = attached_type(node, source) {
        return Container::attached(&type_name);
    }
    let contained = (node.start_byte(), node.end_byte());
    let mut current = node;
    for _ in 0..MAX_CONTAINER_DEPTH {
        let Some(parent) = current.parent() else {
            break;
        };
        if let Some((name, line)) = definitions.strictly_containing(parent, contained) {
            return Container::enclosing(name, *line);
        }
        if let Some(type_name) = attached_type(parent, source) {
            return Container::attached(&type_name);
        }
        current = parent;
    }
    Container::Module
}

/// Returns whether a tag names a declaration that owns members.
fn owns_members(tag: &str) -> bool {
    matches!(
        tag.as_bytes(),
        b"class" | b"struct" | b"interface" | b"trait" | b"enum" | b"union" | b"type"
    )
}

/// Returns the type a node attaches its contents to, when it is one.
///
/// Only three shapes attach across a lexical boundary, one per grammar family
/// that has one: a Rust `impl` block, a Go method receiver, and a C++
/// out-of-line definition written `Type::member`.  Everything else nests
/// lexically and is answered by [`DefinitionIndex`].
fn attached_type(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "impl_item" => field_text(node, "type", source).map(type_stem),
        "method_declaration" => receiver_type(node, source),
        "function_declarator" => qualified_scope(node, source),
        _ => None,
    }
}

/// Returns a Go method's receiver type name, without pointer or generics.
fn receiver_type(node: Node<'_>, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    for index in 0..u32::try_from(receiver.named_child_count()).unwrap_or(u32::MAX) {
        let Some(child) = receiver.named_child(index) else {
            continue;
        };
        if child.kind() != "parameter_declaration" {
            continue;
        }
        if let Some(text) = field_text(child, "type", source) {
            return Some(type_stem(text));
        }
    }
    None
}

/// Returns the scope of a C++ declarator written `Type::member`.
fn qualified_scope(node: Node<'_>, source: &str) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    if declarator.kind() != "qualified_identifier" {
        return None;
    }
    let scope = declarator.child_by_field_name("scope")?;
    Some(type_stem(node_text(scope, source)))
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field)
        .map(|child| node_text(child, source))
}

/// Reduces a written type to the bare name a declaration can be found by.
///
/// `impl<T> Stack<T>`, `*Stack[T]`, and `Stack` all attach to the same
/// declared type, so generic arguments, pointers, and references are dropped
/// rather than carried into a name lookup that would never match.
fn type_stem(text: &str) -> String {
    let text = text.trim();
    let end = text.find(['<', '[']).unwrap_or(text.len());
    let head = text.get(..end).unwrap_or(text).trim();
    let last = head.split_whitespace().next_back().unwrap_or(head);
    last.trim_start_matches(['&', '*']).to_owned()
}

#[cfg(test)]
mod tests {
    use super::type_stem;

    #[test]
    fn type_stem_reduces_generic_pointer_and_reference_receivers() {
        assert_eq!(type_stem("Stack<T>"), "Stack");
        assert_eq!(type_stem("*Stack[T]"), "Stack");
        assert_eq!(type_stem("&'a mut Worker"), "Worker");
        assert_eq!(type_stem("HashMap<String, u32>"), "HashMap");
        assert_eq!(type_stem("Worker"), "Worker");
    }
}
