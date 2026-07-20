//! Long-poll "wanted" records: a client has requested compilation of a
//! coordinate and is waiting for the result (INDEX-PLAN §11).
//!
//! Rows are ephemeral. On restart all unfulfilled wants are replayed or the
//! client times out and re-requests. The `wanted` table is delete-anytime.

use rusqlite::{Connection, OptionalExtension, Result, params};

// ─────────────────────────────────────────────────────────────────────────────
// Row type
// ─────────────────────────────────────────────────────────────────────────────

/// A single row from the `wanted` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedRow {
    /// Auto-assigned row id (used as the handle for `mark_fulfilled`).
    pub id: i64,
    /// Canonical `pkg@version` coordinate that the client requested.
    pub coordinate: String,
    /// Unix timestamp (seconds) when the request arrived.
    pub requested_at: i64,
    /// `true` once the corresponding job has reached the `Done` state.
    pub fulfilled: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// DDL
// ─────────────────────────────────────────────────────────────────────────────

/// Create the `wanted` table if it does not already exist.
///
/// Idempotent — safe to call on every [`super::ScratchStore`] open.
pub fn create_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS wanted (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            coordinate   TEXT    NOT NULL,
            requested_at INTEGER NOT NULL,
            fulfilled    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS wanted_coordinate ON wanted(coordinate);
        CREATE INDEX IF NOT EXISTS wanted_fulfilled  ON wanted(fulfilled);",
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// DML helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Record that a client wants `coordinate` compiled, returning the new row id.
pub fn add(connection: &Connection, coordinate: &str, requested_at: i64) -> Result<i64> {
    connection.execute(
        "INSERT INTO wanted (coordinate, requested_at) VALUES (?1, ?2)",
        params![coordinate, requested_at],
    )?;
    Ok(connection.last_insert_rowid())
}

/// Return every `wanted` row that has not yet been fulfilled, ordered by
/// `requested_at` ascending (oldest first).
pub fn list_unfulfilled(connection: &Connection) -> Result<Vec<WantedRow>> {
    let mut statement = connection.prepare(
        "SELECT id, coordinate, requested_at, fulfilled
         FROM wanted WHERE fulfilled = 0
         ORDER BY requested_at ASC",
    )?;
    let rows = statement.query_map([], row_from_sql)?;
    rows.collect()
}

/// Look up a single `wanted` row by its id, returning `None` if absent.
pub fn get(connection: &Connection, id: i64) -> Result<Option<WantedRow>> {
    connection
        .query_row(
            "SELECT id, coordinate, requested_at, fulfilled FROM wanted WHERE id = ?1",
            params![id],
            row_from_sql,
        )
        .optional()
}

/// Mark a `wanted` row as fulfilled.
///
/// No-ops silently if the id does not exist.
pub fn mark_fulfilled(connection: &Connection, id: i64) -> Result<()> {
    connection.execute(
        "UPDATE wanted SET fulfilled = 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal row deserializer
// ─────────────────────────────────────────────────────────────────────────────

fn row_from_sql(row: &rusqlite::Row<'_>) -> Result<WantedRow> {
    let fulfilled_int: i64 = row.get(3)?;
    Ok(WantedRow {
        id: row.get(0)?,
        coordinate: row.get(1)?,
        requested_at: row.get(2)?,
        fulfilled: fulfilled_int != 0,
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
    fn add_and_list_unfulfilled() {
        let conn = open_memory();
        let id1 = add(&conn, "serde@1.0.0", 1000).unwrap();
        let id2 = add(&conn, "tokio@1.35.0", 1001).unwrap();
        let unfulfilled = list_unfulfilled(&conn).unwrap();
        assert_eq!(unfulfilled.len(), 2);
        assert_eq!(unfulfilled[0].id, id1);
        assert_eq!(unfulfilled[1].id, id2);
    }

    #[test]
    fn mark_fulfilled_removes_from_list() {
        let conn = open_memory();
        let id = add(&conn, "anyhow@1.0.0", 999).unwrap();
        mark_fulfilled(&conn, id).unwrap();
        let unfulfilled = list_unfulfilled(&conn).unwrap();
        assert!(unfulfilled.is_empty());
        let row = get(&conn, id).unwrap().unwrap();
        assert!(row.fulfilled);
    }
}
