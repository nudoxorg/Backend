//! `LanguageSpec` for Rust.
//!
//! Cursor-walks the tree-sitter parse to emit:
//!
//! - **definitions** — named items with parent nesting (functions, types,
//!   traits, impls, modules, type aliases, macro definitions);
//! - **imports** — fully expanded `use` trees (braces, aliases, globs);
//! - **references** — full qualifier chains as single [`RawReference`] entries,
//!   with method-call receiver shapes;
//! - **module_path** — in-crate module path derived from the file's relative
//!   path under the standard `src/` layout.
//!
//! Node-kind string constants below track the **pinned arborium grammar**.
//! Cross-check against `categorize_rust` in `super` (treesitter/mod.rs) when
//! the grammar version changes.

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportPrefix, ImportSource, LanguageSpec, PackageLayout,
	RawDefinition, RawReference, ReceiverShape, node_text, walk_preorder,
};

// ─── Node-kind constants (pinned arborium grammar) ───────────────────────────
//
// These are the exact strings produced by the tree-sitter-rust grammar bundled
// in arborium. Update here if the grammar is re-pinned.

const K_FUNCTION_ITEM: &str = "function_item";
const K_STRUCT_ITEM: &str = "struct_item";
const K_ENUM_ITEM: &str = "enum_item";
const K_UNION_ITEM: &str = "union_item";
const K_TRAIT_ITEM: &str = "trait_item";
const K_IMPL_ITEM: &str = "impl_item";
const K_MOD_ITEM: &str = "mod_item";
const K_TYPE_ITEM: &str = "type_item";
const K_MACRO_DEFINITION: &str = "macro_definition";

const K_USE_DECLARATION: &str = "use_declaration";
const K_USE_LIST: &str = "use_list";           // `{a, b, c}`
const K_USE_AS_CLAUSE: &str = "use_as_clause"; // `foo as Bar`
const K_USE_WILDCARD: &str = "use_wildcard";   // `*`
const K_SCOPED_USE_TREE: &str = "scoped_use_tree"; // `a::b::{…}`

const K_CALL_EXPRESSION: &str = "call_expression";
const K_MACRO_INVOCATION: &str = "macro_invocation";
const K_SCOPED_IDENTIFIER: &str = "scoped_identifier";
const K_SCOPED_TYPE_IDENTIFIER: &str = "scoped_type_identifier";
const K_TYPE_IDENTIFIER: &str = "type_identifier";
const K_IDENTIFIER: &str = "identifier";
const K_FIELD_EXPRESSION: &str = "field_expression";
// K_FIELD_IDENTIFIER ("field_identifier") is the kind of the `field` child of
// a field_expression; kept here for documentation but we use node_text directly.
#[allow(dead_code)]
const K_FIELD_IDENTIFIER: &str = "field_identifier";
const K_TOKEN_TREE: &str = "token_tree"; // macro body — skip references inside

// ─── Rust structural extractor ───────────────────────────────────────────────

/// Rust structural extractor implementing [`LanguageSpec`].
pub struct RustSpec;

impl LanguageSpec for RustSpec {
	/// Walk the parse tree and collect all named definition sites with nesting.
	///
	/// An `impl_item` contributes the name of its `type` field (the implementing
	/// type) so nested methods attribute to the ADT rather than a synthetic
	/// `impl` frame. A `function_item` whose nearest enclosing definition is an
	/// `impl_item` is emitted with `DefKind::Method`.
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut defs: Vec<RawDefinition> = Vec::new();
		// Stack of (body_end_byte, definition_index) — maintained so we know
		// the parent index of every node we visit.
		let mut stack: Vec<(usize, usize)> = Vec::new();

		// Pre-order walk: parents always visited before their children by
		// construction, so the stack is always consistent.
		walk_preorder(tree, |node, _depth| {
			// Pop frames whose body has already ended (we have moved past them).
			let byte_start = node.start_byte();
			stack.retain(|(end, _)| *end > byte_start);

			let kind = node.kind();
			let def_kind = match kind {
				K_FUNCTION_ITEM => {
					// Method if immediately inside an impl frame.
					let in_impl = stack.last().map(|(_, idx)| matches!(defs[*idx].kind, DefKind::Impl { .. })).unwrap_or(false);
					if in_impl { DefKind::Method } else { DefKind::Function }
				}
				K_STRUCT_ITEM | K_ENUM_ITEM | K_UNION_ITEM | K_TYPE_ITEM => DefKind::Type,
				K_TRAIT_ITEM => DefKind::Trait,
				K_IMPL_ITEM => DefKind::Impl {
					of: node
						.child_by_field_name("trait")
						.map(|t| node_text(t, src).to_owned()),
				},
				K_MOD_ITEM => DefKind::Module,
				K_MACRO_DEFINITION => DefKind::Function,
				_ => return,
			};

			// Derive the name. For `impl_item` use the `type` field (the ADT
			// being implemented) as the frame name; all other items use `name`.
			let name_node = if kind == K_IMPL_ITEM {
				node.child_by_field_name("type")
			} else {
				node.child_by_field_name("name")
			};

			let (name, name_span) = match name_node {
				Some(n) => (node_text(n, src).to_owned(), n.byte_range()),
				None => return, // unnamed / anonymous — skip
			};

			let body_span = node.byte_range();
			let parent = stack.last().map(|(_, idx)| *idx);

			let idx = defs.len();
			defs.push(RawDefinition { name, kind: def_kind, name_span, body_span: body_span.clone(), parent });

			// Push onto the stack so nested items can find us as their parent.
			stack.push((body_span.end, idx));
		});

		defs
	}

	/// Walk `use_declaration` nodes and expand them into flat [`ImportBinding`]s.
	///
	/// Handles:
	/// - Simple: `use a::b::C;`
	/// - Aliased: `use a::b::C as D;`
	/// - Braced: `use a::{b, c::D};` (recursively expanded)
	/// - Glob: `use a::*;`
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut out: Vec<ImportBinding> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			if node.kind() != K_USE_DECLARATION {
				return;
			}
			// The `argument` field is the top-level path/tree inside the `use`.
			if let Some(arg) = node.child_by_field_name("argument") {
				let span = node.byte_range();
				collect_use_bindings(arg, &[], src, span.start, &mut out);
			}
		});

		out
	}

	/// Walk the parse tree and emit one [`RawReference`] per qualified use-site.
	///
	/// Key invariant: a `scoped_identifier` / `scoped_type_identifier` is emitted
	/// as **one** reference for the full chain; its inner identifier children are
	/// skipped to avoid double-counting.
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut out: Vec<RawReference> = Vec::new();
		// Track byte ranges we have already covered (to skip inner segments of
		// scoped paths that we emitted as one reference).
		let mut covered: Vec<std::ops::Range<usize>> = Vec::new();
		// Track token_tree ranges to skip macro body internals.
		let mut token_tree_ranges: Vec<std::ops::Range<usize>> = Vec::new();

		// First pass: collect all token_tree ranges to exclude from ref scan.
		walk_preorder(tree, |node, _depth| {
			if node.kind() == K_TOKEN_TREE {
				token_tree_ranges.push(node.byte_range());
			}
		});

		walk_preorder(tree, |node, _depth| {
			let span = node.byte_range();

			// Skip macro body internals.
			if token_tree_ranges.iter().any(|r| r.start < span.start && span.end <= r.end) {
				return;
			}

			// Skip inner segments already covered by a scoped-path reference.
			if covered.iter().any(|r| r.start <= span.start && span.end <= r.end) {
				return;
			}

			match node.kind() {
				// ── Scoped paths: emit as one reference ──────────────────────
				K_SCOPED_IDENTIFIER | K_SCOPED_TYPE_IDENTIFIER => {
					// Only emit the outermost scoped path; inner ones are nested
					// inside this node and will be filtered by `covered`.
					let segments = collect_scoped_segments(node, src);
					if segments.is_empty() {
						return;
					}
					// Determine kind from context (parent node).
					let parent_kind = node.parent().map(|p| p.kind());
					let kind = scoped_ref_kind(node, parent_kind);
					covered.push(span.clone());
					out.push(RawReference { segments, span, kind, receiver: None });
				}

				// ── Function / method calls ───────────────────────────────────
				K_CALL_EXPRESSION => {
					// The `function` field is what is being called.
					if let Some(func) = node.child_by_field_name("function") {
						match func.kind() {
							// plain identifier call: `foo()`
							K_IDENTIFIER => {
								let name = node_text(func, src).to_owned();
								let s = func.byte_range();
								if !covered.iter().any(|r| r.start <= s.start && s.end <= r.end) {
									covered.push(s.clone());
									out.push(RawReference {
										segments: vec![name],
										span: s,
										kind: ReferenceKind::FunctionCall,
										receiver: None,
									});
								}
							}
							// method call: `expr.method(args)`
							K_FIELD_EXPRESSION => {
								emit_method_call(func, src, &covered, &mut out);
								covered.push(func.byte_range());
							}
							// scoped call: `Foo::bar()` — already handled above as
							// scoped_identifier; nothing additional needed here.
							_ => {}
						}
					}
				}

				// ── Macro invocations ─────────────────────────────────────────
				K_MACRO_INVOCATION => {
					// The `macro` field is the path of the macro.
					if let Some(macro_node) = node.child_by_field_name("macro") {
						let segments = ident_or_scoped_segments(macro_node, src);
						if !segments.is_empty() {
							let s = macro_node.byte_range();
							if !covered.iter().any(|r| r.start <= s.start && s.end <= r.end) {
								covered.push(s.clone());
								out.push(RawReference {
									segments,
									span: s,
									kind: ReferenceKind::MacroInvocation,
									receiver: None,
								});
							}
						}
					}
				}

				// ── Bare type identifiers in type position ────────────────────
				K_TYPE_IDENTIFIER => {
					let s = node.byte_range();
					if !covered.iter().any(|r| r.start <= s.start && s.end <= r.end) {
						let name = node_text(node, src).to_owned();
						covered.push(s.clone());
						out.push(RawReference {
							segments: vec![name],
							span: s,
							kind: ReferenceKind::TypeReference,
							receiver: None,
						});
					}
				}

				// ── Field access (not a call) ─────────────────────────────────
				K_FIELD_EXPRESSION => {
					// Only emit if not the function part of a call_expression
					// (those are handled in K_CALL_EXPRESSION above).
					let is_callee = node
						.parent()
						.and_then(|p| {
							if p.kind() == K_CALL_EXPRESSION {
								p.child_by_field_name("function")
							} else {
								None
							}
						})
						.map(|f| f.id() == node.id())
						.unwrap_or(false);
					if !is_callee {
						let s = node.byte_range();
						if !covered.iter().any(|r| r.start <= s.start && s.end <= r.end) {
							if let Some(field) = node.child_by_field_name("field") {
								let receiver_node = node.child_by_field_name("value");
								let receiver = receiver_node.map(|r| {
									let rt = node_text(r, src);
									if rt == "self" { ReceiverShape::SelfRef } else { ReceiverShape::Expr(rt.to_owned()) }
								});
								let field_name = node_text(field, src).to_owned();
								covered.push(s.clone());
								out.push(RawReference {
									segments: vec![field_name],
									span: s,
									kind: ReferenceKind::FieldAccess,
									receiver,
								});
							}
						}
					}
				}

				_ => {}
			}
		});

		out
	}

	/// Derive the in-crate module path from a file path relative to the package
	/// root, following standard Rust `src/` layout conventions.
	///
	/// Rules:
	/// - The crate name (`layout.package`) is the root segment, matching the
	///   rust-analyzer producer's `{crate}::{item}` fq scheme (so `src/lib.rs`
	///   items attribute to `crate::item`, not a bare `item`).
	/// - Strip a leading `src/` component.
	/// - `lib.rs`, `main.rs`, `mod.rs` → the directory's module path (filename
	///   dropped; crate root `src/lib.rs` → `[crate]`).
	/// - `foo.rs` → `[crate, …parent, "foo"]`.
	fn module_path(&self, rel: &Path, layout: &PackageLayout) -> Vec<String> {
		let mut components: Vec<&str> = rel
			.components()
			.filter_map(|c| {
				if let std::path::Component::Normal(s) = c {
					s.to_str()
				} else {
					None
				}
			})
			.collect();

		// Strip leading `src/`.
		if components.first() == Some(&"src") {
			components.remove(0);
		}

		// Determine if the last component is a "boundary" file that names the
		// directory module rather than adding a new segment.
		let is_boundary = components
			.last()
			.map(|f| matches!(*f, "lib.rs" | "main.rs" | "mod.rs"))
			.unwrap_or(false);

		if is_boundary {
			// Drop the filename; the parent directory is already represented by
			// the preceding segments.
			components.pop();
		} else if let Some(last) = components.last_mut() {
			// Strip the `.rs` extension from the leaf filename.
			if let Some(stem) = last.strip_suffix(".rs") {
				*last = stem;
			}
		}

		std::iter::once(layout.package.clone())
			.chain(components.into_iter().map(str::to_owned))
			.collect()
	}
}

// ─── Import expansion helpers ─────────────────────────────────────────────────

/// Recursively expand a `use` tree node into flat [`ImportBinding`]s, prepending
/// `prefix_segs` (the path segments accumulated from outer scoped nodes).
fn collect_use_bindings(
	node: tree_sitter::Node,
	prefix_segs: &[String],
	src: &str,
	use_start: usize,
	out: &mut Vec<ImportBinding>,
) {
	match node.kind() {
		// `a::b::{…}` — recurse into the list with extended prefix.
		K_SCOPED_USE_TREE => {
			let path_segs: Vec<String> = if let Some(path) = node.child_by_field_name("path") {
				ident_or_scoped_segments(path, src)
			} else {
				Vec::new()
			};
			let mut extended: Vec<String> = prefix_segs.to_vec();
			extended.extend(path_segs);
			if let Some(list) = node.child_by_field_name("list") {
				collect_use_bindings(list, &extended, src, use_start, out);
			}
		}

		// `{a, b, c}` — expand each child.
		K_USE_LIST => {
			let mut cursor = node.walk();
			for child in node.children(&mut cursor) {
				if child.is_named() {
					collect_use_bindings(child, prefix_segs, src, use_start, out);
				}
			}
		}

		// `foo as Bar` — use `Bar` as local, full path as source.
		K_USE_AS_CLAUSE => {
			let name_node = node.child_by_field_name("name");
			let alias_node = node.child_by_field_name("alias");
			if let (Some(name_n), Some(alias_n)) = (name_node, alias_node) {
				let mut segs = prefix_segs.to_vec();
				segs.push(node_text(name_n, src).to_owned());
				let local = node_text(alias_n, src).to_owned();
				let source = segs_to_source(segs);
				let span = use_start..node.end_byte();
				out.push(ImportBinding { local, source, span });
			}
		}

		// `*` glob.
		K_USE_WILDCARD => {
			let source = ImportSource::Glob(ImportPrefix {
				dependency: None,
				path: prefix_segs.to_vec(),
			});
			let span = use_start..node.end_byte();
			out.push(ImportBinding { local: String::new(), source, span });
		}

		// Plain `identifier` or `scoped_identifier` — a leaf binding.
		K_IDENTIFIER | K_SCOPED_IDENTIFIER => {
			let leaf_segs = ident_or_scoped_segments(node, src);
			if leaf_segs.is_empty() {
				return;
			}
			let local = leaf_segs.last().unwrap().clone();
			let mut segs = prefix_segs.to_vec();
			segs.extend(leaf_segs);
			let source = segs_to_source(segs);
			let span = use_start..node.end_byte();
			out.push(ImportBinding { local, source, span });
		}

		_ => {
			// Anything else (e.g. `self` in `use foo::{self}`) — treat as a
			// leaf identifier.
			let text = node_text(node, src);
			if text.is_empty() || text == "," || text == "{" || text == "}" {
				return;
			}
			let local = text.to_owned();
			let mut segs = prefix_segs.to_vec();
			segs.push(local.clone());
			let source = segs_to_source(segs);
			let span = use_start..node.end_byte();
			out.push(ImportBinding { local, source, span });
		}
	}
}

/// Convert a flat segment list to [`ImportSource`].
///
/// All paths are emitted as [`ImportSource::Internal`]. The resolver classifies
/// them as external when the head segment matches a known dependency (see
/// REFERENCES-PLAN §4.2). `crate::`, `self::`, and `super::` prefixes are kept
/// as literal head segments.
fn segs_to_source(segs: Vec<String>) -> ImportSource {
	ImportSource::Internal(segs)
}

// ─── Reference assembly helpers ───────────────────────────────────────────────

/// Collect the full segment list from a `scoped_identifier` /
/// `scoped_type_identifier` node, walking the recursive `path` + `name`
/// structure.
fn collect_scoped_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	let mut segs: Vec<String> = Vec::new();
	collect_scoped_into(node, src, &mut segs);
	segs
}

fn collect_scoped_into(node: tree_sitter::Node, src: &str, out: &mut Vec<String>) {
	match node.kind() {
		K_SCOPED_IDENTIFIER | K_SCOPED_TYPE_IDENTIFIER => {
			if let Some(path) = node.child_by_field_name("path") {
				collect_scoped_into(path, src, out);
			}
			if let Some(name) = node.child_by_field_name("name") {
				out.push(node_text(name, src).to_owned());
			}
		}
		K_IDENTIFIER | K_TYPE_IDENTIFIER => {
			out.push(node_text(node, src).to_owned());
		}
		_ => {
			// `self`, `crate`, `super` are `self`, `crate`, `super` node kinds
			// in the grammar but their text is still the keyword.
			let text = node_text(node, src);
			if !text.is_empty() {
				out.push(text.to_owned());
			}
		}
	}
}

/// Get segments from either a plain `identifier`/`type_identifier` or a
/// `scoped_identifier`/`scoped_type_identifier`.
fn ident_or_scoped_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	match node.kind() {
		K_SCOPED_IDENTIFIER | K_SCOPED_TYPE_IDENTIFIER => collect_scoped_segments(node, src),
		K_IDENTIFIER | K_TYPE_IDENTIFIER => vec![node_text(node, src).to_owned()],
		_ => {
			let text = node_text(node, src);
			if text.is_empty() { vec![] } else { vec![text.to_owned()] }
		}
	}
}

/// Determine the [`ReferenceKind`] for a `scoped_identifier` /
/// `scoped_type_identifier` from context.
fn scoped_ref_kind(node: tree_sitter::Node, parent_kind: Option<&str>) -> ReferenceKind {
	match (node.kind(), parent_kind) {
		(_, Some(K_USE_DECLARATION | K_SCOPED_USE_TREE | K_USE_AS_CLAUSE | K_USE_LIST)) => {
			ReferenceKind::Import
		}
		(K_SCOPED_TYPE_IDENTIFIER, _) => ReferenceKind::TypeReference,
		// A scoped_identifier that is the function in a call_expression is a
		// qualified function call (UFCS or associated fn).
		(K_SCOPED_IDENTIFIER, Some(K_CALL_EXPRESSION)) => ReferenceKind::FunctionCall,
		(K_SCOPED_IDENTIFIER, _) => ReferenceKind::TypeReference,
		_ => ReferenceKind::TypeReference,
	}
}

/// Emit a [`RawReference`] for a `field_expression` used as the callee of a
/// `call_expression` (i.e. a method call `receiver.method(args)`).
fn emit_method_call(
	field_expr: tree_sitter::Node,
	src: &str,
	covered: &[std::ops::Range<usize>],
	out: &mut Vec<RawReference>,
) {
	let s = field_expr.byte_range();
	if covered.iter().any(|r| r.start <= s.start && s.end <= r.end) {
		return;
	}
	let method_node = field_expr.child_by_field_name("field");
	let receiver_node = field_expr.child_by_field_name("value");

	let method_name = match method_node {
		Some(m) => node_text(m, src).to_owned(),
		None => return,
	};

	let receiver = receiver_node.map(|r| {
		let rt = node_text(r, src);
		if rt == "self" || rt == "Self" {
			ReceiverShape::SelfRef
		} else {
			ReceiverShape::Expr(rt.to_owned())
		}
	});

	out.push(RawReference {
		segments: vec![method_name],
		span: s,
		kind: ReferenceKind::MethodCall,
		receiver,
	});
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::Path;

	use arborium_tree_sitter as tree_sitter;

	use super::RustSpec;
	use super::super::spec::{DefKind, ImportSource, LanguageSpec, PackageLayout, ReceiverShape};
	use ir::syntax::ReferenceKind;

	// ── Parse helper ────────────────────────────────────────────────────────────

	fn parse(src: &str) -> tree_sitter::Tree {
		let mut parser = tree_sitter::Parser::new();
		parser.set_language(&arborium::get_language("rust").unwrap()).unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn layout() -> PackageLayout {
		PackageLayout { package: "mycrate".into() }
	}

	// ── Reference tests ─────────────────────────────────────────────────────────

	/// A free function call `foo()` emits a FunctionCall reference.
	#[test]
	fn free_fn_call() {
		let src = "fn main() { foo(); }";
		let tree = parse(src);
		let refs = RustSpec.references(&tree, src);
		let call = refs.iter().find(|r| r.segments == &["foo"]);
		assert!(call.is_some(), "expected reference to `foo`, got {refs:?}");
		assert_eq!(call.unwrap().kind, ReferenceKind::FunctionCall);
		assert!(call.unwrap().receiver.is_none());
	}

	/// A method call `self.process()` emits a MethodCall with SelfRef receiver.
	#[test]
	fn method_call_self_receiver() {
		let src = "fn go(self) { self.process(); }";
		let tree = parse(src);
		let refs = RustSpec.references(&tree, src);
		let mc = refs.iter().find(|r| r.segments == &["process"]);
		assert!(mc.is_some(), "expected `process` method call, got {refs:?}");
		assert_eq!(mc.unwrap().kind, ReferenceKind::MethodCall);
		assert_eq!(mc.unwrap().receiver, Some(ReceiverShape::SelfRef));
	}

	/// A method call `obj.do_thing()` emits a MethodCall with Expr receiver.
	#[test]
	fn method_call_expr_receiver() {
		let src = "fn f(obj: Foo) { obj.do_thing(); }";
		let tree = parse(src);
		let refs = RustSpec.references(&tree, src);
		let mc = refs.iter().find(|r| r.segments == &["do_thing"]);
		assert!(mc.is_some(), "expected `do_thing` method call, got {refs:?}");
		assert_eq!(mc.unwrap().kind, ReferenceKind::MethodCall);
		assert_eq!(mc.unwrap().receiver, Some(ReceiverShape::Expr("obj".into())));
	}

	/// `a::b::c` is emitted as ONE reference with segments `["a","b","c"]`.
	#[test]
	fn scoped_path_is_single_reference() {
		let src = "fn f() { let _ = a::b::c; }";
		let tree = parse(src);
		let refs = RustSpec.references(&tree, src);
		let chain = refs.iter().find(|r| r.segments == &["a", "b", "c"]);
		assert!(chain.is_some(), "expected chain [a,b,c] as single ref, got {refs:?}");
		// No separate `a::b` or `b` or `c` references.
		assert!(
			refs.iter().all(|r| r.segments != &["a", "b"]),
			"inner scoped path should not be emitted separately"
		);
	}

	/// A macro invocation `println!(…)` emits MacroInvocation.
	#[test]
	fn macro_invocation() {
		let src = r#"fn f() { println!("hi"); }"#;
		let tree = parse(src);
		let refs = RustSpec.references(&tree, src);
		let mac = refs.iter().find(|r| r.segments == &["println"]);
		assert!(mac.is_some(), "expected `println` macro ref, got {refs:?}");
		assert_eq!(mac.unwrap().kind, ReferenceKind::MacroInvocation);
	}

	// ── Import tests ─────────────────────────────────────────────────────────────

	/// `use std::collections::HashMap as Map;` → local = "Map", Internal path.
	#[test]
	fn use_as_clause() {
		let src = "use std::collections::HashMap as Map;";
		let tree = parse(src);
		let imports = RustSpec.imports(&tree, src);
		let binding = imports.iter().find(|b| b.local == "Map");
		assert!(binding.is_some(), "expected `Map` binding, got {imports:?}");
		match &binding.unwrap().source {
			ImportSource::Internal(segs) => {
				assert!(segs.contains(&"HashMap".to_string()), "expected HashMap in path, got {segs:?}");
			}
			other => panic!("expected Internal, got {other:?}"),
		}
	}

	/// `use a::{b, c::D};` expands to two bindings.
	#[test]
	fn nested_use_braces() {
		let src = "use a::{b, c::D};";
		let tree = parse(src);
		let imports = RustSpec.imports(&tree, src);
		let locals: Vec<&str> = imports.iter().map(|b| b.local.as_str()).collect();
		assert!(locals.contains(&"b"), "expected `b` binding, got {locals:?}");
		assert!(locals.contains(&"D"), "expected `D` binding, got {locals:?}");
		// Verify the path for `D` contains `c` and `D`.
		let d_binding = imports.iter().find(|b| b.local == "D").unwrap();
		match &d_binding.source {
			ImportSource::Internal(segs) => {
				assert!(segs.contains(&"c".to_string()), "expected `c` in D's path, got {segs:?}");
				assert!(segs.contains(&"D".to_string()), "expected `D` in D's path, got {segs:?}");
			}
			other => panic!("expected Internal for D, got {other:?}"),
		}
	}

	/// `use foo::bar::*;` emits a Glob binding.
	#[test]
	fn glob_use() {
		let src = "use foo::bar::*;";
		let tree = parse(src);
		let imports = RustSpec.imports(&tree, src);
		let glob = imports.iter().find(|b| b.local.is_empty());
		assert!(glob.is_some(), "expected glob binding, got {imports:?}");
		match &glob.unwrap().source {
			ImportSource::Glob(prefix) => {
				assert!(
					prefix.path.contains(&"foo".to_string()),
					"expected `foo` in glob prefix, got {:?}",
					prefix.path
				);
				assert!(
					prefix.path.contains(&"bar".to_string()),
					"expected `bar` in glob prefix, got {:?}",
					prefix.path
				);
			}
			other => panic!("expected Glob, got {other:?}"),
		}
	}

	// ── Definition tests ─────────────────────────────────────────────────────────

	/// An `impl` block with a method: the method's parent should be the impl def,
	/// and the impl's name should reflect the implementing type.
	#[test]
	fn impl_block_with_method() {
		let src = "struct Foo; impl Foo { fn bar(&self) {} }";
		let tree = parse(src);
		let defs = RustSpec.definitions(&tree, src);

		// Find the impl definition.
		let impl_def = defs.iter().find(|d| matches!(d.kind, DefKind::Impl { .. }));
		assert!(impl_def.is_some(), "expected impl definition, got {defs:?}");
		assert_eq!(impl_def.unwrap().name, "Foo");

		// Find the method.
		let method_def = defs.iter().find(|d| d.name == "bar");
		assert!(method_def.is_some(), "expected `bar` method definition, got {defs:?}");
		assert_eq!(method_def.unwrap().kind, DefKind::Method);

		// Verify nesting: bar's parent is the impl.
		let impl_idx = defs.iter().position(|d| matches!(d.kind, DefKind::Impl { .. })).unwrap();
		assert_eq!(
			method_def.unwrap().parent,
			Some(impl_idx),
			"method parent should be impl index {impl_idx}"
		);
	}

	/// A free function at module level has `parent = None`.
	#[test]
	fn free_function_has_no_parent() {
		let src = "fn standalone() {}";
		let tree = parse(src);
		let defs = RustSpec.definitions(&tree, src);
		let f = defs.iter().find(|d| d.name == "standalone");
		assert!(f.is_some(), "expected `standalone` definition, got {defs:?}");
		assert_eq!(f.unwrap().kind, DefKind::Function);
		assert_eq!(f.unwrap().parent, None);
	}

	/// A trait definition is extracted as `DefKind::Trait`.
	#[test]
	fn trait_definition() {
		let src = "trait Animal { fn speak(&self); }";
		let tree = parse(src);
		let defs = RustSpec.definitions(&tree, src);
		let tr = defs.iter().find(|d| d.name == "Animal");
		assert!(tr.is_some(), "expected `Animal` trait, got {defs:?}");
		assert_eq!(tr.unwrap().kind, DefKind::Trait);
	}

	/// `impl Trait for Type` captures the trait name in `DefKind::Impl { of }`.
	#[test]
	fn trait_impl_captures_of() {
		let src = "impl Display for MyType { fn fmt(&self, f: &mut Formatter) {} }";
		let tree = parse(src);
		let defs = RustSpec.definitions(&tree, src);
		let impl_def = defs.iter().find(|d| matches!(&d.kind, DefKind::Impl { of: Some(t) } if t == "Display"));
		assert!(impl_def.is_some(), "expected impl Display for MyType, got {defs:?}");
	}

	/// A struct item is `DefKind::Type`.
	#[test]
	fn struct_is_type_def() {
		let src = "struct Point { x: f64, y: f64 }";
		let tree = parse(src);
		let defs = RustSpec.definitions(&tree, src);
		let s = defs.iter().find(|d| d.name == "Point");
		assert!(s.is_some(), "expected `Point` definition, got {defs:?}");
		assert_eq!(s.unwrap().kind, DefKind::Type);
	}

	// ── Module path tests ────────────────────────────────────────────────────────

	/// `src/lib.rs` → crate root → `[]`.
	#[test]
	fn module_path_lib_rs() {
		let path = RustSpec.module_path(Path::new("src/lib.rs"), &layout());
		assert_eq!(path, Vec::<String>::new());
	}

	/// `src/main.rs` → crate root → `[]`.
	#[test]
	fn module_path_main_rs() {
		let path = RustSpec.module_path(Path::new("src/main.rs"), &layout());
		assert_eq!(path, Vec::<String>::new());
	}

	/// `src/foo.rs` → `["foo"]`.
	#[test]
	fn module_path_foo_rs() {
		let path = RustSpec.module_path(Path::new("src/foo.rs"), &layout());
		assert_eq!(path, vec!["foo".to_string()]);
	}

	/// `src/foo/mod.rs` → `["foo"]`.
	#[test]
	fn module_path_foo_mod_rs() {
		let path = RustSpec.module_path(Path::new("src/foo/mod.rs"), &layout());
		assert_eq!(path, vec!["foo".to_string()]);
	}

	/// `src/foo/bar.rs` → `["foo", "bar"]`.
	#[test]
	fn module_path_nested() {
		let path = RustSpec.module_path(Path::new("src/foo/bar.rs"), &layout());
		assert_eq!(path, vec!["foo".to_string(), "bar".to_string()]);
	}

	/// A path without `src/` prefix is handled gracefully.
	#[test]
	fn module_path_no_src_prefix() {
		let path = RustSpec.module_path(Path::new("lib/util.rs"), &layout());
		assert_eq!(path, vec!["lib".to_string(), "util".to_string()]);
	}
}
