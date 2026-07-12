//! The `LanguageSpec` contract: structured extraction of definitions, imports,
//! and qualified references from a tree-sitter parse.
//!
//! This supersedes the flat `(kind, parent_kind)` classifiers (still in
//! [`super`] for the legacy snippet path) with output rich enough for the
//! resolver (REFERENCES-PLAN §4.1): definition nesting, full qualifier chains,
//! import binding structure, and method-call receiver shapes.
//!
//! Each language implements one [`LanguageSpec`]; the resolver (`generate::
//! resolve`) is language-agnostic and consumes only the [`Extraction`] boundary.

use std::{ops::Range, path::Path};

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

// ─── Definition sites ────────────────────────────────────────────────────────

/// The syntactic kind of a definition site, so the resolver can normalize
/// enclosing-frame names per language (e.g. an `impl T` frame contributes `T`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefKind {
	/// A free function.
	Function,
	/// A method (a function with a receiver / bound to a type or class).
	Method,
	/// A named data type: struct, record, enum, class-as-data.
	Type,
	/// A trait / protocol / interface definition.
	Trait,
	/// An `impl`/extension frame. `of` is the implemented trait/protocol path
	/// spelling when present (`impl Trait for T`), else `None` (inherent impl).
	Impl { of: Option<String> },
	/// A module / namespace / package frame (inline `mod`, class-as-namespace).
	Module,
	/// A class frame that contributes its name to nested members' FQNs
	/// (Python/TS/Java). Distinct from [`DefKind::Type`] only where a language
	/// needs both a data shape and a namespace frame.
	Class,
}

/// One named definition site, with nesting expressed via `parent`.
///
/// Body spans are properly nested by construction (they mirror the tree), so
/// the resolver attributes a use-site to the innermost `body_span` that
/// contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDefinition {
	/// The declared name (last segment only).
	pub name: String,
	/// What kind of definition this is.
	pub kind: DefKind,
	/// Span of the name token (the definition occurrence's span).
	pub name_span: Range<usize>,
	/// Full item extent — the containment interval for attribution.
	pub body_span: Range<usize>,
	/// Index into the definitions vec of the lexically-enclosing definition,
	/// or `None` at file top level.
	pub parent: Option<usize>,
}

// ─── Imports ─────────────────────────────────────────────────────────────────

/// A module prefix targeted by a glob import (`use x::*`, `from x import *`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPrefix {
	/// External dependency name, or `None` for an in-package prefix.
	pub dependency: Option<String>,
	/// The module path segments of the prefix.
	pub path: Vec<String>,
}

/// Where a name binding points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
	/// An in-package path (`use crate::a::b`, `from .a import b`).
	Internal(Vec<String>),
	/// A path inside an external dependency (`use serde::Serialize`).
	External { dependency: String, path: Vec<String> },
	/// A glob import bringing every member of a prefix into scope.
	Glob(ImportPrefix),
}

/// One entry in the file's name-binding table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBinding {
	/// The locally-visible name (post-`as`-rename). Empty for glob imports.
	pub local: String,
	/// What the binding resolves to.
	pub source: ImportSource,
	/// Span of the binding site.
	pub span: Range<usize>,
}

// ─── References ──────────────────────────────────────────────────────────────

/// The syntactic receiver of a method call (`x.m()`), used to attribute
/// `self`/`cls` calls to the enclosing type and to feed the resolver's
/// receiver-based method matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverShape {
	/// `self` / `this` — resolve against the enclosing type/class.
	SelfRef,
	/// `cls` / a class-name receiver (Python classmethod style).
	ClassRef,
	/// Any other receiver expression, kept as source text.
	Expr(String),
}

/// One use-site, with its full qualifier chain captured as a single reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawReference {
	/// The qualifier chain: `["foo","bar","baz"]` for `foo::bar::baz` — one ref.
	pub segments: Vec<String>,
	/// Span of the whole chain.
	pub span: Range<usize>,
	/// What kind of reference this is (`MethodCall` now emitted).
	pub kind: ReferenceKind,
	/// The receiver shape for method calls (`x.m()`), else `None`.
	pub receiver: Option<ReceiverShape>,
}

// ─── Layout & the extraction boundary ────────────────────────────────────────

/// Package layout facts a [`LanguageSpec`] needs to derive module paths from a
/// file's location.
#[derive(Debug, Clone)]
pub struct PackageLayout {
	/// The package/crate name (the root module segment for some languages).
	pub package: String,
}

/// Everything one file's extraction yields, at the language-agnostic boundary
/// the resolver consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
	/// Named definition sites, in document order (parents before children not
	/// guaranteed; use `parent` indices for nesting).
	pub definitions: Vec<RawDefinition>,
	/// The file's name-binding table.
	pub imports: Vec<ImportBinding>,
	/// Qualified use-sites.
	pub references: Vec<RawReference>,
	/// This file's module path under the language's layout conventions.
	pub module_path: Vec<String>,
}

/// A per-language structural extractor.
///
/// Implementations are cursor-walks over the tree-sitter parse (not `.scm`
/// queries): the structured output — parent indices, chain assembly, receiver
/// shapes — needs Rust post-processing regardless (REFERENCES-PLAN §4.1).
pub trait LanguageSpec: Send + Sync {
	/// Named definition sites with nesting.
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition>;

	/// The file's name-binding table.
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding>;

	/// Use-sites, each with its full qualifier chain as one reference.
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference>;

	/// File path (relative to the package root) → module path.
	fn module_path(&self, rel: &Path, layout: &PackageLayout) -> Vec<String>;

	/// Run all three extractors plus module-path derivation for one file.
	fn extract(
		&self,
		tree: &tree_sitter::Tree,
		src: &str,
		rel: &Path,
		layout: &PackageLayout,
	) -> Extraction {
		Extraction {
			definitions: self.definitions(tree, src),
			imports: self.imports(tree, src),
			references: self.references(tree, src),
			module_path: self.module_path(rel, layout),
		}
	}
}

// ─── Shared cursor-walk helpers ──────────────────────────────────────────────

/// Pre-order walk over every node, invoking `visit(node, depth)`.
///
/// Extractors use this for flat scans (references, import tokens); definition
/// nesting is recovered with the tree-sitter [`Node`](tree_sitter::Node) API
/// (`child_by_field_name`, `parent`) since it needs field access.
pub fn walk_preorder(tree: &tree_sitter::Tree, mut visit: impl FnMut(tree_sitter::Node, usize)) {
	let mut cursor = tree.root_node().walk();
	let mut depth = 0usize;
	loop {
		visit(cursor.node(), depth);
		if cursor.goto_first_child() {
			depth += 1;
			continue;
		}
		loop {
			if cursor.goto_next_sibling() {
				break;
			}
			if !cursor.goto_parent() {
				return;
			}
			depth -= 1;
		}
	}
}

/// The UTF-8 text of a node's span in `src`.
pub fn node_text<'s>(node: tree_sitter::Node, src: &'s str) -> &'s str {
	node.utf8_text(src.as_bytes()).unwrap_or("")
}

/// The text of `node`'s child under field `field`, if present.
pub fn field_text<'s>(node: tree_sitter::Node, field: &str, src: &'s str) -> Option<&'s str> {
	node.child_by_field_name(field).map(|c| node_text(c, src))
}
