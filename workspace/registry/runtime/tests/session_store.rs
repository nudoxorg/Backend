//! Pipeline part: **per-session graph state** (`runtime::session`).
//!
//! Tests for accumulating a user's exploration into a session-scoped graph
//! that merges, snapshots cheaply, clears, and optionally persists.

mod support;

use runtime::session::RelationKind;
use runtime::session::{
    Edge, MemorySessionStore, Persistence, SessionGraph, SessionId, SessionStore,
};
use support::symbol_id;

fn session(n: u64) -> SessionId {
    SessionId::from_uuid(heart::Guid::from_u128(0xEE55_0000 + u128::from(n)))
}

fn edge(from: u64, kind: RelationKind, to: u64) -> Edge {
    Edge { from: symbol_id(from), kind, to: symbol_id(to) }
}

fn graph(nodes: &[u64], edges: &[Edge]) -> SessionGraph {
    SessionGraph {
        nodes: nodes.iter().copied().map(symbol_id).collect(),
        edges: edges.iter().copied().collect(),
    }
}

/// Merging an addition unions nodes and edges into the session.
///
/// Act: `merge(session, addition)`.
/// Assert: the returned response contains the union of prior and new
///   nodes/edges.
#[tokio::test]
async fn merge_unions_graph_state() {
    let store = MemorySessionStore::new(Persistence::Ephemeral);
    let id = session(1);

    let first = graph(&[1, 2], &[edge(1, RelationKind::Member, 2)]);
    let second = graph(&[2, 3], &[edge(2, RelationKind::Reference, 3)]);
    store.merge_into(id, first).await.expect("first merge succeeds");
    let merged = store.merge_into(id, second).await.expect("second merge succeeds");

    let expected = graph(
        &[1, 2, 3],
        &[edge(1, RelationKind::Member, 2), edge(2, RelationKind::Reference, 3)],
    );
    assert_eq!(*merged, expected, "merge is the union of both additions");
}

/// Merges are idempotent for repeated nodes/edges.
///
/// Assert: merging an overlapping addition does not duplicate shared
///   nodes/edges.
#[tokio::test]
async fn merge_is_idempotent_for_overlap() {
    let store = MemorySessionStore::new(Persistence::Ephemeral);
    let id = session(2);

    let addition = graph(&[1, 2], &[edge(1, RelationKind::Occurrence, 2)]);
    let once = store.merge_into(id, addition.clone()).await.expect("merge succeeds");
    let twice = store.merge_into(id, addition).await.expect("re-merge succeeds");

    assert_eq!(*once, *twice, "re-merging the same delta changes nothing");
    assert_eq!(twice.nodes.len(), 2);
    assert_eq!(twice.edges.len(), 1);
}

/// Clearing a session removes its state (and any persisted file).
///
/// Assert: after `clear(session)`, the session is empty.
#[tokio::test]
async fn clear_removes_session_state() {
    let directory = support::TempDir::new("session-clear");
    let store = MemorySessionStore::new(Persistence::Directory(directory.path().to_owned()));
    let id = session(3);

    store
        .merge_into(id, graph(&[1], &[edge(1, RelationKind::Extends, 1)]))
        .await
        .expect("merge succeeds");
    store.persist(id).await.expect("persist succeeds");
    store.clear(id).await.expect("clear succeeds");

    let snapshot = store.snapshot(id).await.expect("cleared session is still open");
    assert!(snapshot.is_empty(), "clear empties the in-memory graph");
    let reloaded = store.load(id).await.expect("load succeeds");
    assert!(reloaded.is_none(), "clear removes the persisted snapshot too");
}

/// Sessions are isolated from one another.
///
/// Assert: merging into session A does not affect session B.
#[tokio::test]
async fn sessions_are_isolated() {
    let store = MemorySessionStore::new(Persistence::Ephemeral);
    let (a, b) = (session(4), session(5));

    store.open(b).await.expect("open succeeds");
    store
        .merge_into(a, graph(&[1, 2], &[edge(1, RelationKind::Member, 2)]))
        .await
        .expect("merge succeeds");

    let untouched = store.snapshot(b).await.expect("snapshot succeeds");
    assert!(untouched.is_empty(), "a merge into A never leaks into B");
}

/// A persisted session reloads from disk on open.
///
/// Assert: when a directory is configured, a merged session survives a reopen.
#[tokio::test]
async fn persisted_sessions_reload_on_open() {
    let directory = support::TempDir::new("session-reload");
    let id = session(6);
    let accumulated = graph(&[7, 8], &[edge(7, RelationKind::Implements, 8)]);

    {
        let store = MemorySessionStore::new(Persistence::Directory(directory.path().to_owned()));
        store.merge_into(id, accumulated.clone()).await.expect("merge succeeds");
        store.persist(id).await.expect("persist succeeds");
    }

    let reopened = MemorySessionStore::new(Persistence::Directory(directory.path().to_owned()));
    let restored = reopened.open(id).await.expect("open succeeds");
    assert_eq!(*restored, accumulated, "the merged session survives a store reopen");
}
