//! Diagram rule: **qdrant semantic search is heavy and must be explicitly gated
//! — never implicit.** The default search surface is tantivy text search.
//!
//! TDD specs (`todo!()`) for `server::search::semantic` + `runtime::vector::gate`.

/// A plain search request uses the precise (tantivy) surface, not qdrant.
///
/// Act: issue a default search (no semantic opt-in).
/// Assert: results come from the text index; the qdrant/embedder path is never
///   invoked (e.g. an embedder spy records zero calls).
#[tokio::test]
async fn default_search_is_text_not_semantic() {
    todo!("assert default search never touches qdrant/embeddings");
}

/// Semantic search requires an explicit opt-in / gate token.
///
/// Act: request semantic search WITHOUT the gate.
/// Assert: rejected (gate not satisfied) — semantic search is unreachable by
///   default.
#[tokio::test]
async fn semantic_search_without_gate_is_rejected() {
    todo!("assert semantic search is refused without the gate");
}

/// Semantic search runs only once the gate is satisfied.
///
/// Act: request semantic search WITH the gate token.
/// Assert: the query is embedded and matched against qdrant, returning hits.
#[tokio::test]
async fn semantic_search_with_gate_runs() {
    todo!("assert gated semantic search embeds + queries qdrant");
}

/// The semantic path never runs implicitly as a fallback of text search.
///
/// Assert: an empty text result does NOT silently escalate to a semantic query.
#[tokio::test]
async fn text_search_does_not_implicitly_escalate_to_semantic() {
    todo!("assert no implicit text->semantic fallback");
}
