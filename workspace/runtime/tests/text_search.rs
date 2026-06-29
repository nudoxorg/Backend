//! Pipeline part: **precise text search** (`runtime::text`).
//!
//! TDD specs for the tantivy-backed default search surface — finding symbols by
//! name/signature without touching the semantic layer.

/// A symbol is found by its exact name.
///
/// Assert: indexing `axum::Router` then searching `Router` returns it.
#[tokio::test]
async fn finds_symbol_by_exact_name() {
    todo!("assert exact-name text search");
}

/// Partial-name queries return all matching symbols.
///
/// Assert: a partial query returns every symbol whose name contains it.
#[tokio::test]
async fn partial_name_returns_all_matches() {
    todo!("assert partial-name matching");
}

/// The result limit is respected.
///
/// Assert: with 10 matches and a limit of 3, exactly 3 hits are returned.
#[tokio::test]
async fn limit_is_respected() {
    todo!("assert the search limit is honored");
}

/// Re-indexing the same occurrence updates rather than duplicates.
///
/// Assert: indexing the same occurrence id twice keeps a single document.
#[tokio::test]
async fn reindex_updates_without_duplicating() {
    todo!("assert upsert-by-occurrence, no duplicates");
}

/// The index persists across a reopen.
#[tokio::test]
async fn index_persists_across_reopen() {
    todo!("assert text index persistence across reopen");
}
