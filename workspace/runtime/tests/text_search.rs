//! Pipeline part: **precise text search** (`runtime::text`).
//!
//! Tests for the tantivy-backed default search surface — finding symbols by
//! name/signature without touching the semantic layer. Runs against real
//! tantivy indexes in temporary directories; no services required.

mod support;

use std::num::NonZeroUsize;

use futures::TryStreamExt;
use heart::{Scored, Symbol};
use runtime::text::{TextIndex, TextQuery};
use support::{TempDir, symbol};

fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("test limits are nonzero")
}

async fn run_search(index: &TextIndex, terms: &str, n: usize) -> Vec<Scored<Symbol>> {
    index
        .search(&TextQuery::new(terms), limit(n), None)
        .try_collect()
        .await
        .expect("search over a healthy index succeeds")
}

/// A symbol is found by its exact name.
///
/// Assert: indexing `axum::Router` then searching `Router` returns it.
#[tokio::test]
async fn finds_symbol_by_exact_name() {
    let directory = TempDir::new("text-exact");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    index.upsert_batch(&[symbol(1, "Router", "axum::Router")]).expect("batch commits");

    let hits = run_search(&index, "Router", 10).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].value.name.fully_qualified, "axum::Router");
}

/// Partial-name queries return all matching symbols.
///
/// Assert: a partial query returns every symbol whose name contains it.
#[tokio::test]
async fn partial_name_returns_all_matches() {
    let directory = TempDir::new("text-partial");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    index
        .upsert_batch(&[
            symbol(1, "Router", "axum::Router"),
            symbol(2, "Route", "axum::routing::Route"),
            symbol(3, "RouterService", "axum::serve::RouterService"),
            symbol(4, "Extension", "axum::Extension"),
        ])
        .expect("batch commits");

    let hits = run_search(&index, "Rout", 10).await;
    let mut names: Vec<_> = hits.iter().map(|hit| hit.value.name.plain.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Route", "Router", "RouterService"]);
}

/// The result limit is respected.
///
/// Assert: with 10 matches and a limit of 3, exactly 3 hits are returned.
#[tokio::test]
async fn limit_is_respected() {
    let directory = TempDir::new("text-limit");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    let symbols: Vec<_> = (0..10)
        .map(|n| symbol(n, &format!("Widget{n}"), &format!("toolkit::Widget{n}")))
        .collect();
    index.upsert_batch(&symbols).expect("batch commits");

    let hits = run_search(&index, "Widget", 3).await;
    assert_eq!(hits.len(), 3, "exactly the limit, no more");
}

/// Re-indexing the same occurrence updates rather than duplicates.
///
/// Assert: indexing the same occurrence id twice keeps a single document.
#[tokio::test]
async fn reindex_updates_without_duplicating() {
    let directory = TempDir::new("text-upsert");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    index.upsert_batch(&[symbol(1, "Router", "axum::Router")]).expect("first commit");
    // Same id, revised path — must replace, not append.
    index.upsert_batch(&[symbol(1, "Router", "axum::routing::Router")]).expect("second commit");

    let hits = run_search(&index, "Router", 10).await;
    assert_eq!(hits.len(), 1, "upsert by id never duplicates");
    assert_eq!(hits[0].value.name.fully_qualified, "axum::routing::Router");
}

/// The index persists across a reopen.
#[tokio::test]
async fn index_persists_across_reopen() {
    let directory = TempDir::new("text-reopen");
    {
        let index = TextIndex::open_or_create(directory.path()).expect("index opens");
        index.upsert_batch(&[symbol(1, "Router", "axum::Router")]).expect("batch commits");
    }

    let reopened = TextIndex::open_or_create(directory.path()).expect("index reopens");
    let hits = run_search(&reopened, "Router", 10).await;
    assert_eq!(hits.len(), 1, "committed documents survive a reopen");
}

/// Keyset pagination resumes strictly after the cursor and never overlaps.
///
/// Uses [`heart::Cursor::mint_enforced`] to build the resume token — the caller
/// (here the test harness) has just verified the live snapshot, so the enforced
/// brand is correct.
#[tokio::test]
async fn cursor_resumes_without_overlap() {
    let directory = TempDir::new("text-cursor");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    let symbols: Vec<_> = (0..6)
        .map(|n| symbol(n, &format!("Widget{n}"), &format!("toolkit::Widget{n}")))
        .collect();
    index.upsert_batch(&symbols).expect("batch commits");

    let first = run_search(&index, "Widget", 2).await;
    assert_eq!(first.len(), 2);
    let last = first.last().expect("page is non-empty");
    // mint_enforced: we just ran the search against the live snapshot.
    let cursor = heart::Cursor::mint_enforced(
        (last.score, last.value.id),
        index.snapshot().expect("snapshot hashes"),
    );

    let second: Vec<Scored<Symbol>> = index
        .search(&TextQuery::new("Widget"), limit(10), Some(cursor))
        .try_collect()
        .await
        .expect("resumed search succeeds");
    assert_eq!(second.len(), 4, "the resumed page holds exactly the remainder");
    let first_ids: Vec<_> = first.iter().map(|hit| hit.value.id).collect();
    assert!(
        second.iter().all(|hit| !first_ids.contains(&hit.value.id)),
        "pages never overlap"
    );
}

/// A cursor minted against an older snapshot is flagged, not silently served.
///
/// The cursor is minted with [`heart::Cursor::mint_enforced`] against the
/// snapshot at first-page time; after the index moves, the search must reject
/// it with [`runtime::error::TextError::Cursor`].
#[tokio::test]
async fn stale_cursor_is_rejected() {
    let directory = TempDir::new("text-stale-cursor");
    let index = TextIndex::open_or_create(directory.path()).expect("index opens");
    index.upsert_batch(&[symbol(1, "Router", "axum::Router")]).expect("batch commits");

    let first = run_search(&index, "Router", 1).await;
    let last = first.last().expect("page is non-empty");
    // Capture the snapshot before the index moves.
    let cursor = heart::Cursor::mint_enforced(
        (last.score, last.value.id),
        index.snapshot().expect("snapshot hashes"),
    );

    // The index moves underneath the cursor.
    index.upsert_batch(&[symbol(2, "Route", "axum::routing::Route")]).expect("batch commits");

    let resumed: Result<Vec<Scored<Symbol>>, _> = index
        .search(&TextQuery::new("Router"), limit(10), Some(cursor))
        .try_collect()
        .await;
    assert!(
        matches!(resumed, Err(runtime::error::TextError::Cursor(_))),
        "a snapshot mismatch surfaces as a cursor error"
    );
}
