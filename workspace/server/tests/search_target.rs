//! Pipeline part: **read-only search surface** (`server::search::SearchTarget`).
//!
//! TDD specs for the abstract+literal query surface the server exposes over the
//! runtime stores.

/// A literal query searches the precise (tantivy) index.
///
/// Act: `SearchTarget::search(Search { query: Query::Literal(..), .. }, None)`.
/// Assert: returns a stream of scored items matching the literal.
#[tokio::test]
async fn literal_query_hits_the_text_index() {
    todo!("assert literal queries hit the precise index");
}

/// A natural-language query goes through the semantic (qdrant) store.
///
/// Act: `Query::Abstract(AbstractQuery::NaturalLanguage(..))`.
/// Assert: the query is embedded and matched semantically.
#[tokio::test]
async fn natural_language_query_is_semantic() {
    todo!("assert NL queries are embedded + semantic");
}

/// A code-snippet query is treated as an abstract semantic query.
#[tokio::test]
async fn code_snippet_query_is_semantic() {
    todo!("assert code-snippet queries are semantic");
}

/// A scope filter restricts results by language and package.
///
/// Assert: a `Filter` with `language`/`package` set only yields matching items.
#[tokio::test]
async fn scope_filter_restricts_results() {
    todo!("assert language/package scoping");
}

/// Pagination bounds the number of results via limit/offset.
#[tokio::test]
async fn pagination_bounds_results() {
    todo!("assert limit/offset pagination");
}

/// `get_by_id` fetches a single item by its Guid.
///
/// Assert: returns `Some(item)` for a known id, `None` otherwise.
#[tokio::test]
async fn get_by_id_fetches_one() {
    todo!("assert get_by_id Some/None");
}
