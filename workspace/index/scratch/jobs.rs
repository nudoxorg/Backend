//! Ingest and producer work items stored in the ephemeral `jobs` table.
//!
//! Jobs are the scheduler's unit of work. Each row represents one unit of
//! ingest or produce work — keyed by a hex string that callers derive from a
//! `heart::JobKey` or a UUID. The table is delete-anytime; incomplete jobs are
//! simply re-enqueued on restart.

use rusqlite::{Connection, OptionalExtension, Result, params};

// ─────────────────────────────────────────────────────────────────────────────
// State enum
// ─────────────────────────────────────────────────────────────────────────────

/// The lifecycle state of a [`JobRow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    /// Waiting to be picked up by a worker.
    Queued,
    /// Reserved by a worker but not yet running (claim recorded in `claims`).
    Claimed,
    /// Actively being processed by a worker.
    Running,
    /// Completed successfully.
    Done,
    /// Permanently failed (all retries exhausted or non-retryable error).
    Failed,
}

impl JobState {
    /// The stored token for this state.
    pub fn as_token(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Claimed => "claimed",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    /// Parse a stored token back into a [`JobState`].
    ///
    /// Returns `None` on an unrecognized token rather than panicking.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "queued" => Some(JobState::Queued),
            "claimed" => Some(JobState::Claimed),
            "running" => Some(JobState::Running),
            "done" => Some(JobState::Done),
            "failed" => Some(JobState::Failed),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Row type
// ─────────────────────────────────────────────────────────────────────────────

/// A single row from the `jobs` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    /// Hex-encoded job key (derived from `heart::JobKey::hex()` or a UUID).
    pub job_key: String,
    /// Free-form kind tag identifying what kind of work this job represents
    /// (e.g. `"ingest_package"`, `"produce_ir"`).
    pub kind: String,
    /// Current lifecycle state.
    pub state: JobState,
    /// Number of times this job has been attempted.
    pub attempts: i64,
    /// Unix timestamp (seconds) when this job was first enqueued.
    pub enqueued_at: i64,
    /// Unix timestamp (seconds) of the most recent state transition.
    pub updated_at: i64,
    /// Optional JSON payload carrying job-specific parameters.
    pub payload: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// DDL
// ─────────────────────────────────────────────────────────────────────────────

/// Create the `jobs` table if it does not already exist.
///
/// Idempotent — safe to call on every [`super::ScratchStore`] open.
pub fn create_table(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            job_key     TEXT    PRIMARY KEY,
            kind        TEXT    NOT NULL,
            state       TEXT    NOT NULL,
            attempts    INTEGER NOT NULL DEFAULT 0,
            enqueued_at INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL,
            payload     TEXT
        );
        CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state);",
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// DML helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Insert a new job row.
///
/// Fails with a constraint error if `job_key` already exists.
pub fn enqueue(connection: &Connection, row: &JobRow) -> Result<()> {
    connection.execute(
        "INSERT INTO jobs (job_key, kind, state, attempts, enqueued_at, updated_at, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.job_key,
            row.kind,
            row.state.as_token(),
            row.attempts,
            row.enqueued_at,
            row.updated_at,
            row.payload,
        ],
    )?;
    Ok(())
}

/// Fetch a single job by its key, returning `None` if absent.
pub fn get(connection: &Connection, job_key: &str) -> Result<Option<JobRow>> {
    connection
        .query_row(
            "SELECT job_key, kind, state, attempts, enqueued_at, updated_at, payload
             FROM jobs WHERE job_key = ?1",
            params![job_key],
            row_from_sql,
        )
        .optional()
}

/// Transition a job to a new state, updating its `updated_at` timestamp.
pub fn set_state(
    connection: &Connection,
    job_key: &str,
    state: JobState,
    updated_at: i64,
) -> Result<()> {
    connection.execute(
        "UPDATE jobs SET state = ?1, updated_at = ?2 WHERE job_key = ?3",
        params![state.as_token(), updated_at, job_key],
    )?;
    Ok(())
}

/// Return up to `limit` jobs in the `Queued` state, ordered by `enqueued_at`.
pub fn next_queued(connection: &Connection, limit: i64) -> Result<Vec<JobRow>> {
    let mut statement = connection.prepare(
        "SELECT job_key, kind, state, attempts, enqueued_at, updated_at, payload
         FROM jobs WHERE state = 'queued'
         ORDER BY enqueued_at ASC
         LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit], row_from_sql)?;
    rows.collect()
}

/// Return a terminal job to `Queued` with a fresh attempt budget.
///
/// # Why re-queueing needs its own function
///
/// [`set_state`] alone would move a `Failed` row back to `Queued` while leaving
/// `attempts` at whatever exhausted the retry policy, so the very next failure
/// would dead-letter it immediately — a "retry" that gets one shot at best and
/// none at worst. `enqueued_at` is reset too, so the row takes its place at the
/// back of `next_queued`'s FIFO rather than jumping ahead of work that has been
/// waiting longer.
///
/// Callers must check the current state first: this is for jobs in a *terminal*
/// state (`Done`/`Failed`). Applying it to a `Claimed`/`Running` job would
/// hand a second worker a job someone already holds.
pub fn requeue_terminal(connection: &Connection, job_key: &str, now: i64) -> Result<()> {
    connection.execute(
        "UPDATE jobs SET state = ?1, attempts = 0, enqueued_at = ?2, updated_at = ?2
         WHERE job_key = ?3 AND state IN ('done', 'failed')",
        params![JobState::Queued.as_token(), now, job_key],
    )?;
    Ok(())
}

/// Increment the attempt counter for a job (call before each execution attempt).
pub fn increment_attempts(connection: &Connection, job_key: &str, updated_at: i64) -> Result<()> {
    connection.execute(
        "UPDATE jobs SET attempts = attempts + 1, updated_at = ?1 WHERE job_key = ?2",
        params![updated_at, job_key],
    )?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal row deserializer
// ─────────────────────────────────────────────────────────────────────────────

fn row_from_sql(row: &rusqlite::Row<'_>) -> Result<JobRow> {
    let state_token: String = row.get(2)?;
    let state = JobState::from_token(&state_token).unwrap_or(JobState::Failed);
    Ok(JobRow {
        job_key: row.get(0)?,
        kind: row.get(1)?,
        state,
        attempts: row.get(3)?,
        enqueued_at: row.get(4)?,
        updated_at: row.get(5)?,
        payload: row.get(6)?,
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
    fn enqueue_and_get_round_trips() {
        let conn = open_memory();
        let row = JobRow {
            job_key: "abc123".to_owned(),
            kind: "ingest_package".to_owned(),
            state: JobState::Queued,
            attempts: 0,
            enqueued_at: 1_000_000,
            updated_at: 1_000_000,
            payload: Some(r#"{"pkg":"serde"}"#.to_owned()),
        };
        enqueue(&conn, &row).unwrap();
        let fetched = get(&conn, "abc123").unwrap().expect("row should exist");
        assert_eq!(fetched, row);
    }

    #[test]
    fn set_state_transitions() {
        let conn = open_memory();
        let row = JobRow {
            job_key: "key1".to_owned(),
            kind: "produce_ir".to_owned(),
            state: JobState::Queued,
            attempts: 0,
            enqueued_at: 1,
            updated_at: 1,
            payload: None,
        };
        enqueue(&conn, &row).unwrap();
        set_state(&conn, "key1", JobState::Done, 2).unwrap();
        let fetched = get(&conn, "key1").unwrap().unwrap();
        assert_eq!(fetched.state, JobState::Done);
        assert_eq!(fetched.updated_at, 2);
    }

    #[test]
    fn next_queued_ordering() {
        let conn = open_memory();
        for i in 0..3i64 {
            enqueue(
                &conn,
                &JobRow {
                    job_key: format!("k{i}"),
                    kind: "t".to_owned(),
                    state: JobState::Queued,
                    attempts: 0,
                    enqueued_at: 10 - i, // reverse order
                    updated_at: 1,
                    payload: None,
                },
            )
            .unwrap();
        }
        let queued = next_queued(&conn, 10).unwrap();
        // should be ascending enqueued_at: 8, 9, 10 → keys k2, k1, k0
        assert_eq!(queued[0].job_key, "k2");
        assert_eq!(queued[2].job_key, "k0");
    }

    #[test]
    fn job_state_token_round_trips() {
        for (state, token) in [
            (JobState::Queued, "queued"),
            (JobState::Claimed, "claimed"),
            (JobState::Running, "running"),
            (JobState::Done, "done"),
            (JobState::Failed, "failed"),
        ] {
            assert_eq!(state.as_token(), token);
            assert_eq!(JobState::from_token(token), Some(state));
        }
        assert_eq!(JobState::from_token("bogus"), None);
    }
}
