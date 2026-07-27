//! Unit tests for the Go producer lowering.
//!
//! These tests are fully self-contained: they embed literal JSON fixtures,
//! deserialize them into `oracle::Output`, lower them, and assert on the
//! resulting IR entries.  No Go toolchain or subprocess is required.

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    kinds::{ty::Type, Alias, Const, Enum, Field, Function, Record, Static, Trait},
    package::PackageId,
};

use crate::{lower::GoId, oracle, producer::GoProducer};

// ---------------------------------------------------------------------------
// Fixture JSON
// ---------------------------------------------------------------------------

/// A compact Go package that exercises:
///   - A struct with fields and methods.
///   - An interface (Trait in IR).
///   - A generic function.
///   - An iota const block (Enum in IR).
///   - A package-level var and a regular const.
///   - A true alias.
const FIXTURE_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/shapes",
      "name": "shapes",
      "doc": "Package shapes provides geometric primitives.",
      "decls": [
        {
          "kind": "type",
          "name": "Shape",
          "exported": true,
          "doc": "Shape is the base interface.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Area",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "float64" } }]
                }
              }
            ],
            "allMethods": [
              {
                "name": "Area",
                "exported": true,
                "signature": {
                  "kind": "func",
                  "results": [{ "name": "", "type": { "kind": "basic", "name": "float64" } }]
                }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Rect",
          "exported": true,
          "doc": "Rect is a rectangle.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "Width", "exported": true, "type": { "kind": "basic", "name": "float64" } },
              { "name": "Height", "exported": true, "type": { "kind": "basic", "name": "float64" } }
            ]
          },
          "methods": [
            {
              "name": "Area",
              "exported": true,
              "doc": "Area returns the area of the rectangle.",
              "pointerRecv": false,
              "signature": {
                "kind": "func",
                "results": [{ "name": "", "type": { "kind": "basic", "name": "float64" } }]
              }
            }
          ],
          "fieldDocs": { "Width": "Width in pixels.", "Height": "Height in pixels." }
        },
        {
          "kind": "func",
          "name": "Scale",
          "exported": true,
          "doc": "Scale scales a shape by a factor.",
          "typeParams": [
            { "name": "S", "constraint": { "kind": "named", "pkg": "example.com/m/shapes", "name": "Shape" } }
          ],
          "signature": {
            "kind": "func",
            "params": [
              { "name": "s", "type": { "kind": "typeParam", "name": "S" } },
              { "name": "factor", "type": { "kind": "basic", "name": "float64" } }
            ],
            "results": [
              { "name": "", "type": { "kind": "basic", "name": "float64" } }
            ]
          }
        },
        {
          "kind": "const",
          "name": "Pi",
          "exported": true,
          "doc": "Pi is the mathematical constant.",
          "type": { "kind": "basic", "name": "float64" },
          "value": "3.14159265358979323846264338327950288",
          "constGroup": 1,
          "groupHasIota": false
        },
        {
          "kind": "var",
          "name": "DefaultShape",
          "exported": true,
          "doc": "DefaultShape is the default shape.",
          "type": { "kind": "named", "pkg": "example.com/m/shapes", "name": "Rect" }
        },
        {
          "kind": "type",
          "name": "Color",
          "exported": true,
          "doc": "Color is an iota enum.",
          "underlying": { "kind": "basic", "name": "int" }
        },
        {
          "kind": "const",
          "name": "Red",
          "exported": true,
          "doc": "Red is the red color.",
          "type": { "kind": "named", "pkg": "example.com/m/shapes", "name": "Color" },
          "value": "0",
          "constGroup": 2,
          "groupHasIota": true
        },
        {
          "kind": "const",
          "name": "Green",
          "exported": true,
          "doc": "Green is the green color.",
          "type": { "kind": "named", "pkg": "example.com/m/shapes", "name": "Color" },
          "value": "1",
          "constGroup": 2,
          "groupHasIota": true
        },
        {
          "kind": "alias",
          "name": "Point",
          "exported": true,
          "doc": "Point is an alias for Rect.",
          "target": { "kind": "named", "pkg": "example.com/m/shapes", "name": "Rect" }
        }
      ]
    }
  ]
}
"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("go"), PackageName::new("example.com/m"))
}

fn lower_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            FIXTURE_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("fixture must lower without error")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn fixture_deserializes() {
    let out: oracle::Output = serde_json::from_str(FIXTURE_JSON).expect("valid JSON");
    assert_eq!(out.packages.len(), 1);
    assert_eq!(out.packages[0].import_path, "example.com/m/shapes");
}

#[test]
fn package_module_declared() {
    let pkg = lower_fixture();
    // There must be an entry named "example.com/m/shapes" (the Go package module).
    let found = pkg
        .iter()
        .any(|(_, e)| e.sym().name == "example.com/m/shapes");
    assert!(found, "package module entry must be declared");
}

#[test]
fn struct_rect_declared() {
    let pkg = lower_fixture();
    let rect = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Rect")
        .expect("Rect must be declared");
    assert!(
        rect.1.downcast::<Record>().is_some(),
        "Rect must be a Record"
    );
}

#[test]
fn struct_fields_declared() {
    let pkg = lower_fixture();
    // Width and Height must be Field entries.
    let width = pkg.iter().find(|(_, e)| e.sym().name == "Width");
    let height = pkg.iter().find(|(_, e)| e.sym().name == "Height");
    assert!(width.is_some(), "Width field must be declared");
    assert!(height.is_some(), "Height field must be declared");
    assert!(
        width.unwrap().1.downcast::<Field>().is_some(),
        "Width must be a Field"
    );
    assert!(
        height.unwrap().1.downcast::<Field>().is_some(),
        "Height must be a Field"
    );
}

/// The fixture declares `Area` twice: once as a method on the `Rect` struct
/// (value receiver) and once as a member of the `Shape` interface (no
/// receiver — an interface method is a requirement, not a bound method). So
/// this asserts on the *set*, which also pins that the two are kept as
/// distinct declarations rather than merged.
#[test]
fn method_declared_with_receiver() {
    use nudox_ir::kinds::function::Receiver;

    let pkg = lower_fixture();
    let receivers: Vec<Option<Receiver>> = pkg
        .iter()
        .filter(|(_, e)| e.sym().name == "Area")
        .map(|(_, e)| {
            e.downcast::<Function>()
                .expect("every Area must be a Function")
                .body()
                .receiver
        })
        .collect();

    assert!(
        receivers.contains(&Some(Receiver::Owned)),
        "the Rect.Area value-receiver method must lower to Receiver::Owned, got {receivers:?}"
    );
    assert!(
        receivers.contains(&None),
        "the Shape.Area interface requirement must carry no receiver, got {receivers:?}"
    );
}

#[test]
fn method_has_output_param() {
    let pkg = lower_fixture();
    let area = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Area")
        .expect("Area method must be declared");
    let fn_body = area
        .1
        .downcast::<Function>()
        .expect("Area must be a Function");
    // Area returns float64 — one output param.
    assert_eq!(
        fn_body.body().output_params.len(),
        1,
        "Area must have exactly one output param (float64)"
    );
}

#[test]
fn interface_shape_is_trait() {
    let pkg = lower_fixture();
    let shape = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Shape")
        .expect("Shape must be declared");
    assert!(
        shape.1.downcast::<Trait>().is_some(),
        "Shape (interface) must lower to Trait"
    );
}

#[test]
fn iota_enum_color_declared() {
    let pkg = lower_fixture();
    let color = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Color")
        .expect("Color must be declared");
    assert!(
        color.1.downcast::<Enum>().is_some(),
        "Color (iota type) must lower to Enum"
    );
}

#[test]
fn iota_enum_variants_declared() {
    let pkg = lower_fixture();
    // Red and Green must be Variant entries.
    let red = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Red")
        .expect("Red variant must be declared");
    let green = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Green")
        .expect("Green variant must be declared");

    use nudox_ir::kinds::Variant;
    assert!(
        red.1.downcast::<Variant>().is_some(),
        "Red must be a Variant"
    );
    assert!(
        green.1.downcast::<Variant>().is_some(),
        "Green must be a Variant"
    );

    // Discriminants preserved.
    assert_eq!(
        red.1.downcast::<Variant>().unwrap().body().discr.as_deref(),
        Some("0")
    );
    assert_eq!(
        green
            .1
            .downcast::<Variant>()
            .unwrap()
            .body()
            .discr
            .as_deref(),
        Some("1")
    );
}

#[test]
fn generic_func_scale_declared() {
    let pkg = lower_fixture();
    let scale = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Scale")
        .expect("Scale must be declared");
    let fn_body = scale
        .1
        .downcast::<Function>()
        .expect("Scale must be a Function");
    // Scale has one type param `S`.
    assert_eq!(
        fn_body.body().generics.len(),
        1,
        "Scale must have exactly one generic parameter"
    );
}

#[test]
fn generic_func_scale_multi_input_output() {
    let pkg = lower_fixture();
    let scale = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Scale")
        .expect("Scale must be declared");
    let fn_body = scale
        .1
        .downcast::<Function>()
        .expect("Scale must be a Function");
    // Scale(s S, factor float64) float64 → 2 inputs, 1 output.
    assert_eq!(
        fn_body.body().input_params.len(),
        2,
        "Scale must have 2 input params"
    );
    assert_eq!(
        fn_body.body().output_params.len(),
        1,
        "Scale must have 1 output param"
    );
}

#[test]
fn const_pi_declared() {
    let pkg = lower_fixture();
    let pi = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Pi")
        .expect("Pi must be declared");
    assert!(pi.1.downcast::<Const>().is_some(), "Pi must be a Const");
    assert_eq!(
        pi.1.downcast::<Const>().unwrap().body().value.as_deref(),
        Some("3.14159265358979323846264338327950288")
    );
}

#[test]
fn var_default_shape_declared() {
    let pkg = lower_fixture();
    let ds = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "DefaultShape")
        .expect("DefaultShape must be declared");
    assert!(
        ds.1.downcast::<Static>().is_some(),
        "DefaultShape (var) must lower to Static"
    );
    assert!(
        ds.1.downcast::<Static>().unwrap().body().mutable,
        "package-level var must be mutable"
    );
}

#[test]
fn alias_point_declared() {
    let pkg = lower_fixture();
    let point = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Point")
        .expect("Point (alias) must be declared");
    assert!(
        point.1.downcast::<Alias>().is_some(),
        "Point must lower to Alias"
    );
}

#[test]
fn method_parent_is_rect() {
    let pkg = lower_fixture();
    // Check that Rect has at least 3 children: Width, Height, Area.
    // (Direct parent-ref comparison requires internal index knowledge; checking
    // child count via Rect is the safe approach given the public API.)
    let rect = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Rect")
        .expect("Rect must be declared");
    // Rect should have children: Width, Height, Area (at minimum).
    assert!(
        rect.1.children().len() >= 3,
        "Rect must have at least 3 children (Width, Height, Area), got {}",
        rect.1.children().len()
    );
}

// ---------------------------------------------------------------------------
// Item 1: Named types lower to Nominal, not Any
// ---------------------------------------------------------------------------

/// A struct with a field whose type is another declared struct.  Before the
/// fix, the field type was always `Type::Any`; now it must be `Type::Nominal`.
const NOMINAL_FIELD_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/geo",
      "name": "geo",
      "doc": "Package geo.",
      "decls": [
        {
          "kind": "type",
          "name": "Point",
          "exported": true,
          "doc": "Point is a 2D point.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "X", "exported": true, "type": { "kind": "basic", "name": "float64" } },
              { "name": "Y", "exported": true, "type": { "kind": "basic", "name": "float64" } }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Rect",
          "exported": true,
          "doc": "Rect has two Point fields.",
          "underlying": {
            "kind": "struct",
            "fields": [
              {
                "name": "TopLeft",
                "exported": true,
                "type": { "kind": "named", "pkg": "example.com/m/geo", "name": "Point" }
              },
              {
                "name": "BottomRight",
                "exported": true,
                "type": { "kind": "named", "pkg": "example.com/m/geo", "name": "Point" }
              }
            ]
          }
        }
      ]
    }
  ]
}
"#;

fn lower_nominal_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            NOMINAL_FIELD_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("nominal fixture must lower without error")
}

/// The key regression test: `TopLeft` must lower to `Type::Nominal`, not `Any`.
#[test]
fn struct_field_named_type_is_nominal_not_any() {
    let pkg = lower_nominal_fixture();
    let top_left = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "TopLeft")
        .expect("TopLeft field must be declared");
    let field_body = top_left
        .1
        .downcast::<Field>()
        .expect("TopLeft must be a Field");
    match field_body.body().ty.as_ref() {
        Some(Type::Nominal(_)) => {}
        other => panic!(
            "TopLeft's type must be Type::Nominal (a named ref to Point), got {other:?}"
        ),
    }
}

/// Mirror check: primitive-typed fields must still lower to a Primitive, not
/// accidentally become Nominal.
#[test]
fn struct_field_primitive_type_stays_primitive() {
    let pkg = lower_nominal_fixture();
    let x = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "X")
        .expect("X field must be declared");
    let field_body = x.1.downcast::<Field>().expect("X must be a Field");
    match field_body.body().ty.as_ref() {
        Some(Type::Primitive(_)) => {}
        other => panic!("X's type must be Type::Primitive(Float), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Item 2: TypeParam lowers to TypeVar, not SelfType
// ---------------------------------------------------------------------------

const GENERIC_STRUCT_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/coll",
      "name": "coll",
      "doc": "Package coll.",
      "decls": [
        {
          "kind": "type",
          "name": "Box",
          "exported": true,
          "doc": "Box is a generic container.",
          "typeParams": [
            { "name": "T", "constraint": { "kind": "interface" } }
          ],
          "underlying": {
            "kind": "struct",
            "fields": [
              {
                "name": "Value",
                "exported": true,
                "type": { "kind": "typeParam", "name": "T" }
              }
            ]
          }
        }
      ]
    }
  ]
}
"#;

fn lower_generic_struct_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            GENERIC_STRUCT_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("generic struct fixture must lower without error")
}

#[test]
fn type_param_use_lowers_to_typevar_not_self_type() {
    let pkg = lower_generic_struct_fixture();
    let value_field = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Value")
        .expect("Value field must be declared");
    let field_body = value_field
        .1
        .downcast::<Field>()
        .expect("Value must be a Field");
    match field_body.body().ty.as_ref() {
        Some(Type::TypeVar(name)) => {
            assert_eq!(name, "T", "TypeVar name must match the type param name");
        }
        other => panic!(
            "Value's type (a TypeParam use) must be Type::TypeVar, not {:?}",
            other
        ),
    }
}

// ---------------------------------------------------------------------------
// Item 3: implements facts → super_types
// ---------------------------------------------------------------------------

const IMPLEMENTS_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/shapes",
      "name": "shapes",
      "doc": "Package shapes.",
      "decls": [
        {
          "kind": "type",
          "name": "Shape",
          "exported": true,
          "doc": "Shape is an interface.",
          "underlying": {
            "kind": "interface",
            "explicitMethods": [
              {
                "name": "Area",
                "exported": true,
                "signature": { "kind": "func", "results": [{ "type": { "kind": "basic", "name": "float64" } }] }
              }
            ],
            "allMethods": [
              {
                "name": "Area",
                "exported": true,
                "signature": { "kind": "func", "results": [{ "type": { "kind": "basic", "name": "float64" } }] }
              }
            ]
          }
        },
        {
          "kind": "type",
          "name": "Circle",
          "exported": true,
          "doc": "Circle implements Shape.",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "Radius", "exported": true, "type": { "kind": "basic", "name": "float64" } }
            ]
          },
          "implements": [
            { "kind": "named", "pkg": "example.com/m/shapes", "name": "Shape" }
          ]
        }
      ]
    }
  ]
}
"#;

fn lower_implements_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            IMPLEMENTS_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("implements fixture must lower without error")
}

#[test]
fn implements_populates_super_types() {
    let pkg = lower_implements_fixture();
    let circle = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Circle")
        .expect("Circle must be declared");
    let record_body = circle
        .1
        .downcast::<Record>()
        .expect("Circle must be a Record");
    let super_types = &record_body.body().super_types;
    assert_eq!(
        super_types.len(),
        1,
        "Circle must have exactly 1 super_type (Shape), got {}",
        super_types.len()
    );
    // The super_type must be a Nominal ref, not Any.
    assert!(
        matches!(super_types[0], Type::Nominal(_)),
        "super_type must be Type::Nominal(Shape ref), got {:?}",
        super_types[0]
    );
}

// ---------------------------------------------------------------------------
// Item 4: Deprecation is parsed from doc strings
// ---------------------------------------------------------------------------

const DEPRECATED_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/old",
      "name": "old",
      "doc": "Package old.",
      "decls": [
        {
          "kind": "func",
          "name": "OldFunc",
          "exported": true,
          "doc": "OldFunc does a thing.\n\nDeprecated: use NewFunc instead.",
          "signature": { "kind": "func" }
        },
        {
          "kind": "func",
          "name": "NewFunc",
          "exported": true,
          "doc": "NewFunc does the thing better.",
          "signature": { "kind": "func" }
        }
      ]
    }
  ]
}
"#;

fn lower_deprecated_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            DEPRECATED_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("deprecated fixture must lower without error")
}

#[test]
fn deprecated_doc_populates_deprecation() {
    let pkg = lower_deprecated_fixture();
    let old_func = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "OldFunc")
        .expect("OldFunc must be declared");
    let dep = old_func.1.sym().deprecation.as_ref();
    assert!(
        dep.is_some(),
        "OldFunc must have a Deprecation; doc contains 'Deprecated: use NewFunc instead.'"
    );
    let note = dep.unwrap().note.as_deref().unwrap_or("");
    assert!(
        note.contains("NewFunc"),
        "Deprecation note must include the reason, got: {note:?}"
    );
}

#[test]
fn non_deprecated_func_has_no_deprecation() {
    let pkg = lower_deprecated_fixture();
    let new_func = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "NewFunc")
        .expect("NewFunc must be declared");
    assert!(
        new_func.1.sym().deprecation.is_none(),
        "NewFunc must not be deprecated"
    );
}

// ---------------------------------------------------------------------------
// Item 5: Struct field tags and embedded markers
// ---------------------------------------------------------------------------

const TAGS_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/data",
      "name": "data",
      "doc": "Package data.",
      "decls": [
        {
          "kind": "type",
          "name": "Base",
          "exported": true,
          "doc": "Base is embedded.",
          "underlying": { "kind": "struct", "fields": [] }
        },
        {
          "kind": "type",
          "name": "User",
          "exported": true,
          "doc": "User has tags and an embedded field.",
          "underlying": {
            "kind": "struct",
            "fields": [
              {
                "name": "Name",
                "exported": true,
                "tag": "json:\"name,omitempty\"",
                "embedded": false,
                "type": { "kind": "basic", "name": "string" }
              },
              {
                "name": "Base",
                "exported": true,
                "tag": "",
                "embedded": true,
                "type": { "kind": "named", "pkg": "example.com/m/data", "name": "Base" }
              }
            ]
          }
        }
      ]
    }
  ]
}
"#;

fn lower_tags_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            TAGS_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("tags fixture must lower without error")
}

#[test]
fn struct_field_tag_stored_in_attrs() {
    let pkg = lower_tags_fixture();
    // Find the Name field on User (there's also a Name field potentially
    // on Base if any — use parent scoping through children count).
    // Strategy: look for a Field with an attr token "tag".
    let name_field = pkg
        .iter()
        .find(|(_, e)| {
            e.sym().name == "Name"
                && e.downcast::<Field>().is_some()
                && e.sym().attrs.iter().any(|a| a.token == "tag")
        })
        .expect("Name field with tag attr must be declared");
    let tag_attr = name_field
        .1
        .sym()
        .attrs
        .iter()
        .find(|a| a.token == "tag")
        .expect("tag attr must exist");
    assert_eq!(
        tag_attr.arg.as_deref(),
        Some("json:\"name,omitempty\""),
        "tag arg must be the raw struct tag"
    );
}

#[test]
fn embedded_field_marked_in_attrs() {
    let pkg = lower_tags_fixture();
    // The embedded Base field on User must carry an AttrTok { token: "embedded" }.
    // There are two "Base" entries: the type itself and the embedded field.
    // The embedded field has downcast::<Field>().is_some().
    let embedded_field = pkg
        .iter()
        .find(|(_, e)| {
            e.sym().name == "Base"
                && e.downcast::<Field>().is_some()
                && e.sym().attrs.iter().any(|a| a.token == "embedded")
        })
        .expect("Base embedded field must have embedded attr");
    let _ = embedded_field; // assertion is in the find predicate
}

// ---------------------------------------------------------------------------
// Item 7a: IsComparable → AttrTok on interface
// ---------------------------------------------------------------------------

const COMPARABLE_JSON: &str = r#"
{
  "module": { "path": "example.com/m", "dir": "/tmp/m", "goVersion": "1.22" },
  "packages": [
    {
      "importPath": "example.com/m/cmp",
      "name": "cmp",
      "doc": "Package cmp.",
      "decls": [
        {
          "kind": "type",
          "name": "Keyed",
          "exported": true,
          "doc": "Keyed requires comparable.",
          "underlying": {
            "kind": "interface",
            "isComparable": true,
            "explicitMethods": [],
            "allMethods": []
          }
        },
        {
          "kind": "type",
          "name": "Open",
          "exported": true,
          "doc": "Open is a plain interface.",
          "underlying": {
            "kind": "interface",
            "isComparable": false,
            "explicitMethods": [
              {
                "name": "Do",
                "exported": true,
                "signature": { "kind": "func" }
              }
            ],
            "allMethods": [
              {
                "name": "Do",
                "exported": true,
                "signature": { "kind": "func" }
              }
            ]
          }
        }
      ]
    }
  ]
}
"#;

fn lower_comparable_fixture() -> nudox_ir::package::IrPackage<GoId> {
    let producer = GoProducer;
    producer
        .lower_bytes(
            COMPARABLE_JSON.as_bytes(),
            PackageId::path("example.com/m"),
            &lineage(),
        )
        .expect("comparable fixture must lower without error")
}

#[test]
fn is_comparable_surfaces_as_attr_tok() {
    let pkg = lower_comparable_fixture();
    let keyed = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Keyed")
        .expect("Keyed interface must be declared");
    assert!(
        keyed.1.sym().attrs.iter().any(|a| a.token == "comparable"),
        "Keyed (isComparable=true) must have AttrTok {{token: \"comparable\"}}"
    );
}

#[test]
fn non_comparable_interface_has_no_comparable_attr() {
    let pkg = lower_comparable_fixture();
    let open = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Open")
        .expect("Open interface must be declared");
    assert!(
        !open.1.sym().attrs.iter().any(|a| a.token == "comparable"),
        "Open (isComparable=false) must not have comparable attr"
    );
}
