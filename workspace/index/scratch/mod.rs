//! Ephemeral scratch store — `scratch.sqlite`.
//!
//! # Purpose
//!
//! The scratch store holds the scheduler's working state: job queue, long-poll
//! wanted records, writer-sticky exploration sessions, and worker lease claims
//! (INDEX-PLAN ID-2, ID-19, §2, §11, §13). It is **not** part of the
//! versioned catalog: it is never replicated to remotes, never snapshotted by
//! Dolt, and may be deleted at any time. On restart the scheduler rebuilds its
//! in-memory picture from the durable catalog and re-enqueues whatever work is
//! still outstanding.
//!
//! # Storage
//!
//! Plain [`rusqlite::Connection`]. The schema is initialized automatically on
//! [`ScratchStore::open`] or [`ScratchStore::open_in_memory`] via
//! `CREATE TABLE IF NOT EXISTS`, so re-opening a deleted database is safe.
//!
//! # Module layout
//!
//! Each table lives in its own sub-module with its row type and free functions.
//! [`ScratchStore`] wraps a connection and delegates to those free functions
//! through typed methods, keeping the public API in one place.

pub mod claims;
pub mod jobs;
pub mod sessions;
pub mod wanted;

// Re-export row and enum types so callers can use them without reaching into
// the sub-modules.
pub use claims::ClaimRow;
pub use jobs::{JobRow, JobState};
pub use sessions::SessionRow;
pub use wanted::WantedRow;

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors that can arise from scratch-store operations.
#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    /// A rusqlite operation failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The requested row does not exist.
    ///
    /// Returned by operations that require a row to be present (e.g. strict
    /// session lookup). Operations that tolerate absence return `Option` instead.
    #[error("scratch row not found: {description}")]
    NotFound {
        /// Human-readable description of what was missing (table + key).
        description: String,
    },
}

impl ScratchError {
    /// Whether this error is a SQLite `UNIQUE`/primary-key constraint violation.
    ///
    /// Consumers that treat "row already exists" as success (e.g. an idempotent
    /// `create_session` racing a concurrent creator) can branch on this without
    /// taking a direct `rusqlite` dependency to pattern-match the error code.
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            ScratchError::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ScratchStore
// ─────────────────────────────────────────────────────────────────────────────

/// The ephemeral scratch store backed by a single SQLite connection.
///
/// Open with [`ScratchStore::open`] (file-backed, WAL mode) or
/// [`ScratchStore::open_in_memory`] (in-process, useful in tests). Both paths
/// call [`initialize_schema`] internally so the store is always ready
/// immediately after construction.
///
/// All methods on this type are thin delegates to the free functions in the
/// sub-modules; prefer the typed methods over calling sub-module functions
/// directly.
#[derive(Debug)]
pub struct ScratchStore {
    connection: rusqlite::Connection,
}

impl ScratchStore {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Open (or create) a file-backed scratch database at `path`.
    ///
    /// WAL journal mode and foreign-key enforcement are both activated before
    /// the schema is initialized. The file is created if it does not exist.
    pub fn open(path: &std::path::Path) -> Result<Self, ScratchError> {
        let connection = rusqlite::Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        initialize_schema(&connection)?;
        Ok(Self { connection })
    }

    /// Open an in-memory scratch database.
    ///
    /// Foreign-key enforcement is activated. WAL mode is intentionally skipped
    /// because in-memory databases do not support it.
    ///
    /// Primarily used in tests and short-lived single-process contexts.
    pub fn open_in_memory() -> Result<Self, ScratchError> {
        let connection = rusqlite::Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        initialize_schema(&connection)?;
        Ok(Self { connection })
    }

    // ── jobs ──────────────────────────────────────────────────────────────────

    /// Insert a new job into the queue.
    ///
    /// Fails with a constraint error if `row.job_key` already exists.
    pub fn enqueue_job(&self, row: &JobRow) -> Result<(), ScratchError> {
        jobs::enqueue(&self.connection, row)?;
        Ok(())
    }

    /// Fetch a single job by its key, returning `None` if absent.
    pub fn get_job(&self, job_key: &str) -> Result<Option<JobRow>, ScratchError> {
        Ok(jobs::get(&self.connection, job_key)?)
    }

    /// Transition a job to a new state, updating its `updated_at` timestamp.
    pub fn set_job_state(
        &self,
        job_key: &str,
        state: JobState,
        updated_at: i64,
    ) -> Result<(), ScratchError> {
        jobs::set_state(&self.connection, job_key, state, updated_at)?;
        Ok(())
    }

    /// Return up to `limit` queued jobs ordered by `enqueued_at` ascending.
    pub fn next_queued_jobs(&self, limit: i64) -> Result<Vec<JobRow>, ScratchError> {
        Ok(jobs::next_queued(&self.connection, limit)?)
    }

    /// Increment the attempt counter for a job.
    pub fn increment_job_attempts(
        &self,
        job_key: &str,
        updated_at: i64,
    ) -> Result<(), ScratchError> {
        jobs::increment_attempts(&self.connection, job_key, updated_at)?;
        Ok(())
    }

    // ── wanted ────────────────────────────────────────────────────────────────

    /// Record that a client wants `coordinate` compiled, returning the new row id.
    pub fn add_wanted(
        &self,
        coordinate: &str,
        requested_at: i64,
    ) -> Result<i64, ScratchError> {
        Ok(wanted::add(&self.connection, coordinate, requested_at)?)
    }

    /// Return all unfulfilled wanted rows, oldest first.
    pub fn list_unfulfilled_wanted(&self) -> Result<Vec<WantedRow>, ScratchError> {
        Ok(wanted::list_unfulfilled(&self.connection)?)
    }

    /// Mark a wanted row as fulfilled.
    pub fn mark_wanted_fulfilled(&self, id: i64) -> Result<(), ScratchError> {
        wanted::mark_fulfilled(&self.connection, id)?;
        Ok(())
    }

    // ── sessions ──────────────────────────────────────────────────────────────

    /// Open a new session pinned to `writer_host`.
    pub fn create_session(
        &self,
        session_id: &str,
        writer_host: &str,
        now: i64,
    ) -> Result<(), ScratchError> {
        sessions::create_session(&self.connection, session_id, writer_host, now)?;
        Ok(())
    }

    /// Update `last_seen_at` for an existing session.
    pub fn touch_session(&self, session_id: &str, now: i64) -> Result<(), ScratchError> {
        sessions::touch(&self.connection, session_id, now)?;
        Ok(())
    }

    /// Fetch a full session row, returning `None` if absent.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRow>, ScratchError> {
        Ok(sessions::get(&self.connection, session_id)?)
    }

    /// Return the writer host this session is pinned to, or `None` if absent.
    ///
    /// This is the primary entry-point for the writer-sticky guarantee
    /// (INDEX-PLAN ID-19). The router calls this before dispatching any request
    /// that belongs to an existing session.
    pub fn writer_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, ScratchError> {
        Ok(sessions::writer_for(&self.connection, session_id)?)
    }

    /// Return the JSON-encoded exploration-graph state for a session, or `None`
    /// when the session does not exist or has no graph recorded yet.
    ///
    /// Exploration sessions (interactive symbol navigation) accumulate a
    /// join-semilattice graph in the `graph_state` column. This accessor is the
    /// read half of that store; [`ScratchStore::set_session_graph_state`] is the
    /// write half.
    pub fn session_graph_state(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, ScratchError> {
        Ok(sessions::get(&self.connection, session_id)?.and_then(|row| row.graph_state))
    }

    /// Replace the JSON-encoded exploration-graph state for a session, updating
    /// its `last_seen_at`. No-ops silently if the session does not exist, so
    /// callers must [`ScratchStore::create_session`] first.
    pub fn set_session_graph_state(
        &self,
        session_id: &str,
        graph_state: Option<&str>,
        now: i64,
    ) -> Result<(), ScratchError> {
        sessions::set_graph_state(&self.connection, session_id, graph_state, now)?;
        Ok(())
    }

    /// Delete a session.
    pub fn delete_session(&self, session_id: &str) -> Result<(), ScratchError> {
        sessions::delete(&self.connection, session_id)?;
        Ok(())
    }

    // ── claims ────────────────────────────────────────────────────────────────

    /// Attempt to acquire a lease claim on `job_key` for `claimed_by`.
    ///
    /// Returns `true` on success, `false` if a live claim already exists.
    pub fn claim_job(
        &self,
        job_key: &str,
        claimed_by: &str,
        claimed_at: i64,
        lease_expires_at: i64,
    ) -> Result<bool, ScratchError> {
        Ok(claims::claim(
            &self.connection,
            job_key,
            claimed_by,
            claimed_at,
            lease_expires_at,
        )?)
    }

    /// Fetch the current claim on `job_key`, returning `None` if absent.
    ///
    /// The read half of the lease protocol: a settling worker calls this to
    /// re-check it still owns a live claim before committing a terminal
    /// transition (the scratch analog of the postgres `lease_still_held` guard).
    pub fn get_job_claim(&self, job_key: &str) -> Result<Option<ClaimRow>, ScratchError> {
        Ok(claims::get(&self.connection, job_key)?)
    }

    /// Release a job claim.
    pub fn release_job_claim(&self, job_key: &str) -> Result<(), ScratchError> {
        claims::release(&self.connection, job_key)?;
        Ok(())
    }

    /// Renew a job claim with a new `lease_expires_at`.
    pub fn renew_job_claim(
        &self,
        job_key: &str,
        new_lease_expires_at: i64,
    ) -> Result<(), ScratchError> {
        claims::renew(&self.connection, job_key, new_lease_expires_at)?;
        Ok(())
    }

    /// Drop every job claim — restart recovery: a fresh process holds no
    /// leases, so any surviving claim row is a stale artifact of the crash.
    pub fn clear_all_job_claims(&self) -> Result<usize, ScratchError> {
        self.connection
            .execute("DELETE FROM claims", [])
            .map_err(ScratchError::from)
    }

    /// Return all claims whose lease has expired as of `now`.

    pub fn expired_claims(&self, now: i64) -> Result<Vec<ClaimRow>, ScratchError> {
        Ok(claims::expired(&self.connection, now)?)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Schema initialization (private)
// ─────────────────────────────────────────────────────────────────────────────

/// Create all four scratch tables if they do not already exist.
///
/// Called once during [`ScratchStore::open`] / [`ScratchStore::open_in_memory`].
/// All DDL statements use `CREATE TABLE IF NOT EXISTS` so this is idempotent.
fn initialize_schema(connection: &rusqlite::Connection) -> Result<(), ScratchError> {
    jobs::create_table(connection)?;
    wanted::create_table(connection)?;
    sessions::create_table(connection)?;
    claims::create_table(connection)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Integration tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ScratchStore {
        ScratchStore::open_in_memory().expect("in-memory open must not fail")
    }

    // ── jobs round-trip ───────────────────────────────────────────────────────

    #[test]
    fn jobs_enqueue_and_get_round_trip() {
        let scratch = store();
        let row = JobRow {
            job_key: "deadbeef".to_owned(),
            kind: "ingest_package".to_owned(),
            state: JobState::Queued,
            attempts: 0,
            enqueued_at: 1_000,
            updated_at: 1_000,
            payload: Some(r#"{"pkg":"rand"}"#.to_owned()),
        };
        scratch.enqueue_job(&row).expect("enqueue must succeed");
        let fetched = scratch
            .get_job("deadbeef")
            .expect("get must not error")
            .expect("row must exist");
        assert_eq!(fetched.job_key, "deadbeef");
        assert_eq!(fetched.state, JobState::Queued);
        assert_eq!(fetched.payload.as_deref(), Some(r#"{"pkg":"rand"}"#));
    }

    // ── wanted round-trip ─────────────────────────────────────────────────────

    #[test]
    fn wanted_add_and_list_unfulfilled_round_trip() {
        let scratch = store();
        let id = scratch
            .add_wanted("tokio@1.35.0", 500)
            .expect("add must succeed");
        let unfulfilled = scratch
            .list_unfulfilled_wanted()
            .expect("list must not error");
        assert_eq!(unfulfilled.len(), 1);
        assert_eq!(unfulfilled[0].id, id);
        assert_eq!(unfulfilled[0].coordinate, "tokio@1.35.0");
        assert!(!unfulfilled[0].fulfilled);

        scratch
            .mark_wanted_fulfilled(id)
            .expect("mark must succeed");
        let after_fulfill = scratch
            .list_unfulfilled_wanted()
            .expect("list must not error");
        assert!(after_fulfill.is_empty());
    }

    // ── sessions round-trip ───────────────────────────────────────────────────

    #[test]
    fn sessions_create_and_writer_for_round_trip() {
        let scratch = store();
        scratch
            .create_session("sess-abc", "writer-primary.internal", 1000)
            .expect("create must succeed");

        let writer = scratch
            .writer_for_session("sess-abc")
            .expect("lookup must not error");
        assert_eq!(
            writer.as_deref(),
            Some("writer-primary.internal"),
            "writer-sticky: returned host must match the one at session creation"
        );

        // touch should not change the writer host
        scratch
            .touch_session("sess-abc", 9999)
            .expect("touch must succeed");
        let writer_after_touch = scratch
            .writer_for_session("sess-abc")
            .expect("lookup must not error");
        assert_eq!(writer_after_touch.as_deref(), Some("writer-primary.internal"));
    }

    #[test]
    fn sessions_graph_state_round_trip() {
        let scratch = store();
        scratch
            .create_session("sess-graph", "writer-graph.internal", 1000)
            .expect("create must succeed");

        // A freshly created session has no graph recorded yet.
        assert_eq!(
            scratch
                .session_graph_state("sess-graph")
                .expect("read must not error"),
            None
        );

        // Writing then reading round-trips the JSON payload verbatim.
        scratch
            .set_session_graph_state("sess-graph", Some(r#"{"nodes":[1,2]}"#), 2000)
            .expect("set must succeed");
        assert_eq!(
            scratch
                .session_graph_state("sess-graph")
                .expect("read must not error")
                .as_deref(),
            Some(r#"{"nodes":[1,2]}"#)
        );

        // Setting an unknown session no-ops and never errors.
        scratch
            .set_session_graph_state("no-such-session", Some("{}"), 3000)
            .expect("set on unknown session must be a silent no-op");
    }

    #[test]
    fn sessions_unknown_returns_none() {
        let scratch = store();
        let result = scratch
            .writer_for_session("no-such-session")
            .expect("lookup must not error");
        assert_eq!(result, None);
    }

    // ── claims round-trip — second claim must fail ────────────────────────────

    #[test]
    fn claims_second_live_claim_fails() {
        let scratch = store();

        let first = scratch
            .claim_job("job-xyz", "worker-1", 1000, 5000)
            .expect("first claim must not error");
        assert!(first, "first claim must succeed");

        // Worker 2 tries to claim while worker 1's lease is still live (expires 5000).
        let second = scratch
            .claim_job("job-xyz", "worker-2", 1001, 6000)
            .expect("second claim must not error");
        assert!(!second, "second claim on a live lease must fail");
    }

    #[test]
    fn claims_expired_lease_can_be_taken_over() {
        let scratch = store();
        scratch
            .claim_job("job-yyy", "worker-1", 1000, 1500)
            .expect("initial claim must succeed");

        // At t=2000 the lease has expired; worker-2 should succeed.
        let taken_over = scratch
            .claim_job("job-yyy", "worker-2", 2000, 4000)
            .expect("takeover must not error");
        assert!(taken_over, "expired claim must be reclaimable");
    }
}
