//! High-fidelity assertions for the Java IR lowering.
//!
//! These tests feed synthetic oracle JSON through
//! [`compiler::languages::java::context::lower_extraction`] — no JDK / doclet
//! jar required — and assert the structural contracts that snapshot tests
//! alone are too coarse to pin:
//!
//! 1. Interface method Index paths (`com.example::Drawable.draw` → Function)
//! 2. Structured `Symbol.deprecation` on deprecated class/method
//! 3. Non-empty `doc_links` when `{@link …}` targets resolve in-extraction
//! 4. Enum field `TypedBinding.ty` is populated
//! 5. Sealed interface → `TraitDef.sealed = Some(true)`
//! 6. `SymbolTable` resolves an interface method path

use std::path::PathBuf;

use compiler::graph::symtab::SymbolTable;
use compiler::languages::java::context::lower_extraction;
use compiler::languages::java::schema::Extraction;
use ir::entry::NudoxPath;
use ir::kind::{Deprecation, Entry, TypedBinding};
use ir::protocols::TraitDef;
use ir::ty::Type as IrType;
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn local(s: &str) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(s))
}

/// Minimal top-level type skeleton; callers overlay the fields they care about.
fn type_json(
	qualified: &str,
	simple: &str,
	kind: &str,
	package: &str,
	extra: serde_json::Value,
) -> serde_json::Value {
	let mut base = json!({
		"qualifiedName": qualified,
		"simpleName": simple,
		"kind": kind,
		"package": package,
		"module": null,
		"enclosing": null,
		"nesting": "TOP_LEVEL",
		"superclass": null,
		"doc": null,
		"docKind": null,
		"position": null,
	});
	if let Some(obj) = base.as_object_mut() {
		if let Some(extra_obj) = extra.as_object() {
			for (k, v) in extra_obj {
				obj.insert(k.clone(), v.clone());
			}
		}
	}
	base
}

fn extraction(types: Vec<serde_json::Value>) -> Extraction {
	serde_json::from_value(json!({
		"format": 1,
		"javaVersion": "25",
		"types": types,
	}))
	.expect("synthetic extraction deserializes")
}

fn prim(name: &str) -> serde_json::Value {
	json!({ "kind": "primitive", "name": name, "annotations": [] })
}

fn declared(name: &str) -> serde_json::Value {
	json!({
		"kind": "declared",
		"name": name,
		"args": [],
		"owner": null,
		"annotations": [],
	})
}

// ---------------------------------------------------------------------------
// 1 + 6. Interface method Index path + SymbolTable resolution
// ---------------------------------------------------------------------------

#[test]
fn interface_method_has_function_index_path() {
	let ext = extraction(vec![type_json(
		"com.example.Drawable",
		"Drawable",
		"INTERFACE",
		"com.example",
		json!({
			"modifiers": ["public"],
			"methods": [{
				"name": "draw",
				"modifiers": ["public", "abstract"],
				"params": [{
					"name": "canvas",
					"type": declared("java.lang.String"),
					"annotations": [],
				}],
				"return": { "kind": "void" },
				"doc": "Draws this shape.",
				"docKind": "TRADITIONAL",
				"deprecated": false,
				"annotations": [],
				"thrown": [],
				"typeParams": [],
				"varargs": false,
				"default": false,
				"receiver": null,
				"annotationDefault": null,
				"origin": "EXPLICIT",
				"position": null,
			}],
		}),
	)]);

	let index = lower_extraction(&ext);
	let path = local("com.example::Drawable.draw");

	let entry = index
		.entries_by_path
		.get(&path)
		.unwrap_or_else(|| panic!("missing Index entry for {path:?}"));
	assert!(
		matches!(entry, Entry::Function(_)),
		"interface method must be Entry::Function, got {entry:?}"
	);

	// TraitDef.members must list the method path alongside any nested types.
	let trait_path = local("com.example::Drawable");
	match index.entries_by_path.get(&trait_path) {
		Some(Entry::TraitDef(sym)) => {
			let members = sym.inner.members.as_ref().expect("TraitDef.members");
			assert!(
				members.iter().any(|m| m == &path),
				"TraitDef.members should contain {path:?}, got {members:?}"
			);
			// TraitMethod shape retained for protocol fidelity.
			let required = sym.inner.required_methods.as_ref().expect("required_methods");
			assert!(required.iter().any(|m| m.name == "draw"));
		}
		other => panic!("expected TraitDef for Drawable, got {other:?}"),
	}
}

#[test]
fn symbol_table_resolves_interface_method() {
	let ext = extraction(vec![type_json(
		"com.example.Drawable",
		"Drawable",
		"INTERFACE",
		"com.example",
		json!({
			"modifiers": ["public"],
			"methods": [{
				"name": "draw",
				"modifiers": ["public", "abstract"],
				"params": [],
				"return": { "kind": "void" },
				"doc": null,
				"docKind": null,
				"deprecated": false,
				"annotations": [],
				"thrown": [],
				"typeParams": [],
				"varargs": false,
				"default": false,
				"receiver": null,
				"annotationDefault": null,
				"origin": "EXPLICIT",
				"position": null,
			}],
		}),
	)]);

	let index = lower_extraction(&ext);
	let table = SymbolTable::build(&index);
	let expected = local("com.example::Drawable.draw");

	// path_segments expands `com.example::Drawable.draw` →
	// ["com","example","Drawable","draw"], so the exact key uses `::`.
	assert_eq!(
		table.resolve_exact("com::example::Drawable::draw"),
		Some(&expected),
		"SymbolTable must resolve the interface method by exact fq"
	);
	// Unique leaf should also resolve.
	assert_eq!(table.resolve_suffix("draw"), Some(&expected));
}

// ---------------------------------------------------------------------------
// 2. Structured deprecation
// ---------------------------------------------------------------------------

#[test]
fn deprecated_class_and_method_have_structured_deprecation() {
	let ext = extraction(vec![type_json(
		"com.example.LegacyApi",
		"LegacyApi",
		"CLASS",
		"com.example",
		json!({
			"modifiers": ["public"],
			"deprecated": true,
			"doc": "Legacy surface.\n\n@deprecated Use ModernApi instead.",
			"docKind": "TRADITIONAL",
			"annotations": [{
				"type": "java.lang.Deprecated",
				"values": { "since": { "kind": "string", "value": "9" } },
			}],
			"methods": [{
				"name": "oldProcess",
				"modifiers": ["public"],
				"params": [],
				"return": { "kind": "void" },
				"doc": "Old entry point.\n\n@deprecated Prefer newProcess.",
				"docKind": "TRADITIONAL",
				"deprecated": true,
				"annotations": [{
					"type": "java.lang.Deprecated",
					"values": {},
				}],
				"thrown": [],
				"typeParams": [],
				"varargs": false,
				"default": false,
				"receiver": null,
				"annotationDefault": null,
				"origin": "EXPLICIT",
				"position": null,
			}],
		}),
	)]);

	let index = lower_extraction(&ext);

	match index.entries_by_path.get(&local("com.example::LegacyApi")) {
		Some(Entry::RecordType(sym)) => {
			assert_eq!(
				sym.deprecation,
				Some(Deprecation {
					since: Some("9".into()),
					note: Some("Use ModernApi instead.".into()),
				})
			);
			// Prose section retained too.
			let doc = sym.documentation.as_deref().unwrap_or("");
			assert!(doc.contains("Deprecated:"), "docs should keep Deprecated: section");
		}
		other => panic!("expected RecordType LegacyApi, got {other:?}"),
	}

	match index.entries_by_path.get(&local("com.example::LegacyApi.oldProcess")) {
		Some(Entry::Function(sym)) => {
			assert_eq!(
				sym.deprecation,
				Some(Deprecation {
					since: None,
					note: Some("Prefer newProcess.".into()),
				})
			);
		}
		other => panic!("expected Function oldProcess, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// 3. doc_links from {@link}
// ---------------------------------------------------------------------------

#[test]
fn doc_links_resolve_in_extraction_targets() {
	let animal = type_json(
		"com.example.Animal",
		"Animal",
		"CLASS",
		"com.example",
		json!({
			"modifiers": ["public"],
			"methods": [{
				"name": "sound",
				"modifiers": ["public"],
				"params": [],
				"return": declared("java.lang.String"),
				"doc": null,
				"docKind": null,
				"deprecated": false,
				"annotations": [],
				"thrown": [],
				"typeParams": [],
				"varargs": false,
				"default": false,
				"receiver": null,
				"annotationDefault": null,
				"origin": "EXPLICIT",
				"position": null,
			}],
		}),
	);
	let dog = type_json(
		"com.example.Dog",
		"Dog",
		"CLASS",
		"com.example",
		json!({
			"modifiers": ["public"],
			"doc": "A canine. See {@link Animal} and {@link Animal#sound()}.",
			"docKind": "TRADITIONAL",
			"superclass": declared("com.example.Animal"),
		}),
	);

	let index = lower_extraction(&extraction(vec![animal, dog]));
	match index.entries_by_path.get(&local("com.example::Dog")) {
		Some(Entry::RecordType(sym)) => {
			let links = sym.doc_links.as_ref().expect("doc_links should be Some");
			assert!(!links.is_empty(), "doc_links must be non-empty for {{@link Animal}}");
			assert_eq!(
				links.get("com.example.Animal"),
				Some(&local("com.example::Animal")),
			);
			assert_eq!(
				links.get("com.example.Animal#sound()"),
				Some(&local("com.example::Animal.sound")),
			);
		}
		other => panic!("expected RecordType Dog, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// 4. Enum field TypedBinding.ty
// ---------------------------------------------------------------------------

#[test]
fn enum_field_typed_binding_ty_is_set() {
	let ext = extraction(vec![type_json(
		"com.example.Status",
		"Status",
		"ENUM",
		"com.example",
		json!({
			"modifiers": ["public"],
			"enumConstants": [
				{ "name": "ACTIVE", "annotations": [], "deprecated": false, "doc": null, "docKind": null, "source": null, "position": null },
			],
			"fields": [{
				"name": "code",
				"type": prim("int"),
				"modifiers": ["private", "final"],
				"constant": null,
				"annotations": [],
				"deprecated": false,
				"origin": "EXPLICIT",
				"doc": "Wire code for this status.",
				"docKind": "TRADITIONAL",
				"position": null,
			}],
		}),
	)]);

	let index = lower_extraction(&ext);
	match index.entries_by_path.get(&local("com.example::Status.code")) {
		Some(Entry::Field(sym)) => {
			let TypedBinding { ty, value, mutable } = &sym.inner;
			assert!(ty.is_some(), "enum field TypedBinding.ty must be set");
			assert!(
				matches!(ty, Some(IrType::Primitive(_))),
				"expected primitive int, got {ty:?}"
			);
			assert_eq!(value, &None);
			assert_eq!(mutable, &Some(false), "final → mutable: false");
		}
		other => panic!("expected Field Status.code, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// 5. Sealed interface → TraitDef.sealed
// ---------------------------------------------------------------------------

#[test]
fn sealed_interface_sets_trait_def_sealed() {
	let ext = extraction(vec![type_json(
		"com.example.Shape",
		"Shape",
		"INTERFACE",
		"com.example",
		json!({
			"modifiers": ["public", "sealed"],
			"permits": [
				declared("com.example.Shape.Circle"),
				declared("com.example.Shape.Rectangle"),
			],
			"methods": [{
				"name": "area",
				"modifiers": ["public", "abstract"],
				"params": [],
				"return": prim("double"),
				"doc": null,
				"docKind": null,
				"deprecated": false,
				"annotations": [],
				"thrown": [],
				"typeParams": [],
				"varargs": false,
				"default": false,
				"receiver": null,
				"annotationDefault": null,
				"origin": "EXPLICIT",
				"position": null,
			}],
		}),
	)]);

	let index = lower_extraction(&ext);
	match index.entries_by_path.get(&local("com.example::Shape")) {
		Some(Entry::TraitDef(sym)) => {
			let TraitDef { sealed, attributes, members, .. } = &sym.inner;
			assert_eq!(sealed, &Some(true), "sealed interface must set TraitDef.sealed");
			let attrs = attributes.as_ref().expect("sealed attributes");
			assert!(
				attrs.iter().any(|a| matches!(a, ir::protocols::TraitAttribute::Sealed)),
				"expected TraitAttribute::Sealed, got {attrs:?}"
			);
			assert!(
				attrs.iter().any(|a| matches!(
					a,
					ir::protocols::TraitAttribute::Custom { name, .. } if name == "permits"
				)),
				"expected Custom(permits), got {attrs:?}"
			);
			// Method path also indexed.
			let area = local("com.example::Shape.area");
			assert!(
				members.as_ref().is_some_and(|m| m.contains(&area)),
				"sealed interface members should include method path"
			);
		}
		other => panic!("expected TraitDef Shape, got {other:?}"),
	}
}

// ---------------------------------------------------------------------------
// Extra fidelity checks (type-use annotations, class Declared:, enum implements)
// ---------------------------------------------------------------------------

#[test]
fn type_use_annotations_survive_lowering() {
	use compiler::languages::java::schema;
	use compiler::languages::java::types::lower_type;

	let mirror = schema::TypeMirror::Declared {
		name: "java.lang.String".into(),
		args: vec![],
		owner: None,
		annotations: vec![schema::Annotation {
			ty: "org.jspecify.annotations.Nullable".into(),
			values: Default::default(),
		}],
	};
	match lower_type(&mirror) {
		IrType::TypeOperator(op) => {
			assert!(op.operator.contains("Nullable"));
			assert_eq!(*op.r#type, IrType::Primitive(ir::primitives::Primitive::String));
		}
		other => panic!("expected TypeOperator wrapper, got {other:?}"),
	}
}

#[test]
fn abstract_class_emits_declared_section() {
	let ext = extraction(vec![type_json(
		"com.example.Animal",
		"Animal",
		"CLASS",
		"com.example",
		json!({
			"modifiers": ["public", "abstract"],
			"doc": "Base animal.",
			"docKind": "TRADITIONAL",
		}),
	)]);
	let index = lower_extraction(&ext);
	match index.entries_by_path.get(&local("com.example::Animal")) {
		Some(Entry::RecordType(sym)) => {
			let doc = sym.documentation.as_deref().unwrap_or("");
			assert!(
				doc.contains("Declared: `abstract`"),
				"abstract class should emit Declared: section, got {doc:?}"
			);
		}
		other => panic!("expected RecordType Animal, got {other:?}"),
	}
}

#[test]
fn enum_implements_documented() {
	let iface = type_json(
		"com.example.Wireable",
		"Wireable",
		"INTERFACE",
		"com.example",
		json!({ "modifiers": ["public"] }),
	);
	let en = type_json(
		"com.example.Status",
		"Status",
		"ENUM",
		"com.example",
		json!({
			"modifiers": ["public"],
			"interfaces": [declared("com.example.Wireable")],
			"enumConstants": [
				{ "name": "OK", "annotations": [], "deprecated": false, "doc": null, "docKind": null, "source": null, "position": null },
			],
		}),
	);
	let index = lower_extraction(&extraction(vec![iface, en]));
	match index.entries_by_path.get(&local("com.example::Status")) {
		Some(Entry::SumType(sym)) => {
			// Structural supers / protocols on the SumType container.
			let supers = sym.inner.super_types.as_ref().expect("enum super_types");
			assert!(!supers.is_empty(), "enum implements should be structural super_types");
			let protocols = sym
				.inner
				.implemented_protocols
				.as_ref()
				.expect("enum implemented_protocols");
			assert!(
				protocols.iter().any(|p| format!("{p:?}").contains("Wireable")),
				"expected Wireable protocol path, got {protocols:?}"
			);
		}
		other => panic!("expected SumType Status, got {other:?}"),
	}
}

#[test]
fn overload_docs_are_merged() {
	let ext = extraction(vec![type_json(
		"com.example.Printer",
		"Printer",
		"CLASS",
		"com.example",
		json!({
			"modifiers": ["public"],
			"methods": [
				{
					"name": "print",
					"modifiers": ["public"],
					"params": [{
						"name": "s",
						"type": declared("java.lang.String"),
						"annotations": [],
					}],
					"return": { "kind": "void" },
					"doc": "Print a string.",
					"docKind": "TRADITIONAL",
					"deprecated": false,
					"annotations": [],
					"thrown": [],
					"typeParams": [],
					"varargs": false,
					"default": false,
					"receiver": null,
					"annotationDefault": null,
					"origin": "EXPLICIT",
					"position": null,
				},
				{
					"name": "print",
					"modifiers": ["public"],
					"params": [{
						"name": "n",
						"type": prim("int"),
						"annotations": [],
					}],
					"return": { "kind": "void" },
					"doc": "Print an integer value.",
					"docKind": "TRADITIONAL",
					"deprecated": false,
					"annotations": [],
					"thrown": [],
					"typeParams": [],
					"varargs": false,
					"default": false,
					"receiver": null,
					"annotationDefault": null,
					"origin": "EXPLICIT",
					"position": null,
				},
			],
		}),
	)]);
	let index = lower_extraction(&ext);
	match index.entries_by_path.get(&local("com.example::Printer.print")) {
		Some(Entry::Function(sym)) => {
			let doc = sym.documentation.as_deref().unwrap_or("");
			assert!(doc.contains("Print a string."), "primary overload docs kept");
			assert!(
				doc.contains("Overload 2") && doc.contains("Print an integer value."),
				"secondary overload docs must be merged, got {doc:?}"
			);
			assert!(
				sym.inner.overloads.as_ref().is_some_and(|o| o.len() == 2),
				"both signatures folded into overloads"
			);
		}
		other => panic!("expected Function print, got {other:?}"),
	}
}
