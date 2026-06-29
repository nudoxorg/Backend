//! Pipeline part: **symbol search + graph fusion** (`server::search::SymbolStore`).
//!
//! TDD specs for the store that combines semantic + precise search
//! (`SearchTarget`) with graph relationships (`GraphStore`).

/// A symbol is found by name and returned as a scored hit.
///
/// Assert: searching an ingested symbol yields a `Scored<Symbol>` with its
///   name/kind.
#[tokio::test]
async fn finds_symbol_and_scores_it() {
    todo!("assert a symbol search returns a scored hit");
}

/// `related_hits` walks a hit's relationships and scores them.
///
/// Act: `related_hits(&scored_symbol)`.
/// Assert: returns neighboring symbols (via the graph) each carrying a score.
#[tokio::test]
async fn related_hits_walks_and_scores_relationships() {
    todo!("assert related_hits fuses graph neighbors with scores");
}

/// An empty index returns no hits (not an error).
#[tokio::test]
async fn empty_index_returns_no_hits() {
    todo!("assert empty index -> empty results");
}

/// A kind filter restricts results to one symbol kind.
///
/// Assert: filtering by `SymbolKind::Struct` returns only structs.
#[tokio::test]
async fn kind_filter_restricts_results() {
    todo!("assert kind filtering");
}
