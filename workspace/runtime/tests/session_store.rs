//! Pipeline part: **per-session graph state** (`runtime::session`).
//!
//! TDD specs for accumulating a user's exploration into a session-scoped graph
//! that merges, snapshots cheaply, clears, and optionally persists.

/// Merging an addition unions nodes and edges into the session.
///
/// Act: `merge(session, addition)`.
/// Assert: the returned response contains the union of prior and new
///   nodes/edges.
#[tokio::test]
async fn merge_unions_graph_state() {
    todo!("assert merge unions nodes/edges");
}

/// Merges are left-biased and idempotent for repeated nodes/edges.
///
/// Assert: merging an overlapping addition does not duplicate shared
///   nodes/edges.
#[tokio::test]
async fn merge_is_idempotent_for_overlap() {
    todo!("assert idempotent merge for overlapping state");
}

/// Clearing a session removes its state (and any persisted file).
///
/// Assert: after `clear(session)`, the session is empty.
#[tokio::test]
async fn clear_removes_session_state() {
    todo!("assert clear empties the session");
}

/// Sessions are isolated from one another.
///
/// Assert: merging into session A does not affect session B.
#[tokio::test]
async fn sessions_are_isolated() {
    todo!("assert per-session isolation");
}

/// A persisted session reloads from disk on open.
///
/// Assert: when a directory is configured, a merged session survives a reopen.
#[tokio::test]
async fn persisted_sessions_reload_on_open() {
    todo!("assert session persistence across reopen");
}
