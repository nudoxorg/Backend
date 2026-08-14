//! Verify that every [`Vertex`] variant's `typename()` matches a concrete type
//! declared in `schema.graphql`.
//!
//! The variant name in the `Vertex` enum must exactly match the type name in
//! `schema.graphql` (Trustfall uses string comparison for typename dispatch).

use std::sync::Arc;

use nudox_engine::graph::vertex::{OccurrenceVertex, SymbolVertex, Vertex};
use nudox_ir::change::IntroId;
use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::fixtures::build_rich_view,
};
use trustfall::provider::Typename as _;

fn dummy_pkg() -> Arc<PackageView> {
    let view = build_rich_view();
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

fn dummy_sym(pkg: Arc<PackageView>, n: u8) -> SymbolVertex {
    SymbolVertex {
        package: pkg,
        intro: IntroId::from_raw([n; 32]),
    }
}

fn expected_type_names() -> Vec<(&'static str, Vertex)> {
    let pkg = dummy_pkg();
    vec![
        ("Package", Vertex::Package(pkg.clone())),
        ("Function", Vertex::Function(dummy_sym(pkg.clone(), 6))),
        ("Record", Vertex::Record(dummy_sym(pkg.clone(), 3))),
        ("Trait", Vertex::Trait(dummy_sym(pkg.clone(), 8))),
        ("Impl", Vertex::Impl(dummy_sym(pkg.clone(), 9))),
        ("Enum", Vertex::Enum(dummy_sym(pkg.clone(), 10))),
        ("Field", Vertex::Field(dummy_sym(pkg.clone(), 4))),
        ("Const", Vertex::Const(dummy_sym(pkg.clone(), 14))),
        ("Alias", Vertex::Alias(dummy_sym(pkg.clone(), 16))),
        // The five formerly-collapsed kinds now have dedicated variants.
        ("Static", Vertex::Static(dummy_sym(pkg.clone(), 15))),
        ("Variant", Vertex::Variant(dummy_sym(pkg.clone(), 11))),
        ("Module", Vertex::Module(dummy_sym(pkg.clone(), 1))),
        ("Reexport", Vertex::Reexport(dummy_sym(pkg.clone(), 22))),
        ("Param", Vertex::Param(dummy_sym(pkg.clone(), 7))),
        (
            "OtherSymbol",
            Vertex::OtherSymbol(dummy_sym(pkg.clone(), 255)),
        ),
        (
            "Occurrence",
            Vertex::Occurrence(OccurrenceVertex {
                owner: dummy_sym(pkg.clone(), 23),
                occ_index: 0,
            }),
        ),
        (
            "SourceLocation",
            Vertex::SourceLocation(dummy_sym(pkg.clone(), 6)),
        ),
    ]
}

/// Each `Vertex` variant's `typename()` must exactly match the corresponding
/// concrete type name in `schema.graphql`.
#[test]
fn all_vertex_typenames_match_schema() {
    for (expected_name, vertex) in expected_type_names() {
        let actual = vertex.typename();
        assert_eq!(
            actual, expected_name,
            "Vertex variant produces wrong typename: got '{actual}', expected '{expected_name}'"
        );
    }
}

/// `typename()` must be stable: two calls on the same vertex must agree.
#[test]
fn typename_is_stable() {
    let pkg = dummy_pkg();
    let sv = dummy_sym(pkg.clone(), 6);
    let vertex = Vertex::Function(sv);

    let t1 = vertex.typename();
    let t2 = vertex.typename();
    assert_eq!(t1, t2, "typename() must be stable across repeated calls");
}
