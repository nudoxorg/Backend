//! `LanguageSpec` for TypeScript / TSX / JavaScript.
//!
//! Implements [`LanguageSpec`] for TypeScript by cursor-walking the tree-sitter
//! CST.  Three independent passes share the same tree:
//!
//! 1. **Definitions** — a pre-order stack walk that tracks nesting depth so each
//!    [`RawDefinition`] records its lexically-enclosing parent index.
//! 2. **Imports** — a flat pre-order walk that fires on every `import_statement`
//!    (ESM), `export_statement` with a re-export source (barrel), or
//!    `lexical_declaration`/`variable_declaration` containing `require()` (CJS).
//! 3. **References** — a flat pre-order walk that assembles full qualifier chains
//!    from `call_expression`, `member_expression`, and `type_identifier` nodes.
//!
//! ## Grammar pin
//! Tested against the `tree-sitter-typescript` grammar shipped with `arborium`.
//! Known node kinds used (dual-accept for older grammar revisions noted inline):
//!
//! - `function_declaration`, `method_definition`, `class_declaration`,
//!   `interface_declaration`, `type_alias_declaration`, `enum_declaration`
//! - `call_expression`, `member_expression`, `property_identifier`
//!   (+ legacy alias `property_access_expression`)
//! - `type_identifier`, `nested_type_identifier`
//! - `import_statement`, `import_clause`, `named_imports`, `import_specifier`,
//!   `namespace_import`
//! - JSX (tsx grammar only): `jsx_opening_element`, `jsx_self_closing_element`
//!
//! ## Oracle / deferred
//! - Deep re-export chain resolution (more than one hop) is deferred to the
//!   oracle layer.  We emit what we can cheaply parse in one pass.
//! - Method-call target resolution (which overloaded definition owns a given
//!   `obj.method()`) is oracle: we record the receiver text / SelfRef shape and
//!   leave pointer resolution to the resolver.
//! - Dynamic `require()` paths (template literals, variables) are skipped.

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportSource, LanguageSpec, PackageLayout, RawDefinition,
	RawReference, ReceiverShape, node_text, walk_preorder,
};

// ─── Node-kind constants (grammar pin) ──────────────────────────────────────
//
// All tree-sitter node kind strings are centralised here.  When the grammar
// version shipped by arborium changes, update only these constants.

/// `function_declaration` — top-level and nested named functions.
const K_FUNCTION_DECL: &str = "function_declaration";
/// `method_definition` — class method bodies.
const K_METHOD_DEF: &str = "method_definition";
/// `class_declaration` — `class Foo { … }`.
const K_CLASS_DECL: &str = "class_declaration";
/// `interface_declaration` — `interface Foo { … }`.
const K_INTERFACE_DECL: &str = "interface_declaration";
/// `type_alias_declaration` — `type Foo = …`.
const K_TYPE_ALIAS: &str = "type_alias_declaration";
/// `enum_declaration` — `enum Color { … }`.
const K_ENUM_DECL: &str = "enum_declaration";

/// `call_expression` — any invocation `f(…)` or `obj.m(…)`.
const K_CALL_EXPR: &str = "call_expression";
/// `member_expression` — `obj.prop`.
const K_MEMBER_EXPR: &str = "member_expression";
/// Legacy grammar alias for `member_expression` (some older grammars).
const K_MEMBER_EXPR_LEGACY: &str = "property_access_expression";
/// `property_identifier` — the right-hand name in `obj.prop`.
/// Accessed via [`collect_member_chain`]'s `"property"` field lookup, not matched directly.
#[allow(dead_code)]
const K_PROP_ID: &str = "property_identifier";
/// `type_identifier` — names in type positions.
const K_TYPE_ID: &str = "type_identifier";
/// `nested_type_identifier` — qualified type `A.B` in type position.
const K_NESTED_TYPE_ID: &str = "nested_type_identifier";

/// `import_statement` — `import … from '…'`.
const K_IMPORT_STMT: &str = "import_statement";
/// `import_clause` — the binding list between `import` and `from`.
const K_IMPORT_CLAUSE: &str = "import_clause";
/// `named_imports` — the `{ … }` braced specifier list.
const K_NAMED_IMPORTS: &str = "named_imports";
/// `import_specifier` — one `name` or `name as local` entry.
const K_IMPORT_SPEC: &str = "import_specifier";
/// `namespace_import` — `* as ns`.
const K_NS_IMPORT: &str = "namespace_import";
/// `export_statement` — catches `export { x } from './y'` re-exports.
const K_EXPORT_STMT: &str = "export_statement";
/// `string` — the module-specifier literal.
const K_STRING: &str = "string";

/// JSX opening element (tsx grammar).  Guarded: only matched when the node
/// kind is present; absent in the plain typescript grammar.
const K_JSX_OPENING: &str = "jsx_opening_element";
/// JSX self-closing element `<Foo />` (tsx grammar).
const K_JSX_SELF_CLOSING: &str = "jsx_self_closing_element";

// ─── TypeScript structural extractor ────────────────────────────────────────

/// TypeScript / TSX / JS structural extractor.
///
/// Pass one grammar for `.ts`/`.mts`/`.cts` files and the `tsx` grammar for
/// `.tsx`/`.jsx` files; both are handled correctly.
pub struct TypescriptSpec;

// ─── Module-specifier helpers ────────────────────────────────────────────────

/// Strip the opening/closing quote characters from a tree-sitter `string` or
/// `template_string` node's text.  Returns an empty string when the literal is
/// empty or has no recognised delimiters.
fn unquote(raw: &str) -> &str {
	let raw = raw.trim();
	if raw.len() < 2 {
		return raw;
	}
	let (first, last) = (raw.as_bytes()[0], raw.as_bytes()[raw.len() - 1]);
	if (first == b'\'' || first == b'"' || first == b'`') && first == last {
		&raw[1..raw.len() - 1]
	} else {
		raw
	}
}

/// Classify a module specifier string into [`ImportSource`].
///
/// - A specifier starting with `.` or `/` is internal; split by `/` to produce
///   the path segments (leading `.`/`..` kept only when meaningful, otherwise
///   stripped to bare path segments like the deno/oxc convention).
/// - Everything else is an external package: the first `/`-segment is the
///   dependency name (scoped packages starting with `@` consume two segments),
///   the rest form the intra-package path.
fn classify_specifier(specifier: &str) -> ImportSource {
	if specifier.starts_with('.') || specifier.starts_with('/') {
		// Internal (relative or absolute) path.
		let segments: Vec<String> = specifier
			.split('/')
			.filter(|s| !s.is_empty() && *s != "." && *s != "..")
			.map(str::to_owned)
			.collect();
		ImportSource::Internal(segments)
	} else {
		// External bare specifier: `react`, `@scope/pkg`, `lodash/fp`.
		let parts: Vec<&str> = specifier.splitn(3, '/').collect();
		let (dependency, path) = if specifier.starts_with('@') && parts.len() >= 2 {
			// Scoped package: `@scope/pkg` or `@scope/pkg/sub`.
			let dep = format!("{}/{}", parts[0], parts[1]);
			let path: Vec<String> = if parts.len() > 2 {
				parts[2].split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect()
			} else {
				vec![]
			};
			(dep, path)
		} else {
			let dep = parts[0].to_owned();
			let path: Vec<String> = if parts.len() > 1 {
				parts[1..].join("/").split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect()
			} else {
				vec![]
			};
			(dep, path)
		};
		ImportSource::External { dependency, path }
	}
}

// ─── Member-expression chain collector ──────────────────────────────────────

/// Walk a `member_expression` (or `property_access_expression`) node and
/// collect the flat identifier chain from outermost to innermost, e.g.:
///
/// ```text
/// a.b.c  →  ["a", "b", "c"]
/// ```
///
/// We recurse into the `object` field to handle chained expressions.
fn collect_member_chain(node: tree_sitter::Node, src: &str) -> Vec<String> {
	if node.kind() == K_MEMBER_EXPR || node.kind() == K_MEMBER_EXPR_LEGACY {
		let mut chain = Vec::new();
		if let Some(obj) = node.child_by_field_name("object") {
			chain.extend(collect_member_chain(obj, src));
		}
		if let Some(prop) = node.child_by_field_name("property") {
			chain.push(node_text(prop, src).to_owned());
		}
		chain
	} else {
		// Leaf: an identifier, `this`, etc.
		vec![node_text(node, src).to_owned()]
	}
}

// ─── LanguageSpec implementation ─────────────────────────────────────────────

impl LanguageSpec for TypescriptSpec {
	// ── Definitions ─────────────────────────────────────────────────────────
	//
	// We do a manual pre-order DFS (not via `walk_preorder`) so we can maintain
	// an explicit scope stack.  Each definition pushed to the stack becomes the
	// `parent` for any definitions we encounter while inside its body span.

	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut defs: Vec<RawDefinition> = Vec::new();
		// (definition index, body_span_end) — tracks the current scope.
		let mut scope_stack: Vec<(usize, usize)> = Vec::new();

		// Inner recursive DFS.  We accept a closure so we can borrow `defs` and
		// `scope_stack` mutably.
		fn visit(
			node: tree_sitter::Node,
			src: &str,
			defs: &mut Vec<RawDefinition>,
			scope_stack: &mut Vec<(usize, usize)>,
		) {
			// Pop scopes whose body spans have ended before this node starts.
			let node_start = node.byte_range().start;
			while let Some(&(_, end)) = scope_stack.last() {
				if node_start >= end {
					scope_stack.pop();
				} else {
					break;
				}
			}

			let kind = node.kind();
			let def_kind: Option<DefKind> = match kind {
				K_FUNCTION_DECL => Some(DefKind::Function),
				K_METHOD_DEF => Some(DefKind::Method),
				K_CLASS_DECL => Some(DefKind::Class),
				K_INTERFACE_DECL => Some(DefKind::Trait),
				K_TYPE_ALIAS | K_ENUM_DECL => Some(DefKind::Type),
				_ => None,
			};

			if let Some(dk) = def_kind {
				if let Some(name_node) = node.child_by_field_name("name") {
					let name = node_text(name_node, src).to_owned();
					let name_range = name_node.byte_range();
					let body_range = node.byte_range();
					let parent = scope_stack.last().map(|&(idx, _)| idx);
					let def_idx = defs.len();
					defs.push(RawDefinition {
						name,
						kind: dk,
						name_span: name_range.start..name_range.end,
						body_span: body_range.start..body_range.end,
						parent,
					});
					// Push this definition onto the scope stack so nested
					// definitions see it as their parent.
					scope_stack.push((def_idx, body_range.end));
				}
			}

			// Recurse into all children.
			let mut cursor = node.walk();
			if cursor.goto_first_child() {
				loop {
					visit(cursor.node(), src, defs, scope_stack);
					if !cursor.goto_next_sibling() {
						break;
					}
				}
			}
		}

		visit(tree.root_node(), src, &mut defs, &mut scope_stack);
		defs
	}

	// ── Imports ──────────────────────────────────────────────────────────────
	//
	// ESM `import` statements, re-export barrels, and best-effort CJS `require`.
	// We use `walk_preorder` for the flat scan; detailed node traversal is done
	// locally once we find an import-shaped root.

	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut bindings: Vec<ImportBinding> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			match node.kind() {
				K_IMPORT_STMT => {
					// Extract the module specifier string.
					let specifier_raw = match node.child_by_field_name("source") {
						Some(n) => unquote(node_text(n, src)).to_owned(),
						None => return,
					};
					let source_node = match node.child_by_field_name("source") {
						Some(n) => n,
						None => return,
					};

					// Walk the import_clause to find named, default, and namespace
					// bindings.
					let mut node_cur = node.walk();
					let clause = node
						.children(&mut node_cur)
						.find(|c| c.kind() == K_IMPORT_CLAUSE);

					if let Some(clause) = clause {
						emit_import_clause(
							clause,
							src,
							&specifier_raw,
							source_node.byte_range().start..source_node.byte_range().end,
							&mut bindings,
						);
					}
				}

				K_EXPORT_STMT => {
					// `export { x } from './y'`  — re-export barrel (one hop).
					// Only process when a `source` field is present.
					let source_node = match node.child_by_field_name("source") {
						Some(n) => n,
						None => return,
					};
					let specifier_raw = unquote(node_text(source_node, src)).to_owned();
					let src_span =
						source_node.byte_range().start..source_node.byte_range().end;

					// Collect the exported names from `export_clause` children.
					let mut node_cur2 = node.walk();
					for child in node.children(&mut node_cur2) {
						if child.kind() == "export_clause" {
							let mut child_cur = child.walk();
							for spec in child.children(&mut child_cur) {
								if spec.kind() == "export_specifier" {
									// The exported local name is under the `name`
									// field; an `as` rename lives under `alias`.
									let local = spec
										.child_by_field_name("name")
										.map(|n| node_text(n, src).to_owned())
										.unwrap_or_default();
									if local.is_empty() {
										continue;
									}
									let source = classify_specifier(&specifier_raw);
									bindings.push(ImportBinding {
										local,
										source,
										span: src_span.clone(),
									});
								}
							}
						}
					}
				}

				// CJS: `const foo = require('bar')` or
				//       `const { a, b } = require('bar')`.
				// Best-effort: only handle the trivial single-identifier and
				// destructuring patterns; skip dynamic specifiers.
				"lexical_declaration" | "variable_declaration" => {
					try_emit_require(node, src, &mut bindings);
				}

				_ => {}
			}
		});

		bindings
	}

	// ── References ───────────────────────────────────────────────────────────
	//
	// One `RawReference` per interesting use-site.  We walk pre-order and emit
	// at the *outermost* call/member/type node to capture the full chain.
	// Visited-node deduplication prevents double-counting sub-chains.

	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut refs: Vec<RawReference> = Vec::new();
		// Track byte-range starts already emitted to suppress inner duplicates
		// when we walk into a member chain's sub-nodes.
		let mut emitted: std::collections::HashSet<usize> = std::collections::HashSet::new();

		walk_preorder(tree, |node, _depth| {
			let start = node.byte_range().start;
			let end = node.byte_range().end;

			match node.kind() {
				// ── Call expressions ─────────────────────────────────────────
				K_CALL_EXPR => {
					if emitted.contains(&start) {
						return;
					}
					let function_node = match node.child_by_field_name("function") {
						Some(n) => n,
						None => return,
					};

					if function_node.kind() == K_MEMBER_EXPR
						|| function_node.kind() == K_MEMBER_EXPR_LEGACY
					{
						// `obj.method(…)` → MethodCall
						let chain = collect_member_chain(function_node, src);
						if chain.is_empty() {
							return;
						}
						let receiver = if chain[0] == "this" || chain[0] == "self" {
							ReceiverShape::SelfRef
						} else {
							ReceiverShape::Expr(chain[0].clone())
						};
						emitted.insert(start);
						refs.push(RawReference {
							segments: chain,
							span: start..end,
							kind: ReferenceKind::MethodCall,
							receiver: Some(receiver),
						});
					} else if function_node.kind() == "identifier" {
						// `foo(…)` → FunctionCall
						let name = node_text(function_node, src).to_owned();
						emitted.insert(start);
						refs.push(RawReference {
							segments: vec![name],
							span: start..end,
							kind: ReferenceKind::FunctionCall,
							receiver: None,
						});
					}
				}

				// ── Member expressions not under a call ──────────────────────
				K_MEMBER_EXPR | K_MEMBER_EXPR_LEGACY => {
					if emitted.contains(&start) {
						return;
					}
					// Only emit if the parent is NOT a call_expression's `function`
					// child (already handled above) and not the `object` side of
					// another member expression (will be handled at the outer node).
					let parent_kind =
						node.parent().map(|p| p.kind()).unwrap_or("");
					if parent_kind == K_CALL_EXPR || parent_kind == K_MEMBER_EXPR
						|| parent_kind == K_MEMBER_EXPR_LEGACY
					{
						return;
					}
					let chain = collect_member_chain(node, src);
					if chain.is_empty() {
						return;
					}
					emitted.insert(start);
					refs.push(RawReference {
						segments: chain,
						span: start..end,
						kind: ReferenceKind::FieldAccess,
						receiver: None,
					});
				}

				// ── Type identifiers ─────────────────────────────────────────
				K_TYPE_ID => {
					if emitted.contains(&start) {
						return;
					}
					// Skip if the parent is a `nested_type_identifier`; we emit
					// the whole chain at the outer node.
					if node.parent().map(|p| p.kind()).unwrap_or("") == K_NESTED_TYPE_ID
					{
						return;
					}
					let name = node_text(node, src).to_owned();
					emitted.insert(start);
					refs.push(RawReference {
						segments: vec![name],
						span: start..end,
						kind: ReferenceKind::TypeReference,
						receiver: None,
					});
				}

				K_NESTED_TYPE_ID => {
					if emitted.contains(&start) {
						return;
					}
					// `module.Type` in type position.
					let mut segs: Vec<String> = Vec::new();
					if let Some(module) = node.child_by_field_name("module") {
						segs.push(node_text(module, src).to_owned());
					}
					if let Some(name) = node.child_by_field_name("name") {
						segs.push(node_text(name, src).to_owned());
					}
					if segs.is_empty() {
						return;
					}
					emitted.insert(start);
					refs.push(RawReference {
						segments: segs,
						span: start..end,
						kind: ReferenceKind::TypeReference,
						receiver: None,
					});
				}

				// ── JSX components (tsx grammar only) ────────────────────────
				//
				// `<Component />` or `<Module.Component>…</Module.Component>`.
				// We guard on the node kind being present in the grammar; in the
				// plain typescript grammar these nodes simply don't appear.
				K_JSX_OPENING | K_JSX_SELF_CLOSING => {
					if emitted.contains(&start) {
						return;
					}
					// The component name is the first named child of the element.
					// Only capitalised names (PascalCase) are component references;
					// lowercase names are intrinsic HTML elements.
					let name_node = node.named_child(0);
					if let Some(name_node) = name_node {
						let chain = collect_member_chain(name_node, src);
						if let Some(first) = chain.first() {
							let is_component = first
								.chars()
								.next()
								.map(|c| c.is_uppercase())
								.unwrap_or(false);
							if is_component {
								emitted.insert(start);
								refs.push(RawReference {
									segments: chain,
									span: start..end,
									kind: ReferenceKind::TypeReference,
									receiver: None,
								});
							}
						}
					}
				}

				_ => {}
			}
		});

		refs
	}

	// ── Module path ───────────────────────────────────────────────────────────
	//
	// Convention mirrors deno/oxc `assign_unique_module_names`:
	//
	//   src/util.ts        →  ["src", "util"]
	//   src/index.ts       →  ["src"]           (index → directory)
	//   components/Foo.tsx →  ["components", "Foo"]
	//   app.ts             →  ["app"]
	//
	// Recognised TS/JS extensions stripped: .ts .tsx .mts .cts .js .jsx .mjs .cjs

	fn module_path(&self, rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		const TS_EXTS: &[&str] =
			&["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

		let stem = rel
			.file_stem()
			.and_then(|s| s.to_str())
			.unwrap_or("");
		let ext = rel.extension().and_then(|s| s.to_str()).unwrap_or("");

		// Directory segments from parent path (non-empty, non-"." parts).
		let dir_segs: Vec<String> = rel
			.parent()
			.map(|p| {
				p.components()
					.filter_map(|c| match c {
						std::path::Component::Normal(s) => s.to_str().map(str::to_owned),
						_ => None,
					})
					.collect()
			})
			.unwrap_or_default();

		let mut segs = dir_segs;

		if TS_EXTS.contains(&ext) {
			// `index` files collapse to their directory.
			if stem != "index" {
				segs.push(stem.to_owned());
			}
		} else {
			// Unknown extension — keep as-is.
			if let Some(name) = rel.file_name().and_then(|s| s.to_str()) {
				segs.push(name.to_owned());
			}
		}

		segs
	}
}

// ─── Import-clause helper ────────────────────────────────────────────────────

/// Walk a single `import_clause` node and push all resulting [`ImportBinding`]s.
///
/// Handles:
///   - `import Foo from '…'`       — default import
///   - `import { a, b as c } from '…'` — named imports
///   - `import * as ns from '…'`   — namespace import (bound prefix, NOT glob)
///   - `import Foo, { a } from '…'` — combined default + named
fn emit_import_clause(
	clause: tree_sitter::Node,
	src: &str,
	specifier: &str,
	src_span: std::ops::Range<usize>,
	out: &mut Vec<ImportBinding>,
) {
	let mut cursor = clause.walk();

	// Iterate all children of the import_clause.
	for child in clause.children(&mut cursor) {
		match child.kind() {
			// Default import: `import Foo from '…'`
			// The identifier sits directly under import_clause with no field name.
			"identifier" => {
				let local = node_text(child, src).to_owned();
				out.push(ImportBinding {
					local,
					source: classify_specifier(specifier),
					span: src_span.clone(),
				});
			}

			// Named imports: `import { a, b as c } from '…'`
			K_NAMED_IMPORTS => {
				let mut inner = child.walk();
				for spec in child.children(&mut inner) {
					if spec.kind() == K_IMPORT_SPEC {
						// `name` field = original export name
						// `alias` field = local name after `as` (if present)
						let original =
							spec.child_by_field_name("name").map(|n| node_text(n, src));
						let alias =
							spec.child_by_field_name("alias").map(|n| node_text(n, src));
						let local = alias.or(original).unwrap_or("").to_owned();
						if local.is_empty() {
							continue;
						}
						out.push(ImportBinding {
							local,
							source: classify_specifier(specifier),
							span: src_span.clone(),
						});
					}
				}
			}

			// Namespace import: `import * as ns from '…'`
			// This is a BOUND PREFIX binding — `ns.x` later resolves through it.
			// We emit `local = "ns"`, `source = External/Internal{…}`.
			K_NS_IMPORT => {
				// The identifier after `* as` is a direct child of namespace_import.
				let mut ns_cur = child.walk();
				let ns_name = child
					.children(&mut ns_cur)
					.find(|c| c.kind() == "identifier")
					.map(|n| node_text(n, src).to_owned())
					.unwrap_or_default();
				if ns_name.is_empty() {
					continue;
				}
				// A namespace import is NOT a glob: it binds `ns` as a named
				// handle to the entire module.  Represent as Internal/External
				// (not Glob) so the resolver can route `ns.foo` through it.
				out.push(ImportBinding {
					local: ns_name,
					source: classify_specifier(specifier),
					span: src_span.clone(),
				});
			}

			_ => {}
		}
	}
}

// ─── CJS require() helper ────────────────────────────────────────────────────

/// Best-effort extraction of CommonJS `require()` patterns.
///
/// Handles:
///   - `const foo = require('bar')`
///   - `const { a, b } = require('bar')`
///
/// Skips dynamic specifiers (template literals, variables).
fn try_emit_require(
	decl_node: tree_sitter::Node,
	src: &str,
	out: &mut Vec<ImportBinding>,
) {
	let mut decl_cursor = decl_node.walk();
	for declarator in decl_node.children(&mut decl_cursor) {
		if declarator.kind() != "variable_declarator" {
			continue;
		}

		// The value must be a `call_expression` of the form `require("…")`.
		let value = match declarator.child_by_field_name("value") {
			Some(v) => v,
			None => continue,
		};
		if value.kind() != K_CALL_EXPR {
			continue;
		}

		// Check the callee is exactly `require`.
		let callee = match value.child_by_field_name("function") {
			Some(f) => f,
			None => continue,
		};
		if node_text(callee, src) != "require" {
			continue;
		}

		// Extract the first argument as a string literal.
		let args = match value.child_by_field_name("arguments") {
			Some(a) => a,
			None => continue,
		};
		let mut args_cur = args.walk();
		let specifier_node = args
			.children(&mut args_cur)
			.find(|c| c.kind() == K_STRING || c.kind() == "string");
		let specifier = match specifier_node {
			Some(n) => unquote(node_text(n, src)).to_owned(),
			None => continue,
		};
		if specifier.is_empty() {
			continue;
		}

		let src_span = declarator.byte_range().start..declarator.byte_range().end;

		// Pattern-match the LHS binding.
		let name_node = match declarator.child_by_field_name("name") {
			Some(n) => n,
			None => continue,
		};

		if name_node.kind() == "identifier" {
			// `const foo = require('bar')`
			let local = node_text(name_node, src).to_owned();
			out.push(ImportBinding {
				local,
				source: classify_specifier(&specifier),
				span: src_span,
			});
		} else if name_node.kind() == "object_pattern" {
			// `const { a, b } = require('bar')`
			let mut inner = name_node.walk();
			for prop in name_node.children(&mut inner) {
				let local = match prop.kind() {
					// `{ a }` — shorthand property
					"shorthand_property_identifier_pattern" => {
						node_text(prop, src).to_owned()
					}
					// `{ a: b }` — renamed property
					"pair_pattern" => prop
						.child_by_field_name("value")
						.map(|n| node_text(n, src).to_owned())
						.unwrap_or_default(),
					_ => continue,
				};
				if local.is_empty() {
					continue;
				}
				out.push(ImportBinding {
					local,
					source: classify_specifier(&specifier),
					span: src_span.clone(),
				});
			}
		}
	}
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;
	use super::super::spec::PackageLayout;

	// ── Parse helpers ────────────────────────────────────────────────────────

	fn parse_ts(src: &str) -> tree_sitter::Tree {
		let lang = arborium::get_language("typescript").unwrap();
		let mut parser = arborium_tree_sitter::Parser::new();
		parser.set_language(&lang).unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn parse_tsx(src: &str) -> tree_sitter::Tree {
		let lang = arborium::get_language("tsx").unwrap();
		let mut parser = arborium_tree_sitter::Parser::new();
		parser.set_language(&lang).unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn layout() -> PackageLayout { PackageLayout { package: "myapp".into() } }

	// ── module_path ──────────────────────────────────────────────────────────

	#[test]
	fn module_path_strips_ts_extension() {
		let spec = TypescriptSpec;
		assert_eq!(
			spec.module_path(Path::new("src/util.ts"), &layout()),
			vec!["src", "util"]
		);
	}

	#[test]
	fn module_path_collapses_index() {
		let spec = TypescriptSpec;
		assert_eq!(
			spec.module_path(Path::new("src/index.ts"), &layout()),
			vec!["src"]
		);
	}

	#[test]
	fn module_path_handles_tsx() {
		let spec = TypescriptSpec;
		assert_eq!(
			spec.module_path(Path::new("components/Button.tsx"), &layout()),
			vec!["components", "Button"]
		);
	}

	#[test]
	fn module_path_root_file() {
		let spec = TypescriptSpec;
		assert_eq!(spec.module_path(Path::new("app.ts"), &layout()), vec!["app"]);
	}

	// ── References: FunctionCall ─────────────────────────────────────────────

	#[test]
	fn free_function_call_is_function_call() {
		let src = "foo();";
		let tree = parse_ts(src);
		let refs = TypescriptSpec.references(&tree, src);
		let r = refs.iter().find(|r| r.segments == vec!["foo"]).expect("foo ref");
		assert_eq!(r.kind, ReferenceKind::FunctionCall);
		assert!(r.receiver.is_none());
	}

	// ── References: MethodCall ───────────────────────────────────────────────

	#[test]
	fn obj_method_call_is_method_call() {
		let src = "obj.method();";
		let tree = parse_ts(src);
		let refs = TypescriptSpec.references(&tree, src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall)
			.expect("MethodCall ref");
		assert_eq!(r.segments, vec!["obj", "method"]);
		assert!(matches!(r.receiver, Some(ReceiverShape::Expr(ref s)) if s == "obj"));
	}

	#[test]
	fn this_method_call_is_self_ref() {
		let src = "this.x();";
		let tree = parse_ts(src);
		let refs = TypescriptSpec.references(&tree, src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall)
			.expect("MethodCall ref");
		assert_eq!(r.segments, vec!["this", "x"]);
		assert_eq!(r.receiver, Some(ReceiverShape::SelfRef));
	}

	// ── References: FieldAccess ──────────────────────────────────────────────

	#[test]
	fn property_access_not_under_call_is_field_access() {
		let src = "const v = obj.prop;";
		let tree = parse_ts(src);
		let refs = TypescriptSpec.references(&tree, src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::FieldAccess && r.segments.contains(&"prop".to_owned()))
			.expect("FieldAccess ref");
		assert!(r.segments.contains(&"obj".to_owned()));
	}

	// ── Imports: named with `as` rename ─────────────────────────────────────

	#[test]
	fn named_import_with_as_uses_local_name() {
		let src = "import { Foo as Bar } from './foo';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.iter().find(|b| b.local == "Bar").expect("Bar binding");
		assert!(matches!(&b.source, ImportSource::Internal(segs) if segs == &vec!["foo"]));
	}

	// ── Imports: default import ──────────────────────────────────────────────

	#[test]
	fn default_import_binds_local_name() {
		let src = "import React from 'react';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.iter().find(|b| b.local == "React").expect("React binding");
		assert!(
			matches!(&b.source, ImportSource::External { dependency, path } if dependency == "react" && path.is_empty())
		);
	}

	// ── Imports: namespace import ────────────────────────────────────────────

	#[test]
	fn namespace_import_binds_as_named_not_glob() {
		let src = "import * as ns from './util';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.iter().find(|b| b.local == "ns").expect("ns binding");
		// Namespace imports are bound prefixes — NOT Glob.
		assert!(!matches!(&b.source, ImportSource::Glob(_)));
		assert!(matches!(&b.source, ImportSource::Internal(_)));
	}

	// ── Imports: Internal vs External specifiers ─────────────────────────────

	#[test]
	fn relative_specifier_is_internal() {
		let src = "import { x } from './utils/helper';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.first().expect("binding");
		assert!(matches!(&b.source, ImportSource::Internal(segs) if segs == &vec!["utils", "helper"]));
	}

	#[test]
	fn bare_specifier_is_external() {
		let src = "import { useState } from 'react';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.first().expect("binding");
		assert!(
			matches!(&b.source, ImportSource::External { dependency, .. } if dependency == "react")
		);
	}

	// ── Imports: multiple named bindings ────────────────────────────────────

	#[test]
	fn named_imports_multiple_bindings() {
		let src = "import { a, b, c } from 'mod';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let locals: Vec<&str> = imports.iter().map(|b| b.local.as_str()).collect();
		assert!(locals.contains(&"a"), "expected a; got {locals:?}");
		assert!(locals.contains(&"b"), "expected b; got {locals:?}");
		assert!(locals.contains(&"c"), "expected c; got {locals:?}");
	}

	// ── Definitions: class with nested method ────────────────────────────────

	#[test]
	fn class_method_is_nested_under_class() {
		let src = "class Dog { bark() {} }";
		let tree = parse_ts(src);
		let defs = TypescriptSpec.definitions(&tree, src);
		let class_idx = defs
			.iter()
			.position(|d| d.name == "Dog" && d.kind == DefKind::Class)
			.expect("Dog class");
		let method = defs
			.iter()
			.find(|d| d.name == "bark" && d.kind == DefKind::Method)
			.expect("bark method");
		assert_eq!(method.parent, Some(class_idx));
	}

	// ── Definitions: top-level function ─────────────────────────────────────

	#[test]
	fn top_level_function_has_no_parent() {
		let src = "function greet(name: string): void {}";
		let tree = parse_ts(src);
		let defs = TypescriptSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "greet").expect("greet");
		assert_eq!(d.kind, DefKind::Function);
		assert!(d.parent.is_none());
	}

	// ── Definitions: interface ───────────────────────────────────────────────

	#[test]
	fn interface_declaration_is_trait() {
		let src = "interface Serializable { serialize(): string; }";
		let tree = parse_ts(src);
		let defs = TypescriptSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "Serializable").expect("iface");
		assert_eq!(d.kind, DefKind::Trait);
	}

	// ── Definitions: type alias ──────────────────────────────────────────────

	#[test]
	fn type_alias_is_type_kind() {
		let src = "type UserId = string;";
		let tree = parse_ts(src);
		let defs = TypescriptSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "UserId").expect("type alias");
		assert_eq!(d.kind, DefKind::Type);
	}

	// ── Definitions: enum ────────────────────────────────────────────────────

	#[test]
	fn enum_declaration_is_type_kind() {
		let src = "enum Direction { Up, Down, Left, Right }";
		let tree = parse_ts(src);
		let defs = TypescriptSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "Direction").expect("enum");
		assert_eq!(d.kind, DefKind::Type);
	}

	// ── JSX: <Component /> in tsx grammar ───────────────────────────────────

	#[test]
	fn jsx_component_is_type_reference() {
		let src = "const el = <MyButton onClick={handler} />;";
		let tree = parse_tsx(src);
		let refs = TypescriptSpec.references(&tree, src);
		let r = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::TypeReference && r.segments.contains(&"MyButton".to_owned()));
		assert!(
			r.is_some(),
			"expected TypeReference for MyButton; got {refs:?}"
		);
	}

	#[test]
	fn jsx_intrinsic_element_is_not_emitted() {
		// `<div>` is a lowercase intrinsic — should not produce a TypeReference.
		let src = "const el = <div className='x' />;";
		let tree = parse_tsx(src);
		let refs = TypescriptSpec.references(&tree, src);
		assert!(
			!refs.iter().any(|r| r.segments == vec!["div"] && r.kind == ReferenceKind::TypeReference),
			"div should not be a TypeReference; got {refs:?}"
		);
	}

	// ── specifier classification edge cases ──────────────────────────────────

	#[test]
	fn scoped_package_specifier_is_external() {
		let src = "import { signal } from '@preact/signals-core';";
		let tree = parse_ts(src);
		let imports = TypescriptSpec.imports(&tree, src);
		let b = imports.first().expect("binding");
		assert!(
			matches!(&b.source, ImportSource::External { dependency, .. } if dependency == "@preact/signals-core"),
			"expected scoped package dependency; got {:?}",
			b.source
		);
	}

	#[test]
	fn classify_specifier_bare_with_path() {
		let source = classify_specifier("lodash/fp");
		assert!(
			matches!(source, ImportSource::External { ref dependency, ref path }
				if dependency == "lodash" && path == &vec!["fp"])
		);
	}

	#[test]
	fn classify_specifier_relative_deep() {
		let source = classify_specifier("../../shared/types");
		assert!(
			matches!(source, ImportSource::Internal(ref segs) if segs == &vec!["shared", "types"])
		);
	}
}

// ─── Suppress unused-import warnings when the test feature is absent ─────────
