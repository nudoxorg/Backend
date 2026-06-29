//! Pipeline part: **semantic/vector search** (`runtime::vector`).
//!
//! TDD specs for similarity over the Qdrant-backed store: cosine ranking,
//! similarity from a starting symbol, from a snippet, and language-scoped
//! similarity.

/// Searching with a vector returns the closest points by cosine, ranked.
///
/// Assert: `Semantic::search(vector, k)` returns up to `k` hits ordered by
///   descending cosine similarity.
#[tokio::test]
async fn search_returns_top_k_by_cosine() {
    todo!("assert top-k cosine ordering");
}

/// An exact-code query ranks the matching symbol first.
///
/// Arrange: deterministic embeddings (same text -> same vector).
/// Assert: querying with a symbol's own code yields that symbol as the top hit
///   with score ~1.0.
#[tokio::test]
async fn exact_code_query_ranks_itself_first() {
    todo!("assert exact-code self-match is the top hit");
}

/// Similarity from a starting symbol surfaces its neighbors.
///
/// Act: `similarity::similar_to(symbol_id, k)`.
/// Assert: returns semantically-near symbols, excluding the symbol itself.
#[tokio::test]
async fn similarity_from_symbol_surfaces_neighbors() {
    todo!("assert similarity-from-symbol neighbors");
}

/// Similarity from a snippet works without a stored symbol.
///
/// Act: `snippet::similar_to_snippet(code, k)`.
/// Assert: embeds the snippet and returns near symbols.
#[tokio::test]
async fn similarity_from_snippet_embeds_then_searches() {
    todo!("assert snippet-driven similarity");
}

/// Language-scoped similarity restricts results to one language.
///
/// Act: `language::similar_within(lang, vector, k)`.
/// Assert: every hit belongs to `lang` (payload-level scoping, one collection).
#[tokio::test]
async fn language_scoped_similarity_filters_by_language() {
    todo!("assert language scoping via payload filter");
}
