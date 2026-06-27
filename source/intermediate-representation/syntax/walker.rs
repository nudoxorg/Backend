use std::ops::Range;

use arborium_tree_sitter as tree_sitter;

use crate::syntax::ResolvedReference;

pub fn walk_references(
	tree: &tree_sitter::Tree,
	source: &str,
	mut classify: impl FnMut(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference>,
) -> Vec<ResolvedReference> {
	let mut references = Vec::new();
	let mut cursor = tree.root_node().walk();

	loop {
		let node = cursor.node();
		let parent_kind = node.parent().map(|p| p.kind());
		let span = node.byte_range();
		if let Ok(name) = node.utf8_text(source.as_bytes()) {
			if let Some(rr) = classify(name, node.kind(), parent_kind, span.clone()) {
				references.push(rr);
			}
		}

		if cursor.goto_first_child() {
			continue;
		}

		loop {
			if cursor.goto_next_sibling() {
				break;
			}
			if !cursor.goto_parent() {
				return references;
			}
		}
	}
}
