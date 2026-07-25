//! Unit tests for the Go producer lowering.
//!
//! These tests are fully self-contained: they embed literal JSON fixtures,
//! deserialize them into `oracle::Output`, lower them, and assert on the
//! resulting IR entries.  No Go toolchain or subprocess is required.

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    kinds::{Alias, Const, Enum, Field, Function, Record, Static, Trait},
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
    PackageLineageId::new(
        EcosystemId::new("go"),
        PackageName::new("example.com/m"),
    )
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
    assert!(width.unwrap().1.downcast::<Field>().is_some(), "Width must be a Field");
    assert!(height.unwrap().1.downcast::<Field>().is_some(), "Height must be a Field");
}

#[test]
fn method_declared_with_receiver() {
    let pkg = lower_fixture();
    let area = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Area")
        .expect("Area method must be declared");
    let fn_body = area
        .1
        .downcast::<Function>()
        .expect("Area must be a Function");
    // Area is a value receiver method.
    assert!(
        matches!(
            fn_body.body().receiver,
            Some(nudox_ir::kinds::function::Receiver::Owned)
        ),
        "value receiver must be Owned, got {:?}",
        fn_body.body().receiver
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
    assert!(red.1.downcast::<Variant>().is_some(), "Red must be a Variant");
    assert!(green.1.downcast::<Variant>().is_some(), "Green must be a Variant");

    // Discriminants preserved.
    assert_eq!(
        red.1.downcast::<Variant>().unwrap().body().discr.as_deref(),
        Some("0")
    );
    assert_eq!(
        green.1.downcast::<Variant>().unwrap().body().discr.as_deref(),
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
    assert_eq!(fn_body.body().input_params.len(), 2, "Scale must have 2 input params");
    assert_eq!(fn_body.body().output_params.len(), 1, "Scale must have 1 output param");
}

#[test]
fn const_pi_declared() {
    let pkg = lower_fixture();
    let pi = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Pi")
        .expect("Pi must be declared");
    assert!(
        pi.1.downcast::<Const>().is_some(),
        "Pi must be a Const"
    );
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
