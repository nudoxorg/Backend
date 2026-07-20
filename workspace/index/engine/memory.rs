//! A test-only in-memory implementation of the engine facade, backed by
//! rusqlite (INDEX-PLAN ID-20).
//!
//! **This is never a product mode.** rusqdoltlite (the real versioned engine)
//! is built by another workstream and may not compile yet; this fake lets the
//! catalog's unit and property tests run without it. The versioning calls are
//! *honest fakes*: `dolt_commit` appends to an in-memory commit log (a real hash
//! over the message + parent + a monotonic clock), `head`/`resolve_as_of_time`
//! read that log, and `dolt_branch_create`/`dolt_checkout` track a branch map.
//! No prolly tree, no real time-travel of table state — enough to exercise the
//! catalog's control flow, never enough to ship.
//!
//! Gated on `feature = "test-engine"` (default in this crate) so it is compiled
//! for tests but is trivial to exclude from a hardened build.
#![cfg(feature = "test-engine")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use rusqlite::types::{Value as SqliteValue, ValueRef};
use rusqlite::Connection as SqliteConnection;

use super::{
    BranchName, CatalogEngine, CommitHash, EngineError, MergeOutcome, Row, Value, VersioningEngine,
};

/// A monotonic logical clock so `resolve_as_of_time` has a total order even when
/// several fake commits share a wall-clock millisecond.
static LOGICAL_CLOCK: AtomicI64 = AtomicI64::new(0);

/// One entry in the honest fake commit log.
#[derive(Debug, Clone)]
struct FakeCommit {
    hash: CommitHash,
    /// The `unix_milliseconds` the writer stamped this commit with. The writer
    /// passes it via the last `SET @commit_time` convention below; when absent
    /// we fall back to the logical clock so ordering is still total.
    at_unix_milliseconds: i64,
}

/// The recorded history for one branch.
#[derive(Debug, Default, Clone)]
struct BranchHistory {
    commits: Vec<FakeCommit>,
}

/// In-memory catalog engine: a single rusqlite connection plus a fake commit
/// log per branch. `Mutex` interior mutability mirrors the `&self` method shape
/// of the real binding *and* keeps the type `Send + Sync`, which the
/// [`Catalog`](crate::store::Catalog) supertrait bound requires. The
/// transaction method never holds the connection lock across the body callback,
/// so the re-entrant `execute`/`query_rows` inside a transaction cannot
/// deadlock.
pub struct MemoryEngine {
    connection: Mutex<SqliteConnection>,
    branches: Mutex<HashMap<String, BranchHistory>>,
    current_branch: Mutex<String>,
    /// The `unix_milliseconds` the next `dolt_commit` should stamp, if the
    /// writer set one via [`MemoryEngine::stage_commit_time`]. Honest-fake stand-in
    /// for the engine reading the transaction's commit timestamp.
    pending_commit_time: Mutex<Option<i64>>,
}

impl MemoryEngine {
    /// Open a fresh in-memory catalog with an empty `main` branch history.
    pub fn open_in_memory() -> Result<Self, EngineError> {
        let connection = SqliteConnection::open_in_memory()
            .map_err(|error| EngineError::Open(error.to_string()))?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|error| EngineError::Open(error.to_string()))?;
        let mut branches = HashMap::new();
        branches.insert("main".to_owned(), BranchHistory::default());
        Ok(Self {
            connection: Mutex::new(connection),
            branches: Mutex::new(branches),
            current_branch: Mutex::new("main".to_owned()),
            pending_commit_time: Mutex::new(None),
        })
    }

    /// Test hook: stamp the next `dolt_commit` with an explicit instant so
    /// `resolve_as_of_time` boundary cases are deterministic.
    pub fn stage_commit_time(&self, unix_milliseconds: i64) {
        *self.locked_pending_commit_time() = Some(unix_milliseconds);
    }

    fn locked_pending_commit_time(&self) -> std::sync::MutexGuard<'_, Option<i64>> {
        self.pending_commit_time
            .lock()
            .expect("pending_commit_time mutex poisoned")
    }

    fn next_logical(&self) -> i64 {
        LOGICAL_CLOCK.fetch_add(1, Ordering::SeqCst)
    }

    fn current_branch_name(&self) -> String {
        self.current_branch
            .lock()
            .expect("current_branch mutex poisoned")
            .clone()
    }

    fn lock_branches(
        &self,
        operation: &'static str,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, BranchHistory>>, EngineError> {
        self.branches.lock().map_err(|_| EngineError::Versioning {
            operation,
            detail: "branches mutex poisoned".to_owned(),
        })
    }
}

/// Translate a facade [`Value`] into a rusqlite bind value.
fn to_sqlite(value: &Value) -> SqliteValue {
    match value {
        Value::Null => SqliteValue::Null,
        Value::Integer(integer) => SqliteValue::Integer(*integer),
        Value::Real(real) => SqliteValue::Real(*real),
        Value::Text(text) => SqliteValue::Text(text.clone()),
        Value::Blob(blob) => SqliteValue::Blob(blob.clone()),
    }
}

/// A borrowed rusqlite row presented behind the facade's [`Row`] trait.
struct SqliteRow<'a, 'stmt> {
    inner: &'a rusqlite::Row<'stmt>,
}

impl<'a, 'stmt> Row for SqliteRow<'a, 'stmt> {
    fn get_integer(&self, index: usize) -> Result<i64, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Integer(value)) => Ok(value),
            Ok(ValueRef::Null) => Err(EngineError::UnexpectedNull { index }),
            Ok(other) => Err(EngineError::UnexpectedColumnType {
                index,
                detail: format!("expected INTEGER, found {:?}", other.data_type()),
            }),
            Err(error) => Err(EngineError::Statement(error.to_string())),
        }
    }

    fn get_real(&self, index: usize) -> Result<f64, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Real(value)) => Ok(value),
            Ok(ValueRef::Integer(value)) => Ok(value as f64),
            Ok(ValueRef::Null) => Err(EngineError::UnexpectedNull { index }),
            Ok(other) => Err(EngineError::UnexpectedColumnType {
                index,
                detail: format!("expected REAL, found {:?}", other.data_type()),
            }),
            Err(error) => Err(EngineError::Statement(error.to_string())),
        }
    }

    fn get_text(&self, index: usize) -> Result<String, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Text(bytes)) => String::from_utf8(bytes.to_vec())
                .map_err(|error| EngineError::UnexpectedColumnType {
                    index,
                    detail: format!("TEXT was not valid UTF-8: {error}"),
                }),
            Ok(ValueRef::Null) => Err(EngineError::UnexpectedNull { index }),
            Ok(other) => Err(EngineError::UnexpectedColumnType {
                index,
                detail: format!("expected TEXT, found {:?}", other.data_type()),
            }),
            Err(error) => Err(EngineError::Statement(error.to_string())),
        }
    }

    fn get_blob(&self, index: usize) -> Result<Vec<u8>, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Blob(bytes)) => Ok(bytes.to_vec()),
            Ok(ValueRef::Null) => Err(EngineError::UnexpectedNull { index }),
            Ok(other) => Err(EngineError::UnexpectedColumnType {
                index,
                detail: format!("expected BLOB, found {:?}", other.data_type()),
            }),
            Err(error) => Err(EngineError::Statement(error.to_string())),
        }
    }

    fn get_optional_integer(&self, index: usize) -> Result<Option<i64>, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Null) => Ok(None),
            _ => self.get_integer(index).map(Some),
        }
    }

    fn get_optional_real(&self, index: usize) -> Result<Option<f64>, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Null) => Ok(None),
            _ => self.get_real(index).map(Some),
        }
    }

    fn get_optional_text(&self, index: usize) -> Result<Option<String>, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Null) => Ok(None),
            _ => self.get_text(index).map(Some),
        }
    }

    fn get_optional_blob(&self, index: usize) -> Result<Option<Vec<u8>>, EngineError> {
        match self.inner.get_ref(index) {
            Ok(ValueRef::Null) => Ok(None),
            _ => self.get_blob(index).map(Some),
        }
    }
}

impl CatalogEngine for MemoryEngine {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, EngineError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EngineError::Statement("connection mutex poisoned".to_owned()))?;
        let bound: Vec<SqliteValue> = params.iter().map(to_sqlite).collect();
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            bound.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        connection
            .execute(sql, param_refs.as_slice())
            .map_err(|error| EngineError::Statement(error.to_string()))
    }

    fn query_rows<T>(
        &self,
        sql: &str,
        params: &[Value],
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EngineError::Statement("connection mutex poisoned".to_owned()))?;
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| EngineError::Statement(error.to_string()))?;
        let bound: Vec<SqliteValue> = params.iter().map(to_sqlite).collect();
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            bound.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        let mut rows = statement
            .query(param_refs.as_slice())
            .map_err(|error| EngineError::Statement(error.to_string()))?;
        let mut collected = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| EngineError::Statement(error.to_string()))?
        {
            let facade_row = SqliteRow { inner: row };
            collected.push(map(&facade_row)?);
        }
        Ok(collected)
    }

    fn transaction(
        &self,
        body: &mut dyn FnMut(&dyn CatalogEngine) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        // Use a SAVEPOINT via raw SQL so the borrow of the connection is not
        // held across the `body` callback (which re-borrows `self`).
        self.execute("BEGIN", &[])
            .map_err(|error| EngineError::Transaction(error.to_string()))?;
        match body(self) {
            Ok(()) => self
                .execute("COMMIT", &[])
                .map(|_| ())
                .map_err(|error| EngineError::Transaction(error.to_string())),
            Err(error) => {
                // Best-effort rollback; surface the original body error.
                let _ = self.execute("ROLLBACK", &[]);
                Err(error)
            }
        }
    }
}

impl VersioningEngine for MemoryEngine {
    fn dolt_add_all(&self) -> Result<(), EngineError> {
        // No staging area in the fake; a no-op that always succeeds.
        Ok(())
    }

    fn dolt_commit(&self, message: &str) -> Result<CommitHash, EngineError> {
        let branch_name = self.current_branch_name();
        let logical = self.next_logical();
        let at = self.locked_pending_commit_time().take().unwrap_or(logical);
        let mut branches = self
            .branches
            .lock()
            .map_err(|_| EngineError::Versioning {
                operation: "dolt_commit",
                detail: "branches mutex poisoned".to_owned(),
            })?;
        let history = branches.entry(branch_name.clone()).or_default();
        let parent = history.commits.last().map(|commit| commit.hash.0.clone());
        let mut hasher = blake3::Hasher::new();
        hasher.update(branch_name.as_bytes());
        hasher.update(message.as_bytes());
        hasher.update(&logical.to_le_bytes());
        if let Some(parent) = &parent {
            hasher.update(parent.as_bytes());
        }
        let hash = CommitHash(hasher.finalize().to_hex().to_string());
        history.commits.push(FakeCommit {
            hash: hash.clone(),
            at_unix_milliseconds: at,
        });
        Ok(hash)
    }

    fn dolt_branch_create(&self, name: &BranchName) -> Result<(), EngineError> {
        let source = self.current_branch_name();
        let mut branches = self.lock_branches("dolt_branch_create")?;
        let seed = branches.get(&source).cloned().unwrap_or_default();
        branches.entry(name.0.clone()).or_insert(seed);
        Ok(())
    }

    fn dolt_checkout(&self, name: &BranchName) -> Result<(), EngineError> {
        if !self.lock_branches("dolt_checkout")?.contains_key(&name.0) {
            return Err(EngineError::Versioning {
                operation: "dolt_checkout",
                detail: format!("no such branch `{}`", name.0),
            });
        }
        *self
            .current_branch
            .lock()
            .map_err(|_| EngineError::Versioning {
                operation: "dolt_checkout",
                detail: "current_branch mutex poisoned".to_owned(),
            })? = name.0.clone();
        Ok(())
    }

    fn dolt_merge(&self, from: &BranchName) -> Result<MergeOutcome, EngineError> {
        // The fake never conflicts: it records a synthetic merge commit on the
        // current branch.
        let merge = self.dolt_commit(&format!("merge {}", from.0))?;
        Ok(MergeOutcome::Clean { commit: merge })
    }

    fn dolt_gc(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn head(&self) -> Result<CommitHash, EngineError> {
        let branch_name = self.current_branch_name();
        self.lock_branches("head")?
            .get(&branch_name)
            .and_then(|history| history.commits.last().map(|commit| commit.hash.clone()))
            .ok_or(EngineError::Versioning {
                operation: "head",
                detail: "branch has no commits yet".to_owned(),
            })
    }

    fn resolve_as_of_time(
        &self,
        unix_milliseconds: i64,
    ) -> Result<Option<CommitHash>, EngineError> {
        let branch_name = self.current_branch_name();
        let branches = self.lock_branches("resolve_as_of_time")?;
        let history = match branches.get(&branch_name) {
            Some(history) => history,
            None => return Ok(None),
        };
        // Newest commit whose stamped instant is <= the requested instant.
        Ok(history
            .commits
            .iter()
            .rev()
            .find(|commit| commit.at_unix_milliseconds <= unix_milliseconds)
            .map(|commit| commit.hash.clone()))
    }

    fn query_rows_at<T>(
        &self,
        _commit_reference: &str,
        table: &str,
        projection: &str,
        where_clause: &str,
        params: &[Value],
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError> {
        // The fake has no prolly-tree per-commit table state, so it reads the
        // *current* table (honest limitation, documented at the type level). It
        // still validates the table name and honors the caller's bound where
        // clause, so callers exercise the same code path they will hit in
        // production.
        if !crate::tables::is_known_table(table) {
            return Err(EngineError::Statement(format!(
                "unknown catalog table {table:?} in historical read"
            )));
        }
        let sql = if where_clause.is_empty() {
            format!("SELECT {projection} FROM {table}")
        } else {
            format!("SELECT {projection} FROM {table} WHERE {where_clause}")
        };
        self.query_rows(&sql, params, map)
    }
}
