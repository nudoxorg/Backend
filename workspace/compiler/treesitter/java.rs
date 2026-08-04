//! `LanguageSpec` for Java.
//!
//! Cursor-walks the tree-sitter parse to emit:
//!
//! - **Definitions** — class, interface, enum, record, method, constructor,
//!   with proper parent-index nesting for inner types and methods.
//! - **Imports** — single-type, on-demand glob, static single, static glob.
//! - **References** — method invocations (qualified and bare), field accesses,
//!   type references, and method references (`Foo::bar`).
//! - **module_path** — derived from the relative file path, not the parse tree
//!   (the `module_path` signature receives no tree).
//!
//! # Grammar version
//!
//! Tested against `arborium`'s bundled `tree-sitter-java` grammar (grammar-pin:
//! the node-kind constants below exactly match the kinds emitted by that
//! version; update them if the grammar is upgraded).
//!
//! # Design notes
//!
//! ## Unqualified instance-method calls
//!
//! Java's `foo()` inside an instance method is syntactically indistinguishable
//! from a static call in the same class without type resolution.  We emit it as
//! `ReferenceKind::MethodCall` with `receiver: Some(ReceiverShape::SelfRef)`.
//! This is a name-level approximation — the oracle (or the resolver) must use
//! the enclosing class + scope to decide between `this.foo()` and
//! `EnclosingClass.foo()`.  Overload selection is out of scope at this layer;
//! all overloads share the same unqualified name so FQN assembly is still
//! correct.
//!
//! ## module_path derivation
//!
//! `module_path` receives only a relative path, not the tree.  We derive the
//! package from the path by stripping a well-known source-root prefix
//! (`src/main/java/`, `src/test/java/`, `src/`) and taking the *directory*
//! segments (dropping `FileName.java`).  This mirrors the Java convention that
//! a file at `src/main/java/com/example/Foo.java` belongs to package
//! `com.example`.  In non-Maven/non-standard layouts without one of those
//! prefixes we fall back to all directory segments.  The assumption is that the
//! project is well-formed (file path == package declaration) — the resolver
//! may override this with the parsed `package_declaration` node when the tree
//! is available.
//!
//! ## `java.lang.*` implicit import
//!
//! Java implicitly imports `java.lang.*`.  We do NOT synthesise a
//! `ImportBinding` for it here; the resolver is expected to inject it or handle
//! bare `String`, `Object`, etc. references specially.

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportPrefix, ImportSource, LanguageSpec, PackageLayout,
	RawDefinition, RawReference, ReceiverShape, field_text, node_text, walk_preorder,
};

// ─── Grammar node-kind constants ────────────────────────────────────────────
// Update these if the bundled tree-sitter-java grammar is upgraded.

/// `class Foo { … }`
const K_CLASS_DECL: &str = "class_declaration";
/// `interface Foo { … }`
const K_IFACE_DECL: &str = "interface_declaration";
/// `enum Foo { … }`
const K_ENUM_DECL: &str = "enum_declaration";
/// `record Foo(…) { … }` (Java 16+)
const K_RECORD_DECL: &str = "record_declaration";
/// `void foo() { … }`
const K_METHOD_DECL: &str = "method_declaration";
/// `Foo() { … }`
const K_CTOR_DECL: &str = "constructor_declaration";

/// `import a.b.C;` or `import static a.b.C.m;`
const K_IMPORT_DECL: &str = "import_declaration";

/// `obj.method(args)` or `method(args)`
const K_METHOD_INVOC: &str = "method_invocation";
/// `obj.field`
const K_FIELD_ACCESS: &str = "field_access";
/// A simple identifier used as a type (e.g. `Foo` in `Foo x`)
const K_TYPE_IDENT: &str = "type_identifier";
/// A qualified type like `java.util.List`
const K_SCOPED_TYPE: &str = "scoped_type_identifier";
/// `Foo::bar` method reference
const K_METHOD_REF: &str = "method_reference";

/// A dotted qualified name used in imports / package declarations.
const K_SCOPED_IDENT: &str = "scoped_identifier";

// ─── JavaSpec ────────────────────────────────────────────────────────────────

/// Java structural extractor.
pub struct JavaSpec;

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Collect all dot-separated name segments from a `scoped_identifier` or plain
/// `identifier` node, returning them as a `Vec<String>` left-to-right.
///
/// Tree-sitter's Java grammar encodes `a.b.c` as:
/// ```text
/// (scoped_identifier
///   scope: (scoped_identifier scope: (identifier "a") name: (identifier "b"))
///   name:  (identifier "c"))
/// ```
/// so we recurse into the `scope` child.
fn scoped_ident_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	match node.kind() {
		"identifier" => vec![node_text(node, src).to_owned()],
		"scoped_identifier" => {
			let mut segs = Vec::new();
			if let Some(scope) = node.child_by_field_name("scope") {
				segs.extend(scoped_ident_segments(scope, src));
			}
			if let Some(name) = node.child_by_field_name("name") {
				segs.push(node_text(name, src).to_owned());
			}
			segs
		}
		// `scoped_type_identifier` has the same structure
		"scoped_type_identifier" => {
			let mut segs = Vec::new();
			if let Some(scope) = node.child_by_field_name("scope") {
				segs.extend(scoped_ident_segments(scope, src));
			}
			if let Some(name) = node.child_by_field_name("name") {
				segs.push(node_text(name, src).to_owned());
			}
			segs
		}
		// type_identifier is always a leaf
		"type_identifier" => vec![node_text(node, src).to_owned()],
		_ => vec![node_text(node, src).to_owned()],
	}
}

/// Walk a `field_access` chain `a.b.c` into segments `["a", "b", "c"]`.
///
/// Grammar shape:
/// ```text
/// (field_access
///   object: (field_access object: (identifier "a") field: (identifier "b"))
///   field:  (identifier "c"))
/// ```
fn field_access_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	if node.kind() == K_FIELD_ACCESS {
		let mut segs = Vec::new();
		if let Some(obj) = node.child_by_field_name("object") {
			segs.extend(field_access_segments(obj, src));
		}
		if let Some(field) = node.child_by_field_name("field") {
			segs.push(node_text(field, src).to_owned());
		}
		segs
	} else {
		// Base: plain identifier or type_identifier
		vec![node_text(node, src).to_owned()]
	}
}

/// Walk a `method_invocation` node and extract:
/// - The qualifier chain segments (may include the method name at the end for
///   qualified calls; bare calls produce `[name]`).
/// - The `ReceiverShape` (SelfRef for bare/`this`, Expr(text) for everything else).
/// - A flag indicating whether this is a qualified (`obj.method`) call.
fn method_invoc_segments(
	node: tree_sitter::Node,
	src: &str,
) -> (Vec<String>, Option<ReceiverShape>) {
	// The `name` field is always present (the called method's simple name).
	let method_name = field_text(node, "name", src).unwrap_or("").to_owned();

	if let Some(obj_node) = node.child_by_field_name("object") {
		let obj_text = node_text(obj_node, src);
		// Build the qualifier chain from the object side.
		let mut segs = field_access_segments(obj_node, src);
		segs.push(method_name);
		let receiver = if obj_text == "this" || obj_text == "super" {
			ReceiverShape::SelfRef
		} else {
			ReceiverShape::Expr(obj_text.to_owned())
		};
		(segs, Some(receiver))
	} else {
		// Bare call: `foo()` — implicit `this` receiver.
		(vec![method_name], Some(ReceiverShape::SelfRef))
	}
}

/// Build an `ImportSource` from the flat segment list of an import path.
///
/// We always classify the first segment as the `dependency`.  For typical Java
/// imports (`java.*`, `com.*`, `org.*`, `javax.*`, `net.*`) this is always an
/// external dependency from the JDK or a library.  The resolver may reclassify
/// single-segment imports or project-local packages as `Internal`.
fn segments_to_source(segs: Vec<String>, is_glob: bool) -> ImportSource {
	if segs.is_empty() {
		return ImportSource::Internal(segs);
	}
	if is_glob {
		let dependency = Some(segs[0].clone());
		let path = segs[1..].to_vec();
		return ImportSource::Glob(ImportPrefix { dependency, path });
	}
	let dependency = segs[0].clone();
	let path = segs[1..].to_vec();
	ImportSource::External { dependency, path }
}

// ─── Definitions ─────────────────────────────────────────────────────────────

impl JavaSpec {
	/// Recursive helper: walk the subtree rooted at `node`, appending to
	/// `out`.  `scope_stack` tracks the indices of the enclosing type/class
	/// definitions so we can set `parent` correctly.
	fn collect_definitions(
		node: tree_sitter::Node,
		src: &str,
		out: &mut Vec<RawDefinition>,
		scope_stack: &mut Vec<usize>,
	) {
		let kind_str = node.kind();

		// Decide if this node is a definition we care about.
		let def_info: Option<(DefKind, bool)> = match kind_str {
			K_CLASS_DECL => Some((DefKind::Class, true)),
			K_IFACE_DECL => Some((DefKind::Trait, true)),
			K_ENUM_DECL => Some((DefKind::Type, true)),
			K_RECORD_DECL => Some((DefKind::Type, true)),
			K_METHOD_DECL => Some((DefKind::Method, false)),
			K_CTOR_DECL => Some((DefKind::Method, false)),
			_ => None,
		};

		if let Some((def_kind, is_type_scope)) = def_info {
			if let Some(name_node) = node.child_by_field_name("name") {
				let name = node_text(name_node, src).to_owned();
				let name_span = name_node.byte_range();
				let body_span = node.byte_range();
				let parent = scope_stack.last().copied();

				let idx = out.len();
				out.push(RawDefinition { name, kind: def_kind, name_span, body_span, parent });

				// Types push themselves onto the scope stack so their nested
				// members get the correct parent index.
				if is_type_scope {
					scope_stack.push(idx);
					let mut cursor = node.walk();
					for child in node.children(&mut cursor) {
						Self::collect_definitions(child, src, out, scope_stack);
					}
					scope_stack.pop();
					return; // Children already visited above.
				}
				// Methods also recurse (for anonymous/local classes inside).
				scope_stack.push(idx);
				let mut cursor = node.walk();
				for child in node.children(&mut cursor) {
					Self::collect_definitions(child, src, out, scope_stack);
				}
				scope_stack.pop();
				return;
			}
		}

		// Not a definition node — recurse into children.
		let mut cursor = node.walk();
		for child in node.children(&mut cursor) {
			Self::collect_definitions(child, src, out, scope_stack);
		}
	}
}

// ─── LanguageSpec implementation ─────────────────────────────────────────────

impl LanguageSpec for JavaSpec {
	/// Named definition sites with nesting (class → method → inner class …).
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut out = Vec::new();
		let mut stack: Vec<usize> = Vec::new();
		Self::collect_definitions(tree.root_node(), src, &mut out, &mut stack);
		out
	}

	/// The file's import-binding table.
	///
	/// Handles four forms:
	/// - `import a.b.C;`            → `External{dependency:"a", path:["b","C"]}`, local `"C"`
	/// - `import a.b.*;`            → `Glob(ImportPrefix{dependency:Some("a"), path:["b"]})`, local `""`
	/// - `import static a.b.C.m;`  → `External{dependency:"a", path:["b","C","m"]}`, local `"m"`
	/// - `import static a.b.C.*;`  → `Glob(ImportPrefix{dependency:Some("a"), path:["b","C"]})`, local `""`
	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut out = Vec::new();

		walk_preorder(tree, |node, _depth| {
			if node.kind() != K_IMPORT_DECL {
				return;
			}

			let span = node.byte_range();

			// Detect `static` keyword child.
			let is_static = {
				let mut cur = node.walk();
				node.children(&mut cur).any(|c| c.kind() == "static")
			};

			// The identifier/scoped_identifier child carries the path.
			let path_node = {
				let mut cur = node.walk();
				node.children(&mut cur).find(|c| {
					matches!(c.kind(), K_SCOPED_IDENT | "identifier")
				})
			};

			let Some(path_node) = path_node else {
				return;
			};

			// Detect glob: the grammar emits an `asterisk` child for `.*`.
			let is_glob = {
				let mut cur = node.walk();
				node.children(&mut cur).any(|c| c.kind() == "asterisk")
			};

			if is_glob {
				// The path_node covers everything up to the `.*`.
				let segs = scoped_ident_segments(path_node, src);
				let source = segments_to_source(segs, true);
				out.push(ImportBinding { local: String::new(), source, span });
			} else {
				let segs = scoped_ident_segments(path_node, src);
				// For static imports the last segment is the member name.
				// For regular imports the last segment is the class name.
				// Both static and regular single-type imports bind the last segment
				// as the simple local name (e.g. `import a.b.C` → "C";
				// `import static a.b.C.m` → "m").
				let local = segs.last().cloned().unwrap_or_default();
				let _ = is_static; // used above for documentation; same local name rule
				let source = segments_to_source(segs, false);
				out.push(ImportBinding { local, source, span });
			}
		});

		out
	}

	/// Qualified use-sites with full chain captured as one `RawReference`.
	///
	/// Emits:
	/// - `method_invocation`  → `MethodCall` (qualified or bare/SelfRef)
	/// - `field_access`       → `FieldAccess`
	/// - `type_identifier` /
	///   `scoped_type_identifier` in non-import positions → `TypeReference`
	/// - `method_reference`   → `MethodCall` (segments = `[qualifier, "bar"]`)
	///
	/// We use a flat preorder walk and *skip* the named children of nodes we
	/// handle atomically (method_invocation, field_access) to avoid emitting
	/// spurious sub-references from within a chain we already captured as one
	/// `RawReference`.  The walk_preorder helper visits every node, so we mark
	/// node spans we have "consumed" in a skip-set.
	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut out: Vec<RawReference> = Vec::new();

		// We need a two-pass approach: first collect the spans of all
		// "atomic" nodes (method_invocation, field_access, method_reference)
		// so the inner walk can suppress them.  Instead of a two-pass we use
		// the preorder walker but track which byte-start positions we've
		// already captured so that nested nodes are silently skipped.
		//
		// A node at position P is "consumed" if any ancestor already emitted a
		// RawReference whose `span` contains P.

		walk_preorder(tree, |node, _depth| {
			let nkind = node.kind();
			let span = node.byte_range();

			// Skip if we already captured an ancestor of this node.
			if out.iter().any(|r| r.span.start <= span.start && span.end <= r.span.end) {
				return;
			}

			match nkind {
				K_METHOD_INVOC => {
					let (segs, receiver) = method_invoc_segments(node, src);
					if !segs.is_empty() {
						out.push(RawReference {
							segments: segs,
							span,
							kind: ReferenceKind::MethodCall,
							receiver,
						});
					}
				}

				K_FIELD_ACCESS => {
					let segs = field_access_segments(node, src);
					if segs.len() >= 2 {
						out.push(RawReference {
							segments: segs,
							span,
							kind: ReferenceKind::FieldAccess,
							receiver: None,
						});
					}
				}

				// `Foo::bar` method reference.
				K_METHOD_REF => {
					// Grammar: (method_reference) children are the qualifier
					// expression and the method name separated by `::`.
					// The qualifier is child 0, `::` is child 1, name is child 2.
					let mut qualifier_segs: Vec<String> = Vec::new();
					let mut method_name: Option<String> = None;
					let mut cursor = node.walk();
					let mut saw_colons = false;
					for child in node.children(&mut cursor) {
						match child.kind() {
							"::" => {
								saw_colons = true;
							}
							"identifier" | "type_identifier" if saw_colons => {
								method_name = Some(node_text(child, src).to_owned());
							}
							_ if !saw_colons => {
								qualifier_segs.extend(scoped_ident_segments(child, src));
							}
							_ => {}
						}
					}
					if let Some(m) = method_name {
						qualifier_segs.push(m);
					}
					if !qualifier_segs.is_empty() {
						out.push(RawReference {
							segments: qualifier_segs,
							span,
							kind: ReferenceKind::MethodCall,
							receiver: None,
						});
					}
				}

				K_TYPE_IDENT | K_SCOPED_TYPE => {
					// Only emit if not inside an import_declaration (imports
					// are handled separately).
					let in_import = {
						let mut p = node.parent();
						let mut found = false;
						while let Some(parent) = p {
							if parent.kind() == K_IMPORT_DECL {
								found = true;
								break;
							}
							p = parent.parent();
						}
						found
					};
					if !in_import {
						let segs = scoped_ident_segments(node, src);
						if !segs.is_empty() {
							out.push(RawReference {
								segments: segs,
								span,
								kind: ReferenceKind::TypeReference,
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

	/// Derive the Java package path from the file's relative path.
	///
	/// Algorithm:
	/// 1. Try to strip one of the conventional Maven/Gradle source-root
	///    prefixes: `src/main/java/`, `src/test/java/`, `src/main/kotlin/`,
	///    `src/test/kotlin/`, or `src/`.
	/// 2. Drop the trailing filename component (the `.java` file itself).
	/// 3. Return the remaining directory segments as the package path.
	///
	/// Example: `src/main/java/com/example/util/Foo.java` →
	///          `["com", "example", "util"]`.
	///
	/// Assumes the project is well-formed (file location == package declaration).
	fn module_path(&self, rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		// Collect all path components as UTF-8 strings, skipping empty ones.
		let components: Vec<&str> = rel
			.components()
			.filter_map(|c| c.as_os_str().to_str())
			.filter(|s| !s.is_empty())
			.collect();

		if components.is_empty() {
			return Vec::new();
		}

		// Attempt to strip a source-root prefix.
		// Each prefix is represented as a slice of segments.
		const PREFIXES: &[&[&str]] = &[
			&["src", "main", "java"],
			&["src", "test", "java"],
			&["src", "main", "kotlin"],
			&["src", "test", "kotlin"],
			&["src"],
		];

		let stripped: &[&str] = 'strip: {
			for prefix in PREFIXES {
				if components.len() > prefix.len()
					&& components[..prefix.len()]
						.iter()
						.zip(*prefix)
						.all(|(a, b)| a == b)
				{
					break 'strip &components[prefix.len()..];
				}
			}
			&components[..]
		};

		// Drop the last component (the filename), keep only directory segments.
		if stripped.len() <= 1 {
			return Vec::new(); // Only the filename, no package segments.
		}
		stripped[..stripped.len() - 1]
			.iter()
			.map(|s| s.to_string())
			.collect()
	}
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use arborium_tree_sitter as tree_sitter;

	use super::*;
	use super::super::spec::{ImportPrefix, ImportSource, LanguageSpec, PackageLayout, ReceiverShape};

	// ── Parse helper ─────────────────────────────────────────────────────────

	fn parse(src: &str) -> tree_sitter::Tree {
		let lang = arborium::get_language("java").unwrap();
		let mut parser = tree_sitter::Parser::new();
		parser.set_language(&lang).unwrap();
		parser.parse(src.as_bytes(), None).unwrap()
	}

	fn spec() -> JavaSpec { JavaSpec }

	fn layout() -> PackageLayout { PackageLayout { package: "myapp".to_owned() } }

	// ── References ───────────────────────────────────────────────────────────

	/// `obj.foo()` must produce MethodCall with Expr("obj") receiver and
	/// segments `["obj", "foo"]`.
	#[test]
	fn qualified_method_call_has_expr_receiver() {
		let src = "class T { void m() { obj.foo(); } }";
		let tree = parse(src);
		let refs = spec().references(&tree, src);
		let mc = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall && r.segments == ["obj", "foo"])
			.expect("expected MethodCall [obj, foo]");
		assert_eq!(mc.receiver, Some(ReceiverShape::Expr("obj".to_owned())));
	}

	/// Bare `foo()` inside a method must produce MethodCall with SelfRef receiver.
	#[test]
	fn bare_call_has_selfref_receiver() {
		let src = "class T { void m() { foo(); } }";
		let tree = parse(src);
		let refs = spec().references(&tree, src);
		let mc = refs
			.iter()
			.find(|r| r.kind == ReferenceKind::MethodCall && r.segments == ["foo"])
			.expect("expected bare MethodCall [foo]");
		assert_eq!(mc.receiver, Some(ReceiverShape::SelfRef));
	}

	/// `Type.staticMethod()` → segments `["Type", "staticMethod"]`, MethodCall.
	#[test]
	fn static_call_segments() {
		let src = "class T { void m() { Math.abs(x); } }";
		let tree = parse(src);
		let refs = spec().references(&tree, src);
		assert!(
			refs.iter().any(|r| {
				r.kind == ReferenceKind::MethodCall
					&& r.segments.first().map(String::as_str) == Some("Math")
					&& r.segments.last().map(String::as_str) == Some("abs")
			}),
			"expected MethodCall Math.abs; got {refs:?}"
		);
	}

	/// `obj.field` (standalone) → FieldAccess, segments `["obj", "field"]`.
	#[test]
	fn field_access_segments_test() {
		let src = "class T { void m() { int x = obj.field; } }";
		let tree = parse(src);
		let refs = spec().references(&tree, src);
		assert!(
			refs.iter().any(|r| r.kind == ReferenceKind::FieldAccess
				&& r.segments == ["obj", "field"]),
			"expected FieldAccess [obj, field]; got {refs:?}"
		);
	}

	/// `Foo::bar` method reference → MethodCall, segments `["Foo", "bar"]`.
	#[test]
	fn method_reference_segments() {
		let src = "class T { void m() { func(Foo::bar); } }";
		let tree = parse(src);
		let refs = spec().references(&tree, src);
		assert!(
			refs.iter().any(|r| r.kind == ReferenceKind::MethodCall
				&& r.segments == ["Foo", "bar"]),
			"expected MethodCall [Foo, bar]; got {refs:?}"
		);
	}

	// ── Imports ──────────────────────────────────────────────────────────────

	/// `import a.b.C;` → External{dependency:"a", path:["b","C"]}, local "C".
	#[test]
	fn import_single_type() {
		let src = "import a.b.C;\nclass T {}";
		let tree = parse(src);
		let imps = spec().imports(&tree, src);
		let imp = imps.iter().find(|i| i.local == "C").expect("import C not found");
		assert_eq!(
			imp.source,
			ImportSource::External { dependency: "a".to_owned(), path: vec!["b".to_owned(), "C".to_owned()] }
		);
	}

	/// `import a.b.*;` → Glob(ImportPrefix{dependency:Some("a"), path:["b"]}), local "".
	#[test]
	fn import_on_demand_glob() {
		let src = "import a.b.*;\nclass T {}";
		let tree = parse(src);
		let imps = spec().imports(&tree, src);
		let imp = imps.first().expect("no imports");
		assert_eq!(
			imp.source,
			ImportSource::Glob(ImportPrefix {
				dependency: Some("a".to_owned()),
				path:       vec!["b".to_owned()],
			})
		);
		assert_eq!(imp.local, "");
	}

	/// `import static a.b.C.m;` → External{dependency:"a", path:["b","C","m"]}, local "m".
	#[test]
	fn import_static_member() {
		let src = "import static a.b.C.m;\nclass T {}";
		let tree = parse(src);
		let imps = spec().imports(&tree, src);
		let imp = imps.iter().find(|i| i.local == "m").expect("static import m not found");
		assert_eq!(
			imp.source,
			ImportSource::External {
				dependency: "a".to_owned(),
				path:       vec!["b".to_owned(), "C".to_owned(), "m".to_owned()],
			}
		);
	}

	// ── Definitions / nesting ────────────────────────────────────────────────

	/// A class with a method and an inner class: checks parent indices.
	///
	/// ```java
	/// class Outer {
	///     void outerMethod() {}
	///     class Inner {
	///         void innerMethod() {}
	///     }
	/// }
	/// ```
	#[test]
	fn nested_class_parent_indices() {
		let src = "class Outer { void outerMethod() {} class Inner { void innerMethod() {} } }";
		let tree = parse(src);
		let defs = spec().definitions(&tree, src);

		// Must contain Outer, outerMethod, Inner, innerMethod.
		let outer = defs.iter().find(|d| d.name == "Outer").expect("Outer");
		let outer_idx = defs.iter().position(|d| d.name == "Outer").unwrap();
		let outer_method = defs.iter().find(|d| d.name == "outerMethod").expect("outerMethod");
		let inner = defs.iter().find(|d| d.name == "Inner").expect("Inner");
		let inner_idx = defs.iter().position(|d| d.name == "Inner").unwrap();
		let inner_method = defs.iter().find(|d| d.name == "innerMethod").expect("innerMethod");

		// Outer is top-level.
		assert_eq!(outer.parent, None, "Outer should have no parent");
		// outerMethod is direct child of Outer.
		assert_eq!(outer_method.parent, Some(outer_idx));
		// Inner is a direct child of Outer.
		assert_eq!(inner.parent, Some(outer_idx));
		// innerMethod is a child of Inner.
		assert_eq!(inner_method.parent, Some(inner_idx));
	}

	// ── module_path ──────────────────────────────────────────────────────────

	/// `src/main/java/com/x/App.java` → `["com", "x"]`.
	#[test]
	fn module_path_maven_layout() {
		let rel = PathBuf::from("src/main/java/com/x/App.java");
		let path = spec().module_path(&rel, &layout());
		assert_eq!(path, vec!["com".to_owned(), "x".to_owned()]);
	}

	/// `src/test/java/com/example/FooTest.java` → `["com", "example"]`.
	#[test]
	fn module_path_test_layout() {
		let rel = PathBuf::from("src/test/java/com/example/FooTest.java");
		let path = spec().module_path(&rel, &layout());
		assert_eq!(path, vec!["com".to_owned(), "example".to_owned()]);
	}

	/// `com/example/Bar.java` (no source-root prefix) → `["com", "example"]`.
	#[test]
	fn module_path_no_prefix() {
		let rel = PathBuf::from("com/example/Bar.java");
		let path = spec().module_path(&rel, &layout());
		assert_eq!(path, vec!["com".to_owned(), "example".to_owned()]);
	}

	/// A file directly in the root `src/` prefix with no further dirs.
	#[test]
	fn module_path_src_only_returns_empty() {
		let rel = PathBuf::from("src/Foo.java");
		let path = spec().module_path(&rel, &layout());
		assert_eq!(path, Vec::<String>::new());
	}
}
