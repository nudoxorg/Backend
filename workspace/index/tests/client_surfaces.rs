#![cfg(feature = "server")]
//! Diagram: **client** surfaces.
//!
//! Semantic (qdrant) is the gated similar-items surface; precise symbol search
//! no longer rides tantivy. Graph expansion still answers the connections path.

mod server_common;

use std::time::Duration;

use index::server::search::SearchPlanner;
use index::server::search::planner::Plan;
use heart::SymbolKind;
use heart::client::query::ExecutionQuery as Query;

/// The default item-search request is a literal (precise) plan — never semantic.
///
/// Assert: a non-semantic internal search request lowers to a literal query and
///   plans precise (the symbol-tantivy backend is gone; the plan still is precise).
#[tokio::test]
async fn default_item_search_plans_precise() {
    let request = server_common::literal_search("Deserialize", 8);
    assert!(
        matches!(request.query, Query::Literal(_)),
        "no semantic opt-in ⇒ the literal surface"
    );
    assert!(matches!(SearchPlanner::new().plan(&request), Plan::Precise));
}

/// Similar items/examples (the sidebar) come from qdrant — explicitly requested.
///
/// Assert: the "similar items" surface is a separate, deliberate authorization
///   drawing from the semantic budget — never folded into the default search.
#[tokio::test]
async fn similar_items_sidebar_is_explicit_qdrant() {
    let planner = SearchPlanner::with_quota(1, Duration::from_secs(60));

    // Default (literal) searches never touch the semantic budget…
    let request = server_common::literal_search("Deserialize", 8);
    for _ in 0..3 {
        assert!(matches!(planner.plan(&request), Plan::Precise));
    }

    // …so the explicit sidebar call still finds its budget intact,
    let gate = planner
        .authorize_similar()
        .expect("the untouched budget authorizes the sidebar");
    assert_eq!(
        gate.reason(),
        "similar-items sidebar",
        "the authorization is audited by purpose"
    );

    // and once spent, the sidebar is refused rather than run unmetered.
    assert!(
        planner.authorize_similar().is_none(),
        "the sidebar draws from the same bounded semantic budget"
    );
}

/// Connections between items are surfaced from the graph store.
///
/// Assert: an item's connections come from graph expansion — an expansion
///   round-trip over the live stack.
#[tokio::test]
async fn connections_are_surfaced_from_the_graph() {
    let Some((server, _data)) =
        server_common::assembled_server("connections_are_surfaced_from_the_graph").await
    else {
        return;
    };
    let package = server_common::package_id("serde");
    let symbol = server_common::rust_symbol(
        package,
        "Deserialize",
        "serde::Deserialize",
        SymbolKind::Trait,
    );
    let hit = heart::Scored::new(symbol, heart::Score::try_new(1.0).expect("one is finite"));

    // The graph store is the authority: expansion answers (an unknown symbol is
    // an empty neighbourhood), and every connection carries a score.
    let cap = server_common::read_cap(&server);
    let connections = server
        .expand(&cap, &hit)
        .await
        .expect("graph expansion answers");
    assert!(
        connections
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score),
        "connections come back ranked, straight from the graph surface"
    );
}
