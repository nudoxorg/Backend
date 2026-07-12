//! JSDoc extraction (OXC-PLAN §1.3, §5.4). Ports deno_doc's *semantics* on top
//! of oxc's tag splitter: leading-`*` strip, `@ignore` suppression, `@module`
//! module-doc marker, `@deprecated` → `ir` deprecation.
//!
//! LEAF FILE — fill the `todo!()` bodies.

use oxc_ast::ast::Program;
use oxc_syntax::node::NodeId;

use super::{DocFacts, Extractor};

impl<'a> Extractor<'a> {
	/// Resolve the JSDoc attached to `node_id` into [`DocFacts`]: description
	/// text, `@deprecated` → `Deprecation`, `@ignore` flag. Uses
	/// `self.semantic.jsdoc().get_one_by_node(node_id)`; falls back to a manual
	/// `program.comments` scan by `attached_to` when the finder misses.
	pub(crate) fn jsdoc_for_node(&self, node_id: NodeId) -> DocFacts {
		let _ = node_id;
		todo!("jsdoc.rs: node → DocFacts")
	}

	/// Module documentation = the first `/**` block whose parsed tags contain
	/// `@module` (deno_doc semantics — NOT merely the first comment).
	pub(crate) fn module_doc(&self, program: &Program<'a>) -> Option<String> {
		let _ = program;
		todo!("jsdoc.rs: first @module comment → module doc")
	}
}
