//! Diagram: **client** surfaces.
//!
//! "qdrant is showing similar items/examples in a sidebar · default interface is
//! tantivy for searching through items · most connections are surfaced on (maybe
//! someday) an embedded graph".

/// The default item-search response is backed by tantivy.
///
/// Assert: the primary search response is text/precise, not semantic.
#[tokio::test]
async fn default_item_search_is_tantivy() {
    todo!("assert the default client search surface is tantivy");
}

/// Similar items/examples (the sidebar) come from qdrant — explicitly requested.
///
/// Assert: the "similar items" surface returns semantically-near examples from
///   qdrant, and is a separate, explicit call (not folded into the default
///   search).
#[tokio::test]
async fn similar_items_sidebar_is_explicit_qdrant() {
    todo!("assert similar-items sidebar is an explicit qdrant call");
}

/// Connections between items are surfaced from the graph store.
///
/// Assert: an item's connections come from terminus graph expansion (the
///   embedded-graph surface).
#[tokio::test]
async fn connections_are_surfaced_from_the_graph() {
    todo!("assert item connections come from the graph store");
}
