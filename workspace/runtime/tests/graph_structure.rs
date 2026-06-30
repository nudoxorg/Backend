//! Diagram: **terminus is the source of truth for the API surface + structure.**
//!
//! TDD specs (`todo!()`) for `runtime::graph::structure`.

/// The graph yields a package's API surface structure.
///
/// Assert: reading structure returns the package's modules/types/functions and
///   how they nest, sourced from terminus.
#[tokio::test]
async fn structure_returns_api_surface() {
    todo!("assert structure read from the graph");
}

/// The graph is authoritative when stores disagree.
///
/// Assert: where the text/vector indexes and the graph disagree about an item's
///   shape, the graph (source of truth) wins.
#[tokio::test]
async fn graph_is_authoritative_on_structure() {
    todo!("assert graph is the source of truth for structure");
}

/// Structural relationships (members, implements, references) are navigable.
///
/// Assert: from a type you can reach its members and the traits it implements.
#[tokio::test]
async fn structural_relationships_are_navigable() {
    todo!("assert navigable structural relationships");
}
