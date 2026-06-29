//! Pipeline part: **graph relationships** (`runtime::graph`).
//!
//! TDD specs for the terminus-backed `GraphStore`: occurrences, references,
//! relatedness, neighbor expansion, and cross-version resolution/diffing.

/// `get_occurrences` returns every symbol whose declaration holds the input.
///
/// Assert: for a symbol used in three declarations, all three are returned.
#[tokio::test]
async fn get_occurrences_returns_holders() {
    todo!("assert occurrences of a symbol");
}

/// `get_references` returns what points at a symbol.
///
/// Assert: callers/users of a symbol are returned by `get_references`.
#[tokio::test]
async fn get_references_returns_pointers() {
    todo!("assert references to a symbol");
}

/// `are_related` is true for directly-linked symbols, false otherwise.
#[tokio::test]
async fn are_related_detects_direct_links() {
    todo!("assert relatedness of linked vs unlinked symbols");
}

/// Expansion walks a node's neighbors up to a bounded depth/breadth.
///
/// Act: `expansion::expand(uri, depth, breadth)`.
/// Assert: the returned subgraph respects the depth and breadth bounds.
#[tokio::test]
async fn expansion_is_depth_and_breadth_bounded() {
    todo!("assert bounded neighbor expansion");
}

/// Cross-version resolution stabilizes identifiers across versions.
///
/// Act: `resolution::resolve_across(prev_version, next_version)`.
/// Assert: a symbol present in both versions keeps one stable `GlobalSymbolId`
///   (diffing maps the old occurrence onto the new).
#[tokio::test]
async fn resolution_stabilizes_ids_across_versions() {
    todo!("assert stable ids across version diffing");
}
