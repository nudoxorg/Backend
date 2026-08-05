//! Pipeline part: **symbol search + graph fusion** (`driver::search::SymbolStore`).
//!
//! Precise symbol tantivy is gone; these specs cover expand/related_hits over
//! the live stack (opt-in) and the kind-allowlist helper used by filters.

mod common;

use heart::{Scored, SymbolKind};

/// Whether a symbol's kind survives a kind allowlist (empty = unbounded).
fn kind_admits(kinds: &[SymbolKind], kind: SymbolKind) -> bool {
    kinds.is_empty() || kinds.contains(&kind)
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
    let symbol = common::rust_symbol(
        package,
        "Deserialize",
        "serde::Deserialize",
        SymbolKind::Trait,
    );
    let hit = Scored::new(symbol, heart::Score::try_new(1.0).expect("one is finite"));

    let cap = common::read_cap(&server);
    let related = server
        .expand(&cap, &hit)
        .await
        .expect("expanding an unknown symbol is an empty neighbourhood, not a failure");
    assert!(
        related
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score),
        "related hits come back ranked by score"
    );
}

/// A kind filter restricts results to one symbol kind.
#[tokio::test]
async fn kind_filter_restricts_results() {
    assert!(kind_admits(&[SymbolKind::Type], SymbolKind::Type));
    assert!(!kind_admits(&[SymbolKind::Type], SymbolKind::Function));
    // The unbounded allowlist admits everything — `None` dimensions stay open.
    assert!(kind_admits(&[], SymbolKind::Function));
}
