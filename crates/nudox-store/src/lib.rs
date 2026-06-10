//! SQLite-backed [`GlobalSymbolStore`] and [`FutureParseQueue`].
//!
//! Both implementations share a single [`sqlx::SqlitePool`] via [`NudoxStore`].
//! SQLite is the canonical link between TerminusDB document identities and the
//! occurrence system's [`GlobalSymbolId`] UUIDs.
//!
//! # GlobalSymbolId derivation
//!
//! IDs are UUID v5 values computed as:
//! ```text
//! UUID_v5(NUDOX_SYMBOL_NS, "{terminus_instance}\0{entry_uri}")
//! ```
//! where `terminus_instance` is `"{org}/{db}"` from TerminusDB config and
//! `entry_uri` is the TerminusDB document `@id`, e.g.
//! `"Entry/rust/serde/Serialize"`.
//!
//! This makes IDs **stable and computable offline** — anyone who knows the two
//! inputs can compute the ID without a database round-trip.
//!
//! # Usage
//!
//! ```no_run
//! use nudox_store::NudoxStore;
//! use std::path::Path;
//! use std::sync::Arc;
//!
//! # async fn example() -> nudox_core::Result<()> {
//! let store = NudoxStore::open(Path::new("nudox-links.db"), "my_org/my_db").await?;
//!
//! // Pass to orchestrator:
//! // Orchestrator::new(
//! //     Arc::clone(&store) as Arc<dyn GlobalSymbolStore>,
//! //     blob_store,
//! //     Arc::clone(&store) as Arc<dyn FutureParseQueue>,
//! //     search,
//! //     vector,
//! // )
//!
//! // Called by compiler after a library parse:
//! store.register_library("rust", "serde", "1.0.0", [
//!     "Entry/rust/serde/Serialize",
//!     "Entry/rust/serde/Deserialize",
//! ]).await?;
//! # Ok(())
//! # }
//! ```

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tracing::instrument;
use uuid::Uuid;

use nudox_core::{
    BlobRef, Error, FutureParseQueue, GlobalSymbolId, GlobalSymbolStore, LibRef, OccurrenceId,
    Result,
};

// ── Namespace UUID ────────────────────────────────────────────────────────────

/// Fixed UUID v5 namespace for nudox global symbol IDs.
///
/// Any system that knows `(terminus_instance, entry_uri)` can compute the
/// same `GlobalSymbolId` offline using this namespace constant.
pub const NUDOX_SYMBOL_NS: Uuid = uuid::uuid!("6e756478-2073-796d-626f-6c2d6e730001");

/// Compute the deterministic [`GlobalSymbolId`] for a TerminusDB entry.
///
/// ```
/// use nudox_store::symbol_id;
/// let id = symbol_id("nudox_org/nudox_lib", "Entry/rust/serde/Serialize");
/// ```
pub fn symbol_id(terminus_instance: &str, entry_uri: &str) -> GlobalSymbolId {
    let name = format!("{}\x00{}", terminus_instance, entry_uri);
    GlobalSymbolId(Uuid::new_v5(&NUDOX_SYMBOL_NS, name.as_bytes()))
}

// ── NudoxStore ────────────────────────────────────────────────────────────────

/// SQLite store that is the canonical link between TerminusDB document
/// identities and the occurrence system.
///
/// Implements both [`GlobalSymbolStore`] and [`FutureParseQueue`] so both
/// share one connection pool and one migration.  Pass the same `Arc<NudoxStore>`
/// (coerced to the appropriate trait object) for both orchestrator arguments.
pub struct NudoxStore {
    pool: SqlitePool,
    terminus_instance: String,
}

impl NudoxStore {
    /// Open (or create) the SQLite database at `db_path` and run any pending
    /// migrations.
    ///
    /// `terminus_instance` should be `"{org}/{db}"` from the TerminusDB
    /// config, e.g. `"nudox_org/nudox_lib"`.
    pub async fn open(
        db_path: &Path,
        terminus_instance: impl Into<String>,
    ) -> Result<Arc<Self>> {
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            // WAL mode is faster for concurrent reads and a single writer.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePool::connect_with(opts)
            .await
            .map_err(|e| Error::GlobalStore(format!("sqlite open {}: {e}", db_path.display())))?;

        Self::ensure_schema(&pool).await?;

        Ok(Arc::new(Self {
            pool,
            terminus_instance: terminus_instance.into(),
        }))
    }

    async fn ensure_schema(pool: &SqlitePool) -> Result<()> {
        // Execute each DDL statement individually — splitting avoids any
        // multi-statement execution quirks while keeping the schema readable.
        let stmts = [
            "CREATE TABLE IF NOT EXISTS global_symbols (
                id                  TEXT NOT NULL PRIMARY KEY,
                terminus_instance   TEXT NOT NULL,
                entry_uri           TEXT NOT NULL,
                symbol_name         TEXT NOT NULL,
                lang                TEXT NOT NULL,
                lib_name            TEXT NOT NULL,
                lib_version         TEXT NOT NULL,
                registered_at       INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                UNIQUE (terminus_instance, entry_uri)
            )",
            "CREATE INDEX IF NOT EXISTS idx_gs_lookup
                ON global_symbols (lib_name, lib_version, symbol_name)",
            "CREATE INDEX IF NOT EXISTS idx_gs_library
                ON global_symbols (terminus_instance, lib_name, lib_version)",
            "CREATE TABLE IF NOT EXISTS occurrence_associations (
                global_symbol_id    TEXT NOT NULL REFERENCES global_symbols(id) ON DELETE CASCADE,
                occurrence_id       TEXT NOT NULL,
                associated_at       INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                PRIMARY KEY (global_symbol_id, occurrence_id)
            )",
            "CREATE INDEX IF NOT EXISTS idx_oa_occurrence
                ON occurrence_associations (occurrence_id)",
            "CREATE TABLE IF NOT EXISTS deferred_queue (
                blob_ref        TEXT NOT NULL,
                lib_name        TEXT NOT NULL,
                lib_version     TEXT NOT NULL,
                enqueued_at     INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                PRIMARY KEY (blob_ref, lib_name, lib_version)
            )",
            "CREATE INDEX IF NOT EXISTS idx_dq_lib
                ON deferred_queue (lib_name, lib_version)",
        ];

        for stmt in &stmts {
            sqlx::query(stmt)
                .execute(pool)
                .await
                .map_err(|e| Error::GlobalStore(format!("schema init: {e}")))?;
        }
        Ok(())
    }

    /// The TerminusDB instance this store is scoped to.
    pub fn terminus_instance(&self) -> &str {
        &self.terminus_instance
    }

    /// Register all symbols from a newly-parsed library.
    ///
    /// Called by the compiler after every successful `run_rust_pipeline` /
    /// `run_typescript_pipeline`.  Pass the TerminusDB Entry URIs from the
    /// `DocStore` — only keys that start with `"Entry/"` represent symbols;
    /// callers should filter out Kind/schema documents before calling this.
    ///
    /// Returns the number of newly-inserted rows (existing rows are skipped
    /// via `INSERT OR IGNORE`).
    ///
    /// # Entry URI → symbol_name extraction
    ///
    /// `entry_uri` is `"Entry/{lang}/{crate_name}/{symbol_path}"`.  The
    /// `symbol_name` stored is the **last** path component after the crate
    /// name, e.g. `"Serialize"` from `"Entry/rust/serde/Serialize"`, or
    /// `"Serialize"` from `"Entry/rust/serde/ser/Serialize"`.  This matches
    /// what occurrence callers pass as `symbol_name` in [`LibRef`] lookups.
    #[instrument(skip(self, entry_uris), fields(terminus = %self.terminus_instance, lib_name, lib_version))]
    pub async fn register_library<'a>(
        &self,
        lang: &str,
        lib_name: &str,
        lib_version: &str,
        entry_uris: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::GlobalStore(format!("begin tx: {e}")))?;

        let mut inserted = 0usize;

        for uri in entry_uris {
            // Extract last path component as symbol_name.
            let symbol_name = uri.rsplit('/').next().unwrap_or(uri);
            let id = symbol_id(&self.terminus_instance, uri).to_string();

            let rows = sqlx::query(
                "INSERT OR IGNORE INTO global_symbols \
                 (id, terminus_instance, entry_uri, symbol_name, lang, lib_name, lib_version) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&self.terminus_instance)
            .bind(uri)
            .bind(symbol_name)
            .bind(lang)
            .bind(lib_name)
            .bind(lib_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| Error::GlobalStore(format!("insert symbol: {e}")))?
            .rows_affected();

            inserted += rows as usize;
        }

        tx.commit()
            .await
            .map_err(|e| Error::GlobalStore(format!("commit: {e}")))?;

        tracing::info!(
            lib_name,
            lib_version,
            inserted,
            "library symbols registered"
        );

        Ok(inserted)
    }

    /// Return the [`GlobalSymbolId`] and TerminusDB entry URI for a symbol,
    /// if the library has been registered.
    ///
    /// Useful for inspection and for cross-referencing back to TerminusDB.
    pub async fn resolve_symbol(
        &self,
        lib_name: &str,
        lib_version: &str,
        symbol_name: &str,
    ) -> Result<Option<(GlobalSymbolId, String)>> {
        let row = sqlx::query(
            "SELECT id, entry_uri FROM global_symbols \
             WHERE lib_name = ? AND lib_version = ? AND symbol_name = ? \
             LIMIT 1",
        )
        .bind(lib_name)
        .bind(lib_version)
        .bind(symbol_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::GlobalStore(format!("resolve_symbol: {e}")))?;

        match row {
            None => Ok(None),
            Some(r) => {
                use sqlx::Row;
                let id_str: &str = r.try_get("id").map_err(|e| Error::GlobalStore(e.to_string()))?;
                let entry_uri: String = r.try_get("entry_uri").map_err(|e| Error::GlobalStore(e.to_string()))?;
                let uuid = Uuid::parse_str(id_str)
                    .map_err(|e| Error::GlobalStore(format!("bad uuid in db: {e}")))?;
                Ok(Some((GlobalSymbolId(uuid), entry_uri)))
            }
        }
    }

    /// Count how many symbols are registered for a library.
    pub async fn symbol_count(&self, lib_name: &str, lib_version: &str) -> Result<u64> {
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT COUNT(*) as n FROM global_symbols WHERE lib_name = ? AND lib_version = ?",
        )
        .bind(lib_name)
        .bind(lib_version)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::GlobalStore(e.to_string()))?;

        let n: i64 = row.try_get("n").map_err(|e| Error::GlobalStore(e.to_string()))?;
        Ok(n as u64)
    }

    /// Count how many blob refs are currently deferred for a library.
    pub async fn deferred_count(&self, lib_name: &str, lib_version: &str) -> Result<u64> {
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT COUNT(*) as n FROM deferred_queue WHERE lib_name = ? AND lib_version = ?",
        )
        .bind(lib_name)
        .bind(lib_version)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Queue(e.to_string()))?;

        let n: i64 = row.try_get("n").map_err(|e| Error::Queue(e.to_string()))?;
        Ok(n as u64)
    }
}

// ── GlobalSymbolStore ─────────────────────────────────────────────────────────

#[async_trait]
impl GlobalSymbolStore for NudoxStore {
    #[instrument(skip(self), fields(lib_name = %lib.name, lib_version = %lib.version, symbol_name))]
    async fn lookup(&self, lib: &LibRef, symbol_name: &str) -> Result<Option<GlobalSymbolId>> {
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT id FROM global_symbols \
             WHERE lib_name = ? AND lib_version = ? AND symbol_name = ? \
             LIMIT 1",
        )
        .bind(&lib.name)
        .bind(&lib.version)
        .bind(symbol_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::GlobalStore(format!("lookup: {e}")))?;

        match row {
            None => Ok(None),
            Some(r) => {
                let id_str: &str = r.try_get("id").map_err(|e| Error::GlobalStore(e.to_string()))?;
                let uuid = Uuid::parse_str(id_str)
                    .map_err(|e| Error::GlobalStore(format!("bad uuid in db: {e}")))?;
                Ok(Some(GlobalSymbolId(uuid)))
            }
        }
    }

    #[instrument(skip(self), fields(global_id = %global_id, occurrence_id = %occurrence))]
    async fn associate(&self, global_id: GlobalSymbolId, occurrence: OccurrenceId) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO occurrence_associations \
             (global_symbol_id, occurrence_id) VALUES (?, ?)",
        )
        .bind(global_id.to_string())
        .bind(occurrence.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| Error::GlobalStore(format!("associate: {e}")))?;
        Ok(())
    }
}

// ── FutureParseQueue ──────────────────────────────────────────────────────────

#[async_trait]
impl FutureParseQueue for NudoxStore {
    #[instrument(skip(self), fields(lib_name = %lib.name, lib_version = %lib.version, blob_ref = %blob_ref))]
    async fn enqueue(&self, lib: LibRef, blob_ref: BlobRef) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO deferred_queue \
             (blob_ref, lib_name, lib_version) VALUES (?, ?, ?)",
        )
        .bind(&blob_ref.0)
        .bind(&lib.name)
        .bind(&lib.version)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::Queue(format!("enqueue: {e}")))?;
        Ok(())
    }

    #[instrument(skip(self), fields(lib_name = %lib.name, lib_version = %lib.version))]
    async fn drain_for_lib(&self, lib: &LibRef) -> Result<Vec<BlobRef>> {
        use sqlx::Row;

        // Atomic drain: select then delete in one transaction.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::Queue(format!("begin tx: {e}")))?;

        let rows = sqlx::query(
            "SELECT blob_ref FROM deferred_queue \
             WHERE lib_name = ? AND lib_version = ?",
        )
        .bind(&lib.name)
        .bind(&lib.version)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| Error::Queue(format!("drain select: {e}")))?;

        sqlx::query(
            "DELETE FROM deferred_queue WHERE lib_name = ? AND lib_version = ?",
        )
        .bind(&lib.name)
        .bind(&lib.version)
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::Queue(format!("drain delete: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| Error::Queue(format!("commit: {e}")))?;

        rows.into_iter()
            .map(|r| {
                let s: String = r.try_get("blob_ref").map_err(|e| Error::Queue(e.to_string()))?;
                Ok(BlobRef(s))
            })
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_core::{FutureParseQueue, GlobalSymbolStore};
    use tempfile::NamedTempFile;

    async fn open_test_store() -> Arc<NudoxStore> {
        let tmp = NamedTempFile::new().unwrap();
        NudoxStore::open(tmp.path(), "test_org/test_db").await.unwrap()
    }

    // ── symbol_id ──────────────────────────────────────────────────────────────

    #[test]
    fn symbol_id_is_deterministic() {
        let a = symbol_id("nudox_org/nudox_lib", "Entry/rust/serde/Serialize");
        let b = symbol_id("nudox_org/nudox_lib", "Entry/rust/serde/Serialize");
        assert_eq!(a, b);
    }

    #[test]
    fn symbol_id_differs_by_instance() {
        let a = symbol_id("org/db_a", "Entry/rust/serde/Serialize");
        let b = symbol_id("org/db_b", "Entry/rust/serde/Serialize");
        assert_ne!(a, b);
    }

    #[test]
    fn symbol_id_differs_by_entry_uri() {
        let a = symbol_id("org/db", "Entry/rust/serde/Serialize");
        let b = symbol_id("org/db", "Entry/rust/serde/Deserialize");
        assert_ne!(a, b);
    }

    // ── register_library ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn register_library_inserts_symbols() {
        let store = open_test_store().await;
        let n = store
            .register_library("rust", "serde", "1.0.0", [
                "Entry/rust/serde/Serialize",
                "Entry/rust/serde/Deserialize",
            ])
            .await
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.symbol_count("serde", "1.0.0").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn register_library_is_idempotent() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();
        // Second call with same URI should be ignored.
        let n2 = store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();
        assert_eq!(n2, 0, "duplicate registration should be ignored");
        assert_eq!(store.symbol_count("serde", "1.0.0").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn register_library_extracts_symbol_name() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", [
                "Entry/rust/serde/ser/Serialize",
            ])
            .await
            .unwrap();
        // Lookup must work using just the bare name "Serialize"
        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let result = store.lookup(&lib, "Serialize").await.unwrap();
        assert!(result.is_some());
    }

    // ── GlobalSymbolStore ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn lookup_returns_none_before_registration() {
        let store = open_test_store().await;
        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let result = store.lookup(&lib, "Serialize").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn lookup_returns_stable_id_after_registration() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();

        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let gid = store.lookup(&lib, "Serialize").await.unwrap().unwrap();

        // Must equal the offline-computed UUID.
        let expected = symbol_id("test_org/test_db", "Entry/rust/serde/Serialize");
        assert_eq!(gid, expected);
    }

    #[tokio::test]
    async fn lookup_is_version_scoped() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();

        let lib_v1 = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let lib_v2 = LibRef { name: "serde".into(), version: "2.0.0".into() };

        assert!(store.lookup(&lib_v1, "Serialize").await.unwrap().is_some());
        assert!(store.lookup(&lib_v2, "Serialize").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn associate_records_occurrence() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();

        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let gid = store.lookup(&lib, "Serialize").await.unwrap().unwrap();
        let occ = OccurrenceId(Uuid::new_v4());

        store.associate(gid, occ).await.unwrap();
        // Second call is idempotent.
        store.associate(gid, occ).await.unwrap();
    }

    // ── FutureParseQueue ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn enqueue_and_drain_round_trips() {
        let store = open_test_store().await;
        let lib = LibRef { name: "tokio".into(), version: "1.0.0".into() };

        store.enqueue(lib.clone(), BlobRef("blob-a".into())).await.unwrap();
        store.enqueue(lib.clone(), BlobRef("blob-b".into())).await.unwrap();
        // Duplicate is ignored.
        store.enqueue(lib.clone(), BlobRef("blob-a".into())).await.unwrap();

        assert_eq!(store.deferred_count("tokio", "1.0.0").await.unwrap(), 2);

        let mut drained = store.drain_for_lib(&lib).await.unwrap();
        drained.sort_by_key(|r| r.0.clone());
        assert_eq!(drained, vec![BlobRef("blob-a".into()), BlobRef("blob-b".into())]);

        // Queue is now empty.
        assert_eq!(store.deferred_count("tokio", "1.0.0").await.unwrap(), 0);
        assert!(store.drain_for_lib(&lib).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn drain_is_scoped_to_lib_version() {
        let store = open_test_store().await;
        let lib_v1 = LibRef { name: "tokio".into(), version: "1.0.0".into() };
        let lib_v2 = LibRef { name: "tokio".into(), version: "2.0.0".into() };

        store.enqueue(lib_v1.clone(), BlobRef("for-v1".into())).await.unwrap();
        store.enqueue(lib_v2.clone(), BlobRef("for-v2".into())).await.unwrap();

        let drained_v1 = store.drain_for_lib(&lib_v1).await.unwrap();
        assert_eq!(drained_v1, vec![BlobRef("for-v1".into())]);
        // v2 queue is untouched.
        assert_eq!(store.deferred_count("tokio", "2.0.0").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn resolve_symbol_returns_entry_uri() {
        let store = open_test_store().await;
        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();

        let (gid, uri) = store
            .resolve_symbol("serde", "1.0.0", "Serialize")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(uri, "Entry/rust/serde/Serialize");
        assert_eq!(gid, symbol_id("test_org/test_db", "Entry/rust/serde/Serialize"));
    }

    // ── persistence across reopen ─────────────────────────────────────────────

    #[tokio::test]
    async fn symbols_persist_across_db_reopen() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        // Write symbols and a deferred queue entry, then close the pool.
        {
            let store = NudoxStore::open(&db_path, "test_org/test_db").await.unwrap();
            store
                .register_library("rust", "serde", "1.0.0", [
                    "Entry/rust/serde/Serialize",
                    "Entry/rust/serde/Deserialize",
                ])
                .await
                .unwrap();
            let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
            store.enqueue(lib, BlobRef("deferred-blob".into())).await.unwrap();
        }

        // `tmp` keeps the file alive; re-open at the same path.
        let store2 = NudoxStore::open(&db_path, "test_org/test_db").await.unwrap();
        assert_eq!(store2.symbol_count("serde", "1.0.0").await.unwrap(), 2);
        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        assert!(store2.lookup(&lib, "Serialize").await.unwrap().is_some());
        assert!(store2.lookup(&lib, "Deserialize").await.unwrap().is_some());
        assert_eq!(store2.deferred_count("serde", "1.0.0").await.unwrap(), 1);
        let drained = store2.drain_for_lib(&lib).await.unwrap();
        assert_eq!(drained, vec![BlobRef("deferred-blob".into())]);
    }
}
