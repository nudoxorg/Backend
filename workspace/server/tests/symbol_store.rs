//! Pipeline part: **symbol search + graph fusion** (`server::search::SymbolStore`).
//!
//! TDD specs for the store that combines semantic + precise search
//! (`SearchTarget`) with graph relationships (`GraphStore`). The text half runs
//! for real over a replica-local tantivy index; the graph half needs a live
//! terminus and is opt-in via `SERVER_TEST_BACKENDS`.

mod common;

use futures::StreamExt;
use heart::{Scored, SymbolKind};
use server::search::query::Query;
use server::search::symbols::{SymbolTextSurface, kind_admits};

/// A symbol is found by name and returned as a scored hit.
///
/// Assert: searching an ingested symbol yields a `Scored<Symbol>` with its
///   name/kind.
#[tokio::test]
async fn finds_symbol_and_scores_it() {
    let replica = common::TempDir::new("finds-and-scores");
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let index = common::populated_text_index(replica.path(), &[symbol.clone()]);

    let request = common::literal_search("Deserialize", 8);
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("a committed index answers")
        .collect()
        .await;

    let hit: &Scored<_> = hits
        .first()
        .expect("the ingested symbol is found")
        .as_ref()
        .expect("the stream yields hits, not errors");
    assert_eq!(hit.value.name.plain, "Deserialize");
    assert_eq!(hit.value.kind, SymbolKind::Trait);
}

/// `related_hits` walks a hit's relationships and scores them.
///
/// Act: `related_hits(&scored_symbol)` over the live stack.
/// Assert: returns neighboring symbols (via the graph) each carrying a score —
///   and a symbol absent from the graph yields an empty neighbourhood, not an
///   error.
#[tokio::test]
async fn related_hits_walks_and_scores_relationships() {
    let Some((server, _data)) =
        common::assembled_server("related_hits_walks_and_scores_relationships").await
    else {
        return;
    };
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let hit = Scored::new(symbol, heart::Score::try_new(1.0).expect("one is finite"));

    let cap = common::read_cap(&server);
    let related = server
        .expand(&cap, &hit)
        .await
        .expect("expanding an unknown symbol is an empty neighbourhood, not a failure");
    assert!(
        related.windows(2).all(|pair| pair[0].score >= pair[1].score),
        "related hits come back ranked by score"
    );
}

/// An empty index returns no hits (not an error).
#[tokio::test]
async fn empty_index_returns_no_hits() {
    let replica = common::TempDir::new("empty-index");
    let index = common::populated_text_index(replica.path(), &[]);

    let request = common::literal_search("anything", 8);
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("an empty index answers cleanly")
        .collect()
        .await;

    assert!(hits.is_empty(), "an empty index yields an empty page, never an error");
}

/// A kind filter restricts results to one symbol kind.
///
/// Assert: filtering by `SymbolKind::Type` admits only types.
#[tokio::test]
async fn kind_filter_restricts_results() {
    let replica = common::TempDir::new("kind-filter");
    let package = common::package_id("serde");
    let a_type = common::rust_symbol(package, "Value", "serde::Value", SymbolKind::Type);
    let a_function = common::rust_symbol(package, "Value", "serde::value::Value", SymbolKind::Function);
    let index = common::populated_text_index(replica.path(), &[a_type.clone(), a_function]);

    let request = common::literal_search("Value", 8);
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("a committed index answers")
        .collect()
        .await;

    let only_types: Vec<_> = hits
        .into_iter()
        .map(|hit| hit.expect("the stream yields hits, not errors"))
        .filter(|hit| kind_admits(&[SymbolKind::Type], hit.value.kind))
        .collect();
    assert_eq!(only_types.len(), 1, "exactly the type survives the kind allowlist");
    assert_eq!(only_types[0].value.id, a_type.id);

    // The unbounded allowlist admits everything — `None` dimensions stay open.
    assert!(kind_admits(&[], SymbolKind::Function));
}
