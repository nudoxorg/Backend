//! `LanguageSpec` for Nix.
//!
//! Cursor-walks the tree-sitter parse (arborium-nix 2.18.1, grammar pinned
//! below) to emit:
//!
//! - **definitions** — attrpath bindings in `{ foo = ...; }`, `rec { ... }`,
//!   `let ... in`, `let_attrset` — with nesting via attrset containment;
//! - **imports** — `inherit a b;` and `inherit (src) a b;` sites;
//! - **references** — function applications, attribute selections, and bare
//!   variable uses (with honest `with`-scope handling);
//! - **module_path** — derived from the file path: `default.nix` maps to its
//!   directory, other `.nix` files add their stem.
//!
//! # Grammar-pin comment
//!
//! Node-kind string constants below are validated against
//! `arborium-nix 2.18.1 / grammar/src/node-types.json`.  The legacy
//! `categorize_nix` match arms in `super` (treesitter/mod.rs) dual-accept
//! older short names (`apply`, `select`, `inherit`, `inherited_attrs`,
//! `attrspath`, `variable_expression`, `attr_identifier`).  This extractor
//! uses only the 2.18.1 names but mirrors the same dual-accept pattern via
//! the `is_identifier_node` helper so a grammar re-pin is a one-line change.
//!
//! # Attrpath-definition approach
//!
//! A `binding` node in a `binding_set` (inside `attrset_expression`,
//! `rec_attrset_expression`, `let_attrset_expression`, or `let_expression`)
//! carries:
//!   - `attrpath` — one or more `.`-separated `identifier` attrs
//!   - `expression` — the bound value
//!
//! We emit the **last segment** of the attrpath as the definition name.
//! Multi-segment paths (`a.b.c = 1;`) collapse to `c`; this is consistent
//! with how attrsets project their leaf names into scope.  For
//! `a.b.c = ...` the parent is resolved as the innermost enclosing
//! `binding_set`'s containing definition (same body-span containment logic
//! used for Rust).
//!
//! `DefKind` assignment:
//!   - `function_expression` (lambda) value → `DefKind::Function`
//!   - `attrset_expression` / `rec_attrset_expression` / `let_attrset_expression`
//!     value → `DefKind::Module` (namespace container)
//!   - anything else → `DefKind::Type` (a named value binding)
//!
//! # `with`-scope honesty
//!
//! `with pkgs; <body>` brings every member of `pkgs` into scope —
//! statically unknowable without evaluation.  This extractor **does not**
//! try to qualify bare names inside a `with` body.  All `variable_expression`
//! nodes are emitted as `VariableUse(segments=[name])` regardless of enclosing
//! `with` scopes.  The resolver will match them only when the name is unique in
//! the index; otherwise they are left unresolved — honesty over guessing.
//!
//! # Builtins
//!
//! `map`, `filter`, `fetchTarball`, `import`, `builtins.foo`, etc. are
//! extracted as normal `FunctionCall` / `VariableUse` / `FieldAccess` refs.
//! The resolver maps catalogued builtins (from `compile::nix::builtins`) to
//! `External` during resolution.  No special-casing is needed here.
//!
//! # `callPackage` argument injection
//!
//! Statically unknowable (oracle / snix required).  Deferred.

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportSource, LanguageSpec, PackageLayout, RawDefinition,
	RawReference, node_text, walk_preorder,
};

// ─── Node-kind constants (arborium-nix 2.18.1) ───────────────────────────────
//
// Cross-check against grammar/src/node-types.json when re-pinning.
// Legacy short names (apply, select, …) are accepted in categorize_nix in
// super; this extractor only emits the 2.18.1 long forms.
//
// Some constants below are documentary only (grammar reference; unused in the
// current walk but referenced when extending the extractor).

/// `binding` — `<attrpath> = <expression> ;` inside a binding_set.
const K_BINDING: &str = "binding";
/// `binding_set` — the `{ … }` interior of an attrset / let (documentary).
#[allow(dead_code)]
const K_BINDING_SET: &str = "binding_set";
/// `attrpath` — dot-separated attribute path in a binding or select (documentary).
#[allow(dead_code)]
const K_ATTRPATH: &str = "attrpath";
/// `attrset_expression` — `{ … }`.
const K_ATTRSET: &str = "attrset_expression";
/// `rec_attrset_expression` — `rec { … }`.
const K_REC_ATTRSET: &str = "rec_attrset_expression";
/// `let_attrset_expression` — `let { … }` (legacy form, still in grammar).
const K_LET_ATTRSET: &str = "let_attrset_expression";
/// `let_expression` — `let <bindings> in <body>` (documentary; bindings are
/// reached via their `binding` children which walk_preorder visits directly).
#[allow(dead_code)]
const K_LET_EXPR: &str = "let_expression";
/// `function_expression` — `<param>: <body>` or `{ formals }: <body>`.
const K_FUNCTION: &str = "function_expression";
/// `apply_expression` — `<function> <argument>`.
const K_APPLY: &str = "apply_expression";
/// `select_expression` — `<expr> . <attrpath>`.
const K_SELECT: &str = "select_expression";
/// `variable_expression` — a bare name reference.
const K_VARIABLE: &str = "variable_expression";
/// `with_expression` — `with <env> ; <body>` (documentary; handled implicitly
/// since variable_expression nodes inside are emitted as VariableUse).
#[allow(dead_code)]
const K_WITH: &str = "with_expression";
/// `inherit` — `inherit <attrs> ;` (bare, no source).
const K_INHERIT: &str = "inherit";
/// `inherit_from` — `inherit ( <expr> ) <attrs> ;`.
const K_INHERIT_FROM: &str = "inherit_from";
/// `inherited_attrs` — the identifier list inside an inherit / inherit_from
/// (documentary; accessed via field_name "attrs" in parent nodes).
#[allow(dead_code)]
const K_INHERITED_ATTRS: &str = "inherited_attrs";
/// `identifier` — a single name token.
const K_IDENTIFIER: &str = "identifier";

// ─── NixSpec ─────────────────────────────────────────────────────────────────

/// Nix structural extractor implementing [`LanguageSpec`].
pub struct NixSpec;

impl LanguageSpec for NixSpec {
	/// Walk all `binding` nodes in every `binding_set` and emit one
	/// [`RawDefinition`] per attrpath segment.
	///
	/// Nesting is recovered via body-span containment: the definitions vec is
	/// built in pre-order; a stack of `(body_end_byte, def_index)` tracks the
	/// innermost definition that is still open at each new binding site.
	///
	/// Multi-segment attrpaths (`a.b.c = 1`) emit intermediate `Module`
	/// definitions for each prefix so the occurrence FQN (`a::b::c`) matches
	/// the IR NudoxPath (`a/b/c` → segments `["a","b","c"]`). Nested attrset
	/// values continue to use the body-span parent stack.
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut defs: Vec<RawDefinition> = Vec::new();
		// Stack of (body_end_byte, definition_index).
		let mut stack: Vec<(usize, usize)> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			// Evict frames whose body has ended.
			let byte_start = node.start_byte();
			stack.retain(|(end, _)| *end > byte_start);

			if node.kind() != K_BINDING {
				return;
			}

			// Resolve the attrpath.
			let attrpath_node = match node.child_by_field_name("attrpath") {
				Some(n) => n,
				None => return,
			};
			let value_node = match node.child_by_field_name("expression") {
				Some(n) => n,
				None => return,
			};

			// Collect attrpath segments (identifier attrs; skip string/interpolation).
			let segments = collect_attrpath_segments(attrpath_node, src);
			if segments.is_empty() {
				return;
			}

			// Identifier spans aligned with each segment (best-effort).
			let id_spans = identifier_spans(attrpath_node);
			let body_span = node.byte_range();
			let leaf_kind = classify_value_kind(value_node);

			// Outer parent from the nested-attrset body stack.
			let mut parent = stack.last().map(|(_, idx)| *idx);

			// Emit intermediate segments as Module so FQN mirrors IR paths.
			// For `a.b.c = …` we emit a, a.b (Module), a.b.c (leaf kind).
			for (i, seg) in segments.iter().enumerate() {
				let is_leaf = i + 1 == segments.len();
				let kind = if is_leaf { leaf_kind.clone() } else { DefKind::Module };
				let name_span = id_spans
					.get(i)
					.cloned()
					.unwrap_or_else(|| {
						if is_leaf {
							last_identifier_span(attrpath_node)
						} else {
							attrpath_node.byte_range()
						}
					});
				let idx = defs.len();
				defs.push(RawDefinition {
					name: seg.clone(),
					kind,
					name_span,
					// Intermediate prefixes share the binding body so nested
					// containment still attributes references correctly.
					body_span: body_span.clone(),
					parent,
				});
				parent = Some(idx);
			}

			// Push the leaf as a potential parent for nested bindings
			// (relevant when the value is an attrset with its own binding_set).
			if let Some(leaf_idx) = parent {
				stack.push((body_span.end, leaf_idx));
			}
		});

		defs
	}

	/// Walk `inherit` and `inherit_from` nodes and emit one [`ImportBinding`]
	/// per inherited name.
	///
	/// - `inherit a b;` → two bindings, each `source = Internal([name])`.
	/// - `inherit (src) a b;` → two bindings, each `source = Internal([src_text, name])`.
	///
	/// The `expression` field of `inherit_from` can be any Nix expression
	/// (e.g. `pkgs`, `pkgs.lib`); we capture its source text as the first
	/// segment of the `Internal` path.  The resolver promotes to `External`
	/// when the head segment matches a known dependency.
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut out: Vec<ImportBinding> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			match node.kind() {
				K_INHERIT => {
					// `inherit a b c ;`
					// The grammar: inherit { attrs: inherited_attrs }
					let attrs_node = match node.child_by_field_name("attrs") {
						Some(n) => n,
						None => return,
					};
					let span = node.byte_range();
					for name in collect_inherited_names(attrs_node, src) {
						out.push(ImportBinding {
							local: name.clone(),
							source: ImportSource::Internal(vec![name]),
							span: span.clone(),
						});
					}
				}

				K_INHERIT_FROM => {
					// `inherit ( <expr> ) a b c ;`
					// The grammar: inherit_from { expression: _, attrs: inherited_attrs }
					let expr_node = match node.child_by_field_name("expression") {
						Some(n) => n,
						None => return,
					};
					let attrs_node = match node.child_by_field_name("attrs") {
						Some(n) => n,
						None => return,
					};
					let src_text = node_text(expr_node, src).to_owned();
					let span = node.byte_range();
					for name in collect_inherited_names(attrs_node, src) {
						out.push(ImportBinding {
							local: name.clone(),
							source: ImportSource::Internal(vec![src_text.clone(), name]),
							span: span.clone(),
						});
					}
				}

				_ => {}
			}
		});

		out
	}

	/// Walk the parse tree and emit one [`RawReference`] per qualified use-site.
	///
	/// Three reference kinds are emitted:
	///
	/// 1. **`apply_expression`** → `FunctionCall`.  The `function` field is
	///    walked to extract the callee segments:
	///    - `variable_expression` → `[name]`
	///    - `select_expression` → the full attrpath chain (see below)
	///    - nested `apply_expression` → recurse to peel off the outermost
	///      application and grab the function at the root (curried calls:
	///      `f x y` parses as `(f x) y`; we grab `f` from the inner apply).
	///
	/// 2. **`select_expression`** → `FieldAccess`.  The full chain
	///    `<base>.<attr1>.<attr2>` is emitted as ONE reference with segments
	///    `[base_name, attr1, attr2]`.  Only emitted when `select_expression`
	///    is **not** the direct `function` child of an `apply_expression`
	///    (those are covered by case 1 above, which deduplicates via the
	///    `covered` set).
	///
	/// 3. **`variable_expression`** → `VariableUse`.  Bare name, segments
	///    `[name]`.  Emitted only when not already covered by a select or
	///    apply reference.
	///
	/// `with`-scope: all `variable_expression` nodes inside a `with_expression`
	/// body are emitted as `VariableUse` with their literal name; no
	/// qualification is attempted.  See module-level doc for rationale.
	///
	/// String interpolations and other expression kinds are not emitted
	/// (too noisy, low resolver hit rate).
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut out: Vec<RawReference> = Vec::new();
		// Byte ranges we have already emitted to avoid double-counting.
		let mut covered: Vec<std::ops::Range<usize>> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			let span = node.byte_range();

			// Skip ranges already covered by an outer reference.
			if covered.iter().any(|r| r.start <= span.start && span.end <= r.end) {
				return;
			}

			match node.kind() {
				// ── Function application: `f x` / `builtins.map f xs` ─────────
				K_APPLY => {
					if let Some(func_node) = node.child_by_field_name("function") {
						let segments = callee_segments(func_node, src);
						if !segments.is_empty() {
							let func_span = func_node.byte_range();
							covered.push(func_span.clone());
							out.push(RawReference {
								segments,
								span: func_span,
								kind: ReferenceKind::FunctionCall,
								receiver: None,
							});
						}
					}
				}

				// ── Attribute selection: `foo.bar` ────────────────────────────
				//
				// Only when not already covered (i.e. not the function of an apply).
				K_SELECT => {
					let segs = select_segments(node, src);
					if segs.is_empty() {
						return;
					}
					covered.push(span.clone());
					out.push(RawReference {
						segments: segs,
						span,
						kind: ReferenceKind::FieldAccess,
						receiver: None,
					});
				}

				// ── Bare variable: `pkgs`, `lib`, etc. ───────────────────────
				K_VARIABLE => {
					if let Some(name_node) = node.child_by_field_name("name") {
						let name = node_text(name_node, src).to_owned();
						if !name.is_empty() {
							covered.push(span.clone());
							out.push(RawReference {
								segments: vec![name],
								span,
								kind: ReferenceKind::VariableUse,
								receiver: None,
							});
						}
					}
				}

				_ => {}
			}
		});

		out
	}

	/// Derive the module path from a `.nix` file's relative path.
	///
	/// **IR alignment:** the Nix producer mints pure attrpath NudoxPaths
	/// (`strings/trim`, `lib/attrsets/mapAttrs`) with **no** file-path prefix.
	/// Returning a non-empty module path here would make occurrence FQNs like
	/// `lib::strings::trim` fail to anchor against index paths
	/// `strings/trim` → `strings::trim`. File identity is carried by the
	/// occurrence's own path field; attrpath nesting is recovered via the
	/// definition parent chain.
	///
	/// Therefore `module_path` is always empty for Nix.
	fn module_path(&self, _rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		Vec::new()
	}
}

// ─── Definition helpers ───────────────────────────────────────────────────────

/// Collect the identifier segments of an `attrpath` node.
///
/// Only `identifier`-kind attrs are included.  Dynamic attrs (`${ … }` and
/// string keys) are skipped — they cannot be statically named.
fn collect_attrpath_segments<'s>(attrpath: tree_sitter::Node<'_>, src: &'s str) -> Vec<String> {
	let mut segs = Vec::new();
	let mut cursor = attrpath.walk();
	for child in attrpath.children(&mut cursor) {
		if child.kind() == K_IDENTIFIER {
			segs.push(node_text(child, src).to_owned());
		}
	}
	segs
}

/// Span of the last `identifier` child inside an `attrpath` node.
///
/// Falls back to the attrpath's own byte range when no identifier is found
/// (e.g. a pure dynamic-key attrpath, which `collect_attrpath_segments` would
/// have already returned empty for).
fn last_identifier_span(attrpath: tree_sitter::Node<'_>) -> std::ops::Range<usize> {
	identifier_spans(attrpath)
		.into_iter()
		.last()
		.unwrap_or_else(|| attrpath.byte_range())
}

/// Spans of every `identifier` child inside an `attrpath`, in source order.
fn identifier_spans(attrpath: tree_sitter::Node<'_>) -> Vec<std::ops::Range<usize>> {
	let mut spans = Vec::new();
	let mut cursor = attrpath.walk();
	for child in attrpath.children(&mut cursor) {
		if child.kind() == K_IDENTIFIER {
			spans.push(child.byte_range());
		}
	}
	spans
}

/// Choose a [`DefKind`] for a definition based on its bound value's node kind.
///
/// | Value kind                                                   | DefKind       |
/// |--------------------------------------------------------------|---------------|
/// | `function_expression`                                        | `Function`    |
/// | `attrset_expression`, `rec_attrset_expression`,              | `Module`      |
/// | `let_attrset_expression`                                     |               |
/// | anything else (integer, string, list, select, apply, …)      | `Type`        |
///
/// `Module` for attrset-valued bindings reflects their role as namespace
/// containers in Nixpkgs (`pkgs.lib`, `config.services.foo`, etc.).
fn classify_value_kind(value: tree_sitter::Node<'_>) -> DefKind {
	match value.kind() {
		K_FUNCTION => DefKind::Function,
		K_ATTRSET | K_REC_ATTRSET | K_LET_ATTRSET => DefKind::Module,
		_ => DefKind::Type,
	}
}

// ─── Import helpers ───────────────────────────────────────────────────────────

/// Collect all `identifier`-kind attr names from an `inherited_attrs` node.
fn collect_inherited_names(attrs: tree_sitter::Node<'_>, src: &str) -> Vec<String> {
	let mut names = Vec::new();
	let mut cursor = attrs.walk();
	for child in attrs.children(&mut cursor) {
		// Only static identifier attrs; skip string/interpolation keys.
		if child.kind() == K_IDENTIFIER {
			let name = node_text(child, src).to_owned();
			if !name.is_empty() {
				names.push(name);
			}
		}
	}
	names
}

// ─── Reference helpers ───────────────────────────────────────────────────────

/// Recursively extract the callee segments from an `apply_expression`'s
/// `function` child.
///
/// - `variable_expression` → `[name]`
/// - `select_expression`   → `[base, attr1, attr2, …]` (full chain)
/// - `apply_expression`    → recurse on its own `function` (curried apply:
///   `f x y` → `(f x) y`; we peel until we reach a non-apply function node)
/// - anything else         → empty (caller skips)
fn callee_segments(func: tree_sitter::Node<'_>, src: &str) -> Vec<String> {
	match func.kind() {
		K_VARIABLE => {
			if let Some(name_node) = func.child_by_field_name("name") {
				let name = node_text(name_node, src).to_owned();
				if name.is_empty() { vec![] } else { vec![name] }
			} else {
				vec![]
			}
		}
		K_SELECT => select_segments(func, src),
		K_APPLY => {
			// Curried application: peel the inner apply's function.
			if let Some(inner_func) = func.child_by_field_name("function") {
				callee_segments(inner_func, src)
			} else {
				vec![]
			}
		}
		_ => vec![],
	}
}

/// Extract the full attrpath chain from a `select_expression` as a flat
/// segment list.
///
/// `select_expression` has:
///   - `expression` — the base (may itself be a `variable_expression` or
///     a nested `select_expression`)
///   - `attrpath` — the selector path
///
/// We collect: `base_segments ++ attrpath_segments`.
///
/// For `foo.bar.baz`:
///   - base = `foo` (variable_expression → ["foo"])
///   - attrpath = `bar.baz` (→ ["bar", "baz"])
///   - result = ["foo", "bar", "baz"]
fn select_segments(select: tree_sitter::Node<'_>, src: &str) -> Vec<String> {
	let base_node = match select.child_by_field_name("expression") {
		Some(n) => n,
		None => return vec![],
	};
	let attrpath_node = match select.child_by_field_name("attrpath") {
		Some(n) => n,
		None => return vec![],
	};

	let mut segs = base_segments(base_node, src);
	segs.extend(collect_attrpath_segments(attrpath_node, src));
	segs
}

/// Collect the segment list for the base expression of a `select_expression`.
///
/// Handles:
/// - `variable_expression` → `[name]`
/// - nested `select_expression` → recurse
/// - anything else → `[]` (unknown dynamic base; skip)
fn base_segments(base: tree_sitter::Node<'_>, src: &str) -> Vec<String> {
	match base.kind() {
		K_VARIABLE => {
			if let Some(name_node) = base.child_by_field_name("name") {
				let name = node_text(name_node, src).to_owned();
				if name.is_empty() { vec![] } else { vec![name] }
			} else {
				vec![]
			}
		}
		K_SELECT => select_segments(base, src),
		_ => vec![],
	}
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::Path;

	use arborium_tree_sitter as tree_sitter;
	use ir::syntax::ReferenceKind;

	use super::NixSpec;
	use super::super::spec::{DefKind, ImportSource, LanguageSpec, PackageLayout};

	// ── Parse helper ────────────────────────────────────────────────────────────

	fn parse(src: &str) -> tree_sitter::Tree {
		let mut parser = tree_sitter::Parser::new();
		parser
			.set_language(&arborium::get_language("nix").unwrap())
			.unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn layout() -> PackageLayout {
		PackageLayout { package: "nixpkgs".into() }
	}

	// ── Definition tests ─────────────────────────────────────────────────────────

	/// A simple lambda binding `foo = x: x + 1;` emits `DefKind::Function`.
	///
	/// Grammar note: the Nix grammar represents `x: x + 1` as a
	/// `function_expression`; our classifier maps that to `DefKind::Function`.
	#[test]
	fn function_binding_emits_function_def() {
		let src = "{ foo = x: x + 1; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "foo");
		assert!(d.is_some(), "expected 'foo' definition, got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Function);
		assert_eq!(d.unwrap().parent, None); // top-level binding
	}

	/// A formals-style lambda `bar = { x, y }: x + y;` also emits `DefKind::Function`.
	#[test]
	fn formals_lambda_emits_function_def() {
		let src = "{ bar = { x, y }: x + y; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "bar");
		assert!(d.is_some(), "expected 'bar' definition, got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Function);
	}

	/// An attrset-valued binding `ns = { a = 1; };` emits `DefKind::Module`.
	#[test]
	fn attrset_binding_emits_module_def() {
		let src = "{ ns = { a = 1; }; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "ns");
		assert!(d.is_some(), "expected 'ns' definition, got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Module);
	}

	/// A rec-attrset-valued binding also emits `DefKind::Module`.
	#[test]
	fn rec_attrset_binding_emits_module_def() {
		let src = "{ ns = rec { a = 1; }; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "ns");
		assert!(d.is_some(), "expected 'ns' definition, got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Module);
	}

	/// A scalar binding `version = "1.0";` emits `DefKind::Type`.
	#[test]
	fn scalar_binding_emits_type_def() {
		let src = r#"{ version = "1.0"; }"#;
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "version");
		assert!(d.is_some(), "expected 'version' definition, got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Type);
	}

	/// A multi-segment attrpath `a.b = 1;` emits the leaf name `b` as
	/// `DefKind::Type`.
	#[test]
	fn nested_attrpath_emits_leaf_name() {
		let src = "{ a.b = 1; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		// The grammar may represent `a.b = 1` as a single binding with a
		// two-segment attrpath; we emit the leaf `b`.
		let d = defs.iter().find(|d| d.name == "b");
		assert!(d.is_some(), "expected leaf 'b' definition for 'a.b = 1', got {defs:?}");
		assert_eq!(d.unwrap().kind, DefKind::Type);
	}

	/// `let x = 1; in x` emits a definition for `x`.
	#[test]
	fn let_binding_emits_def() {
		let src = "let x = 1; in x";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let d = defs.iter().find(|d| d.name == "x");
		assert!(d.is_some(), "expected 'x' definition from let, got {defs:?}");
	}

	/// Nested attrset: inner binding's parent index points to the outer definition.
	#[test]
	fn nested_binding_parent_index() {
		let src = "{ outer = { inner = 1; }; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);

		let outer_idx = defs.iter().position(|d| d.name == "outer");
		let inner_def = defs.iter().find(|d| d.name == "inner");

		assert!(outer_idx.is_some(), "expected 'outer' definition, got {defs:?}");
		assert!(inner_def.is_some(), "expected 'inner' definition, got {defs:?}");

		// inner's parent should be the outer binding's index.
		assert_eq!(
			inner_def.unwrap().parent,
			outer_idx,
			"inner's parent should be outer; defs = {defs:?}"
		);
	}

	// ── Import tests ─────────────────────────────────────────────────────────────

	/// `inherit (pkgs) lib stdenv;` emits two ImportBindings with
	/// `Internal(["pkgs", "lib"])` and `Internal(["pkgs", "stdenv"])`.
	#[test]
	fn inherit_from_emits_two_imports() {
		let src = "{ inherit (pkgs) lib stdenv; }";
		let tree = parse(src);
		let imports = NixSpec.imports(&tree, src);

		let lib_b = imports.iter().find(|b| b.local == "lib");
		let stdenv_b = imports.iter().find(|b| b.local == "stdenv");

		assert!(lib_b.is_some(), "expected 'lib' import, got {imports:?}");
		assert!(stdenv_b.is_some(), "expected 'stdenv' import, got {imports:?}");

		match &lib_b.unwrap().source {
			ImportSource::Internal(segs) => {
				assert_eq!(segs, &["pkgs".to_string(), "lib".to_string()], "unexpected path for lib: {segs:?}");
			}
			other => panic!("expected Internal for lib, got {other:?}"),
		}
		match &stdenv_b.unwrap().source {
			ImportSource::Internal(segs) => {
				assert_eq!(
					segs,
					&["pkgs".to_string(), "stdenv".to_string()],
					"unexpected path for stdenv: {segs:?}"
				);
			}
			other => panic!("expected Internal for stdenv, got {other:?}"),
		}
	}

	/// `inherit foo;` emits one ImportBinding with `Internal(["foo"])`.
	#[test]
	fn bare_inherit_emits_import() {
		let src = "{ inherit foo; }";
		let tree = parse(src);
		let imports = NixSpec.imports(&tree, src);

		let b = imports.iter().find(|b| b.local == "foo");
		assert!(b.is_some(), "expected 'foo' import, got {imports:?}");

		match &b.unwrap().source {
			ImportSource::Internal(segs) => {
				assert_eq!(segs, &["foo".to_string()], "unexpected segs: {segs:?}");
			}
			other => panic!("expected Internal([foo]), got {other:?}"),
		}
	}

	/// `inherit a b c;` emits three imports.
	#[test]
	fn bare_inherit_multiple_names() {
		let src = "{ inherit a b c; }";
		let tree = parse(src);
		let imports = NixSpec.imports(&tree, src);

		let locals: Vec<&str> = imports.iter().map(|b| b.local.as_str()).collect();
		assert!(locals.contains(&"a"), "expected 'a', got {locals:?}");
		assert!(locals.contains(&"b"), "expected 'b', got {locals:?}");
		assert!(locals.contains(&"c"), "expected 'c', got {locals:?}");
	}

	// ── Reference tests ──────────────────────────────────────────────────────────

	/// `foo.bar` as a standalone select emits FieldAccess with segments
	/// `["foo", "bar"]`.
	#[test]
	fn select_emits_field_access() {
		let src = "foo.bar";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let r = refs.iter().find(|r| r.kind == ReferenceKind::FieldAccess && r.segments == &["foo", "bar"]);
		assert!(
			r.is_some(),
			"expected FieldAccess [\"foo\", \"bar\"], got {refs:?}"
		);
	}

	/// `foo.bar.baz` emits FieldAccess with segments `["foo", "bar", "baz"]`.
	#[test]
	fn deep_select_emits_field_access_chain() {
		let src = "foo.bar.baz";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let r = refs.iter().find(|r| r.kind == ReferenceKind::FieldAccess && r.segments == &["foo", "bar", "baz"]);
		assert!(
			r.is_some(),
			"expected FieldAccess [\"foo\", \"bar\", \"baz\"], got {refs:?}"
		);
	}

	/// `map f xs` emits a FunctionCall for `map`.
	///
	/// Note: `map f xs` parses as `(map f) xs` in Nix (left-associative
	/// application).  Our `callee_segments` peels the inner apply so we get
	/// `map` as the callee of the outer `(map f)` application.
	#[test]
	fn function_application_emits_function_call() {
		let src = "map f xs";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let call = refs.iter().find(|r| r.kind == ReferenceKind::FunctionCall && r.segments == &["map"]);
		assert!(call.is_some(), "expected FunctionCall [\"map\"], got {refs:?}");
	}

	/// `builtins.map f xs` emits FunctionCall with segments `["builtins", "map"]`.
	#[test]
	fn qualified_function_call_segments() {
		let src = "builtins.map f xs";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let call = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::FunctionCall && r.segments == &["builtins", "map"]);
		assert!(call.is_some(), "expected FunctionCall [\"builtins\", \"map\"], got {refs:?}");
	}

	/// A bare identifier `pkgs` in a value position emits VariableUse.
	#[test]
	fn bare_identifier_emits_variable_use() {
		let src = "pkgs";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let r = refs.iter().find(|r| r.kind == ReferenceKind::VariableUse && r.segments == &["pkgs"]);
		assert!(r.is_some(), "expected VariableUse [\"pkgs\"], got {refs:?}");
	}

	/// `with pkgs; [ hello ]` — `hello` inside the `with` body is emitted as
	/// `VariableUse` with segments `["hello"]`, NOT as `pkgs.hello`.
	///
	/// Rationale: `with` is scope-destroying; static qualification is impossible
	/// without evaluation.  The resolver resolves these only when the name is
	/// unique in the index.
	#[test]
	fn with_body_emits_variable_use_not_qualified() {
		let src = "with pkgs; [ hello ]";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		// `hello` must appear as VariableUse with segments ["hello"].
		let hello_ref = refs.iter().find(|r| r.segments == &["hello"]);
		assert!(hello_ref.is_some(), "expected reference to 'hello', got {refs:?}");
		assert_eq!(
			hello_ref.unwrap().kind,
			ReferenceKind::VariableUse,
			"'hello' inside 'with' should be VariableUse, got {refs:?}"
		);

		// Must NOT appear as ["pkgs", "hello"].
		let qualified = refs.iter().find(|r| r.segments == &["pkgs", "hello"]);
		assert!(
			qualified.is_none(),
			"'with pkgs; hello' must not emit [\"pkgs\", \"hello\"] — honesty over guessing"
		);
	}

	/// `with pkgs; [ hello ]` — `pkgs` (the environment expression) emits
	/// VariableUse for the `with` environment itself.
	#[test]
	fn with_environment_emits_variable_use() {
		let src = "with pkgs; hello";
		let tree = parse(src);
		let refs = NixSpec.references(&tree, src);

		let pkgs_ref = refs.iter().find(|r| r.segments == &["pkgs"]);
		assert!(pkgs_ref.is_some(), "expected reference to 'pkgs', got {refs:?}");
	}

	// ── Module path tests ────────────────────────────────────────────────────────

	/// Nix IR paths are pure attrpaths — module_path is always empty so
	/// occurrence FQNs (`strings::trim`) match NudoxPath segments.
	#[test]
	fn module_path_always_empty_for_attrpath_alignment() {
		assert_eq!(
			NixSpec.module_path(Path::new("pkgs/default.nix"), &layout()),
			Vec::<String>::new()
		);
		assert_eq!(
			NixSpec.module_path(Path::new("lib/strings.nix"), &layout()),
			Vec::<String>::new()
		);
		assert_eq!(
			NixSpec.module_path(Path::new("flake.nix"), &layout()),
			Vec::<String>::new()
		);
		assert_eq!(
			NixSpec.module_path(Path::new("default.nix"), &layout()),
			Vec::<String>::new()
		);
	}

	/// Multi-segment attrpath `a.b = 1` emits intermediate Module `a` plus
	/// leaf `b`, so FQN is `a::b` matching IR `a/b`.
	#[test]
	fn multi_segment_attrpath_emits_prefix_modules() {
		let src = "{ a.b = 1; }";
		let tree = parse(src);
		let defs = NixSpec.definitions(&tree, src);
		let a = defs.iter().find(|d| d.name == "a");
		let b = defs.iter().find(|d| d.name == "b");
		assert!(a.is_some(), "expected intermediate 'a', got {defs:?}");
		assert!(b.is_some(), "expected leaf 'b', got {defs:?}");
		assert_eq!(a.unwrap().kind, DefKind::Module);
		assert_eq!(b.unwrap().kind, DefKind::Type);
		// b's parent should be a.
		let a_idx = defs.iter().position(|d| d.name == "a");
		assert_eq!(b.unwrap().parent, a_idx);
	}
}
