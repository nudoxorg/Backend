//! Diagram: **client** surfaces.
//!
//! "qdrant is showing similar items/examples in a sidebar · default interface is
//! tantivy for searching through items · most connections are surfaced on (maybe
//! someday) an embedded graph".

mod common;

use std::time::Duration;

use futures::StreamExt;
use heart::SymbolKind;
use server::http::dto::SearchRequestDto;
use server::search::SearchPlanner;
use server::search::planner::Plan;
use server::search::query::Query;
use server::search::symbols::SymbolTextSurface;

/// The default item-search response is backed by tantivy.
///
/// Assert: a wire request without the semantic opt-in lowers to a literal
///   query, plans precise, and is answered by the text index.
#[tokio::test]
async fn default_item_search_is_tantivy() {
    let wire = SearchRequestDto {
        query: "Deserialize".into(),
        semantic: false,
        ecosystems: Vec::new(),
        packages: Vec::new(),
        limit: std::num::NonZeroU32::new(8).expect("eight is non-zero"),
        cursor: None,
        session: None,
    };
    let request = wire.into_search().expect("a plain query lowers cleanly");
    assert!(
        matches!(request.query, Query::Literal(_)),
        "no semantic opt-in ⇒ the literal (tantivy) surface"
    );
    assert!(matches!(SearchPlanner::new().plan(&request), Plan::Precise));

    // And the text index really answers it.
    let replica = common::TempDir::new("default-surface");
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let index = common::populated_text_index(replica.path(), &[symbol.clone()]);
    let Query::Literal(literal) = &request.query else { unreachable!("asserted literal above") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("the default surface answers")
        .collect()
        .await;
    assert_eq!(
        hits.first().and_then(|hit| hit.as_ref().ok()).map(|hit| hit.value.id),
        Some(symbol.id)
    );
}

/// Similar items/examples (the sidebar) come from qdrant — explicitly requested.
///
/// Assert: the "similar items" surface is a separate, deliberate authorization
///   drawing from the semantic budget — never folded into the default search.
#[tokio::test]
async fn similar_items_sidebar_is_explicit_qdrant() {
    let planner = SearchPlanner::with_quota(1, Duration::from_secs(60));

    // Default (literal) searches never touch the semantic budget…
    let request = common::literal_search("Deserialize", 8);
    for _ in 0..3 {
        assert!(matches!(planner.plan(&request), Plan::Precise));
    }

    // …so the explicit sidebar call still finds its budget intact,
    let gate = planner.authorize_similar().expect("the untouched budget authorizes the sidebar");
    assert_eq!(gate.reason(), "similar-items sidebar", "the authorization is audited by purpose");

    // and once spent, the sidebar is refused rather than run unmetered.
    assert!(
        planner.authorize_similar().is_none(),
        "the sidebar draws from the same bounded semantic budget"
    );
}

/// Connections between items are surfaced from the graph store.
///
/// Assert: an item's connections come from terminus graph expansion (the
///   embedded-graph surface) — an expansion round-trip over the live stack.
#[tokio::test]
async fn connections_are_surfaced_from_the_graph() {
    let Some((server, _data)) =
        common::assembled_server("connections_are_surfaced_from_the_graph").await
    else {
        return;
    };
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let hit = heart::Scored::new(symbol, heart::Score::try_new(1.0).expect("one is finite"));

    // The graph store is the authority: expansion answers (an unknown symbol is
    // an empty neighbourhood), and every connection carries a score.
    let cap = common::read_cap(&server);
    let connections = server.expand(&cap, &hit).await.expect("graph expansion answers");
    assert!(
        connections.windows(2).all(|pair| pair[0].score >= pair[1].score),
        "connections come back ranked, straight from the graph surface"
    );
}
