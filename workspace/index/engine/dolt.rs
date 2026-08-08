//! The real DoltLite-backed facade implementation (INDEX-PLAN ID-1, ID-20).
//!
//! Gated on `feature = "dolt-engine"`. This is the one place the upstream
//! `rusqdoltlite` surface is absorbed: everything above the facade speaks the
//! crate's own [`Value`]/[`Row`]/[`EngineError`] vocabulary, and this module
//! translates to and from the engine's.
//!
//! ## Interior mutability
//! `rusqdoltlite::Connection` is `Send` but **not** `Sync`, and its
//! [`Connection::transaction`](engine::Connection::transaction) takes `&mut self`.
//! The facade requires `&self` methods and a `Send + Sync` engine (the
//! [`Catalog`](crate::store::Catalog) supertrait bound). We therefore wrap the
//! connection in a [`Mutex`], mirroring the memory engine: every call takes the
//! lock for the duration of the statement, and the single-writer discipline
//! (ID-1) means there is never real contention.
//!
//! ## Contract notes absorbed here
//! - The engine has no `AS OF <ts>` SQL: [`resolve_as_of_time`] scans `dolt_log`
//!   (done inside the binding) and historical reads go through the
//!   `dolt_at_<table>(ref)` table-valued function ([`query_rows_at`]).
//! - The engine's [`MergeOutcome`](engine::MergeOutcome) has four variants;
//!   the facade collapses the three clean ones into [`MergeOutcome::Clean`].

use std::sync::Mutex;

use rusqdoltlite as engine;

use sea_orm::sea_query::{Alias, Expr, Func, Query};

use super::stmt;
use super::{
    BranchName, CatalogEngine, CommitHash, EngineError, MergeOutcome, OpenCatalog, Row, Value,
    VersioningEngine,
};

use engine::DoltConnectionExtension;

/// The production catalog engine: a rusqdoltlite connection on `main`, wrapped
/// in a [`Mutex`] so the facade's `&self` / `Send + Sync` contract holds over a
/// `!Sync` connection whose transaction API is `&mut`.
pub struct DoltEngine {
    connection: Mutex<engine::Connection>,
}

impl DoltEngine {
    /// Open (or create) `catalog.dolt` at the given path.
    pub fn open(path: &std::path::Path) -> Result<Self, EngineError> {
        let path_text = path
            .to_str()
            .ok_or_else(|| EngineError::Open(format!("non-UTF-8 catalog path {path:?}")))?;
        let connection = engine::Connection::open(path_text).map_err(from_engine_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Open a private in-memory catalog (Embedded tests / ephemeral catalogs).
    pub fn open_in_memory() -> Result<Self, EngineError> {
        let connection = engine::Connection::open_in_memory().map_err(from_engine_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Lock the connection, mapping a poisoned mutex to a typed engine error.
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, engine::Connection>, EngineError> {
        self.connection
            .lock()
            .map_err(|_| EngineError::Statement("catalog connection mutex poisoned".to_owned()))
    }
}

impl OpenCatalog for DoltEngine {
    fn open_in_memory() -> Result<Self, EngineError> {
        DoltEngine::open_in_memory()
    }

    fn open_at_path(path: &std::path::Path) -> Result<Self, EngineError> {
        DoltEngine::open(path)
    }
}

// SAFETY-of-contract: `engine::Connection` is `Send`; the `Mutex` makes the
// whole engine `Send + Sync` and serializes all access, upholding the "one
// thread in the handle at a time" rule the binding documents.

// ─────────────────────────────────────────────────────────────────────────────
// Value / error translation
// ─────────────────────────────────────────────────────────────────────────────

/// Translate a facade [`Value`] into an engine bind value.
fn to_engine_value(value: &Value) -> engine::Value {
    match value {
        Value::Null => engine::Value::Null,
        Value::Integer(integer) => engine::Value::Integer(*integer),
        Value::Real(real) => engine::Value::Real(*real),
        Value::Text(text) => engine::Value::Text(text.clone()),
        Value::Blob(blob) => engine::Value::Blob(blob.clone()),
    }
}

/// Bind the whole parameter slice into an owned engine-value vector.
fn to_engine_params(params: &[Value]) -> Vec<engine::Value> {
    params.iter().map(to_engine_value).collect()
}

/// Translate an engine [`EngineError`](engine::EngineError) into the facade's.
fn from_engine_error(error: engine::EngineError) -> EngineError {
    use engine::EngineError as Engine;
    match error {
        Engine::CannotOpen { path, message } => {
            EngineError::Open(format!("cannot open {path:?}: {message}"))
        }
        Engine::Busy { message } => EngineError::Statement(format!("busy or locked: {message}")),
        Engine::Constraint { message } => {
            EngineError::Statement(format!("constraint violation: {message}"))
        }
        Engine::Corrupt { message } => EngineError::Statement(format!("corrupt database: {message}")),
        Engine::Sql {
            primary_code,
            extended_message,
            sql,
        } => EngineError::Statement(format!(
            "SQL error (code {primary_code}): {extended_message} — in: {sql}"
        )),
        Engine::TypeMismatch {
            column_index,
            expected,
            actual,
        } => EngineError::UnexpectedColumnType {
            index: column_index,
            detail: format!("expected {expected}, found {actual}"),
        },
        Engine::ColumnIndexOutOfRange {
            column_index,
            column_count,
        } => EngineError::UnexpectedColumnType {
            index: column_index,
            detail: format!("column index out of range (statement has {column_count} columns)"),
        },
        Engine::Dolt { operation, message } => EngineError::Versioning {
            // Leak a &'static str for the operation label; the set of dolt
            // operations is tiny and bounded, so this never grows unboundedly.
            operation: leak_operation(&operation),
            detail: message,
        },
        Engine::InvalidBranchName { name, reason } => {
            EngineError::Versioning {
                operation: "branch_name",
                detail: format!("invalid branch name {name:?}: {reason}"),
            }
        }
        Engine::InvalidCommitHash { value, reason } => EngineError::Versioning {
            operation: "commit_hash",
            detail: format!("invalid commit hash {value:?}: {reason}"),
        },
        Engine::NulInterior { context } => {
            EngineError::Statement(format!("interior NUL byte in {context}"))
        }
        Engine::Utf8 { context } => {
            EngineError::Statement(format!("non-UTF-8 bytes in {context}"))
        }
        Engine::Internal { message } => EngineError::Statement(format!("internal: {message}")),
        Engine::LengthOverflow {
            context,
            byte_length,
        } => EngineError::Statement(format!(
            "value too large for {context}: {byte_length} bytes exceeds engine limit"
        )),
        // Both of these mean "this binary has no versioned catalog engine", which
        // is not an `Open` failure of a particular file — it is the absence of
        // the product's storage engine. They are kept apart from `Open` so a
        // caller can tell "the catalog file is unusable" from "this build cannot
        // have a catalog at all"; the binding's `Display` already names the
        // absent amalgamation or the impostor library's version, so it is carried
        // through verbatim rather than re-worded here.
        error @ (Engine::EngineNotLinked { .. } | Engine::NotDoltLite { .. }) => {
            EngineError::WrongEngine {
                detail: error.to_string(),
            }
        }
    }
}

/// Map a dynamic dolt operation label to a `&'static str` for the facade error.
fn leak_operation(operation: &str) -> &'static str {
    match operation {
        "dolt_add" => "dolt_add",
        "dolt_commit" => "dolt_commit",
        "dolt_branch" => "dolt_branch",
        "dolt_checkout" => "dolt_checkout",
        "dolt_merge" => "dolt_merge",
        "dolt_gc" => "dolt_gc",
        "dolt_hashof" | "head" => "head",
        "dolt_log" => "resolve_as_of_time",
        _ => "dolt",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Row adapter
// ─────────────────────────────────────────────────────────────────────────────

/// Present an engine [`Row`](engine::Row) behind the facade's [`Row`] trait.
struct DoltRow<'row, 'statement> {
    inner: &'row engine::Row<'statement>,
}

impl Row for DoltRow<'_, '_> {
    fn get_integer(&self, index: usize) -> Result<i64, EngineError> {
        self.inner.get_integer(index).map_err(from_engine_error)
    }
    fn get_real(&self, index: usize) -> Result<f64, EngineError> {
        self.inner.get_real(index).map_err(from_engine_error)
    }
    fn get_text(&self, index: usize) -> Result<String, EngineError> {
        self.inner.get_text(index).map_err(from_engine_error)
    }
    fn get_blob(&self, index: usize) -> Result<Vec<u8>, EngineError> {
        self.inner.get_blob(index).map_err(from_engine_error)
    }
    fn get_optional_integer(&self, index: usize) -> Result<Option<i64>, EngineError> {
        self.inner
            .get_optional_integer(index)
            .map_err(from_engine_error)
    }
    fn get_optional_real(&self, index: usize) -> Result<Option<f64>, EngineError> {
        self.inner
            .get_optional_real(index)
            .map_err(from_engine_error)
    }
    fn get_optional_text(&self, index: usize) -> Result<Option<String>, EngineError> {
        self.inner
            .get_optional_text(index)
            .map_err(from_engine_error)
    }
    fn get_optional_blob(&self, index: usize) -> Result<Option<Vec<u8>>, EngineError> {
        self.inner
            .get_optional_blob(index)
            .map_err(from_engine_error)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CatalogEngine
// ─────────────────────────────────────────────────────────────────────────────

impl CatalogEngine for DoltEngine {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, EngineError> {
        let connection = self.locked()?;
        connection
            .execute(sql, &to_engine_params(params))
            .map_err(from_engine_error)
    }

    fn query_rows<T>(
        &self,
        sql: &str,
        params: &[Value],
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError> {
        let connection = self.locked()?;
        connection
            .query_rows(sql, &to_engine_params(params), |row| {
                let facade_row = DoltRow { inner: row };
                map(&facade_row).map_err(to_engine_error)
            })
            .map_err(from_engine_error)
    }

    fn transaction(
        &self,
        body: &mut dyn FnMut(&dyn CatalogEngine) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        // The engine's `transaction(&mut self)` borrows the connection
        // exclusively for the scope, which would deadlock against the re-entrant
        // `execute`/`query_rows` the body issues through `self` (each of which
        // re-locks the same mutex). We therefore drive the transaction with raw
        // `BEGIN`/`COMMIT`/`ROLLBACK` through `execute`, exactly as the memory
        // engine does, so the lock is released between statements.
        self.execute("BEGIN", &[])
            .map_err(|error| EngineError::Transaction(error.to_string()))?;
        match body(self) {
            Ok(()) => self
                .execute("COMMIT", &[])
                .map(|_| ())
                .map_err(|error| EngineError::Transaction(error.to_string())),
            Err(body_error) => {
                // Best-effort rollback; surface the original body error.
                let _ = self.execute("ROLLBACK", &[]);
                Err(body_error)
            }
        }
    }
}

/// Collapse a facade error back into an engine error so a row-mapping closure
/// (which the engine requires to return `engine::EngineError`) can propagate a
/// facade decode failure. The information is preserved in the message; the outer
/// [`query_rows`](CatalogEngine::query_rows) re-wraps it via [`from_engine_error`]
/// into a `Statement` error, so callers still see the original text.
fn to_engine_error(error: EngineError) -> engine::EngineError {
    engine::EngineError::Internal {
        message: error.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VersioningEngine
// ─────────────────────────────────────────────────────────────────────────────

impl VersioningEngine for DoltEngine {
    fn dolt_add_all(&self) -> Result<(), EngineError> {
        self.locked()?.dolt_add_all().map_err(from_engine_error)
    }

    fn dolt_commit(&self, message: &str) -> Result<CommitHash, EngineError> {
        let hash = self
            .locked()?
            .dolt_commit(message)
            .map_err(from_engine_error)?;
        Ok(CommitHash(hash.into_string()))
    }

    fn dolt_branch_create(&self, name: &BranchName) -> Result<(), EngineError> {
        let branch = to_engine_branch(name)?;
        self.locked()?
            .dolt_branch_create(&branch)
            .map_err(from_engine_error)
    }

    fn dolt_checkout(&self, name: &BranchName) -> Result<(), EngineError> {
        let branch = to_engine_branch(name)?;
        self.locked()?
            .dolt_checkout(&branch)
            .map_err(from_engine_error)
    }

    fn dolt_merge(&self, from: &BranchName) -> Result<MergeOutcome, EngineError> {
        let branch = to_engine_branch(from)?;
        let outcome = self.locked()?.dolt_merge(&branch).map_err(from_engine_error)?;
        Ok(match outcome {
            // The three clean outcomes collapse into the facade's `Clean`; the
            // resulting head is best-effort (AlreadyUpToDate carries none, so we
            // fall back to the current head).
            engine::MergeOutcome::AlreadyUpToDate => {
                let head = self.head()?;
                MergeOutcome::Clean { commit: head }
            }
            engine::MergeOutcome::FastForward { new_head }
            | engine::MergeOutcome::MergeCommit { new_head } => MergeOutcome::Clean {
                commit: CommitHash(new_head.into_string()),
            },
            engine::MergeOutcome::Conflicts { conflicted_tables } => {
                MergeOutcome::Conflicts {
                    tables: conflicted_tables,
                }
            }
        })
    }

    fn dolt_gc(&self) -> Result<(), EngineError> {
        self.locked()?.dolt_gc().map_err(from_engine_error)
    }

    fn head(&self) -> Result<CommitHash, EngineError> {
        let hash = self.locked()?.head().map_err(from_engine_error)?;
        Ok(CommitHash(hash.into_string()))
    }

    fn resolve_as_of_time(
        &self,
        unix_milliseconds: i64,
    ) -> Result<Option<CommitHash>, EngineError> {
        let resolved = self
            .locked()?
            .resolve_as_of_time(unix_milliseconds)
            .map_err(from_engine_error)?;
        Ok(resolved.map(|hash| CommitHash(hash.into_string())))
    }

    fn query_rows_at<Ent, T>(
        &self,
        commit_reference: &str,
        configure: &mut dyn FnMut(&mut sea_orm::sea_query::SelectStatement),
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError>
    where
        Ent: crate::entity::Historical,
    {
        // Entity owns its `dolt_at_*` TVF name; commit ref is bound, not
        // interpolated. Building the select stays in sea-query (typed).
        let mut select = Query::select();
        select.from_function(
            Func::cust(Alias::new(Ent::DOLT_AT)).arg(Expr::val(commit_reference)),
            Alias::new("as_of"),
        );
        configure(&mut select);
        stmt::query_select(self, select, map)
    }
}

/// Validate + convert a facade branch name into the engine's checked newtype.
fn to_engine_branch(name: &BranchName) -> Result<engine::BranchName, EngineError> {
    engine::BranchName::parse(&name.0).map_err(from_engine_error)
}
