//! Hashes of Qdrant points whose upsert already succeeded.
//!
//! The in-memory ledger is rebuilt from this table on process start. A missing
//! or corrupt row is rewritten; a recorded hash is not. Rows are written only
//! after `VectorStore::upsert` returns. Deleting a package's points deletes
//! its rows in the same step, so a later re-delivery is not skipped.

use rusqlite::{Connection, Result, params};

/// Create the `vector_points` table if it does not already exist.
pub fn create_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS vector_points (
            package TEXT NOT NULL,
            intro   TEXT NOT NULL,
            hash    BLOB NOT NULL,
            PRIMARY KEY (package, intro)
        );",
    )
}

/// Insert or replace the content hash of one point.
pub fn record(connection: &Connection, package: &str, intro: &str, hash: &[u8; 32]) -> Result<()> {
    connection.execute(
        "INSERT INTO vector_points (package, intro, hash) VALUES (?1, ?2, ?3)
         ON CONFLICT(package, intro) DO UPDATE SET hash = excluded.hash",
        params![package, intro, hash.as_slice()],
    )?;
    Ok(())
}

/// Every recorded hash whose blob is 32 bytes. A shorter or longer blob is
/// omitted so a torn write cannot suppress the next upsert.
pub fn load(connection: &Connection) -> Result<Vec<(String, String, [u8; 32])>> {
    let mut statement = connection.prepare("SELECT package, intro, hash FROM vector_points")?;
    let rows = statement.query_map([], |row| {
        let package: String = row.get(0)?;
        let intro: String = row.get(1)?;
        let hash: Vec<u8> = row.get(2)?;
        Ok((package, intro, hash))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (package, intro, hash) = row?;
        let Ok(hash) = <[u8; 32]>::try_from(hash) else {
            continue;
        };
        out.push((package, intro, hash));
    }
    Ok(out)
}

/// Drop every recorded hash for `package`.
pub fn forget_package(connection: &Connection, package: &str) -> Result<()> {
    connection.execute("DELETE FROM vector_points WHERE package = ?1", params![
        package
    ])?;
    Ok(())
}
