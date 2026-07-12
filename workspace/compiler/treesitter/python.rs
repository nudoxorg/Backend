//! `LanguageSpec` for Python — structural extractor.
//!
//! Implements [`LanguageSpec`] by cursor-walking the tree-sitter parse.
//! Output: definition sites with nesting, the import table, and qualified
//! reference chains with method-call receiver shapes.
//!
//! # Grammar version
//! Pinned to `arborium-python 2.18.1` (tree-sitter-python). Node-kind constants
//! in this file are authoritative for that grammar revision; change them only
//! alongside a grammar version bump.
//!
//! # Relative-import encoding
//! `from .a.b import c` → `Internal(["", "a", "b", "c"])`.
//! One leading `""` segment encodes *each* leading dot: `from .. import x` →
//! `Internal(["", "", "x"])`. The resolver counts leading `""` segments and
//! walks that many levels up from the current module before appending the rest.
//! `from . import x` (no module name, one dot) → `Internal(["", "x"])`.
//!
//! # Out-of-scope
//! - `getattr(obj, "name")` dynamic attribute access (not statically visible).
//! - `__all__` re-export lists (resolved by the symbol table, not here).
//! - Lambda expressions (anonymous, no name to emit as a definition).
//! - Comprehension scopes (don't introduce top-level definitions).

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportPrefix, ImportSource, LanguageSpec, PackageLayout,
	RawDefinition, RawReference, ReceiverShape, node_text, walk_preorder,
};

// ─── Grammar-pin: node-kind constants ────────────────────────────────────────
// All string literals that touch tree-sitter node kinds live here.
// Source: arborium-python 2.18.1 / tree-sitter-python grammar.

/// `def foo(...):` — plain function or method
const K_FUNCTION_DEF: &str = "function_definition";
/// `class Foo:` — class frame
const K_CLASS_DEF: &str = "class_definition";
/// `foo(...)` — call expression
const K_CALL: &str = "call";
/// `obj.attr` — attribute access / dotted chain
const K_ATTRIBUTE: &str = "attribute";
/// `@decorator` — decorator node wrapping any expression
const K_DECORATOR: &str = "decorator";
/// Type annotation wrapper: `x: Foo` or `-> Bar`
const K_TYPE: &str = "type";
/// `import a.b` / `import a.b as c`
const K_IMPORT_STMT: &str = "import_statement";
/// `from a.b import c` / `from . import c`
const K_IMPORT_FROM: &str = "import_from_statement";
/// `a.b.c` inside import statements
const K_DOTTED_NAME: &str = "dotted_name";
/// `x as y` child of import nodes
const K_ALIASED_IMPORT: &str = "aliased_import";
/// `from .foo import bar` — relative module specifier
const K_RELATIVE_IMPORT: &str = "relative_import";
/// The `.` / `..` prefix in a relative import
const K_IMPORT_PREFIX: &str = "import_prefix";
/// `from x import *`
const K_WILDCARD_IMPORT: &str = "wildcard_import";
/// Plain name token
const K_IDENTIFIER: &str = "identifier";
/// Dotted type in annotations: `a.B` inside a `type` node
const K_MEMBER_TYPE: &str = "member_type";

// ─── Python structural extractor ─────────────────────────────────────────────

/// Python structural extractor.
pub struct PythonSpec;

impl LanguageSpec for PythonSpec {
	/// Walk the parse tree and emit named definition sites with parent indices.
	///
	/// Only `function_definition` and `class_definition` are emitted.
	/// The nesting stack tracks the parent index so callers can walk the
	/// containment lattice cheaply.
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut defs: Vec<RawDefinition> = Vec::new();
		// Stack of (def-index, byte-end-of-body).
		let mut stack: Vec<(usize, usize)> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			// Pop frames whose bodies we have exited.
			let node_start = node.start_byte();
			while let Some(&(_, body_end)) = stack.last() {
				if node_start >= body_end {
					stack.pop();
				} else {
					break;
				}
			}

			let kind_str = node.kind();
			let def_kind = match kind_str {
				K_FUNCTION_DEF => {
					// Method if any enclosing frame is a class.
					let is_method = stack.iter().rev().any(|&(idx, _)| {
						matches!(defs[idx].kind, DefKind::Class)
					});
					if is_method { DefKind::Method } else { DefKind::Function }
				}
				K_CLASS_DEF => DefKind::Class,
				_ => return,
			};

			let Some(name_node) = node.child_by_field_name("name") else { return };
			let name = node_text(name_node, src).to_owned();
			let name_span = name_node.start_byte()..name_node.end_byte();
			let body_span = node.start_byte()..node.end_byte();
			let parent = stack.last().map(|&(idx, _)| idx);

			let idx = defs.len();
			defs.push(RawDefinition { name, kind: def_kind, name_span, body_span: body_span.clone(), parent });
			stack.push((idx, body_span.end));
		});

		defs
	}

	/// Walk import statements and emit one [`ImportBinding`] per bound name.
	///
	/// Encoding details:
	/// - `import a.b` → `External { dependency: "a", path: ["b"] }`, `local = "a"`.
	/// - `import a.b as z` → same source, `local = "z"`.
	/// - `from a.b import c` → `External { dependency: "a", path: ["b", "c"] }`.
	/// - `from .a import c` → `Internal(["", "a", "c"])` (one leading `""`).
	/// - `from .. import c` → `Internal(["", "", "c"])` (two leading `""`s).
	/// - `from x import *` → `Glob(ImportPrefix { dependency: Some("x"), path: [] })`.
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut bindings: Vec<ImportBinding> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			match node.kind() {
				K_IMPORT_STMT => {
					collect_import_statement(node, src, &mut bindings);
				}
				K_IMPORT_FROM => {
					collect_import_from(node, src, &mut bindings);
				}
				_ => {}
			}
		});

		bindings
	}

	/// Walk the tree and emit one [`RawReference`] per qualified use-site.
	///
	/// Chains (`obj.attr.method`) are captured as a single reference so the
	/// resolver sees the full qualifier without re-assembly.
	/// Inner identifiers of already-captured chains are skipped via a "covered"
	/// byte-range sentinel.
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut refs: Vec<RawReference> = Vec::new();
		// Byte ranges already emitted as part of a larger chain.
		// We skip sub-nodes whose span is fully contained in a covered range.
		let mut covered: Vec<std::ops::Range<usize>> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			let start = node.start_byte();
			let end   = node.end_byte();

			// Skip nodes covered by an already-emitted chain.
			if covered.iter().any(|r| start >= r.start && end <= r.end) {
				return;
			}

			match node.kind() {
				K_CALL => {
					if let Some(r) = extract_call_ref(node, src) {
						covered.push(r.span.clone());
						refs.push(r);
					}
				}
				K_ATTRIBUTE => {
					// Only emit bare attribute (not the function child of a call).
					// The call case above already covers `obj.method()`.
					let parent_is_call_fn = node
						.parent()
						.map(|p| {
							p.kind() == K_CALL
								&& p.child_by_field_name("function")
									.map(|f| f.id() == node.id())
									.unwrap_or(false)
						})
						.unwrap_or(false);

					if !parent_is_call_fn {
						if let Some(r) = extract_attribute_chain(node, src, ReferenceKind::FieldAccess) {
							covered.push(r.span.clone());
							refs.push(r);
						}
					}
				}
				K_DECORATOR => {
					if let Some(r) = extract_decorator_ref(node, src) {
						covered.push(r.span.clone());
						refs.push(r);
					}
				}
				K_TYPE => {
					// Type annotations: capture the inner name(s) as TypeReference.
					for r in extract_type_refs(node, src) {
						covered.push(r.span.clone());
						refs.push(r);
					}
				}
				_ => {}
			}
		});

		refs
	}

	/// Derive this file's module path from its path relative to the package root.
	///
	/// Convention: directory components are package segments; the file stem is
	/// the final segment.  `__init__.py` collapses to the enclosing directory
	/// (the file *is* the package, not a sub-module).
	///
	/// Examples:
	/// - `pkg/sub/mod.py`    → `["pkg", "sub", "mod"]`
	/// - `pkg/__init__.py`   → `["pkg"]`
	/// - `main.py`           → `["main"]`
	fn module_path(&self, rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("");
		let is_init = stem == "__init__";

		let dir_segments: Vec<String> = rel
			.parent()
			.into_iter()
			.flat_map(|p| p.components())
			.filter_map(|c| c.as_os_str().to_str().map(|s| s.to_owned()))
			.filter(|s| !s.is_empty())
			.collect();

		if is_init {
			// `__init__.py` names its containing directory as the module.
			dir_segments
		} else {
			let mut segs = dir_segments;
			segs.push(stem.to_owned());
			segs
		}
	}
}

// ─── Import extraction helpers ────────────────────────────────────────────────

/// Handle `import a.b` and `import a.b as z` (one or more comma-separated).
fn collect_import_statement(
	node: tree_sitter::Node,
	src: &str,
	out: &mut Vec<ImportBinding>,
) {
	let mut cursor = node.walk();
	for child in node.children(&mut cursor) {
		match child.kind() {
			K_DOTTED_NAME => {
				// `import a.b` — local name is the FIRST segment only (Python
				// binds just `a` in scope for `import a.b`).
				let segments = dotted_name_segments(child, src);
				if segments.is_empty() { continue; }
				let local = segments[0].clone();
				let (dependency, path) = split_external(&segments);
				let span = child.start_byte()..child.end_byte();
				out.push(ImportBinding {
					local,
					source: ImportSource::External { dependency, path },
					span,
				});
			}
			K_ALIASED_IMPORT => {
				// `import a.b as z`
				let name_node = child.child_by_field_name("name");
				let alias_node = child.child_by_field_name("alias");
				if let (Some(name_n), Some(alias_n)) = (name_node, alias_node) {
					let segments = dotted_name_segments(name_n, src);
					if segments.is_empty() { continue; }
					let local = node_text(alias_n, src).to_owned();
					let (dependency, path) = split_external(&segments);
					let span = child.start_byte()..child.end_byte();
					out.push(ImportBinding {
						local,
						source: ImportSource::External { dependency, path },
						span,
					});
				}
			}
			_ => {}
		}
	}
}

/// Handle `from a.b import c, d as e` and `from .a import b` and `from x import *`.
fn collect_import_from(
	node: tree_sitter::Node,
	src: &str,
	out: &mut Vec<ImportBinding>,
) {
	// Identify the module_name child (dotted_name or relative_import).
	let module_name_node = node.child_by_field_name("module_name");

	// Check for wildcard.
	let mut cursor = node.walk();
	let has_wildcard = node.children(&mut cursor).any(|c| c.kind() == K_WILDCARD_IMPORT);
	if has_wildcard {
		let span = node.start_byte()..node.end_byte();
		let source = match &module_name_node {
			Some(mn) => build_import_source_for_from(*mn, src, /*name_seg=*/ None, /*glob=*/true),
			None => ImportSource::Glob(ImportPrefix { dependency: None, path: vec![] }),
		};
		out.push(ImportBinding { local: String::new(), source, span });
		return;
	}

	// The grammar's `name` field holds the imported symbol(s); `module_name`
	// holds the source path.  We use child_by_field_name to locate the module
	// node's id so we can skip it when iterating named children.
	let module_name_id = module_name_node.as_ref().map(|n| n.id());

	// Collect the imported names (the `name` field children only).
	let mut cursor2 = node.walk();
	for name_child in node.named_children(&mut cursor2) {
		// Skip the module_name child — it's the source, not the binding.
		if Some(name_child.id()) == module_name_id {
			continue;
		}

		let name_segs: Option<Vec<String>> = match name_child.kind() {
			K_DOTTED_NAME => Some(dotted_name_segments(name_child, src)),
			K_ALIASED_IMPORT => {
				// We want the raw name segments for source computation.
				name_child
					.child_by_field_name("name")
					.map(|n| dotted_name_segments(n, src))
			}
			_ => None,
		};

		let name_segs = match name_segs {
			Some(s) if !s.is_empty() => s,
			_ => continue,
		};

		// Determine local binding name.
		let local = if name_child.kind() == K_ALIASED_IMPORT {
			name_child
				.child_by_field_name("alias")
				.map(|a| node_text(a, src).to_owned())
				.unwrap_or_else(|| name_segs.last().unwrap().clone())
		} else {
			name_segs.last().unwrap().clone()
		};

		let span = name_child.start_byte()..name_child.end_byte();

		let source = match &module_name_node {
			Some(mn) => build_import_source_for_from(*mn, src, Some(&name_segs), /*glob=*/false),
			None => {
				// `from import c` — malformed; emit as External with empty dep.
				ImportSource::External {
					dependency: name_segs.first().cloned().unwrap_or_default(),
					path:       name_segs[1..].to_vec(),
				}
			}
		};

		out.push(ImportBinding { local, source, span });
	}
}

/// Build the [`ImportSource`] for a `from X import Y` given the module-name node.
///
/// `name_seg` is the imported name segments appended after the module path.
/// `glob` means emit a [`ImportSource::Glob`].
fn build_import_source_for_from(
	module_name: tree_sitter::Node,
	src: &str,
	name_seg: Option<&[String]>,
	glob: bool,
) -> ImportSource {
	match module_name.kind() {
		K_RELATIVE_IMPORT => {
			// Count dots from the import_prefix child.
			let dot_count = count_relative_dots(module_name, src);
			// Optional dotted_name child gives the sub-package path.
			let mut cursor = module_name.walk();
			let pkg_segs: Vec<String> = module_name
				.named_children(&mut cursor)
				.filter(|c| c.kind() == K_DOTTED_NAME)
				.flat_map(|dn| dotted_name_segments(dn, src))
				.collect();

			// Build the segment list: dot_count leading "" entries, then pkg_segs,
			// then optional name_seg (unless glob).
			let mut segs: Vec<String> = (0..dot_count).map(|_| String::new()).collect();
			segs.extend(pkg_segs);
			if !glob {
				if let Some(ns) = name_seg {
					segs.extend_from_slice(ns);
				}
			}

			if glob {
				// For `from .pkg import *` the prefix is the relative path.
				let prefix_segs: Vec<String> = {
					let mut ps: Vec<String> = (0..dot_count).map(|_| String::new()).collect();
					let mut c2 = module_name.walk();
					let pkg2: Vec<String> = module_name
						.named_children(&mut c2)
						.filter(|ch| ch.kind() == K_DOTTED_NAME)
						.flat_map(|dn| dotted_name_segments(dn, src))
						.collect();
					ps.extend(pkg2);
					ps
				};
				ImportSource::Glob(ImportPrefix { dependency: None, path: prefix_segs })
			} else {
				ImportSource::Internal(segs)
			}
		}
		K_DOTTED_NAME => {
			let mod_segs = dotted_name_segments(module_name, src);
			if mod_segs.is_empty() {
				return ImportSource::External { dependency: String::new(), path: vec![] };
			}
			if glob {
				let (dependency, path) = split_external(&mod_segs);
				ImportSource::Glob(ImportPrefix { dependency: Some(dependency), path })
			} else {
				let mut full = mod_segs.clone();
				if let Some(ns) = name_seg {
					full.extend_from_slice(ns);
				}
				let (dependency, path) = split_external(&full);
				ImportSource::External { dependency, path }
			}
		}
		_ => {
			// Fallback: treat the node text as a single segment.
			let seg = node_text(module_name, src).to_owned();
			ImportSource::External { dependency: seg, path: vec![] }
		}
	}
}

/// Count leading dots in a `relative_import` node via its `import_prefix` child.
fn count_relative_dots(relative_import: tree_sitter::Node, src: &str) -> usize {
	let mut cursor = relative_import.walk();
	for child in relative_import.named_children(&mut cursor) {
		if child.kind() == K_IMPORT_PREFIX {
			let text = node_text(child, src);
			return text.chars().filter(|&c| c == '.').count();
		}
	}
	0
}

/// Extract identifier children of a `dotted_name` node as a `Vec<String>`.
fn dotted_name_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	let mut cursor = node.walk();
	node.named_children(&mut cursor)
		.filter(|c| c.kind() == K_IDENTIFIER)
		.map(|c| node_text(c, src).to_owned())
		.collect()
}

/// Split `[a, b, c, ...]` into `(a, [b, c, ...])` for External sources.
fn split_external(segments: &[String]) -> (String, Vec<String>) {
	match segments {
		[] => (String::new(), vec![]),
		[dep] => (dep.clone(), vec![]),
		[dep, rest @ ..] => (dep.clone(), rest.to_vec()),
	}
}

// ─── Reference extraction helpers ────────────────────────────────────────────

/// Extract a reference from a `call` node.
///
/// - `foo(...)` — FunctionCall, segments = `["foo"]`.
/// - `self.method(...)` — MethodCall, receiver = SelfRef, segments = `["self","method"]`.
/// - `cls.make(...)` — MethodCall, receiver = ClassRef, segments = `["cls","make"]`.
/// - `obj.chain.method(...)` — MethodCall, receiver = Expr("obj"), segments full chain.
fn extract_call_ref(node: tree_sitter::Node, src: &str) -> Option<RawReference> {
	let fn_node = node.child_by_field_name("function")?;
	let span = fn_node.start_byte()..fn_node.end_byte();

	match fn_node.kind() {
		K_IDENTIFIER => {
			let name = node_text(fn_node, src).to_owned();
			Some(RawReference {
				segments: vec![name],
				span,
				kind: ReferenceKind::FunctionCall,
				receiver: None,
			})
		}
		K_ATTRIBUTE => {
			let segments = collect_attribute_chain(fn_node, src);
			if segments.is_empty() { return None; }
			let receiver = receiver_from_first_segment(&segments[0]);
			Some(RawReference {
				segments,
				span,
				kind: ReferenceKind::MethodCall,
				receiver: Some(receiver),
			})
		}
		_ => None,
	}
}

/// Extract a bare `attribute` node as a FieldAccess reference.
fn extract_attribute_chain(
	node: tree_sitter::Node,
	src: &str,
	kind: ReferenceKind,
) -> Option<RawReference> {
	let segments = collect_attribute_chain(node, src);
	if segments.is_empty() { return None; }
	let span = node.start_byte()..node.end_byte();
	Some(RawReference { segments, span, kind, receiver: None })
}

/// Recursively flatten an `attribute` node into a segment list.
///
/// `a.b.c` in the tree is `attribute(object: attribute(object: a, attr: b), attr: c)`.
/// Returns `["a", "b", "c"]`.
fn collect_attribute_chain(node: tree_sitter::Node, src: &str) -> Vec<String> {
	let mut segs: Vec<String> = Vec::new();
	collect_attr_recursive(node, src, &mut segs);
	segs
}

fn collect_attr_recursive(node: tree_sitter::Node, src: &str, segs: &mut Vec<String>) {
	if node.kind() == K_ATTRIBUTE {
		if let Some(obj) = node.child_by_field_name("object") {
			collect_attr_recursive(obj, src, segs);
		}
		if let Some(attr) = node.child_by_field_name("attribute") {
			segs.push(node_text(attr, src).to_owned());
		}
	} else if node.kind() == K_IDENTIFIER {
		segs.push(node_text(node, src).to_owned());
	}
	// Other object expressions (subscripts, calls, etc.) contribute their
	// source text as an opaque Expr segment — we capture only the identifier
	// root when present, else skip (dynamic access).
}

/// Map the first segment of a method chain to a [`ReceiverShape`].
fn receiver_from_first_segment(seg: &str) -> ReceiverShape {
	match seg {
		"self" => ReceiverShape::SelfRef,
		"cls"  => ReceiverShape::ClassRef,
		other  => ReceiverShape::Expr(other.to_owned()),
	}
}

/// Extract a reference from a `decorator` node.
///
/// `@deco` → FunctionCall, segments = `["deco"]`.
/// `@app.route` → FunctionCall, segments = `["app","route"]`.
/// `@app.route(...)` — the decorator child is a `call` → handled by the call
/// branch; but if we see a decorator wrapping a call, we delegate.
fn extract_decorator_ref(node: tree_sitter::Node, src: &str) -> Option<RawReference> {
	// The decorator node has a single expression child.
	let mut cursor = node.walk();
	let expr = node.named_children(&mut cursor).next()?;
	let span = expr.start_byte()..expr.end_byte();

	match expr.kind() {
		K_IDENTIFIER => {
			let name = node_text(expr, src).to_owned();
			Some(RawReference {
				segments: vec![name],
				span,
				kind: ReferenceKind::FunctionCall,
				receiver: None,
			})
		}
		K_ATTRIBUTE => {
			let segments = collect_attribute_chain(expr, src);
			if segments.is_empty() { return None; }
			Some(RawReference {
				segments,
				span,
				kind: ReferenceKind::FunctionCall,
				receiver: None,
			})
		}
		K_CALL => {
			// `@deco(args)` — the inner call expression is the decorator.
			// We emit the *function* part as FunctionCall.
			extract_call_ref(expr, src).map(|mut r| {
				r.kind = ReferenceKind::FunctionCall;
				r
			})
		}
		_ => None,
	}
}

/// Extract type references from within a `type` annotation node.
///
/// Handles:
/// - Plain `identifier` → `["Foo"]`.
/// - `member_type` (`a.B` in annotation) → `["a","B"]`.
/// - Nested compound types (unions, generics) — we descend and collect all
///   identifier/member_type children.
fn extract_type_refs(type_node: tree_sitter::Node, src: &str) -> Vec<RawReference> {
	let mut refs: Vec<RawReference> = Vec::new();
	collect_type_refs_recursive(type_node, src, &mut refs);
	refs
}

fn collect_type_refs_recursive(
	node: tree_sitter::Node,
	src: &str,
	out: &mut Vec<RawReference>,
) {
	match node.kind() {
		K_IDENTIFIER => {
			let name = node_text(node, src).to_owned();
			let span = node.start_byte()..node.end_byte();
			out.push(RawReference {
				segments: vec![name],
				span,
				kind: ReferenceKind::TypeReference,
				receiver: None,
			});
		}
		K_MEMBER_TYPE => {
			// `a.B` in annotation context.
			let mut cursor = node.walk();
			let mut parts: Vec<String> = Vec::new();
			for child in node.named_children(&mut cursor) {
				if child.kind() == K_IDENTIFIER {
					parts.push(node_text(child, src).to_owned());
				} else if child.kind() == K_TYPE {
					// Recurse for the right-hand type part.
					// For dotted member_type we collect identifiers directly.
					let mut c2 = child.walk();
					for sub in child.named_children(&mut c2) {
						if sub.kind() == K_IDENTIFIER {
							parts.push(node_text(sub, src).to_owned());
						}
					}
				}
			}
			if !parts.is_empty() {
				let span = node.start_byte()..node.end_byte();
				out.push(RawReference {
					segments: parts,
					span,
					kind: ReferenceKind::TypeReference,
					receiver: None,
				});
			}
		}
		_ => {
			// Descend into compound types (union_type, generic_type, etc.).
			let mut cursor = node.walk();
			for child in node.named_children(&mut cursor) {
				collect_type_refs_recursive(child, src, out);
			}
		}
	}
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::Path;

	use arborium_tree_sitter as tree_sitter;

	use super::*;
	use crate::treesitter::spec::{ImportSource, LanguageSpec, PackageLayout};

	// ── Parse helper ─────────────────────────────────────────────────────────

	fn parse(src: &str) -> (tree_sitter::Tree, String) {
		let mut parser = tree_sitter::Parser::new();
		parser
			.set_language(&arborium::get_language("python").unwrap())
			.unwrap();
		let tree = parser.parse(src.as_bytes(), None).unwrap();
		(tree, src.to_owned())
	}

	fn spec() -> PythonSpec { PythonSpec }

	// ── Definition tests ──────────────────────────────────────────────────────

	#[test]
	fn free_function_is_function_def() {
		let (tree, src) = parse("def foo(x):\n\tpass\n");
		let defs = spec().definitions(&tree, &src);
		assert_eq!(defs.len(), 1);
		assert_eq!(defs[0].name, "foo");
		assert!(matches!(defs[0].kind, DefKind::Function));
		assert_eq!(defs[0].parent, None);
	}

	#[test]
	fn class_def_is_class_kind() {
		let (tree, src) = parse("class MyClass:\n\tpass\n");
		let defs = spec().definitions(&tree, &src);
		assert_eq!(defs.len(), 1);
		assert_eq!(defs[0].name, "MyClass");
		assert!(matches!(defs[0].kind, DefKind::Class));
	}

	#[test]
	fn method_inside_class_is_method_kind_with_parent() {
		let src = "class Foo:\n\tdef bar(self):\n\t\tpass\n";
		let (tree, src) = parse(src);
		let defs = spec().definitions(&tree, &src);
		// Should have Foo (class) + bar (method)
		assert_eq!(defs.len(), 2);
		let class_idx = defs.iter().position(|d| d.name == "Foo").unwrap();
		let method_idx = defs.iter().position(|d| d.name == "bar").unwrap();
		assert!(matches!(defs[class_idx].kind, DefKind::Class));
		assert!(matches!(defs[method_idx].kind, DefKind::Method));
		assert_eq!(defs[method_idx].parent, Some(class_idx));
	}

	#[test]
	fn nested_method_parent_is_class_not_outer_function() {
		let src = "class Outer:\n\tdef method(self):\n\t\tpass\n\ndef free():\n\tpass\n";
		let (tree, src) = parse(src);
		let defs = spec().definitions(&tree, &src);
		let method = defs.iter().find(|d| d.name == "method").unwrap();
		let outer = defs.iter().position(|d| d.name == "Outer").unwrap();
		assert_eq!(method.parent, Some(outer));
		let free = defs.iter().find(|d| d.name == "free").unwrap();
		assert_eq!(free.parent, None);
	}

	// ── Import tests ──────────────────────────────────────────────────────────

	#[test]
	fn import_simple_module() {
		let (tree, src) = parse("import os\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "os");
		assert!(matches!(
			&imps[0].source,
			ImportSource::External { dependency, path }
			if dependency == "os" && path.is_empty()
		));
	}

	#[test]
	fn import_dotted_name_local_is_first_segment() {
		let (tree, src) = parse("import a.b\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "a");
		assert!(matches!(
			&imps[0].source,
			ImportSource::External { dependency, path }
			if dependency == "a" && path == &["b"]
		));
	}

	#[test]
	fn import_aliased() {
		let (tree, src) = parse("import a.b as c\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "c");
		assert!(matches!(
			&imps[0].source,
			ImportSource::External { dependency, path }
			if dependency == "a" && path == &["b"]
		));
	}

	#[test]
	fn from_import_external() {
		let (tree, src) = parse("from os.path import join\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "join");
		assert!(matches!(
			&imps[0].source,
			ImportSource::External { dependency, path }
			if dependency == "os" && path == &["path", "join"]
		));
	}

	#[test]
	fn from_relative_import_one_dot() {
		// `from .rel import x` — relative with one dot
		// Expected: Internal(["", "rel", "x"])
		let (tree, src) = parse("from .rel import x\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "x");
		match &imps[0].source {
			ImportSource::Internal(segs) => {
				assert_eq!(segs.len(), 3, "expected [\"\", \"rel\", \"x\"], got {segs:?}");
				assert_eq!(segs[0], "", "first segment should be empty (one dot)");
				assert_eq!(segs[1], "rel");
				assert_eq!(segs[2], "x");
			}
			other => panic!("expected Internal, got {other:?}"),
		}
	}

	#[test]
	fn from_relative_import_two_dots() {
		// `from .. import x` — two dots, no module name
		// Expected: Internal(["", "", "x"])
		let (tree, src) = parse("from .. import x\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "x");
		match &imps[0].source {
			ImportSource::Internal(segs) => {
				assert!(segs[0].is_empty() && segs[1].is_empty(), "two leading empty segs for '..'; got {segs:?}");
			}
			other => panic!("expected Internal, got {other:?}"),
		}
	}

	#[test]
	fn from_import_glob() {
		let (tree, src) = parse("from x import *\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "");
		assert!(
			matches!(&imps[0].source, ImportSource::Glob(p) if p.dependency == Some("x".to_owned())),
			"expected Glob with dependency x; got {:?}", imps[0].source
		);
	}

	#[test]
	fn from_import_with_alias() {
		let (tree, src) = parse("from a.b import c as d\n");
		let imps = spec().imports(&tree, &src);
		assert_eq!(imps.len(), 1);
		assert_eq!(imps[0].local, "d");
	}

	// ── Reference tests ───────────────────────────────────────────────────────

	fn find_ref<'a>(refs: &'a [RawReference], seg0: &str) -> Option<&'a RawReference> {
		refs.iter().find(|r| r.segments.first().map(|s| s.as_str()) == Some(seg0))
	}

	#[test]
	fn free_function_call() {
		let (tree, src) = parse("foo()\n");
		let refs = spec().references(&tree, &src);
		let r = find_ref(&refs, "foo").expect("should find foo call");
		assert_eq!(r.kind, ReferenceKind::FunctionCall);
		assert_eq!(r.segments, vec!["foo"]);
		assert_eq!(r.receiver, None);
	}

	#[test]
	fn self_method_call() {
		let (tree, src) = parse("self.method()\n");
		let refs = spec().references(&tree, &src);
		// Should find a MethodCall with SelfRef
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall)
			.expect("should find a MethodCall");
		assert_eq!(r.segments, vec!["self", "method"]);
		assert_eq!(r.receiver, Some(ReceiverShape::SelfRef));
	}

	#[test]
	fn cls_method_call() {
		let (tree, src) = parse("cls.make()\n");
		let refs = spec().references(&tree, &src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall)
			.expect("should find a MethodCall");
		assert_eq!(r.segments, vec!["cls", "make"]);
		assert_eq!(r.receiver, Some(ReceiverShape::ClassRef));
	}

	#[test]
	fn obj_field_access() {
		// Bare attribute expression, not a call.
		let (tree, src) = parse("x = obj.attr\n");
		let refs = spec().references(&tree, &src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::FieldAccess)
			.expect("should find FieldAccess");
		assert_eq!(r.segments, vec!["obj", "attr"]);
	}

	#[test]
	fn decorator_attribute_ref() {
		// `@app.route` → FunctionCall with segments ["app", "route"]
		let src = "@app.route\ndef view(): pass\n";
		let (tree, src) = parse(src);
		let refs = spec().references(&tree, &src);
		let r = refs
			.iter()
			.find(|r| r.segments.first().map(|s| s.as_str()) == Some("app"))
			.expect("should find app.route decorator ref");
		assert_eq!(r.kind, ReferenceKind::FunctionCall);
		assert_eq!(r.segments, vec!["app", "route"]);
	}

	#[test]
	fn decorator_simple_call() {
		// `@staticmethod` → FunctionCall
		let src = "@staticmethod\ndef foo(): pass\n";
		let (tree, src) = parse(src);
		let refs = spec().references(&tree, &src);
		let r = refs
			.iter()
			.find(|r| r.segments.first().map(|s| s.as_str()) == Some("staticmethod"))
			.expect("should find staticmethod ref");
		assert_eq!(r.kind, ReferenceKind::FunctionCall);
	}

	#[test]
	fn class_with_method_nesting() {
		let src = "class Foo:\n\tdef bar(self):\n\t\tself.x()\n";
		let (tree, src) = parse(src);
		let defs = spec().definitions(&tree, &src);
		let class_idx = defs.iter().position(|d| d.name == "Foo").unwrap();
		let method_idx = defs.iter().position(|d| d.name == "bar").unwrap();
		// bar is nested under Foo
		assert_eq!(defs[method_idx].parent, Some(class_idx));
		// References inside the body
		let refs = spec().references(&tree, &src);
		let r = refs.iter().find(|r| r.kind == ReferenceKind::MethodCall).unwrap();
		assert_eq!(r.receiver, Some(ReceiverShape::SelfRef));
	}

	// ── Module path tests ─────────────────────────────────────────────────────

	fn layout() -> PackageLayout { PackageLayout { package: "mylib".to_owned() } }

	#[test]
	fn module_path_regular_file() {
		let segs = spec().module_path(Path::new("pkg/sub/mod.py"), &layout());
		assert_eq!(segs, vec!["pkg", "sub", "mod"]);
	}

	#[test]
	fn module_path_init_file() {
		let segs = spec().module_path(Path::new("pkg/__init__.py"), &layout());
		assert_eq!(segs, vec!["pkg"]);
	}

	#[test]
	fn module_path_root_init() {
		let segs = spec().module_path(Path::new("__init__.py"), &layout());
		assert!(segs.is_empty(), "root __init__.py should yield empty path; got {segs:?}");
	}

	#[test]
	fn module_path_top_level_file() {
		let segs = spec().module_path(Path::new("main.py"), &layout());
		assert_eq!(segs, vec!["main"]);
	}
}
