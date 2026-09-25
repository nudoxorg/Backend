//! The engine facade (INDEX-PLAN ID-20).
//!
//! Every access to the versioned catalog engine — plain SQL *and* the `dolt_*`
//! versioning calls — funnels through the [`CatalogEngine`] trait defined here.
//! Wrapping the vendored binding behind one trait makes surface drift a one-file
//! fix, and lets the whole catalog (schema,
//! [`MetaStore`](crate::store::MetaStore), migrations) be written once against a
//! vocabulary that outlives any one engine.
//!
//! The facade owns its **own** value/row/error vocabulary ([`Value`], [`Row`],
//! [`EngineError`]) rather than re-exporting the engine's, so a change in the
//! upstream binding never ripples past this directory.
//!
//! Implementations:
//! - [`dolt::DoltEngine`] — the real DoltLite binding (feature `dolt-engine`,
//!   **on by default**). This is what every build and every test runs.
//! - [`memory::MemoryEngine`] — a rusqlite-backed fake with an honest recorded
//!   commit log (feature `test-engine`, **off by default, opt-in only**).
//!   **Never a product mode**, and no longer the mode anything runs by accident.
//!
//! Which of the two a build uses is decided exactly once, by [`Configured`].
//! Callers name that alias, never a concrete engine, so the decision cannot be
//! re-made — differently — at each construction site.

use std::fmt;

pub mod stmt;

#[cfg(feature = "test-engine")]
pub mod memory;

#[cfg(feature = "dolt-engine")]
pub mod dolt;

mod edge_fact;

/// Row-versioned ledger on the philocalyst Turso fork (`turso_versioning`).
pub mod turso_vc;

pub use stmt::{exec, query};

// ─────────────────────────────────────────────────────────────────────────────
// Which engine this build runs
// ─────────────────────────────────────────────────────────────────────────────

/// The catalog engine **this build** uses.
///
/// This alias exists so that "which engine runs" is one decision in one file
/// rather than a naming convention every caller has to remember. Before it, nine
/// test files each spelled out `engine::memory::MemoryEngine`, which meant the
/// fake was the engine under test *by import*, invisibly, in files whose subject
/// was the catalog rather than the engine. Nothing announced that; nothing could.
///
/// `dolt-engine` wins whenever it is enabled, including when Cargo's feature
/// unification turns both on (`driver` requests `dolt-engine` while another
/// member takes `index`'s defaults). The fake is therefore reachable only in a
/// build that has *deliberately* turned the real engine off — never by
/// unification, and never by default.
#[cfg(feature = "dolt-engine")]
pub type Configured = dolt::DoltEngine;

/// The catalog engine **this build** uses — see the `dolt-engine` variant of
/// this alias for the full contract. Selecting the fake here requires
/// `--no-default-features --features test-engine`.
#[cfg(all(feature = "test-engine", not(feature = "dolt-engine")))]
pub type Configured = memory::MemoryEngine;

#[cfg(not(any(feature = "dolt-engine", feature = "test-engine")))]
compile_error!(
    "`index` has no catalog engine: enable `dolt-engine` (the default, and the only \
     product mode) or, for a build that deliberately excludes the vendored DoltLite \
     amalgamation, `test-engine`. There is no engine-less configuration of this crate — \
     the catalog is the crate."
);

/// How a catalog engine is brought into existence.
///
/// Deliberately *not* part of [`CatalogEngine`]: a caller holding an engine never
/// needs it, and object-safe facade code must not be able to conjure a second
/// engine. It is a separate trait so that [`Configured`] is usable generically —
/// an alias alone lets a caller name the type but not construct one, which would
/// have pushed every construction site straight back to naming a concrete engine
/// and re-deciding what [`Configured`] exists to decide once.
///
/// The two constructors are the two ways a catalog can exist, and both engines
/// answer both: an ephemeral catalog with no file behind it, and one whose bytes
/// are at a path. Anything measuring storage needs the second; anything that only
/// needs correctness should take the first.
pub trait OpenCatalog: VersioningEngine + Sized {
    /// Open a private, process-local catalog with no file backing it. Nothing it
    /// writes is measurable on disk — use [`open_at_path`](Self::open_at_path)
    /// when bytes are the subject.
    fn open_in_memory() -> Result<Self, EngineError>;

    /// Open (creating if absent) the catalog whose bytes live at `path`.
    fn open_at_path(path: &std::path::Path) -> Result<Self, EngineError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Values
// ─────────────────────────────────────────────────────────────────────────────

/// A single SQL-bindable value. Mirrors the engine's cell vocabulary but is our
/// own type, so the engine's shape can drift without touching callers.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A signed 64-bit integer (also carries `INTEGER`-typed booleans as 0/1).
    Integer(i64),
    /// A 64-bit float.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// An opaque byte blob (`BLOB16` ids, `BLOB32` hashes, …).
    Blob(Vec<u8>),
}

impl Value {
    /// Convenience: wrap an owned string.
    pub fn text(value: impl Into<String>) -> Self {
        Value::Text(value.into())
    }

    /// Convenience: a blob from any byte source.
    pub fn blob(bytes: impl Into<Vec<u8>>) -> Self {
        Value::Blob(bytes.into())
    }

    /// Wrap an optional value, mapping `None` to [`Value::Null`].
    pub fn from_optional<T>(value: Option<T>, wrap: impl FnOnce(T) -> Value) -> Value {
        value.map_or(Value::Null, wrap)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rows
// ─────────────────────────────────────────────────────────────────────────────

/// A read-back row, addressed by zero-based column index. The concrete engine
/// supplies a [`Row`] implementor to the row-mapping closure; typed getters
/// return [`EngineError::UnexpectedColumnType`] on a shape mismatch rather than
/// panicking, so a projection bug is a typed error, never a crash.
pub trait Row {
    /// Fetch a non-null `INTEGER` column.
    fn get_integer(&self, index: usize) -> Result<i64, EngineError>;
    /// Fetch a non-null `REAL` column.
    fn get_real(&self, index: usize) -> Result<f64, EngineError>;
    /// Fetch a non-null `TEXT` column.
    fn get_text(&self, index: usize) -> Result<String, EngineError>;
    /// Fetch a non-null `BLOB` column.
    fn get_blob(&self, index: usize) -> Result<Vec<u8>, EngineError>;

    /// Fetch a nullable `INTEGER` column.
    fn get_optional_integer(&self, index: usize) -> Result<Option<i64>, EngineError>;
    /// Fetch a nullable `REAL` column.
    fn get_optional_real(&self, index: usize) -> Result<Option<f64>, EngineError>;
    /// Fetch a nullable `TEXT` column.
    fn get_optional_text(&self, index: usize) -> Result<Option<String>, EngineError>;
    /// Fetch a nullable `BLOB` column.
    fn get_optional_blob(&self, index: usize) -> Result<Option<Vec<u8>>, EngineError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Versioning vocabulary (our own newtypes over the engine's)
// ─────────────────────────────────────────────────────────────────────────────

/// A catalog commit hash (hex) in the engine's history graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitHash(pub String);

impl fmt::Display for CommitHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branch name (`main`, `local/<device>`, `pre-migrate-v4`, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchName(pub String);

impl BranchName {
    /// The advertised writer branch (INDEX-PLAN ID-1: single writer on `main`).
    pub fn main() -> Self {
        BranchName("main".to_owned())
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The outcome of a `dolt_merge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Merge applied cleanly, producing a new commit.
    Clean { commit: CommitHash },
    /// Merge produced conflicts on these tables; resolution is caller policy.
    Conflicts { tables: Vec<String> },
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Every failure the facade can surface. Concrete engines map their native
/// errors into these variants so callers never match on an engine-specific type.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The database file / connection could not be opened.
    #[error("failed to open catalog engine: {0}")]
    Open(String),
    /// A statement failed to prepare or execute.
    #[error("statement failed: {0}")]
    Statement(String),
    /// A read-back column had a type other than the getter expected.
    #[error("column {index} had unexpected type: {detail}")]
    UnexpectedColumnType { index: usize, detail: String },
    /// A read-back column was `NULL` where a non-null getter was used.
    #[error("column {index} was NULL where a value was required")]
    UnexpectedNull { index: usize },
    /// A transaction could not be started, committed, or rolled back.
    #[error("transaction failed: {0}")]
    Transaction(String),
    /// A `dolt_*` versioning call failed.
    #[error("versioning operation `{operation}` failed: {detail}")]
    Versioning {
        operation: &'static str,
        detail: String,
    },
    /// The engine was asked to do something structurally impossible (e.g. a
    /// second writer, or a versioning call on a plain-SQLite fake in a code path
    /// that requires real history).
    #[error("unsupported engine operation: {0}")]
    Unsupported(String),
    /// The build has no versioned catalog engine, so no catalog was opened.
    ///
    /// Distinct from [`Open`](EngineError::Open), which means a real DoltLite
    /// engine could not open a particular file. This means there is no DoltLite
    /// engine *at all* — the vendored amalgamation was absent at build time, or
    /// the library that answered failed the `dolt_version()` capability probe.
    ///
    /// It is separate because the fix is categorically different: nothing a
    /// running process can do resolves it. The vendored engine has to be
    /// restored (`workspace/vendor/doltlite/fetch.nu`) and the binary rebuilt.
    /// Folding it into `Open` is how this failure previously reached callers as
    /// a generic "no such function: dolt_branch" at first use, an arbitrary
    /// distance from the missing file that caused it.
    #[error("no versioned catalog engine in this build: {detail}")]
    WrongEngine {
        /// The rejection as reported by the binding, naming the absent
        /// amalgamation or the version the impostor library reported.
        detail: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// The facade traits
// ─────────────────────────────────────────────────────────────────────────────

/// The plain-SQL surface of the catalog engine — everything that is *not* a
/// versioning call. Both the real DoltLite binding and the in-memory fake
/// implement it identically, so business logic is engine-agnostic.
pub trait CatalogEngine {
    /// Execute a non-query statement, returning the number of affected rows.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, EngineError>;

    /// Run a query, mapping each row through `map` and collecting the results.
    ///
    /// The closure receives a `&dyn Row` (object-safe) so the mapper never names
    /// the engine's concrete row type. The `where Self: Sized` bound keeps this
    /// generic method out of the vtable, so [`CatalogEngine`] stays object-safe
    /// (the transaction body and [`apply`](crate::store::apply) use `execute`
    /// through a `&dyn CatalogEngine`).
    fn query_rows<T>(
        &self,
        sql: &str,
        params: &[Value],
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError>
    where
        Self: Sized;

    /// Run `body` inside one transaction, committing on `Ok` and rolling back on
    /// `Err`. This is the atomicity primitive [`MetaStore::apply_ops`] builds on
    /// (INDEX-PLAN ID-3: business write + outbox rows in **one** transaction).
    ///
    /// `body` receives a plain-SQL handle scoped to the transaction; it cannot
    /// issue versioning calls (those belong to the batch heartbeat commit, not
    /// the per-op transaction).
    fn transaction(
        &self,
        body: &mut dyn FnMut(&dyn CatalogEngine) -> Result<(), EngineError>,
    ) -> Result<(), EngineError>;
}

/// The DoltLite versioning surface (INDEX-PLAN §4 ID-1). Kept separate from
/// [`CatalogEngine`] so the per-op transaction path cannot accidentally reach a
/// `dolt_commit`; only the writer's batch heartbeat (ID-4) holds a
/// [`VersioningEngine`].
pub trait VersioningEngine: CatalogEngine {
    /// Stage all working-set changes (`dolt add -A`).
    fn dolt_add_all(&self) -> Result<(), EngineError>;

    /// Commit the staged working set with a message, returning the new head.
    /// This is the batch heartbeat commit (ID-4).
    fn dolt_commit(&self, message: &str) -> Result<CommitHash, EngineError>;

    /// Create a branch (e.g. `pre-migrate-v4` before a migration, §13).
    fn dolt_branch_create(&self, name: &BranchName) -> Result<(), EngineError>;

    /// Check out a branch (rollback of a failed migration = checkout, §13).
    fn dolt_checkout(&self, name: &BranchName) -> Result<(), EngineError>;

    /// Merge `from` into the current branch (overlay merge, ID-9).
    fn dolt_merge(&self, from: &BranchName) -> Result<MergeOutcome, EngineError>;

    /// Garbage-collect abandoned history (§12 weekly `dolt_gc`).
    fn dolt_gc(&self) -> Result<(), EngineError>;

    /// The current head commit.
    fn head(&self) -> Result<CommitHash, EngineError>;

    /// Resolve an as-of-time read to the newest commit at or before the instant
    /// (INDEX-PLAN §9 `AsOf::Time`). `None` when the instant precedes the first
    /// commit.
    fn resolve_as_of_time(&self, unix_milliseconds: i64)
    -> Result<Option<CommitHash>, EngineError>;

    /// Read one entity table **as of** a historical commit reference
    /// (INDEX-PLAN §9, §18 scenario 1).
    ///
    /// `Ent` is the SeaORM entity (the table is the entity itself — no
    /// parallel name enum). `configure` receives a sea-query select whose
    /// `FROM` is already set (tip table for the memory fake; the entity's
    /// [`crate::entity::Historical::DOLT_AT`] TVF for the real engine) and
    /// only adds columns / WHERE.
    fn query_rows_at<Ent, T>(
        &self,
        commit_reference: &str,
        configure: &mut dyn FnMut(&mut sea_orm::sea_query::SelectStatement),
        map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError>
    where
        Self: Sized,
        Ent: crate::entity::Historical;
}
