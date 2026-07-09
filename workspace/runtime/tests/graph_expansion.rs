//! Pipeline part: **graph relationships** (`runtime::graph`).
//!
//! Tests for the `GraphStore` contract: occurrences, references, relatedness,
//! neighbor expansion, and cross-version resolution/diffing. The terminus
//! transport needs a live service; the contract and the pure algorithms behind
//! it (`expand_via`, `resolution::diff`) are exercised here against an
//! in-memory graph.

mod support;

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use futures::{Stream, StreamExt, TryStreamExt};
use heart::{Language, Name, Scored, Symbol, SymbolId, SymbolKind};
use runtime::error::GraphError;
use runtime::graph::{
    GraphStore, RelationKind,
    expansion::{ExpansionBounds, expand_via},
    resolution::{Resolution, diff},
};
use support::{package_id, symbol_id};

/// An in-memory `GraphStore`: the edge list *is* the graph. Implements the
/// same trait the terminus-backed store does, so these tests pin the contract
/// any implementation must satisfy.
struct MemoryGraph {
    edges: Vec<(SymbolId, RelationKind, SymbolId)>,
}

impl MemoryGraph {
    fn incoming(
        &self,
        kind: RelationKind,
        item: SymbolId,
    ) -> impl Stream<Item = Result<Scored<SymbolId>, GraphError>> + Send {
        let sources: Vec<_> = self
            .edges
            .iter()
            .filter(|(_, edge_kind, to)| *edge_kind == kind && *to == item)
            .map(|(from, _, _)| {
                Ok(Scored::new(*from, heart::Score::try_new(1.0).expect("finite")))
            })
            .collect();
        futures::stream::iter(sources)
    }
}

impl GraphStore for MemoryGraph {
    type Error = GraphError;

    fn get_occurrences(
        &self,
        item: SymbolId,
    ) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
        self.incoming(RelationKind::Occurrence, item)
    }

    fn get_references(
        &self,
        item: SymbolId,
    ) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
        self.incoming(RelationKind::Reference, item)
    }

    async fn are_related(
        &self,
        from: SymbolId,
        to: SymbolId,
    ) -> Result<Option<RelationKind>, Self::Error> {
        Ok(self
            .edges
            .iter()
            .find(|(edge_from, _, edge_to)| *edge_from == from && *edge_to == to)
            .map(|(_, kind, _)| *kind))
    }
}

fn versioned_symbol(id: u64, package: u64, plain: &str, fully_qualified: &str) -> Symbol {
    Symbol {
        id: symbol_id(id),
        package: package_id(package),
        ecosystem: Language::Rust,
        name: Name { plain: plain.into(), fully_qualified: fully_qualified.into() },
        kind: SymbolKind::Type,
    }
}

/// `get_occurrences` returns every symbol whose declaration holds the input.
///
/// Assert: for a symbol used in three declarations, all three are returned.
#[tokio::test]
async fn get_occurrences_returns_holders() {
    let target = symbol_id(0);
    let graph = MemoryGraph {
        edges: vec![
            (symbol_id(1), RelationKind::Occurrence, target),
            (symbol_id(2), RelationKind::Occurrence, target),
            (symbol_id(3), RelationKind::Occurrence, target),
            (symbol_id(4), RelationKind::Reference, target), // not an occurrence
        ],
    };

    let holders: Vec<_> = graph.get_occurrences(target).try_collect().await.expect("streams");
    let mut ids: Vec<_> = holders.iter().map(|hit| hit.value).collect();
    ids.sort_unstable();
    assert_eq!(ids, [symbol_id(1), symbol_id(2), symbol_id(3)]);
}

/// `get_references` returns what points at a symbol.
///
/// Assert: callers/users of a symbol are returned by `get_references`.
#[tokio::test]
async fn get_references_returns_pointers() {
    let target = symbol_id(0);
    let graph = MemoryGraph {
        edges: vec![
            (symbol_id(1), RelationKind::Reference, target),
            (symbol_id(2), RelationKind::Reference, target),
            (symbol_id(3), RelationKind::Member, target), // membership is not a reference
        ],
    };

    let callers: Vec<_> = graph.get_references(target).try_collect().await.expect("streams");
    let mut ids: Vec<_> = callers.iter().map(|hit| hit.value).collect();
    ids.sort_unstable();
    assert_eq!(ids, [symbol_id(1), symbol_id(2)]);
}

/// `are_related` reports the specific kind for directly-linked symbols, and
/// `None` otherwise.
#[tokio::test]
async fn are_related_detects_direct_links() {
    let graph = MemoryGraph {
        edges: vec![(symbol_id(1), RelationKind::Implements, symbol_id(2))],
    };

    let linked = graph.are_related(symbol_id(1), symbol_id(2)).await.expect("query succeeds");
    assert_eq!(linked, Some(RelationKind::Implements), "the specific kind, not just a bool");
    let unlinked = graph.are_related(symbol_id(1), symbol_id(3)).await.expect("query succeeds");
    assert_eq!(unlinked, None);
}

/// Expansion walks a node's neighbors up to a bounded depth/breadth.
///
/// Act: `expansion::expand_via(origin, bounds, fetch)`.
/// Assert: the returned subgraph respects the depth and breadth bounds.
#[tokio::test]
async fn expansion_is_depth_and_breadth_bounded() {
    // A three-generation fan-out: 0 -> {1,2,3}, 1 -> {4,5}, 4 -> {6}.
    let adjacency: BTreeMap<SymbolId, Vec<(RelationKind, SymbolId)>> = BTreeMap::from([
        (symbol_id(0), vec![
            (RelationKind::Member, symbol_id(1)),
            (RelationKind::Member, symbol_id(2)),
            (RelationKind::Member, symbol_id(3)),
        ]),
        (symbol_id(1), vec![
            (RelationKind::Reference, symbol_id(4)),
            (RelationKind::Reference, symbol_id(5)),
        ]),
        (symbol_id(4), vec![(RelationKind::Reference, symbol_id(6))]),
    ]);
    let fetch = |node: SymbolId| {
        let reached = adjacency.get(&node).cloned().unwrap_or_default();
        async move { Ok::<_, GraphError>(reached) }
    };

    let bounds = ExpansionBounds {
        depth: NonZeroUsize::new(2).expect("nonzero"),
        breadth: NonZeroUsize::new(2).expect("nonzero"),
    };
    let edges = expand_via(symbol_id(0), bounds, fetch).await.expect("expansion succeeds");

    assert!(edges.iter().all(|edge| edge.depth.get() <= 2), "depth bound honored");
    for via in [symbol_id(0), symbol_id(1)] {
        let fan_out = edges.iter().filter(|edge| edge.via == via).count();
        assert!(fan_out <= 2, "per-node breadth bound honored at {via}");
    }
    // Depth 2 stops before 4 -> 6.
    assert!(edges.iter().all(|edge| edge.target.value != symbol_id(6)));
    // Nearer edges score higher than deeper ones.
    let depth_one = edges.iter().find(|edge| edge.depth.get() == 1).expect("has depth 1");
    let depth_two = edges.iter().find(|edge| edge.depth.get() == 2).expect("has depth 2");
    assert!(depth_one.target.score > depth_two.target.score);
}

/// Cross-version resolution stabilizes identifiers across versions.
///
/// Act: `resolution::diff(previous, next)`.
/// Assert: a symbol present in both versions keeps one stable `SymbolId`
///   (diffing maps the old occurrence onto the new).
#[tokio::test]
async fn resolution_stabilizes_ids_across_versions() {
    let previous = vec![
        versioned_symbol(1, 1, "Router", "axum::Router"),
        versioned_symbol(2, 1, "Route", "axum::Route"),
        versioned_symbol(3, 1, "Legacy", "axum::Legacy"),
    ];
    let next = vec![
        versioned_symbol(11, 2, "Router", "axum::Router"), // unchanged path
        versioned_symbol(12, 2, "Route", "axum::routing::Route"), // moved
        versioned_symbol(14, 2, "Fresh", "axum::Fresh"),    // new
    ];

    let resolutions = diff(&previous, &next);

    assert!(
        resolutions.contains(&Resolution::Stable(symbol_id(11))),
        "an unchanged path resolves to the one id carried forward"
    );
    let moved = resolutions.iter().find_map(|resolution| match resolution {
        Resolution::Moved { previous, next } => Some((*previous, next.value)),
        _ => None,
    });
    assert_eq!(
        moved,
        Some((symbol_id(2), symbol_id(12))),
        "a same-name, same-kind symbol under a new path maps onto the new id"
    );
    assert!(resolutions.contains(&Resolution::Removed(symbol_id(3))));
    assert!(resolutions.contains(&Resolution::Added(symbol_id(14))));
}

/// Streams from the in-memory store are usable with plain stream combinators
/// (the Send bounds the server relies on hold).
#[tokio::test]
async fn graph_streams_are_send() {
    fn assert_send<T: Send>(value: T) -> T {
        value
    }
    let graph = MemoryGraph { edges: vec![] };
    let mut stream = assert_send(graph.get_references(symbol_id(0)));
    assert!(stream.next().await.is_none());
}
