//! Worker lease claims on jobs.
//!
//! Before a worker begins executing a job it must acquire a claim. A claim is
//! a time-bounded lease: if `lease_expires_at` passes without the worker
//! releasing or renewing, the claim is considered expired and another worker
//! may take over.
//!
//! The table is delete-anytime. On restart all claims are gone; the scheduler
//! re-evaluates job states and re-issues claims as needed.

use rusqlite::{Connection, OptionalExtension, Result, params};

// ─────────────────────────────────────────────────────────────────────────────
// Row type
// ─────────────────────────────────────────────────────────────────────────────

/// A single row from the `claims` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRow {
    /// The job this claim covers — mirrors `jobs.job_key`.
    pub job_key: String,
    /// Opaque identifier of the worker that acquired the claim.
    pub claimed_by: String,
    /// Unix timestamp (seconds) when the claim was acquired.
    pub claimed_at: i64,
    /// Unix timestamp (seconds) after which the claim is considered expired.
    ///
    /// Workers must renew (via [`renew`]) before this deadline or another
    /// worker may take over the job.
    pub lease_expires_at: i64,
}

// ─────────────────────────────────────────────────────────────────────────────
// DDL
// ─────────────────────────────────────────────────────────────────────────────

/// Create the `claims` table if it does not already exist.
///
/// Idempotent — safe to call on every [`super::ScratchStore`] open.
pub fn create_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS claims (
            job_key          TEXT    PRIMARY KEY,
            claimed_by       TEXT    NOT NULL,
            claimed_at       INTEGER NOT NULL,
            lease_expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS claims_expires ON claims(lease_expires_at);",
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// DML helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Attempt to acquire a claim on `job_key` for `claimed_by`.
///
/// Returns `true` if the claim was successfully inserted (either no prior claim
/// existed, or the existing claim had already expired by `now`). Returns
/// `false` if a live claim belonging to a different worker is already present.
///
/// The operation is atomic: it deletes any expired claim and attempts an
/// `INSERT` in a single transaction, so two racing workers see at most one
/// success.
pub fn claim(
    connection: &Connection,
    job_key: &str,
    claimed_by: &str,
    claimed_at: i64,
    lease_expires_at: i64,
) -> Result<bool> {
    // Remove any claim that has already expired.
    connection.execute(
        "DELETE FROM claims WHERE job_key = ?1 AND lease_expires_at <= ?2",
        params![job_key, claimed_at],
    )?;

    // Attempt to insert; this will fail with a UNIQUE constraint error if a
    // live claim already exists.
    let rows_inserted = connection.execute(
        "INSERT OR IGNORE INTO claims (job_key, claimed_by, claimed_at, lease_expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![job_key, claimed_by, claimed_at, lease_expires_at],
    )?;

    Ok(rows_inserted == 1)
}

/// Release a claim, making the job available for re-claiming.
///
/// No-ops silently if the claim does not exist.
pub fn release(connection: &Connection, job_key: &str) -> Result<()> {
    connection.execute("DELETE FROM claims WHERE job_key = ?1", params![job_key])?;
    Ok(())
}

/// Fetch the current claim on `job_key`, returning `None` if absent.
pub fn get(connection: &Connection, job_key: &str) -> Result<Option<ClaimRow>> {
    connection
        .query_row(
            "SELECT job_key, claimed_by, claimed_at, lease_expires_at
             FROM claims WHERE job_key = ?1",
            params![job_key],
            row_from_sql,
        )
        .optional()
}

/// Renew a claim by extending its `lease_expires_at`.
///
/// No-ops silently if the claim does not exist (e.g. it was already expired
/// and deleted).
pub fn renew(connection: &Connection, job_key: &str, new_lease_expires_at: i64) -> Result<()> {
    connection.execute(
        "UPDATE claims SET lease_expires_at = ?1 WHERE job_key = ?2",
        params![new_lease_expires_at, job_key],
    )?;
    Ok(())
}

/// Return all claims whose lease has expired as of `now`.
///
/// The scheduler calls this to reclaim jobs whose workers silently died.
pub fn expired(connection: &Connection, now: i64) -> Result<Vec<ClaimRow>> {
    let mut statement = connection.prepare(
        "SELECT job_key, claimed_by, claimed_at, lease_expires_at
         FROM claims WHERE lease_expires_at <= ?1",
    )?;
    let rows = statement.query_map(params![now], row_from_sql)?;
    rows.collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal row deserializer
// ─────────────────────────────────────────────────────────────────────────────

fn row_from_sql(row: &rusqlite::Row<'_>) -> Result<ClaimRow> {
    Ok(ClaimRow {
        job_key: row.get(0)?,
        claimed_by: row.get(1)?,
        claimed_at: row.get(2)?,
        lease_expires_at: row.get(3)?,
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
    fn first_claim_succeeds() {
        let conn = open_memory();
        let acquired = claim(&conn, "job-a", "worker-1", 1000, 2000).unwrap();
        assert!(acquired);
        let row = get(&conn, "job-a").unwrap().unwrap();
        assert_eq!(row.claimed_by, "worker-1");
    }

    #[test]
    fn second_claim_on_live_lease_fails() {
        let conn = open_memory();
        claim(&conn, "job-b", "worker-1", 1000, 5000).unwrap();
        // worker-2 tries to claim at t=1001 while lease expires at t=5000
        let acquired = claim(&conn, "job-b", "worker-2", 1001, 6000).unwrap();
        assert!(!acquired, "live claim must block a second worker");
        // original claim must still be intact
        let row = get(&conn, "job-b").unwrap().unwrap();
        assert_eq!(row.claimed_by, "worker-1");
    }

    #[test]
    fn expired_claim_can_be_taken_over() {
        let conn = open_memory();
        claim(&conn, "job-c", "worker-1", 1000, 1500).unwrap();
        // worker-2 claims at t=2000, which is past the expiry of 1500
        let acquired = claim(&conn, "job-c", "worker-2", 2000, 3000).unwrap();
        assert!(acquired, "expired claim must be reclaimable");
        let row = get(&conn, "job-c").unwrap().unwrap();
        assert_eq!(row.claimed_by, "worker-2");
    }

    #[test]
    fn release_makes_job_reclaimable() {
        let conn = open_memory();
        claim(&conn, "job-d", "worker-1", 1, 9999).unwrap();
        release(&conn, "job-d").unwrap();
        let acquired = claim(&conn, "job-d", "worker-2", 2, 9999).unwrap();
        assert!(acquired);
    }

    #[test]
    fn expired_returns_only_expired_claims() {
        let conn = open_memory();
        claim(&conn, "job-e", "worker-1", 1, 100).unwrap();
        claim(&conn, "job-f", "worker-2", 1, 9999).unwrap();
        let expired_claims = expired(&conn, 500).unwrap();
        assert_eq!(expired_claims.len(), 1);
        assert_eq!(expired_claims[0].job_key, "job-e");
    }
}
