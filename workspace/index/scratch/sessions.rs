//! Writer-sticky exploration sessions (INDEX-PLAN ID-19).
//!
//! When a client opens a read session it is pinned to whichever catalog writer
//! host served the open request. Every subsequent request in that session must
//! be routed to the same writer so that reads see their own writes. The
//! [`writer_for`] function enforces this contract: given a session id it
//! returns the sticky `writer_host`, or `None` if the session has expired or
//! never existed.
//!
//! Sessions are ephemeral. On restart all sessions are implicitly gone and
//! clients must re-open. The `sessions` table is delete-anytime.

use rusqlite::{Connection, OptionalExtension, Result, params};

// ─────────────────────────────────────────────────────────────────────────────
// Row type
// ─────────────────────────────────────────────────────────────────────────────

/// A single row from the `sessions` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// UUID string identifying this session (client-supplied or
    /// server-generated).
    pub session_id: String,
    /// The writer host this session is permanently pinned to.
    ///
    /// All read requests for this session must be dispatched to this host so
    /// that reads observe their own writes (writer-sticky guarantee, ID-19).
    pub writer_host: String,
    /// Unix timestamp (seconds) when the session was first opened.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the most recent activity on this session.
    pub last_seen_at: i64,
    /// Optional JSON-encoded exploration graph for interactive navigation
    /// sessions.
    pub graph_state: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// DDL
// ─────────────────────────────────────────────────────────────────────────────

/// Create the `sessions` table if it does not already exist.
///
/// Idempotent — safe to call on every [`super::ScratchStore`] open.
pub fn create_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id   TEXT    PRIMARY KEY,
            writer_host  TEXT    NOT NULL,
            created_at   INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            graph_state  TEXT
        );",
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// DML helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Open a new session pinned to `writer_host`.
///
/// Fails with a constraint error if `session_id` already exists.
pub fn create_session(
    connection: &Connection,
    session_id: &str,
    writer_host: &str,
    now: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO sessions (session_id, writer_host, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?3)",
        params![session_id, writer_host, now],
    )?;
    Ok(())
}

/// Update `last_seen_at` for an existing session to keep it alive.
///
/// No-ops silently if the session does not exist.
pub fn touch(connection: &Connection, session_id: &str, now: i64) -> Result<()> {
    connection.execute(
        "UPDATE sessions SET last_seen_at = ?1 WHERE session_id = ?2",
        params![now, session_id],
    )?;
    Ok(())
}

/// Fetch a full session row, returning `None` if absent.
pub fn get(connection: &Connection, session_id: &str) -> Result<Option<SessionRow>> {
    connection
        .query_row(
            "SELECT session_id, writer_host, created_at, last_seen_at, graph_state
             FROM sessions WHERE session_id = ?1",
            params![session_id],
            row_from_sql,
        )
        .optional()
}

/// Return the writer host this session is pinned to, or `None` if the session
/// does not exist.
///
/// This is the primary entry-point for enforcing the writer-sticky guarantee
/// (INDEX-PLAN ID-19): the router calls this to determine where to send the
/// request.
pub fn writer_for(connection: &Connection, session_id: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT writer_host FROM sessions WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
}

/// Update the exploration graph state for a session.
///
/// No-ops silently if the session does not exist.
pub fn set_graph_state(
    connection: &Connection,
    session_id: &str,
    graph_state: Option<&str>,
    now: i64,
) -> Result<()> {
    connection.execute(
        "UPDATE sessions SET graph_state = ?1, last_seen_at = ?2 WHERE session_id = ?3",
        params![graph_state, now, session_id],
    )?;
    Ok(())
}

/// Delete a session, ending its writer-sticky binding.
///
/// No-ops silently if the session does not exist.
pub fn delete(connection: &Connection, session_id: &str) -> Result<()> {
    connection.execute("DELETE FROM sessions WHERE session_id = ?1", params![
        session_id
    ])?;
    Ok(())
}

/// Return all sessions whose `last_seen_at` is older than `cutoff` (for GC).
pub fn stale_sessions(connection: &Connection, cutoff: i64) -> Result<Vec<SessionRow>> {
    let mut statement = connection.prepare(
        "SELECT session_id, writer_host, created_at, last_seen_at, graph_state
         FROM sessions WHERE last_seen_at < ?1",
    )?;
    let rows = statement.query_map(params![cutoff], row_from_sql)?;
    rows.collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal row deserializer
// ─────────────────────────────────────────────────────────────────────────────

fn row_from_sql(row: &rusqlite::Row<'_>) -> Result<SessionRow> {
    Ok(SessionRow {
        session_id: row.get(0)?,
        writer_host: row.get(1)?,
        created_at: row.get(2)?,
        last_seen_at: row.get(3)?,
        graph_state: row.get(4)?,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn open_memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn).unwrap();
        conn
    }

    #[test]
    fn create_and_writer_for_round_trip() {
        let conn = open_memory();
        create_session(&conn, "sess-1", "writer-a.internal", 1000).unwrap();
        let host = writer_for(&conn, "sess-1").unwrap();
        assert_eq!(host.as_deref(), Some("writer-a.internal"));
    }

    #[test]
    fn unknown_session_returns_none() {
        let conn = open_memory();
        let host = writer_for(&conn, "nonexistent").unwrap();
        assert_eq!(host, None);
    }

    #[test]
    fn touch_updates_last_seen_at() {
        let conn = open_memory();
        create_session(&conn, "sess-2", "writer-b.internal", 1000).unwrap();
        touch(&conn, "sess-2", 9999).unwrap();
        let row = get(&conn, "sess-2").unwrap().unwrap();
        assert_eq!(row.last_seen_at, 9999);
        // writer_host must remain unchanged — sticky guarantee
        assert_eq!(row.writer_host, "writer-b.internal");
    }

    #[test]
    fn delete_ends_session() {
        let conn = open_memory();
        create_session(&conn, "sess-3", "writer-c.internal", 1).unwrap();
        delete(&conn, "sess-3").unwrap();
        assert_eq!(writer_for(&conn, "sess-3").unwrap(), None);
    }

    #[test]
    fn stale_sessions_returned() {
        let conn = open_memory();
        create_session(&conn, "old-sess", "writer-x.internal", 1).unwrap();
        create_session(&conn, "new-sess", "writer-y.internal", 9999).unwrap();
        let stale = stale_sessions(&conn, 500).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].session_id, "old-sess");
    }
}
