//! A rusqlite-backed fake of the engine facade (INDEX-PLAN ID-20).
//!
//! **This is never a product mode, and as of 2026-08-08 it is no longer the
//! default one either.** The versioning calls are *honest fakes*: `dolt_commit`
//! appends to an in-memory commit log (a real hash over the message + parent + a
//! monotonic clock), `head`/`resolve_as_of_time` read that log, and
//! `dolt_branch_create`/`dolt_checkout` track a branch map. No prolly tree, no
//! real time-travel of table state — enough to exercise the catalog's control
//! flow, never enough to ship, and *never* enough to justify a measurement.
//!
//! The reason it existed — "rusqdoltlite is built by another workstream and may
//! not compile yet" — stopped being true when the vendored DoltLite amalgamation
//! landed. Keeping the fake as the default outlived that reason by long enough
//! that every `index` test was silently running against it. `dolt-engine` is now
//! the default and [`super::Configured`] resolves to
//! [`DoltEngine`](super::dolt::DoltEngine); this module is reachable only from a
//! build that passes `--no-default-features --features test-engine`.
//!
//! What it is still *for*: a build that must exclude the vendored C amalgamation
//! entirely (no `doltlite.c`, no `cc`, no partial link) and still type-check and
//! exercise the catalog's SQL surface. That is a real configuration; it is not a
//! configuration anything measures or ships.
//!
//! Gated on `feature = "test-engine"`, which is **opt-in**.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use rusqlite::Connection as SqliteConnection;
use rusqlite::types::{Value as SqliteValue, ValueRef};

use sea_orm::sea_query::Query;

use super::stmt;
use super::{
    BranchName, CatalogEngine, CommitHash, EngineError, MergeOutcome, OpenCatalog, Row, Value,
    VersioningEngine,
};

/// A monotonic counter mixed into each fake commit hash so two commits with the
/// same branch, message and parent still get distinct hashes.
///
/// It is deliberately **not** a timestamp. It used to double as one — see
/// [`FakeCommit::at_unix_milliseconds`].
static LOGICAL_CLOCK: AtomicI64 = AtomicI64::new(0);

/// The instant to stamp on a commit: the real wall clock, in the same unit
/// [`heart::query::AsOf::Time`] speaks.
///
/// The real engine reads its own clock and cannot be told what time it is, so
/// neither can this. That symmetry is the point: a fake that accepts a dictated
/// timestamp lets a test be written that the product could never run, which is
/// exactly what happened to `store_apply.rs`'s `AsOf` boundary test.
fn now_unix_milliseconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        // A clock before 1970 is not a recoverable situation for a commit log and
        // is not worth a variant on every versioning call; 0 sorts before every
        // real commit, which is the behaviour a caller would want anyway.
        .unwrap_or(0)
}

/// One entry in the honest fake commit log.
#[derive(Debug, Clone)]
struct FakeCommit {
    hash: CommitHash,
    /// When this commit was made, in unix milliseconds.
    ///
    /// This field previously held [`LOGICAL_CLOCK`]'s counter (0, 1, 2, …)
    /// whenever no explicit time had been staged — a **different unit** in the
    /// same field that `resolve_as_of_time` compares against a caller's
    /// `AsOf::Time`. Any query about a real instant therefore matched every
    /// commit, and the only tests that passed were ones which had first staged a
    /// fabricated instant on the same scale. It is now always a real instant.
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
}

impl MemoryEngine {
    /// Open a fresh in-memory catalog with an empty `main` branch history.
    pub fn open_in_memory() -> Result<Self, EngineError> {
        let connection = SqliteConnection::open_in_memory()
            .map_err(|error| EngineError::Open(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Open a fresh catalog backed by a real file at `path`.
    ///
    /// This is **not** a product mode — it is the same honest-fake versioning
    /// log as [`Self::open_in_memory`], only with rusqlite's storage backend
    /// pointed at a file instead of `:memory:`.
    ///
    /// It was introduced so storage tests would stop reporting zero bytes *by
    /// construction* (`:memory:` writes nothing to disk). That was the right fix
    /// to the wrong problem: the bytes it produced were real SQLite bytes, but
    /// they were **stock SQLite's** bytes, and the product's catalog is a
    /// content-addressed prolly tree that stores the same rows completely
    /// differently. Numbers taken here were never the product's storage
    /// characteristics and must not be quoted as such — the storage suite now
    /// runs on [`super::Configured`], i.e. the real engine.
    ///
    /// What remains: a `test-engine`-only build still needs a constructor with
    /// a file behind it so [`super::OpenCatalog`] has two honest arms.
    pub fn open_at_path(path: &std::path::Path) -> Result<Self, EngineError> {
        let connection =
            SqliteConnection::open(path).map_err(|error| EngineError::Open(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Shared setup for both constructors: enable foreign keys and seed the
    /// empty `main` branch history.
    fn from_connection(connection: SqliteConnection) -> Result<Self, EngineError> {
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|error| EngineError::Open(error.to_string()))?;
        let mut branches = HashMap::new();
        branches.insert("main".to_owned(), BranchHistory::default());
        Ok(Self {
            connection: Mutex::new(connection),
            branches: Mutex::new(branches),
            current_branch: Mutex::new("main".to_owned()),
        })
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

impl OpenCatalog for MemoryEngine {
    fn open_in_memory() -> Result<Self, EngineError> {
        MemoryEngine::open_in_memory()
    }

    fn open_at_path(path: &std::path::Path) -> Result<Self, EngineError> {
        MemoryEngine::open_at_path(path)
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
            Ok(ValueRef::Text(bytes)) => String::from_utf8(bytes.to_vec()).map_err(|error| {
                EngineError::UnexpectedColumnType {
                    index,
                    detail: format!("TEXT was not valid UTF-8: {error}"),
                }
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
        let param_refs: Vec<&dyn rusqlite::ToSql> = bound
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql)
            .collect();
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
        let param_refs: Vec<&dyn rusqlite::ToSql> = bound
            .iter()
            .map(|value| value as &dyn rusqlite::ToSql)
            .collect();
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
        let at = now_unix_milliseconds();
        let mut branches = self.branches.lock().map_err(|_| EngineError::Versioning {
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
        let mut message = String::from("merge ");
        message.push_str(&from.0);
        let merge = self.dolt_commit(&message)?;
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

    fn query_rows_at<Ent, T>(
        &self,
        _commit_reference: &str,
        configure: &mut dyn FnMut(&mut sea_orm::sea_query::SelectStatement),
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError>
    where
        Ent: crate::entity::Historical,
    {
        // No prolly-tree history: read the tip table (documented limitation).
        let mut select = Query::select();
        select.from(Ent::default());
        configure(&mut select);
        stmt::query_select(self, select, map)
    }
}
