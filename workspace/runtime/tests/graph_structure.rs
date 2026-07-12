//! Diagram: **terminus is the source of truth for the API surface + structure.**
//!
//! Tests for `runtime::graph::structure`. The terminus transport needs a live
//! service; the assembly the store performs on its answers
//! (`structure::assemble`) and the navigability of the resulting nodes are
//! exercised here directly.

mod support;

use std::collections::BTreeMap;

use heart::{Language, Name, Symbol, SymbolId, SymbolKind};
use runtime::graph::{
    RelationKind,
    structure::{StructureNode, assemble},
};
use support::{package_id, symbol_id};

fn api_symbol(id: u64, plain: &str, fully_qualified: &str, kind: SymbolKind) -> Symbol {
    Symbol {
        id: symbol_id(id),
        package: package_id(1),
        ecosystem: Language::Rust,
        name: Name { plain: plain.into(), fully_qualified: fully_qualified.into() },
        kind,
    }
}

/// A small package surface: a module holding a type holding a method, plus a
/// trait the type implements.
fn package_surface() -> (Vec<Symbol>, BTreeMap<SymbolId, (RelationKind, SymbolId)>) {
    let symbols = vec![
        api_symbol(1, "routing", "axum::routing", SymbolKind::Module),
        api_symbol(2, "Router", "axum::routing::Router", SymbolKind::Type),
        api_symbol(3, "route", "axum::routing::Router::route", SymbolKind::Function),
        api_symbol(4, "Service", "tower::Service", SymbolKind::Trait),
    ];
    let parents = BTreeMap::from([
        (symbol_id(2), (RelationKind::Member, symbol_id(1))),
        (symbol_id(3), (RelationKind::Member, symbol_id(2))),
    ]);
    (symbols, parents)
}

/// The graph yields a package's API surface structure.
///
/// Assert: reading structure returns the package's modules/types/functions and
///   how they nest, sourced from the graph's symbol + parent-edge answers.
#[tokio::test]
async fn structure_returns_api_surface() {
    let (symbols, parents) = package_surface();
    let nodes = assemble(symbols, &parents);

    assert_eq!(nodes.len(), 4, "every symbol appears exactly once");
    let by_id: BTreeMap<SymbolId, &StructureNode> =
        nodes.iter().map(|node| (node.symbol.id, node)).collect();

    // The module is a root; the type nests under it; the method under the type.
    assert_eq!(by_id[&symbol_id(1)].parent, None);
    assert_eq!(by_id[&symbol_id(2)].parent, Some(symbol_id(1)));
    assert_eq!(by_id[&symbol_id(2)].relation, Some(RelationKind::Member));
    assert_eq!(by_id[&symbol_id(3)].parent, Some(symbol_id(2)));
    // And the kinds cover the surface: module, type, function.
    assert_eq!(by_id[&symbol_id(1)].kind, SymbolKind::Module);
    assert_eq!(by_id[&symbol_id(2)].kind, SymbolKind::Type);
    assert_eq!(by_id[&symbol_id(3)].kind, SymbolKind::Function);
}

/// The graph is authoritative when stores disagree.
///
/// Assert: where a derived index holds a stale copy of an item's shape, the
///   graph's record (what `assemble` was fed) wins.
#[tokio::test]
async fn graph_is_authoritative_on_structure() {
    let (symbols, parents) = package_surface();
    // A stale projection (e.g. a lagging text index) that still calls Router a
    // function and knows nothing of its parent.
    let stale_copy = api_symbol(2, "Router", "axum::Router", SymbolKind::Function);

    let nodes = assemble(symbols, &parents);
    let authoritative = nodes
        .iter()
        .find(|node| node.symbol.id == stale_copy.id)
        .expect("the graph holds the symbol");

    assert_ne!(authoritative.kind, stale_copy.kind, "the copies genuinely disagree");
    assert_eq!(authoritative.kind, SymbolKind::Type, "the graph's kind is served");
    assert_eq!(
        authoritative.symbol.name.fully_qualified, "axum::routing::Router",
        "the graph's path is served"
    );
    assert_eq!(authoritative.parent, Some(symbol_id(1)), "nesting comes from the graph");
}

/// Structural relationships (members, implements, references) are navigable.
///
/// Assert: from a type you can reach its members and the traits it implements.
#[tokio::test]
async fn structural_relationships_are_navigable() {
    let (symbols, parents) = package_surface();
    // The implements-edge set the graph would answer alongside the tree.
    let implements = [(symbol_id(2), RelationKind::Implements, symbol_id(4))];

    let nodes = assemble(symbols, &parents);

    // Members of the type: every node whose parent is the type.
    let members: Vec<SymbolId> = nodes
        .iter()
        .filter(|node| node.parent == Some(symbol_id(2)))
        .map(|node| node.symbol.id)
        .collect();
    assert_eq!(members, [symbol_id(3)], "the method is reachable from its type");

    // Traits implemented by the type, via the edge set.
    let implemented: Vec<SymbolId> = implements
        .iter()
        .filter(|(from, kind, _)| *from == symbol_id(2) && *kind == RelationKind::Implements)
        .map(|(_, _, to)| *to)
        .collect();
    assert_eq!(implemented, [symbol_id(4)], "the trait is reachable from its implementor");
}
