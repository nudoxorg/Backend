//! C# IR fidelity tests — pure unit tests over synthetic oracle schema JSON.
//!
//! These do **not** require a .NET SDK or the Roslyn oracle. They construct
//! minimal `schema::Extraction` documents, run `lower_extraction`, and assert
//! the critical fidelity properties from the C# IR plan (C1–C7, H6, H8).
//!
//! # Coverage
//! 1. Nested Outer/Inner: Outer.members contains Inner path (doc-id nested)
//! 2. Obsolete → `Symbol.deprecation` is `Some`
//! 3. struct vs class kind recoverable from `Declared:` docs
//! 4. Extension method flagged (`extension` note + first-param note)
//! 5. Event `TypedBinding.ty` set (standalone `Entry::Event`)
//! 6. Nullability 3-state distinguishable for string? / string / oblivious

use std::path::PathBuf;

use compiler::compile::csharp::context::lower_extraction;
use compiler::compile::csharp::schema;
use compiler::compile::csharp::types::lower_type;
use ir::entry::{Entry, NudoxPath};
use ir::kind::Deprecation;
use ir::parameter::Parameter as IrParameter;
use ir::ty::Type as IrType;
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn path_display(p: &NudoxPath) -> String {
	match p {
		NudoxPath::Local(pb) => pb.display().to_string(),
		NudoxPath::External { path, dependency } => {
			format!("{}::{}", dependency, path.display())
		}
	}
}

fn find_entry<'a>(index: &'a ir::entry::Index, needle: &str) -> Option<&'a Entry> {
	// Prefer exact name match, then path suffix match (avoids Outer matching Outer.Inner).
	if let Some((_, e)) = index.entries_by_path.iter().find(|(p, e)| {
		e.name() == needle
			&& (path_display(p).ends_with(needle)
				|| path_display(p).ends_with(&format!("::{needle}")))
	}) {
		return Some(e);
	}
	index.entries_by_path.iter().find_map(|(p, e)| {
		let pd = path_display(p);
		if e.name() == needle || pd.ends_with(needle) || pd.contains(&format!("::{needle}")) {
			Some(e)
		} else {
			None
		}
	})
}

/// Minimal CLASS type with optional nested doc-id list and members.
fn type_decl(extra: serde_json::Value) -> serde_json::Value {
	let mut base = json!({
		"docId": "T:Ns.Default",
		"qualifiedName": "Ns.Default",
		"simpleName": "Default",
		"kind": "CLASS",
		"namespace": "Ns",
		"enclosing": null,
		"modifiers": ["public"],
		"typeParams": [],
		"baseType": null,
		"interfaces": [],
		"enumUnderlying": null,
		"delegateSig": null,
		"attributes": [],
		"deprecated": null,
		"hidden": false,
		"forwarded": false,
		"doc": null,
		"docInherited": false,
		"docLinks": null,
		"extensionReceiver": null,
		"members": {
			"fields": [],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	});
	if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
		for (k, v) in extra_obj {
			base_obj.insert(k.clone(), v.clone());
		}
	}
	base
}

fn extraction(types: Vec<serde_json::Value>) -> schema::Extraction {
	let doc = json!({
		"format": 1,
		"dotnetVersion": "10.0",
		"roslyn": "test",
		"mode": "source",
		"assembly": { "name": "Test", "version": null, "tfm": null, "forwardedTypes": [], "ivt": [] },
		"diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
		"namespaces": [{ "name": "Ns", "doc": null }],
		"types": types
	});
	serde_json::from_value(doc).expect("synthetic Extraction deserializes")
}

// ===========================================================================
// C1 — Nested type parent→child via doc-id
// ===========================================================================

/// Oracle emits nested as doc-ids (`T:…`); parent.members must list the child.
#[test]
fn nested_outer_members_contains_inner_path() {
	let outer = type_decl(json!({
		"docId": "T:Ns.Outer",
		"qualifiedName": "Ns.Outer",
		"simpleName": "Outer",
		"kind": "CLASS",
		"namespace": "Ns",
		"members": {
			"fields": [],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [],
			"operators": [],
			"conversions": [],
			"indexers": [],
			// Oracle form: doc-id, not qualified metadata name.
			"nested": ["T:Ns.Outer+Inner"]
		}
	}));
	let inner = type_decl(json!({
		"docId": "T:Ns.Outer+Inner",
		"qualifiedName": "Ns.Outer+Inner",
		"simpleName": "Inner",
		"kind": "CLASS",
		"namespace": "Ns",
		"enclosing": "T:Ns.Outer",
	}));

	let index = lower_extraction(&extraction(vec![outer, inner]));

	let outer_entry = find_entry(&index, "Ns::Outer")
		.or_else(|| find_entry(&index, "Outer"))
		.expect("Outer entry present");
	let Entry::RecordType(sym) = outer_entry else {
		panic!("Outer should be RecordType, got {:?}", outer_entry.kind_tag());
	};
	let members = sym.inner.members.as_ref().expect("Outer.members must be Some");
	let member_paths: Vec<String> = members.iter().map(path_display).collect();
	assert!(
		member_paths.iter().any(|p| p.contains("Outer.Inner") || p.ends_with("Inner")),
		"Outer.members must contain Inner path; got {member_paths:?}"
	);

	// Inner itself must also exist as its own entry.
	assert!(
		find_entry(&index, "Inner").is_some(),
		"Inner type entry must be present in the index"
	);
}

// ===========================================================================
// C2 — Symbol.deprecation from [Obsolete]
// ===========================================================================

#[test]
fn obsolete_sets_symbol_deprecation() {
	let decl = type_decl(json!({
		"docId": "T:Ns.Legacy",
		"qualifiedName": "Ns.Legacy",
		"simpleName": "Legacy",
		"deprecated": {
			"message": "Use Modern instead.",
			"isError": false
		},
		"members": {
			"fields": [],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [{
				"name": "OldMethod",
				"docId": "M:Ns.Legacy.OldMethod",
				"methodKind": "Ordinary",
				"accessibility": "public",
				"isStatic": false,
				"isAbstract": false,
				"isVirtual": false,
				"isOverride": false,
				"isSealed": false,
				"isExtern": false,
				"isAsync": false,
				"isIterator": false,
				"isExtensionMethod": false,
				"isReadonly": false,
				"typeParams": [],
				"parameters": [],
				"returnType": { "kind": "named", "name": "System.Void", "args": [], "nullable": "none", "typeKind": "Struct" },
				"returnsByRef": false,
				"returnsByRefReadonly": false,
				"explicitInterface": null,
				"operatorKind": "none",
				"attributes": [],
				"deprecated": {
					"message": "gone",
					"isError": true
				},
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	}));

	let index = lower_extraction(&extraction(vec![decl]));
	let entry = find_entry(&index, "Legacy").expect("Legacy present");
	let Entry::RecordType(sym) = entry else {
		panic!("expected RecordType");
	};
	assert_eq!(
		sym.deprecation,
		Some(Deprecation {
			note: Some("Use Modern instead.".into()),
			since: None,
		}),
		"type-level Obsolete must set Symbol.deprecation"
	);

	let method = find_entry(&index, "OldMethod").expect("OldMethod present");
	let Entry::Function(msym) = method else {
		panic!("expected Function");
	};
	assert_eq!(
		msym.deprecation,
		Some(Deprecation {
			note: Some("error: gone".into()),
			since: None,
		}),
		"error-level Obsolete prefixes note with error:"
	);
}

// ===========================================================================
// C3 — struct vs class kind recoverable from docs
// ===========================================================================

#[test]
fn struct_and_class_kind_recoverable_from_declared() {
	let class = type_decl(json!({
		"docId": "T:Ns.C",
		"qualifiedName": "Ns.C",
		"simpleName": "C",
		"kind": "CLASS",
		"modifiers": ["public", "sealed"],
	}));
	let strukt = type_decl(json!({
		"docId": "T:Ns.S",
		"qualifiedName": "Ns.S",
		"simpleName": "S",
		"kind": "STRUCT",
		"modifiers": ["public", "readonly"],
	}));
	let record = type_decl(json!({
		"docId": "T:Ns.R",
		"qualifiedName": "Ns.R",
		"simpleName": "R",
		"kind": "RECORD",
		"modifiers": ["public"],
	}));
	let record_struct = type_decl(json!({
		"docId": "T:Ns.RS",
		"qualifiedName": "Ns.RS",
		"simpleName": "RS",
		"kind": "RECORD_STRUCT",
		"modifiers": ["public"],
	}));

	let index = lower_extraction(&extraction(vec![class, strukt, record, record_struct]));

	let doc_of = |name: &str| -> String {
		let e = find_entry(&index, name).unwrap_or_else(|| panic!("{name} missing"));
		e.documentation().unwrap_or("").to_string()
	};

	assert!(
		doc_of("C").contains("Declared: `sealed class`") || doc_of("C").contains("`class`"),
		"class form in docs: {}",
		doc_of("C")
	);
	assert!(
		doc_of("S").contains("`readonly struct`") || doc_of("S").contains("`struct`"),
		"struct form in docs: {}",
		doc_of("S")
	);
	assert!(
		doc_of("R").contains("`record class`") || doc_of("R").contains("record"),
		"record form in docs: {}",
		doc_of("R")
	);
	assert!(
		doc_of("RS").contains("`record struct`"),
		"record struct form in docs: {}",
		doc_of("RS")
	);
}

// ===========================================================================
// C4 — Extension method flagged
// ===========================================================================

#[test]
fn extension_method_is_flagged() {
	let decl = type_decl(json!({
		"docId": "T:Ns.StringExtensions",
		"qualifiedName": "Ns.StringExtensions",
		"simpleName": "StringExtensions",
		"kind": "CLASS",
		"modifiers": ["public", "static"],
		"members": {
			"fields": [],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [{
				"name": "Truncate",
				"docId": "M:Ns.StringExtensions.Truncate(System.String,System.Int32)",
				"methodKind": "Ordinary",
				"accessibility": "public",
				"isStatic": true,
				"isAbstract": false,
				"isVirtual": false,
				"isOverride": false,
				"isSealed": false,
				"isExtern": false,
				"isAsync": false,
				"isIterator": false,
				"isExtensionMethod": true,
				"isReadonly": false,
				"typeParams": [],
				"parameters": [
					{
						"name": "value",
						"type": { "kind": "named", "name": "System.String", "args": [], "nullable": "notAnnotated", "typeKind": "Class" },
						"refKind": "none",
						"isParams": false,
						"hasDefault": false,
						"default": null,
						"scoped": false,
						"attributes": []
					},
					{
						"name": "maxLength",
						"type": { "kind": "named", "name": "System.Int32", "args": [], "nullable": "none", "typeKind": "Struct" },
						"refKind": "none",
						"isParams": false,
						"hasDefault": false,
						"default": null,
						"scoped": false,
						"attributes": []
					}
				],
				"returnType": { "kind": "named", "name": "System.String", "args": [], "nullable": "notAnnotated", "typeKind": "Class" },
				"returnsByRef": false,
				"returnsByRefReadonly": false,
				"explicitInterface": null,
				"operatorKind": "none",
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	}));

	let index = lower_extraction(&extraction(vec![decl]));
	let method = find_entry(&index, "Truncate").expect("Truncate present");
	let Entry::Function(sym) = method else {
		panic!("expected Function");
	};
	let docs = sym.documentation.as_deref().unwrap_or("");
	assert!(
		docs.contains("extension"),
		"extension method must carry extension in Declared note; docs={docs}"
	);

	let inputs = sym.inner.input_parameters.as_ref().expect("inputs");
	let IrParameter::Literal(first) = &inputs[0] else {
		panic!("first param literal");
	};
	let desc = first.description.as_deref().unwrap_or("");
	assert!(
		desc.contains("extension"),
		"first param of extension method must be marked (extension); got {desc:?}"
	);
}

// ===========================================================================
// C5 — Event TypedBinding.ty set
// ===========================================================================

#[test]
fn event_typed_binding_ty_is_set() {
	let decl = type_decl(json!({
		"docId": "T:Ns.StatusService",
		"qualifiedName": "Ns.StatusService",
		"simpleName": "StatusService",
		"members": {
			"fields": [],
			"properties": [],
			"events": [{
				"name": "StatusChanged",
				"docId": "E:Ns.StatusService.StatusChanged",
				"type": {
					"kind": "named",
					"name": "System.EventHandler`1",
					"args": [{
						"kind": "named",
						"name": "Ns.StatusChangedEventArgs",
						"args": [],
						"nullable": "notAnnotated",
						"typeKind": "Class"
					}],
					"nullable": "annotated",
					"typeKind": "Class"
				},
				"accessibility": "public",
				"addAccessibility": null,
				"removeAccessibility": null,
				"isStatic": false,
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": "Raised on status change.",
				"docInherited": false,
				"docLinks": null
			}],
			"constructors": [],
			"methods": [],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	}));

	let index = lower_extraction(&extraction(vec![decl]));
	let event = find_entry(&index, "StatusChanged").expect("StatusChanged event entry");
	let Entry::Event(sym) = event else {
		panic!("expected Entry::Event, got {}", event.kind_tag());
	};
	assert!(
		sym.inner.ty.is_some(),
		"Event TypedBinding.ty must carry the delegate type"
	);
	// Parent members must list the event path.
	let parent = find_entry(&index, "StatusService").expect("StatusService");
	let Entry::RecordType(psym) = parent else {
		panic!("expected RecordType");
	};
	let members = psym.inner.members.as_ref().expect("members");
	assert!(
		members.iter().any(|p| path_display(p).contains("StatusChanged")),
		"parent members must list the event; got {:?}",
		members.iter().map(path_display).collect::<Vec<_>>()
	);
}

// ===========================================================================
// C6 — Nullability 3-state
// ===========================================================================

#[test]
fn nullability_three_states_distinguishable() {
	let annotated = schema::TypeSig::Named {
		name:      "System.String".into(),
		args:      vec![],
		owner:     None,
		nullable:  "annotated".into(),
		type_kind: "Class".into(),
	};
	let not_ann = schema::TypeSig::Named {
		name:      "System.String".into(),
		args:      vec![],
		owner:     None,
		nullable:  "notAnnotated".into(),
		type_kind: "Class".into(),
	};
	let oblivious = schema::TypeSig::Named {
		name:      "System.String".into(),
		args:      vec![],
		owner:     None,
		nullable:  "none".into(),
		type_kind: "Class".into(),
	};

	let a = lower_type(&annotated);
	let n = lower_type(&not_ann);
	let o = lower_type(&oblivious);

	// Named fn (not a closure) so lifetime elision ties the returned &str to `t`.
	fn type_operator(t: &IrType) -> &str {
		match t {
			IrType::TypeOperator(op) => op.operator.as_str(),
			_ => panic!("expected TypeOperator, got {t:?}"),
		}
	}

	assert_eq!(type_operator(&a), "?", "annotated → ?");
	assert_eq!(type_operator(&n), "!", "notAnnotated → !");
	assert_eq!(type_operator(&o), "~", "oblivious → ~");
	assert_ne!(type_operator(&a), type_operator(&n));
	assert_ne!(type_operator(&n), type_operator(&o));
	assert_ne!(type_operator(&a), type_operator(&o));
}

// ===========================================================================
// C7 — Visibility pairs keep Declared note
// ===========================================================================

#[test]
fn visibility_pairs_stamp_declared_accessibility() {
	let decl = type_decl(json!({
		"docId": "T:Ns.Access",
		"qualifiedName": "Ns.Access",
		"simpleName": "Access",
		"members": {
			"fields": [{
				"name": "ProtectedInternalField",
				"docId": "F:Ns.Access.ProtectedInternalField",
				"type": { "kind": "named", "name": "System.Int32", "args": [], "nullable": "none", "typeKind": "Struct" },
				"accessibility": "protectedInternal",
				"isConst": false,
				"constant": null,
				"isReadonly": false,
				"isVolatile": false,
				"isRequired": false,
				"isStatic": false,
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [{
				"name": "PrivateProtectedMethod",
				"docId": "M:Ns.Access.PrivateProtectedMethod",
				"methodKind": "Ordinary",
				"accessibility": "privateProtected",
				"isStatic": false,
				"isAbstract": false,
				"isVirtual": false,
				"isOverride": false,
				"isSealed": false,
				"isExtern": false,
				"isAsync": false,
				"isIterator": false,
				"isExtensionMethod": false,
				"isReadonly": false,
				"typeParams": [],
				"parameters": [],
				"returnType": { "kind": "named", "name": "System.Void", "args": [], "nullable": "none", "typeKind": "Struct" },
				"returnsByRef": false,
				"returnsByRefReadonly": false,
				"explicitInterface": null,
				"operatorKind": "none",
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	}));

	let index = lower_extraction(&extraction(vec![decl]));
	let parent = find_entry(&index, "Access").expect("Access");
	let Entry::RecordType(sym) = parent else {
		panic!("RecordType");
	};
	// Field documentation / decorators carry the original token.
	let field = sym
		.inner
		.fields
		.iter()
		.find_map(|f| match f {
			ir::record::Field::Known(k) => match &k.key {
				ir::record::FieldKey::Ident(n) if n.contains("ProtectedInternal") => Some(k),
				_ => None,
			},
			_ => None,
		})
		.expect("ProtectedInternalField");
	let field_docs = field.documentation.as_deref().unwrap_or("");
	let field_decs = field.attributes.decorators.join(" ");
	assert!(
		field_docs.contains("protectedInternal") || field_decs.contains("protectedInternal"),
		"field must stamp protectedInternal; docs={field_docs:?} decs={field_decs:?}"
	);

	let method = find_entry(&index, "PrivateProtectedMethod").expect("method");
	let Entry::Function(msym) = method else {
		panic!("Function");
	};
	let docs = msym.documentation.as_deref().unwrap_or("");
	assert!(
		docs.contains("privateProtected"),
		"method must stamp privateProtected; docs={docs}"
	);
}

// ===========================================================================
// H6 — special constraints prefixed csharp:
// ===========================================================================

#[test]
fn special_constraints_use_csharp_prefix() {
	use compiler::compile::csharp::types::lower_type_params;

	let params: Vec<schema::TypeParam> = serde_json::from_value(json!([
		{
			"name": "T",
			"variance": "none",
			"constraints": {
				"referenceType": true,
				"valueType": false,
				"notNull": true,
				"unmanaged": false,
				"constructor": true,
				"allowsRefLike": false,
				"types": []
			}
		}
	]))
	.expect("type params");

	let generics = lower_type_params(&params).expect("generics");
	let names: Vec<&str> = generics
		.constraints
		.iter()
		.filter_map(|c| match c {
			ir::generics::Constraint::TraitBound { trait_ref, .. } => Some(trait_ref.name.as_str()),
			_ => None,
		})
		.collect();

	assert!(
		names.iter().any(|n| *n == "csharp:class"),
		"class constraint → csharp:class; got {names:?}"
	);
	assert!(
		names.iter().any(|n| *n == "csharp:notnull"),
		"notnull → csharp:notnull; got {names:?}"
	);
	assert!(
		names.iter().any(|n| *n == "csharp:new()"),
		"new() → csharp:new(); got {names:?}"
	);
	// Must not collide with a bare interface name.
	assert!(!names.iter().any(|n| *n == "class" || *n == "notnull" || *n == "new()"));
}

// ===========================================================================
// H8 — method ref returns → BorrowedRef
// ===========================================================================

#[test]
fn ref_return_is_borrowed_ref() {
	let decl = type_decl(json!({
		"docId": "T:Ns.Box",
		"qualifiedName": "Ns.Box",
		"simpleName": "Box",
		"members": {
			"fields": [],
			"properties": [],
			"events": [],
			"constructors": [],
			"methods": [{
				"name": "GetRef",
				"docId": "M:Ns.Box.GetRef",
				"methodKind": "Ordinary",
				"accessibility": "public",
				"isStatic": false,
				"isAbstract": false,
				"isVirtual": false,
				"isOverride": false,
				"isSealed": false,
				"isExtern": false,
				"isAsync": false,
				"isIterator": false,
				"isExtensionMethod": false,
				"isReadonly": false,
				"typeParams": [],
				"parameters": [],
				"returnType": { "kind": "named", "name": "System.Int32", "args": [], "nullable": "none", "typeKind": "Struct" },
				"returnsByRef": true,
				"returnsByRefReadonly": false,
				"explicitInterface": null,
				"operatorKind": "none",
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}, {
				"name": "GetRefReadonly",
				"docId": "M:Ns.Box.GetRefReadonly",
				"methodKind": "Ordinary",
				"accessibility": "public",
				"isStatic": false,
				"isAbstract": false,
				"isVirtual": false,
				"isOverride": false,
				"isSealed": false,
				"isExtern": false,
				"isAsync": false,
				"isIterator": false,
				"isExtensionMethod": false,
				"isReadonly": false,
				"typeParams": [],
				"parameters": [],
				"returnType": { "kind": "named", "name": "System.Int32", "args": [], "nullable": "none", "typeKind": "Struct" },
				"returnsByRef": true,
				"returnsByRefReadonly": true,
				"explicitInterface": null,
				"operatorKind": "none",
				"attributes": [],
				"deprecated": null,
				"hidden": false,
				"doc": null,
				"docInherited": false,
				"docLinks": null
			}],
			"operators": [],
			"conversions": [],
			"indexers": [],
			"nested": []
		}
	}));

	let index = lower_extraction(&extraction(vec![decl]));

	let check = |name: &str, expect_mutable: bool| {
		let e = find_entry(&index, name).unwrap_or_else(|| panic!("{name}"));
		let Entry::Function(sym) = e else {
			panic!("Function");
		};
		let outs = sym.inner.output_parameters.as_ref().expect("outputs");
		let IrParameter::Literal(ret) = &outs[0] else {
			panic!("literal");
		};
		match ret.r#type.as_ref() {
			Some(IrType::BorrowedRef { is_mutable, .. }) => {
				assert_eq!(*is_mutable, expect_mutable, "{name} mutability");
			}
			other => panic!("{name}: expected BorrowedRef, got {other:?}"),
		}
	};

	check("GetRef", true);
	check("GetRefReadonly", false);
}

// ===========================================================================
// Nested path key sanity (relative_name keeps Outer.Inner)
// ===========================================================================

#[test]
fn nested_type_key_uses_dotted_spine() {
	use compiler::compile::csharp::context::{item_key, relative_name};

	let inner: schema::TypeDecl = serde_json::from_value(json!({
		"docId": "T:Ns.Outer+Inner",
		"qualifiedName": "Ns.Outer+Inner",
		"simpleName": "Inner",
		"kind": "CLASS",
		"namespace": "Ns",
		"enclosing": "Ns.Outer",
	}))
	.expect("decl");

	assert_eq!(relative_name(&inner), "Outer.Inner");
	assert_eq!(
		item_key("Ns", &relative_name(&inner)),
		NudoxPath::Local(PathBuf::from("Ns::Outer.Inner"))
	);
}
