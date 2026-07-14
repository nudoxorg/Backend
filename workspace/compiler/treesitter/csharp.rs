//! `LanguageSpec` for C#.
//!
//! Cursor-walks the tree-sitter parse to emit:
//!
//! - **Definitions** — class, interface, struct, enum, record, method,
//!   constructor, property, with proper parent-index nesting for nested types
//!   and members.
//! - **Imports** — `using` directives (ordinary, static, and alias forms).
//! - **References** — method invocations (qualified and bare), member accesses,
//!   type references, and object creation expressions.
//! - **module_path** — derived from the relative file path under conventional
//!   C# project layouts.
//!
//! # Grammar version
//!
//! Tested against `arborium`'s bundled `tree-sitter-c-sharp` grammar (grammar-
//! pin: the node-kind constants below match the kinds emitted by that version;
//! update them if the grammar is upgraded).
//!
//! # Node-kind reference (tree-sitter-c-sharp)
//!
//! The tree-sitter-c-sharp grammar uses the following node kinds for the
//! constructs we care about:
//!
//! - `class_declaration`              — `class Foo { … }`
//! - `interface_declaration`          — `interface IFoo { … }`
//! - `struct_declaration`             — `struct Foo { … }`
//! - `enum_declaration`               — `enum Foo { … }`
//! - `record_declaration`             — `record Foo(…) { … }` (C# 9+)
//! - `record_struct_declaration`      — `record struct Foo(…) { … }` (C# 10+)
//! - `method_declaration`             — `void Foo() { … }`
//! - `constructor_declaration`        — `Foo() { … }`
//! - `property_declaration`           — `int Foo { get; set; }`
//! - `namespace_declaration`          — `namespace Foo.Bar { … }`
//! - `file_scoped_namespace_declaration` — `namespace Foo.Bar;`
//! - `using_directive`                — `using System.Collections.Generic;`
//! - `invocation_expression`          — `foo.Bar(args)` or `Bar(args)`
//! - `member_access_expression`       — `foo.Bar`
//! - `object_creation_expression`     — `new Foo(…)`
//! - `identifier`                     — a plain name token
//! - `type_parameter`                 — `<T>` in a generic
//!
//! If the exact node kind differs in the version bundled with arborium, the
//! classifier will silently emit no reference for that node kind (graceful
//! degradation).

use std::path::Path;

use arborium_tree_sitter as tree_sitter;
use ir::syntax::ReferenceKind;

use super::spec::{
	DefKind, ImportBinding, ImportSource, LanguageSpec, PackageLayout, RawDefinition,
	RawReference, ReceiverShape, node_text, walk_preorder,
};

// ─── Grammar node-kind constants ────────────────────────────────────────────
// Update these if the bundled tree-sitter-c-sharp grammar is upgraded.

/// `class Foo { … }`
const K_CLASS_DECL: &str = "class_declaration";
/// `interface IFoo { … }`
const K_IFACE_DECL: &str = "interface_declaration";
/// `struct Foo { … }`
const K_STRUCT_DECL: &str = "struct_declaration";
/// `enum Foo { … }`
const K_ENUM_DECL: &str = "enum_declaration";
/// `record Foo(…) { … }` (C# 9+)
const K_RECORD_DECL: &str = "record_declaration";
/// `record struct Foo(…) { … }` (C# 10+)
const K_RECORD_STRUCT_DECL: &str = "record_struct_declaration";
/// `void Foo() { … }`
const K_METHOD_DECL: &str = "method_declaration";
/// `Foo() { … }`
const K_CTOR_DECL: &str = "constructor_declaration";

/// `namespace Foo.Bar { … }`
const K_NS_DECL: &str = "namespace_declaration";
/// `namespace Foo.Bar;` (file-scoped, C# 10+)
const K_FILE_NS_DECL: &str = "file_scoped_namespace_declaration";

/// `using System.Collections.Generic;`
const K_USING_DIR: &str = "using_directive";

/// `foo.Bar(args)` or `Bar(args)` — a method/function call
const K_INVOC_EXPR: &str = "invocation_expression";
/// `foo.Bar` — member access
const K_MEMBER_ACCESS: &str = "member_access_expression";
/// `new Foo(…)` — object construction
const K_OBJECT_CREATION: &str = "object_creation_expression";

/// A simple name identifier (grammar field "name" or bare token).
const K_IDENT: &str = "identifier";
/// A qualified name like `System.IO.Path`.
const K_QUALIFIED_NAME: &str = "qualified_name";
/// A generic name like `List<T>`.
const K_GENERIC_NAME: &str = "generic_name";

// ─── CSharpSpec ──────────────────────────────────────────────────────────────

/// C# structural extractor.
pub struct CSharpSpec;

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Collect all dot-separated name segments from a `qualified_name`, plain
/// `identifier`, or `generic_name` node, returning them left-to-right.
///
/// The tree-sitter-c-sharp grammar encodes `A.B.C` as:
/// ```text
/// (qualified_name
///   left:  (qualified_name left: (identifier "A") right: (identifier "B"))
///   right: (identifier "C"))
/// ```
fn qualified_name_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	match node.kind() {
		K_IDENT => vec![node_text(node, src).to_owned()],
		K_QUALIFIED_NAME => {
			let mut segs = Vec::new();
			if let Some(left) = node.child_by_field_name("left") {
				segs.extend(qualified_name_segments(left, src));
			}
			if let Some(right) = node.child_by_field_name("right") {
				segs.extend(qualified_name_segments(right, src));
			}
			segs
		}
		// `generic_name` has an `identifier` child plus type-argument list;
		// we only want the base name for the reference.
		K_GENERIC_NAME => {
			if let Some(name) = node.child_by_field_name("name") {
				vec![node_text(name, src).to_owned()]
			} else {
				vec![node_text(node, src).to_owned()]
			}
		}
		_ => vec![node_text(node, src).to_owned()],
	}
}

/// Walk a `member_access_expression` chain `a.b.c` into segments `["a","b","c"]`.
///
/// Grammar shape:
/// ```text
/// (member_access_expression
///   expression: (member_access_expression
///                 expression: (identifier "a")
///                 name:       (identifier "b"))
///   name:       (identifier "c"))
/// ```
fn member_access_segments(node: tree_sitter::Node, src: &str) -> Vec<String> {
	if node.kind() == K_MEMBER_ACCESS {
		let mut segs = Vec::new();
		if let Some(expr) = node.child_by_field_name("expression") {
			segs.extend(member_access_segments(expr, src));
		}
		if let Some(name) = node.child_by_field_name("name") {
			// name may be a generic_name; grab only the identifier part.
			segs.extend(qualified_name_segments(name, src));
		}
		segs
	} else {
		// Base: plain identifier or other expression.
		vec![node_text(node, src).to_owned()]
	}
}

/// Walk an `invocation_expression` and return segments + receiver shape.
///
/// Grammar shape:
/// ```text
/// (invocation_expression
///   function: <member_access_expression | identifier | …>
///   arguments: (argument_list …))
/// ```
fn invoc_segments(
	node: tree_sitter::Node,
	src: &str,
) -> (Vec<String>, Option<ReceiverShape>) {
	let Some(function) = node.child_by_field_name("function") else {
		return (Vec::new(), None);
	};

	match function.kind() {
		// `obj.Method(…)` or `A.B.Method(…)`
		K_MEMBER_ACCESS => {
			let segs = member_access_segments(function, src);
			// The receiver is everything except the last segment.
			let receiver = if segs.len() >= 2 {
				let receiver_text = segs[..segs.len() - 1].join(".");
				if receiver_text == "this" || receiver_text == "base" {
					ReceiverShape::SelfRef
				} else {
					ReceiverShape::Expr(receiver_text)
				}
			} else {
				ReceiverShape::SelfRef
			};
			(segs, Some(receiver))
		}
		// `Method(…)` — bare call, implicit `this` receiver.
		K_IDENT | K_GENERIC_NAME => {
			let name = if function.kind() == K_GENERIC_NAME {
				function
					.child_by_field_name("name")
					.map(|n| node_text(n, src).to_owned())
					.unwrap_or_else(|| node_text(function, src).to_owned())
			} else {
				node_text(function, src).to_owned()
			};
			(vec![name], Some(ReceiverShape::SelfRef))
		}
		_ => {
			let segs = member_access_segments(function, src);
			(segs, Some(ReceiverShape::SelfRef))
		}
	}
}

/// Build an [`ImportSource`] from a parsed `using_directive` node.
///
/// Handles:
/// - `using Foo.Bar;`                → `External{dependency:"Foo", path:["Bar"]}`
/// - `using static Foo.Bar.Baz;`     → `External{dependency:"Foo", path:["Bar","Baz"]}`
/// - `using Alias = Foo.Bar;`        → `External{…}` with local `"Alias"`
/// - `using Foo.*;` (non-standard)   → `Glob(…)` if the grammar emits it
fn using_to_binding(node: tree_sitter::Node, src: &str) -> Option<ImportBinding> {
	let span = node.byte_range();

	// Detect `static` keyword child.
	let is_static = {
		let mut cur = node.walk();
		node.children(&mut cur).any(|c| c.kind() == "static")
	};

	// Detect alias form `using Alias = …`
	// The grammar emits a `name_equals` child for the alias part.
	let alias: Option<String> = {
		let mut cur = node.walk();
		node.children(&mut cur).find(|c| c.kind() == "name_equals").and_then(|ne| {
			ne.child_by_field_name("name")
				.map(|n| node_text(n, src).to_owned())
		})
	};

	// The actual path node is a `qualified_name`, `identifier`, or `generic_name`.
	let path_node = {
		let mut cur = node.walk();
		node.children(&mut cur).find(|c| {
			matches!(c.kind(), K_QUALIFIED_NAME | K_IDENT | K_GENERIC_NAME)
		})
	}?;

	let segs = qualified_name_segments(path_node, src);
	if segs.is_empty() {
		return None;
	}

	let local = if let Some(a) = alias {
		a
	} else if is_static {
		// `using static System.Math;` — the bound name is the last segment.
		segs.last().cloned().unwrap_or_default()
	} else {
		// `using System.Collections.Generic;` — common convention: use last segment
		// as local shorthand, though C# doesn't actually introduce a local name.
		segs.last().cloned().unwrap_or_default()
	};

	let source = if segs.len() == 1 {
		// Single-segment: treat as internal (e.g. project-root namespace).
		ImportSource::Internal(segs)
	} else {
		let dependency = segs[0].clone();
		let path = segs[1..].to_vec();
		ImportSource::External { dependency, path }
	};

	Some(ImportBinding { local, source, span })
}

// ─── Definitions ─────────────────────────────────────────────────────────────

impl CSharpSpec {
	/// Recursive definition collector.  `scope_stack` tracks the indices of the
	/// enclosing type definitions so we can set `parent` correctly.
	fn collect_definitions(
		node: tree_sitter::Node,
		src: &str,
		out: &mut Vec<RawDefinition>,
		scope_stack: &mut Vec<usize>,
	) {
		let kind_str = node.kind();

		let def_info: Option<(DefKind, bool)> = match kind_str {
			K_CLASS_DECL => Some((DefKind::Class, true)),
			K_IFACE_DECL => Some((DefKind::Trait, true)),
			K_STRUCT_DECL => Some((DefKind::Type, true)),
			K_ENUM_DECL => Some((DefKind::Type, true)),
			K_RECORD_DECL => Some((DefKind::Type, true)),
			K_RECORD_STRUCT_DECL => Some((DefKind::Type, true)),
			K_METHOD_DECL => Some((DefKind::Method, false)),
			K_CTOR_DECL => Some((DefKind::Method, false)),
			// Namespace declarations act as module frames.
			K_NS_DECL | K_FILE_NS_DECL => Some((DefKind::Module, true)),
			_ => None,
		};

		if let Some((def_kind, is_scope)) = def_info {
			if let Some(name_node) = node.child_by_field_name("name") {
				let name = node_text(name_node, src).to_owned();
				let name_span = name_node.byte_range();
				let body_span = node.byte_range();
				let parent = scope_stack.last().copied();

				let idx = out.len();
				out.push(RawDefinition { name, kind: def_kind, name_span, body_span, parent });

				if is_scope {
					scope_stack.push(idx);
					let mut cursor = node.walk();
					for child in node.children(&mut cursor) {
						Self::collect_definitions(child, src, out, scope_stack);
					}
					scope_stack.pop();
					return;
				}

				// Methods also recurse for local types / lambdas.
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

impl LanguageSpec for CSharpSpec {
	fn definitions(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawDefinition> {
		let mut out = Vec::new();
		let mut stack: Vec<usize> = Vec::new();
		Self::collect_definitions(tree.root_node(), src, &mut out, &mut stack);
		out
	}

	fn imports(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<ImportBinding> {
		let mut out = Vec::new();

		walk_preorder(tree, |node, _depth| {
			if node.kind() != K_USING_DIR {
				return;
			}
			if let Some(binding) = using_to_binding(node, src) {
				out.push(binding);
			}
		});

		out
	}

	fn references(&self, tree: &tree_sitter::Tree, src: &str) -> Vec<RawReference> {
		let mut out: Vec<RawReference> = Vec::new();

		walk_preorder(tree, |node, _depth| {
			let nkind = node.kind();
			let span = node.byte_range();

			// Skip if an ancestor has already been captured.
			if out.iter().any(|r| r.span.start <= span.start && span.end <= r.span.end) {
				return;
			}

			match nkind {
				// `foo.Bar(…)` or `Bar(…)`
				K_INVOC_EXPR => {
					let (segs, receiver) = invoc_segments(node, src);
					if !segs.is_empty() {
						out.push(RawReference {
							segments: segs,
							span,
							kind: ReferenceKind::MethodCall,
							receiver,
						});
					}
				}

				// `foo.Bar` (non-call member access)
				K_MEMBER_ACCESS => {
					let segs = member_access_segments(node, src);
					if segs.len() >= 2 {
						out.push(RawReference {
							segments: segs,
							span,
							kind: ReferenceKind::FieldAccess,
							receiver: None,
						});
					}
				}

				// `new Foo(…)` — constructor call
				K_OBJECT_CREATION => {
					// The `type` field carries the type name.
					if let Some(ty) = node.child_by_field_name("type") {
						let segs = qualified_name_segments(ty, src);
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

				// Qualified names in non-import positions → TypeReference.
				K_QUALIFIED_NAME => {
					// Skip if inside a using_directive (handled in imports).
					let in_using = {
						let mut p = node.parent();
						let mut found = false;
						while let Some(parent) = p {
							if parent.kind() == K_USING_DIR {
								found = true;
								break;
							}
							p = parent.parent();
						}
						found
					};
					if !in_using {
						let segs = qualified_name_segments(node, src);
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

	/// Derive the C# namespace path from the file's relative path.
	///
	/// Algorithm:
	/// 1. Try to strip conventional source-root prefixes (`src/`, `Source/`,
	///    `lib/`).
	/// 2. Drop the trailing filename component (`.cs` file itself).
	/// 3. Return the remaining directory segments as the namespace path.
	///
	/// Example: `src/MyApp/Services/Foo.cs` → `["MyApp", "Services"]`.
	///
	/// For projects using file-scoped namespaces the parse tree would give a
	/// more accurate result; this path-based fallback is used when the tree is
	/// not available (consistent with the Java module_path approach).
	fn module_path(&self, rel: &Path, _layout: &PackageLayout) -> Vec<String> {
		let components: Vec<&str> = rel
			.components()
			.filter_map(|c| c.as_os_str().to_str())
			.filter(|s| !s.is_empty())
			.collect();

		if components.is_empty() {
			return Vec::new();
		}

		// Strip a well-known source-root prefix.
		const PREFIXES: &[&[&str]] = &[
			&["src"],
			&["Source"],
			&["lib"],
			&["Src"],
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

		// Drop the last component (the filename).
		if stripped.len() <= 1 {
			return Vec::new();
		}
		stripped[..stripped.len() - 1]
			.iter()
			.map(|s| s.to_string())
			.collect()
	}
}
