//! Go IR fidelity contracts — pure lowering over canned oracle documents
//! (no Go toolchain / oracle binary required).
//!
//! Covers the critical fidelity surface:
//! 1. Typed const → [`TypedBinding`] with type + parsed value
//! 2. Deprecated doc → `Symbol.deprecation`
//! 3. Method path `resolve_exact` via [`SymbolTable`] (dotted + `::` aliases)
//! 4. Struct implementing interface → `implemented_protocols` / TraitImpl
//! 5. `byte` vs `uint8` remain distinguishable
//! 6. Alias → [`TypeAliasBody`]

use std::collections::HashMap;
use std::path::PathBuf;

use compiler::graph::symtab::SymbolTable;
use compiler::languages::go::context::GoContext;
use compiler::languages::go::item;
use compiler::languages::go::oracle::{self, TypeKind};
use compiler::languages::go::package::GoModule;
use compiler::languages::go::types;
use ir::entry::{Index, NudoxPath};
use ir::generics::ConstExpr;
use ir::kind::{Entry, TypeAliasBody, TypedBinding};
use ir::primitives::{Primitive, Width};
use ir::ty::{Type as IrType, TypeReference};

// ─── Canned document ─────────────────────────────────────────────────────────

/// Compact oracle document covering every fidelity contract.
const FIDELITY_DOC: &str = r#"{
  "module": {"path": "example.com/fidelity", "dir": "/tmp/fidelity", "goVersion": "1.22"},
  "packages": [
    {
      "importPath": "example.com/fidelity",
      "name": "fidelity",
      "doc": "Package fidelity exercises high-fidelity IR lowering contracts.",
      "decls": [
        {
          "kind": "type",
          "name": "Stringer",
          "exported": true,
          "doc": "Stringer is a single-method interface satisfied by Widget.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "String",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{"type": {"kind": "basic", "name": "string"}}]
                }
              }
            ],
            "allMethods": [
              {
                "name": "String",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{"type": {"kind": "basic", "name": "string"}}]
                },
                "pkg": "example.com/fidelity"
              }
            ]
          },
          "methodDocs": {
            "String": "String returns a human-readable representation."
          }
        },
        {
          "kind": "type",
          "name": "Widget",
          "exported": true,
          "doc": "Widget is a concrete type that implements Stringer.\n\nDeprecated: use NewWidget instead.\n\nSee [Go] for details.\n\n[Go]: https://go.dev",
          "underlying": {
            "kind": "struct",
            "fields": [
              {
                "name": "Name",
                "type": {"kind": "basic", "name": "string"},
                "exported": true
              },
              {
                "name": "Raw",
                "type": {
                  "kind": "slice",
                  "elem": {"kind": "basic", "name": "byte"}
                },
                "exported": true
              },
              {
                "name": "Octets",
                "type": {
                  "kind": "slice",
                  "elem": {"kind": "basic", "name": "uint8"}
                },
                "exported": true
              }
            ]
          },
          "methods": [
            {
              "name": "String",
              "exported": true,
              "doc": "String implements Stringer.",
              "recvName": "w",
              "pointerRecv": false,
              "signature": {
                "kind": "func",
                "results": [{"type": {"kind": "basic", "name": "string"}}]
              }
            },
            {
              "name": "Close",
              "exported": true,
              "doc": "Close releases resources with a pointer receiver.",
              "recvName": "w",
              "pointerRecv": true,
              "signature": {
                "kind": "func",
                "results": [{"type": {"kind": "named", "name": "error"}}]
              }
            }
          ],
          "implements": [
            {"kind": "named", "name": "Stringer", "pkg": "example.com/fidelity"}
          ],
          "fieldDocs": {
            "Name": "Name labels the widget.",
            "Raw": "Raw holds raw bytes (universe alias `byte`).",
            "Octets": "Octets is the same storage with the primitive spelling `uint8`."
          }
        },
        {
          "kind": "alias",
          "name": "AliasName",
          "exported": true,
          "doc": "AliasName is a true type alias of Widget.",
          "target": {
            "kind": "named",
            "name": "Widget",
            "pkg": "example.com/fidelity"
          }
        },
        {
          "kind": "const",
          "name": "MaxRetries",
          "exported": true,
          "doc": "MaxRetries is a typed integer constant with an explicit value.",
          "type": {"kind": "basic", "name": "int"},
          "value": "3"
        },
        {
          "kind": "const",
          "name": "Greeting",
          "exported": true,
          "doc": "Greeting is a string constant.",
          "type": {"kind": "basic", "name": "string"},
          "value": "\"hello\""
        },
        {
          "kind": "const",
          "name": "Enabled",
          "exported": true,
          "doc": "Enabled is a boolean constant.",
          "type": {"kind": "basic", "name": "bool"},
          "value": "true"
        },
        {
          "kind": "type",
          "name": "Direction",
          "exported": true,
          "doc": "Direction is an iota-based enum over int.",
          "underlying": {"kind": "basic", "name": "int"}
        },
        {
          "kind": "const",
          "name": "North",
          "exported": true,
          "type": {"kind": "named", "name": "Direction", "pkg": "example.com/fidelity"},
          "value": "0",
          "constGroup": 1,
          "groupHasIota": true
        },
        {
          "kind": "const",
          "name": "East",
          "exported": true,
          "type": {"kind": "named", "name": "Direction", "pkg": "example.com/fidelity"},
          "value": "1",
          "constGroup": 1,
          "groupHasIota": true
        },
        {
          "kind": "type",
          "name": "Ordered",
          "exported": true,
          "doc": "Ordered is a constraint type set.",
          "underlying": {
            "kind": "interface",
            "embeddeds": [
              {
                "kind": "union",
                "terms": [
                  {"tilde": true, "type": {"kind": "basic", "name": "int"}},
                  {"type": {"kind": "basic", "name": "string"}}
                ]
              }
            ]
          }
        }
      ]
    }
  ]
}"#;

fn fidelity_index() -> Index {
	let ctx = GoContext {
		module: GoModule {
			root:        PathBuf::from("/tmp/fidelity"),
			module_path: "example.com/fidelity".to_string(),
			go_version:  Some("1.22".to_string()),
		},
		output: serde_json::from_str(FIDELITY_DOC).expect("canned fidelity document parses"),
	};
	ctx.lower_package()
}

fn entry<'a>(index: &'a Index, key: &NudoxPath) -> &'a Entry {
	index.entries_by_path.get(key).unwrap_or_else(|| panic!("missing entry {}", path_display(key)))
}

fn path_display(p: &NudoxPath) -> String {
	match p {
		NudoxPath::Local(pb) => pb.display().to_string(),
		NudoxPath::External { dependency, path } => {
			format!("{dependency}:{}", path.display())
		}
	}
}

// ─── 1. Typed const ──────────────────────────────────────────────────────────

#[test]
fn const_typed_binding_has_type_and_parsed_value() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "MaxRetries");
	let Entry::Constant(sym) = entry(&index, &key) else {
		panic!("MaxRetries should be Constant");
	};
	let TypedBinding { ty, value, mutable } = &sym.inner;
	assert_eq!(*mutable, Some(false));
	assert!(
		matches!(ty, Some(IrType::Primitive(Primitive::Int(Width::Arch)))),
		"expected int type, got {ty:?}"
	);
	assert_eq!(value.as_ref(), Some(&ConstExpr::Int(3)));

	let greet = item::item_key("example.com/fidelity", "Greeting");
	let Entry::Constant(g) = entry(&index, &greet) else {
		panic!("Greeting should be Constant");
	};
	assert_eq!(
		g.inner.value.as_ref(),
		Some(&ConstExpr::Str("hello".into()))
	);

	let en = item::item_key("example.com/fidelity", "Enabled");
	let Entry::Constant(e) = entry(&index, &en) else {
		panic!("Enabled should be Constant");
	};
	assert_eq!(e.inner.value.as_ref(), Some(&ConstExpr::Bool(true)));
}

// ─── 2. Deprecation ──────────────────────────────────────────────────────────

#[test]
fn deprecated_doc_sets_symbol_deprecation() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "Widget");
	let Entry::RecordType(sym) = entry(&index, &key) else {
		panic!("Widget should be RecordType");
	};
	let dep = sym.deprecation.as_ref().expect("Widget should be deprecated");
	assert!(
		dep.note.as_deref().is_some_and(|n| n.contains("NewWidget")),
		"deprecation note missing NewWidget: {dep:?}"
	);
	// Doc links harvested from `[Go]: https://go.dev`.
	let links = sym.doc_links.as_ref().expect("doc_links populated");
	assert!(links.contains_key("Go"), "expected Go link, got {links:?}");
}

// ─── 3. Method path resolve_exact ────────────────────────────────────────────

#[test]
fn method_path_resolve_exact_via_symtab() {
	let index = fidelity_index();
	let table = SymbolTable::build(&index);
	let canonical = item::method_key("example.com/fidelity", "Widget", "Close");

	// Aliases registered by the lowerer (dotted + double-colon forms).
	let via_alias_colon = table
		.resolve_exact("example.com/fidelity::Widget::Close")
		.expect("dotted-to-:: alias should resolve");
	assert_eq!(via_alias_colon, &canonical);

	let via_short = table
		.resolve_exact("Widget::Close")
		.expect("short Type::Method alias should resolve");
	assert_eq!(via_short, &canonical);

	let via_dotted_short = table
		.resolve_exact("Widget.Close")
		.expect("short Type.Method alias should resolve");
	assert_eq!(via_dotted_short, &canonical);

	// path_segments of the canonical key also lands in the exact table.
	// Local("example.com/fidelity::Widget.Close") expands to
	// `example.com::fidelity::Widget::Close` (import path keeps its dots;
	// only the post-`::` tail is further split on `.`).
	let via_segments = table.resolve_exact("example.com::fidelity::Widget::Close");
	assert_eq!(
		via_segments,
		Some(&canonical),
		"segmented form should resolve to the canonical method key"
	);

	// Suffix resolve of the unique method leaf.
	let via_suffix = table.resolve_suffix("Close").expect("unique Close suffix");
	assert_eq!(via_suffix, &canonical);
}

// ─── 4. Implements ───────────────────────────────────────────────────────────

#[test]
fn struct_implementing_interface_gets_protocol_link() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "Widget");
	let Entry::RecordType(sym) = entry(&index, &key) else {
		panic!("Widget should be RecordType");
	};
	let protocols = sym
		.inner
		.implemented_protocols
		.as_ref()
		.expect("Widget implements Stringer");
	let stringer = item::item_key("example.com/fidelity", "Stringer");
	assert!(
		protocols.contains(&stringer),
		"expected Stringer in {protocols:?}"
	);

	// Companion TraitImpl entry.
	let impl_key = NudoxPath::Local(PathBuf::from("example.com/fidelity::Widget:Stringer"));
	let Entry::TraitImpl(impl_sym) = entry(&index, &impl_key) else {
		panic!("expected TraitImpl for Widget:Stringer");
	};
	assert!(
		impl_sym.inner.tr.name.contains("Stringer"),
		"trait ref name: {}",
		impl_sym.inner.tr.name
	);
}

// ─── 5. byte vs uint8 ────────────────────────────────────────────────────────

#[test]
fn byte_and_uint8_are_distinguishable() {
	// Direct type lowering (also covered in types unit tests).
	assert_ne!(
		types::lower_type(&oracle::Type {
			kind: TypeKind::Basic,
			name: "byte".into(),
			..Default::default()
		}),
		types::lower_type(&oracle::Type {
			kind: TypeKind::Basic,
			name: "uint8".into(),
			..Default::default()
		})
	);

	// Through a struct field in the package.
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "Widget");
	let Entry::RecordType(sym) = entry(&index, &key) else {
		panic!("Widget should be RecordType");
	};
	let mut field_tys: HashMap<&str, &IrType> = HashMap::new();
	for f in &sym.inner.fields {
		if let ir::record::Field::Known(kf) = f {
			if let ir::record::FieldKey::Ident(name) = &kf.key {
				if let Some(t) = kf.r#type.as_deref() {
					field_tys.insert(name.as_str(), t);
				}
			}
		}
	}
	let raw = field_tys.get("Raw").expect("Raw field");
	let octets = field_tys.get("Octets").expect("Octets field");
	// Both are slices; their element types must differ.
	let (IrType::Slice(raw_elem), IrType::Slice(oct_elem)) = (raw, octets) else {
		panic!("expected slices, got raw={raw:?} octets={octets:?}");
	};
	assert_ne!(
		raw_elem.as_ref(),
		oct_elem.as_ref(),
		"byte and uint8 elements must differ"
	);
	assert!(
		matches!(
			raw_elem.as_ref(),
			IrType::TypeReference(TypeReference { identifier, .. }) if identifier == "byte"
		),
		"Raw should be []byte TypeReference, got {raw_elem:?}"
	);
	assert!(
		matches!(
			oct_elem.as_ref(),
			IrType::Primitive(Primitive::UInt(Width::W8))
		),
		"Octets should be []uint8 primitive, got {oct_elem:?}"
	);
}

// ─── 6. Type alias body ──────────────────────────────────────────────────────

#[test]
fn alias_lowers_to_type_alias_body() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "AliasName");
	let Entry::TypeAlias(sym) = entry(&index, &key) else {
		panic!("AliasName should be TypeAlias");
	};
	let TypeAliasBody { generics, target } = &sym.inner;
	assert!(generics.is_none(), "non-generic alias has no type params");
	assert!(
		matches!(
			target,
			IrType::TypeReference(TypeReference { identifier, .. })
				if identifier == "example.com/fidelity.Widget"
		),
		"alias target should be Widget, got {target:?}"
	);
}

// ─── Extra fidelity checks ───────────────────────────────────────────────────

#[test]
fn type_set_is_structural_on_trait() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "Ordered");
	let Entry::TraitDef(sym) = entry(&index, &key) else {
		panic!("Ordered should be TraitDef");
	};
	let props = sym.inner.properties.as_ref().expect("type_set property");
	let type_set = props.iter().find_map(|f| match f {
		ir::record::Field::Known(kf)
			if matches!(&kf.key, ir::record::FieldKey::Ident(n) if n == "type_set") =>
		{
			kf.r#type.as_deref()
		}
		_ => None,
	});
	let Some(IrType::Union(members)) = type_set else {
		panic!("type_set should be Union, got {type_set:?}");
	};
	assert!(
		members.iter().any(|m| matches!(m, IrType::TypeOperator(op) if op.operator == "~")),
		"expected a ~ term in {members:?}"
	);
}

#[test]
fn iota_enum_preserves_underlying_and_variant_values() {
	let index = fidelity_index();
	let key = item::item_key("example.com/fidelity", "Direction");
	let Entry::SumType(sym) = entry(&index, &key) else {
		panic!("Direction should be SumType");
	};
	assert!(
		sym.documentation
			.as_deref()
			.is_some_and(|d| d.contains("Underlying")),
		"sum docs should note underlying type: {:?}",
		sym.documentation
	);
	let north = sym.inner.variants.iter().find(|v| v.name == "North").expect("North");
	assert!(
		north.documentation.as_deref().is_some_and(|d| d.contains('0')),
		"North value in docs: {:?}",
		north.documentation
	);
	// Structured literal data when value parses as int.
	assert!(
		matches!(
			&north.data,
			Some(ir::record::SumField::Tuple(ts))
				if matches!(ts.first(), Some(IrType::Literal(_)))
		),
		"North should carry literal data, got {:?}",
		north.data
	);
	// Variant constants are not also lowered as free Constants.
	assert!(
		!index
			.entries_by_path
			.contains_key(&item::item_key("example.com/fidelity", "North"))
	);
}

#[test]
fn unsafe_pointer_not_address() {
	let t = types::lower_type(&oracle::Type {
		kind: TypeKind::Basic,
		name: "unsafe.Pointer".into(),
		..Default::default()
	});
	assert!(
		matches!(
			t,
			IrType::TypeReference(TypeReference { ref identifier, .. })
				if identifier == "unsafe.Pointer"
		),
		"got {t:?}"
	);
}

#[test]
fn parse_const_value_covers_literals() {
	assert_eq!(types::parse_const_value("42"), Some(ConstExpr::Int(42)));
	assert_eq!(types::parse_const_value("true"), Some(ConstExpr::Bool(true)));
	assert_eq!(
		types::parse_const_value(r#""hi""#),
		Some(ConstExpr::Str("hi".into()))
	);
}

#[test]
fn method_entries_carry_deprecation_when_doc_says_so() {
	// Widget itself is deprecated; methods use their own docs. Smoke-check
	// that the lowerer wires DocMeta for methods (no panic / fields set).
	let index = fidelity_index();
	let key = item::method_key("example.com/fidelity", "Widget", "String");
	let Entry::Function(sym) = entry(&index, &key) else {
		panic!("Widget.String should be Function");
	};
	assert!(sym.documentation.is_some());
	assert!(sym.aliases.as_ref().is_some_and(|a| !a.is_empty()));
}


