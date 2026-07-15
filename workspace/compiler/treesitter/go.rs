//! `LanguageSpec` for Go.
//!
//! Cursor-walks the tree-sitter parse tree to emit:
//!
//! - **definitions** — `function_declaration`, `method_declaration`,
//!   `type_spec` (struct/interface) with proper nesting and the
//!   `Type.Method` name scheme matching the oracle's `method_key`.
//! - **imports** — `import_declaration`/`import_spec` mapped to
//!   `ImportBinding` with local package identifiers and external paths.
//! - **references** — `selector_expression` chains, bare `call_expression`
//!   identifiers, and `type_identifier` references with receivers and kinds.
//! - **module_path** — directory segments of the file path relative to the
//!   package root.
//!
//! # Design notes
//!
//! ## `Type.Method` normalization
//!
//! The Go producer (`compile/go/item.rs`) keys methods as
//! `import/path::Type.Method` (canonical) and also registers symtab aliases
//! for the double-colon spelling `import/path::Type::Method` plus short
//! forms `Type.Method` / `Type::Method`.  To match that scheme the method
//! extractor here emits the method with `kind: DefKind::Method` and
//! `name` = the bare method name (e.g. `"Println"`), and synthesises a
//! parent `DefKind::Type` frame named after the receiver type (stripped of
//! any `*` pointer marker). The resolver rebuilds `Type.Method` /
//! `Type::Method` by joining the parent `Type` frame name with the child
//! method name; both spellings hit the producer's alias table for Index
//! confidence.
//!
//! ## Dot-import (`import . "pkg"`)
//!
//! Dot-imports bring every exported name from the package into the file's
//! namespace without any qualifier prefix. We record these as
//! `ImportSource::Glob(ImportPrefix { dependency: Some(import_path), path: [] })`.
//! Reference resolution for names sourced from a dot-import is **oracle-deferred**:
//! the static extractor cannot determine which bare identifier originates from the
//! dot-imported package vs. the file's own package.
//!
//! ## Embedding / promoted methods
//!
//! Go's struct embedding promotes methods of embedded types. Resolving a
//! promoted-method call (`s.EmbeddedMethod()`) requires knowing the struct
//! layout, which is only available via the type-checker oracle. This is
//! therefore **oracle-deferred** and not attempted here.
//!
//! # Grammar pin
//!
//! Node kind strings below are validated against the `arborium` Go grammar
//! bundled at the version pinned in the workspace `Cargo.lock` (tree-sitter-go).
//! The legacy `categorize_go` match arms in `treesitter/mod.rs` serve as a
//! further cross-reference for the same grammar version.

use std::{collections::HashMap, path::Path};

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportPrefix, ImportSource, LanguageSpec, PackageLayout,
	RawDefinition, RawReference, ReceiverShape, node_text, walk_preorder,
};

// ─── Grammar-pinned node-kind constants ──────────────────────────────────────
// These string literals match the tree-sitter-go grammar bundled in arborium.
// Changing them requires a grammar version bump and re-testing all extractors.
//
// Note: constants that exist for documentation / future use are annotated
// `#[allow(dead_code)]` so that unused-constant lints don't fire.

/// An import group or single import: `import ( … )` / `import "…"`.
const NK_IMPORT_SPEC: &str = "import_spec";
/// A free function definition: `func Name(…) { … }`.
const NK_FUNC_DECL: &str = "function_declaration";
/// A method definition: `func (r T) Name(…) { … }`.
const NK_METHOD_DECL: &str = "method_declaration";
/// The inner spec inside a type declaration: `Name underlying`.
const NK_TYPE_SPEC: &str = "type_spec";
/// `interface { … }` type literal.
const NK_INTERFACE_TYPE: &str = "interface_type";
/// `pkg.Field` / `pkg.Func` / `val.Method` selector expression.
const NK_SELECTOR_EXPR: &str = "selector_expression";
/// A function or method call: `f(…)`.
const NK_CALL_EXPR: &str = "call_expression";
/// A type name in type-position context.
const NK_TYPE_IDENT: &str = "type_identifier";
/// The field name on the right-hand side of a selector expression.
const NK_FIELD_IDENT: &str = "field_identifier";
// "parameter_list" — the method receiver wrapping node (not matched directly;
// we iterate its children for the `parameter_declaration` inside).
/// One parameter declaration inside a parameter list.
const NK_PARAM_DECL: &str = "parameter_declaration";
/// A pointer type: `*T`.
const NK_POINTER_TYPE: &str = "pointer_type";

// ─── Go structural extractor ─────────────────────────────────────────────────

/// Go structural extractor.
pub struct GoSpec;

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Strip the outer double-quotes from an `interpreted_string_literal` value.
///
/// Returns the raw content between the quotes, or the original text if no
/// quotes are found (defensive).
fn unquote(s: &str) -> &str {
	s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

/// Split an import path (e.g. `"github.com/x/y"`) into
/// `(dependency, remainder_segments)`.
///
/// The dependency is the first path segment for module paths
/// (`github.com/x/y` → `"github.com"`) or the entire short stdlib name when
/// there is only one segment (`"fmt"` → `"fmt"`).  The resolver treats the
/// dependency as the external module root.
fn split_import_path(path: &str) -> (String, Vec<String>) {
	let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
	match segments.as_slice() {
		[] => (String::new(), vec![]),
		[only] => (only.to_string(), vec![]),
		[first, rest @ ..] => (first.to_string(), rest.iter().map(|s| s.to_string()).collect()),
	}
}

/// Derive the local package identifier from an import path: the last segment
/// (`"github.com/x/y"` → `"y"`, `"fmt"` → `"fmt"`).
fn last_segment(path: &str) -> String {
	path.split('/').filter(|s| !s.is_empty()).last().unwrap_or(path).to_string()
}

/// Resolve the receiver type name from a `method_declaration`'s receiver
/// `parameter_list` node.
///
/// The receiver is always the first (and only) parameter in the list.  We
/// accept both value receivers `(r T)` and pointer receivers `(r *T)`, and
/// also unnamed receivers `(T)` / `(*T)` (valid since Go 1.22+).
///
/// Returns `None` when the receiver cannot be parsed (defensive).
fn receiver_type_name(receiver_list: tree_sitter::Node, src: &str) -> Option<String> {
	// Walk children of the parameter_list to find the parameter_declaration.
	let mut cursor = receiver_list.walk();
	for child in receiver_list.children(&mut cursor) {
		if child.kind() == NK_PARAM_DECL {
			// The type is in the `type` field of parameter_declaration.
			if let Some(type_node) = child.child_by_field_name("type") {
				return Some(extract_type_name(type_node, src));
			}
			// Unnamed receiver: `(T)` — the parameter_declaration's child
			// may directly be a type_identifier or pointer_type.
			return Some(extract_type_name(child, src));
		}
		// Also accept a bare type_identifier or pointer_type as a direct
		// child of the parameter_list (some grammar versions flatten it).
		if child.kind() == NK_TYPE_IDENT {
			return Some(node_text(child, src).to_string());
		}
		if child.kind() == NK_POINTER_TYPE {
			return Some(extract_type_name(child, src));
		}
	}
	None
}

/// Extract the unqualified type name from a type node, stripping any leading
/// `*` pointer marker.
fn extract_type_name(node: tree_sitter::Node, src: &str) -> String {
	if node.kind() == NK_POINTER_TYPE {
		// The pointed-to type is the first non-punctuation child.
		let mut cursor = node.walk();
		for child in node.children(&mut cursor) {
			if child.kind() == NK_TYPE_IDENT {
				return node_text(child, src).to_string();
			}
		}
		// Fall back to the whole text minus the `*`.
		node_text(node, src).trim_start_matches('*').to_string()
	} else if node.kind() == NK_TYPE_IDENT {
		node_text(node, src).to_string()
	} else {
		// Parameter declaration itself: look for a type_identifier child.
		let mut cursor = node.walk();
		for child in node.children(&mut cursor) {
			if child.kind() == NK_TYPE_IDENT {
				return node_text(child, src).to_string();
			}
			if child.kind() == NK_POINTER_TYPE {
				return extract_type_name(child, src);
			}
		}
		// Final fall-back: raw text.
		node_text(node, src).to_string()
	}
}

// ─── Import extraction ────────────────────────────────────────────────────────

/// Build the import binding table from all `import_declaration` nodes.
///
/// Handles:
/// - Simple imports: `import "fmt"` → local `"fmt"`, external dep `"fmt"`.
/// - Aliased imports: `import myfmt "fmt"` → local `"myfmt"`.
/// - Blank imports: `import _ "pkg"` → local `"_"` (side-effect only).
/// - Dot imports: `import . "pkg"` → `ImportSource::Glob`.
/// - Grouped imports: `import ( "fmt"; "os" )`.
fn extract_imports(tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
	let mut bindings = Vec::new();

	walk_preorder(tree, |node, _depth| {
		if node.kind() != NK_IMPORT_SPEC {
			return;
		}

		// The path is the `path` field — an interpreted_string_literal.
		let path_node = match node.child_by_field_name("path") {
			Some(n) => n,
			None => return,
		};
		let raw_path = node_text(path_node, src);
		let import_path = unquote(raw_path).to_string();
		let span = node.byte_range();

		// The optional name field is an identifier, `.`, `_`, or absent.
		let name_field = node.child_by_field_name("name");
		let alias_text = name_field.map(|n| node_text(n, src));

		let (local, source) = match alias_text {
			// Dot-import: bring everything into scope without qualifier.
			Some(".") => {
				let (dep, path_segs) = split_import_path(&import_path);
				let source =
					ImportSource::Glob(ImportPrefix { dependency: Some(dep), path: path_segs });
				(String::new(), source)
			}
			// Blank import: side-effect only; we still record it.
			Some("_") => {
				let (dep, path_segs) = split_import_path(&import_path);
				let source = ImportSource::External { dependency: dep, path: path_segs };
				("_".to_string(), source)
			}
			// Explicit alias.
			Some(alias) => {
				let (dep, path_segs) = split_import_path(&import_path);
				let source = ImportSource::External { dependency: dep, path: path_segs };
				(alias.to_string(), source)
			}
			// No alias: use the last path segment as the local name.
			None => {
				let local = last_segment(&import_path);
				let (dep, path_segs) = split_import_path(&import_path);
				let source = ImportSource::External { dependency: dep, path: path_segs };
				(local, source)
			}
		};

		bindings.push(ImportBinding { local, source, span });
	});

	bindings
}

// ─── Definition extraction ────────────────────────────────────────────────────

/// Extract all named definition sites from the tree.
///
/// Emits:
/// - `function_declaration` → `DefKind::Function`
/// - `method_declaration` → synthetic `DefKind::Type` parent + `DefKind::Method`
///   child, normalizing pointer receivers (`*T`) to `T`.
/// - `type_spec` with `struct_type` body → `DefKind::Type`
/// - `type_spec` with `interface_type` body → `DefKind::Trait`
///
/// All definitions are at file top level (Go has no nested named types in the
/// syntactic sense; inner type specs inside functions are out of scope for the
/// resolver).
fn extract_definitions(tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
	let mut defs: Vec<RawDefinition> = Vec::new();

	// Receiver-type name → index of the synthetic type frame already emitted.
	// Avoids duplicating a `DefKind::Type` frame when multiple methods share
	// the same receiver type.
	let mut receiver_type_index: HashMap<String, usize> = HashMap::new();

	walk_preorder(tree, |node, _depth| {
		match node.kind() {
			NK_FUNC_DECL => {
				let name_node = match node.child_by_field_name("name") {
					Some(n) => n,
					None => return,
				};
				let name = node_text(name_node, src).to_string();
				let name_span = name_node.byte_range();
				let body_span = node.byte_range();
				defs.push(RawDefinition {
					name,
					kind: DefKind::Function,
					name_span,
					body_span,
					parent: None,
				});
			}

			NK_METHOD_DECL => {
				// Receiver list is in the `receiver` field.
				let receiver_list = match node.child_by_field_name("receiver") {
					Some(n) => n,
					None => return,
				};
				let type_name = match receiver_type_name(receiver_list, src) {
					Some(t) => t,
					None => return,
				};
				let method_name_node = match node.child_by_field_name("name") {
					Some(n) => n,
					None => return,
				};
				let method_name = node_text(method_name_node, src).to_string();
				let method_name_span = method_name_node.byte_range();
				let method_body_span = node.byte_range();

				// Ensure a synthetic type frame exists for this receiver type.
				let type_idx = *receiver_type_index.entry(type_name.clone()).or_insert_with(|| {
					let idx = defs.len();
					// Use the method_declaration span as a proxy; the resolver
					// only uses the name for FQN reconstruction.
					defs.push(RawDefinition {
						name: type_name.clone(),
						kind: DefKind::Type,
						// We have no standalone span for the type from this node alone;
						// use the method's body span as an approximation so the type
						// frame is valid.
						name_span: receiver_list.byte_range(),
						body_span: method_body_span.clone(),
						parent: None,
					});
					idx
				});

				// Widen the synthetic type frame's body_span to cover all its methods.
				if let Some(frame) = defs.get_mut(type_idx) {
					let start = frame.body_span.start.min(method_body_span.start);
					let end = frame.body_span.end.max(method_body_span.end);
					frame.body_span = start..end;
				}

				defs.push(RawDefinition {
					name: method_name,
					kind: DefKind::Method,
					name_span: method_name_span,
					body_span: method_body_span,
					parent: Some(type_idx),
				});
			}

			NK_TYPE_SPEC => {
				let name_node = match node.child_by_field_name("name") {
					Some(n) => n,
					None => return,
				};
				let name = node_text(name_node, src).to_string();
				let name_span = name_node.byte_range();
				let body_span = node.byte_range();

				// Determine kind from the type body.
				let kind = if let Some(type_node) = node.child_by_field_name("type") {
					match type_node.kind() {
						NK_INTERFACE_TYPE => DefKind::Trait,
						_ => DefKind::Type,
					}
				} else {
					DefKind::Type
				};

				defs.push(RawDefinition { name, kind, name_span, body_span, parent: None });
			}

			_ => {}
		}
	});

	defs
}

// ─── Reference extraction ─────────────────────────────────────────────────────

/// Extract all qualified use-sites from the tree.
///
/// Emits:
///
/// 1. `selector_expression` → one `RawReference` with `segments = [operand, field]`.
///    - If the operand is a `package_identifier` that appears in the import
///      table the kind is `FunctionCall` (when the selector is the direct
///      callee of a `call_expression`) or `TypeReference`/`FieldAccess` otherwise.
///    - If the operand is any other value (struct, map, etc.) the kind is
///      `MethodCall` (when under a `call_expression`) or `FieldAccess`, and
///      `receiver` is set to the operand text wrapped in `ReceiverShape::Expr`.
///      Special-case: operand text `"self"` → `ReceiverShape::SelfRef` (Go
///      does not have `self` by convention but some code uses it as a variable
///      name).
///
/// 2. Bare `identifier` that is the direct callee of a `call_expression` (not
///    the right-hand field of a selector) → `FunctionCall`.
///
/// 3. `type_identifier` in type-position → `TypeReference`.
///
/// **Not emitted here (oracle-deferred):** references originating from names
/// brought into scope by dot-imports; promoted/embedded-type method calls.
fn extract_references(tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
	// Build the import table: local name → true (is a package identifier).
	// We use this to distinguish `pkg.Func` (cross-package call) from
	// `val.Method` (method call on a value).
	let mut pkg_names: HashMap<String, bool> = HashMap::new();
	walk_preorder(tree, |node, _| {
		if node.kind() == NK_IMPORT_SPEC {
			let path_node = match node.child_by_field_name("path") {
				Some(n) => n,
				None => return,
			};
			let raw_path = node_text(path_node, src);
			let import_path = unquote(raw_path);

			let name_field = node.child_by_field_name("name");
			let alias_text = name_field.map(|n| node_text(n, src));
			match alias_text {
				Some(".") | Some("_") => {}
				Some(alias) => {
					pkg_names.insert(alias.to_string(), true);
				}
				None => {
					pkg_names.insert(last_segment(import_path), true);
				}
			}
		}
	});

	let mut refs: Vec<RawReference> = Vec::new();
	// Track selector_expression byte ranges we've already emitted so we don't
	// double-emit the `field_identifier` child separately.
	let mut emitted_selector_spans: std::collections::HashSet<usize> = Default::default();

	walk_preorder(tree, |node, _depth| {
		match node.kind() {
			NK_SELECTOR_EXPR => {
				// Skip if we are a nested selector (the outermost selector
				// already captures us).  We detect nesting by checking whether
				// the parent is also a selector_expression.
				let parent_is_selector = node
					.parent()
					.map(|p| p.kind() == NK_SELECTOR_EXPR)
					.unwrap_or(false);
				if parent_is_selector {
					return;
				}

				let operand_node = match node.child_by_field_name("operand") {
					Some(n) => n,
					None => return,
				};
				let field_node = match node.child_by_field_name("field") {
					Some(n) => n,
					None => return,
				};

				let operand_text = node_text(operand_node, src);
				let field_text_val = node_text(field_node, src);
				let span = node.byte_range();

				// Determine if this selector is the callee of a call_expression.
				let under_call = node
					.parent()
					.map(|p| p.kind() == NK_CALL_EXPR && {
						// Make sure this selector is the function/callee child,
						// not an argument.
						p.child_by_field_name("function")
							.map(|f| f.id() == node.id())
							.unwrap_or(false)
					})
					.unwrap_or(false);

				let is_pkg = pkg_names.contains_key(operand_text);

				let (kind, receiver) = if is_pkg {
					// Cross-package reference.
					let k = if under_call {
						ReferenceKind::FunctionCall
					} else {
						// Could be a type or constant; we default to TypeReference
						// unless it looks like a field.
						if field_node.kind() == NK_FIELD_IDENT {
							ReferenceKind::FieldAccess
						} else {
							ReferenceKind::TypeReference
						}
					};
					(k, None)
				} else {
					// Value receiver: method call or field access.
					let recv = if operand_text == "self" {
						ReceiverShape::SelfRef
					} else {
						ReceiverShape::Expr(operand_text.to_string())
					};
					let k = if under_call {
						ReferenceKind::MethodCall
					} else {
						ReferenceKind::FieldAccess
					};
					(k, Some(recv))
				};

				emitted_selector_spans.insert(span.start);
				refs.push(RawReference {
					segments: vec![operand_text.to_string(), field_text_val.to_string()],
					span,
					kind,
					receiver,
				});
			}

			// Bare function call: `f(…)` where `f` is a plain identifier.
			// We only emit this when the identifier is NOT the field side of a
			// selector we already handled above.
			"identifier" => {
				let parent = match node.parent() {
					Some(p) => p,
					None => return,
				};
				if parent.kind() != NK_CALL_EXPR {
					return;
				}
				// Confirm this identifier is the function child, not an argument.
				let is_callee = parent
					.child_by_field_name("function")
					.map(|f| f.id() == node.id())
					.unwrap_or(false);
				if !is_callee {
					return;
				}
				// Skip if the call was already handled as a selector.
				if emitted_selector_spans.contains(&parent.byte_range().start) {
					return;
				}
				let name = node_text(node, src).to_string();
				refs.push(RawReference {
					segments: vec![name],
					span: node.byte_range(),
					kind: ReferenceKind::FunctionCall,
					receiver: None,
				});
			}

			// Type references in type-position (e.g. `var x MyType`).
			NK_TYPE_IDENT => {
				// Skip if the parent is a selector_expression (handled above).
				let parent = node.parent();
				if parent.map(|p| p.kind() == NK_SELECTOR_EXPR).unwrap_or(false) {
					return;
				}
				let name = node_text(node, src).to_string();
				refs.push(RawReference {
					segments: vec![name],
					span: node.byte_range(),
					kind: ReferenceKind::TypeReference,
					receiver: None,
				});
			}

			_ => {}
		}
	});

	refs
}

// ─── LanguageSpec implementation ──────────────────────────────────────────────

impl LanguageSpec for GoSpec {
	/// Extract named definition sites with nesting from the Go parse tree.
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		extract_definitions(tree, src)
	}

	/// Build the import binding table from `import_declaration` nodes.
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		extract_imports(tree, src)
	}

	/// Collect qualified use-sites with kinds and receiver shapes.
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		extract_references(tree, src)
	}

	/// Derive the module path from the file path relative to the package root.
	///
	/// Go packages are directory-scoped: every `.go` file in a directory
	/// belongs to the same package.  The module path is therefore the
	/// directory segments of `rel`, dropping the filename.
	///
	/// Examples:
	/// - `"main.go"` → `[]` (root package).
	/// - `"net/http/server.go"` → `["net", "http"]`.
	fn module_path(&self, rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		rel.parent()
			.map(|dir| {
				dir.components()
					.filter_map(|c| match c {
						std::path::Component::Normal(s) => s.to_str().map(|s| s.to_string()),
						_ => None,
					})
					.collect()
			})
			.unwrap_or_default()
	}
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::Path;

	use arborium_tree_sitter as tree_sitter;
	use ir::syntax::ReferenceKind;

	use super::GoSpec;
	use super::super::spec::{DefKind, ImportSource, LanguageSpec, PackageLayout, ReceiverShape};

	fn parse_go(src: &str) -> tree_sitter::Tree {
		let lang = arborium::get_language("go").unwrap();
		let mut parser = arborium_tree_sitter::Parser::new();
		parser.set_language(&lang).unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn layout() -> PackageLayout { PackageLayout { package: "mypkg".to_string() } }

	// ── imports ──────────────────────────────────────────────────────────────

	#[test]
	fn simple_import_stdlib() {
		let src = r#"package main
import "fmt"
"#;
		let tree = parse_go(src);
		let bindings = GoSpec.imports(&tree, src);
		assert_eq!(bindings.len(), 1);
		let b = &bindings[0];
		assert_eq!(b.local, "fmt");
		assert!(
			matches!(&b.source, ImportSource::External { dependency, path }
				if dependency == "fmt" && path.is_empty()),
			"unexpected source: {:?}",
			b.source
		);
	}

	#[test]
	fn grouped_import_two_packages() {
		let src = r#"package main
import (
	"fmt"
	"os"
)
"#;
		let tree = parse_go(src);
		let bindings = GoSpec.imports(&tree, src);
		let locals: Vec<&str> = bindings.iter().map(|b| b.local.as_str()).collect();
		assert!(locals.contains(&"fmt"), "expected fmt, got {locals:?}");
		assert!(locals.contains(&"os"), "expected os, got {locals:?}");
	}

	#[test]
	fn aliased_import() {
		let src = r#"package main
import myfmt "fmt"
"#;
		let tree = parse_go(src);
		let bindings = GoSpec.imports(&tree, src);
		assert_eq!(bindings.len(), 1);
		assert_eq!(bindings[0].local, "myfmt");
	}

	#[test]
	fn dot_import_becomes_glob() {
		let src = r#"package main
import . "fmt"
"#;
		let tree = parse_go(src);
		let bindings = GoSpec.imports(&tree, src);
		assert_eq!(bindings.len(), 1);
		assert!(
			matches!(&bindings[0].source, ImportSource::Glob(_)),
			"expected Glob, got {:?}",
			bindings[0].source
		);
		assert_eq!(bindings[0].local, "");
	}

	#[test]
	fn external_module_import_path() {
		let src = r#"package main
import "github.com/user/repo"
"#;
		let tree = parse_go(src);
		let bindings = GoSpec.imports(&tree, src);
		assert_eq!(bindings.len(), 1);
		let b = &bindings[0];
		assert_eq!(b.local, "repo");
		assert!(
			matches!(&b.source, ImportSource::External { dependency, path }
				if dependency == "github.com" && path == &["user", "repo"]),
			"unexpected source: {:?}",
			b.source
		);
	}

	// ── references ───────────────────────────────────────────────────────────

	#[test]
	fn package_func_call_produces_function_call_reference() {
		let src = r#"package main
import "fmt"
func main() {
	fmt.Println("hello")
}
"#;
		let tree = parse_go(src);
		let refs = GoSpec.references(&tree, src);
		let selector_ref = refs
			.iter()
			.find(|r| r.segments == vec!["fmt", "Println"])
			.expect("expected fmt.Println reference");
		assert_eq!(
			selector_ref.kind,
			ReferenceKind::FunctionCall,
			"fmt.Println should be FunctionCall"
		);
		assert_eq!(selector_ref.receiver, None);
	}

	#[test]
	fn value_method_call_is_method_call_with_receiver() {
		let src = r#"package main
func process(buf *bytes.Buffer) {
	buf.WriteString("hello")
}
"#;
		let tree = parse_go(src);
		let refs = GoSpec.references(&tree, src);
		let method_ref = refs
			.iter()
			.find(|r| r.segments == vec!["buf", "WriteString"])
			.expect("expected buf.WriteString reference");
		assert_eq!(
			method_ref.kind,
			ReferenceKind::MethodCall,
			"buf.WriteString should be MethodCall"
		);
		assert_eq!(
			method_ref.receiver,
			Some(ReceiverShape::Expr("buf".to_string())),
			"receiver should be Expr(buf)"
		);
	}

	#[test]
	fn field_access_on_struct_not_under_call() {
		let src = r#"package main
type Point struct { X, Y int }
func show(p Point) string {
	return p.X
}
"#;
		let tree = parse_go(src);
		let refs = GoSpec.references(&tree, src);
		let field_ref = refs.iter().find(|r| r.segments == vec!["p", "X"]);
		assert!(field_ref.is_some(), "expected p.X reference");
		assert_eq!(field_ref.unwrap().kind, ReferenceKind::FieldAccess);
	}

	#[test]
	fn bare_function_call_without_selector() {
		let src = r#"package main
func greet() {}
func main() { greet() }
"#;
		let tree = parse_go(src);
		let refs = GoSpec.references(&tree, src);
		let greet_ref = refs.iter().find(|r| r.segments == vec!["greet"]);
		assert!(greet_ref.is_some(), "expected greet() reference");
		assert_eq!(greet_ref.unwrap().kind, ReferenceKind::FunctionCall);
	}

	#[test]
	fn type_identifier_in_var_decl_is_type_reference() {
		let src = r#"package main
type MyType struct {}
func use() {
	var x MyType
	_ = x
}
"#;
		let tree = parse_go(src);
		let refs = GoSpec.references(&tree, src);
		let type_ref = refs.iter().find(|r| r.segments == vec!["MyType"]);
		assert!(type_ref.is_some(), "expected MyType type reference");
		assert_eq!(type_ref.unwrap().kind, ReferenceKind::TypeReference);
	}

	// ── definitions ──────────────────────────────────────────────────────────

	#[test]
	fn function_declaration_extracted() {
		let src = r#"package main
func Hello() {}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);
		let f = defs.iter().find(|d| d.name == "Hello").expect("expected Hello function");
		assert_eq!(f.kind, DefKind::Function);
		assert_eq!(f.parent, None);
	}

	#[test]
	fn method_declaration_normalized_to_type_method() {
		let src = r#"package main
func (b *Buffer) Write(data []byte) {}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);

		// Expect a synthetic Type frame for "Buffer".
		let type_frame =
			defs.iter().find(|d| d.name == "Buffer").expect("expected synthetic Buffer type frame");
		assert_eq!(type_frame.kind, DefKind::Type);
		let type_idx = defs.iter().position(|d| d.name == "Buffer").unwrap();

		// Expect a Method frame for "Write" parented to Buffer.
		let method_frame =
			defs.iter().find(|d| d.name == "Write").expect("expected Write method frame");
		assert_eq!(method_frame.kind, DefKind::Method);
		assert_eq!(
			method_frame.parent,
			Some(type_idx),
			"Write should be parented to Buffer type frame"
		);
	}

	#[test]
	fn pointer_receiver_strips_star() {
		let src = r#"package main
func (s *Server) Serve() {}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);
		// Receiver type should be "Server", not "*Server".
		let type_frame =
			defs.iter().find(|d| d.name == "Server").expect("expected Server type frame (no *)");
		assert_eq!(type_frame.kind, DefKind::Type);
	}

	#[test]
	fn struct_type_declaration_extracted() {
		let src = r#"package main
type Widget struct {
	ID   int
	Name string
}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);
		let widget = defs.iter().find(|d| d.name == "Widget").expect("expected Widget type def");
		assert_eq!(widget.kind, DefKind::Type);
	}

	#[test]
	fn interface_type_declaration_is_trait() {
		let src = r#"package main
type Writer interface {
	Write(p []byte) (int, error)
}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);
		let writer = defs.iter().find(|d| d.name == "Writer").expect("expected Writer def");
		assert_eq!(writer.kind, DefKind::Trait);
	}

	#[test]
	fn multiple_methods_same_receiver_share_type_frame() {
		let src = r#"package main
func (c *Client) Get() {}
func (c *Client) Post() {}
"#;
		let tree = parse_go(src);
		let defs = GoSpec.definitions(&tree, src);
		// Exactly one synthetic "Client" type frame.
		let client_frames: Vec<_> = defs.iter().filter(|d| d.name == "Client").collect();
		assert_eq!(client_frames.len(), 1, "should be exactly one Client type frame");
		let client_idx = defs.iter().position(|d| d.name == "Client").unwrap();
		// Both methods parented to it.
		for method_name in &["Get", "Post"] {
			let m = defs.iter().find(|d| d.name == *method_name).unwrap();
			assert_eq!(m.parent, Some(client_idx));
		}
	}

	// ── module_path ───────────────────────────────────────────────────────────

	#[test]
	fn module_path_root_file_is_empty() {
		let layout = layout();
		let path = GoSpec.module_path(Path::new("main.go"), &layout);
		assert_eq!(path, Vec::<String>::new());
	}

	#[test]
	fn module_path_nested_file_gives_dir_segments() {
		let layout = layout();
		let path = GoSpec.module_path(Path::new("net/http/server.go"), &layout);
		assert_eq!(path, vec!["net", "http"]);
	}

	#[test]
	fn module_path_single_dir_file() {
		let layout = layout();
		let path = GoSpec.module_path(Path::new("handlers/main.go"), &layout);
		assert_eq!(path, vec!["handlers"]);
	}
}
