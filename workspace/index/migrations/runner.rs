//! Migration runner for the schema-v4 catalog (INDEX-PLAN §13, ID-5).
//!
//! Protocol (§13):
//! 1. Read `schema_meta.user_version`; if already `SCHEMA_VERSION` return early
//!    (idempotence check #1).
//! 2. Create a `pre-migrate-v<N>` branch as a rollback point.
//! 3. Execute each DDL statement from [`super::ddl::schema_v4_statements`]. All
//!    statements include `IF NOT EXISTS` (idempotence check #2), so a partial
//!    prior run cannot cause a duplicate-object error.
//! 4. On any execute failure: `dolt_checkout` back to the pre-migrate branch and
//!    surface the error.
//! 5. On success: write `user_version = SCHEMA_VERSION` and return `Ok(())`.
//!
//! Only [`migrate_to_v4`] needs a [`VersioningEngine`]; the version-read and
//! version-write helpers accept the plain [`CatalogEngine`] trait so they can be
//! called from unit tests that use the memory engine without versioning.

use crate::engine::{BranchName, CatalogEngine, EngineError, VersioningEngine};
use crate::SCHEMA_VERSION;

use super::ddl;

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors the migration runner can surface.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// An engine-level SQL or versioning failure.
    #[error("engine error during migration: {0}")]
    Engine(#[from] EngineError),

    /// The `schema_meta` table exists but contained no rows (corrupt state).
    /// The caller should treat the database as requiring a fresh migration.
    #[error("schema_meta table exists but is empty")]
    EmptyMeta,
}

// ─────────────────────────────────────────────────────────────────────────────
// Version helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Read the `user_version` from `schema_meta`.
///
/// Returns `0` when `schema_meta` does not yet exist (cold catalog), so callers
/// can always compare against [`SCHEMA_VERSION`] without special-casing a
/// missing table.
///
/// This function deliberately tolerates engine errors on the SELECT (treating
/// them as "table not found → version 0") while propagating all other errors
/// returned from a successful query that yields unexpected data.
pub fn current_user_version<E: CatalogEngine>(engine: &E) -> Result<u32, MigrationError> {
    // Attempt to read the single row. If the engine returns an error the most
    // common cause is "no such table: schema_meta" on a cold catalog. We treat
    // any engine error on this SELECT as version 0 (not yet migrated).
    let rows = engine.query_rows(
        "SELECT user_version FROM schema_meta LIMIT 1",
        &[],
        &mut |row| row.get_integer(0),
    );

    match rows {
        Err(_engine_err) => {
            // Most likely "no such table". Treat as version 0.
            Ok(0)
        }
        Ok(values) => {
            if values.is_empty() {
                // Table exists but is empty — unusual; treat as version 0 so
                // the migration re-runs the DDL and inserts the row.
                Ok(0)
            } else {
                // LIMIT 1 → at most one element.
                Ok(values[0] as u32)
            }
        }
    }
}

/// Overwrite `schema_meta.user_version` with `version`.
///
/// Uses a DELETE + INSERT pattern to maintain the single-row invariant without
/// relying on engine-specific UPSERT syntax (REPLACE INTO / INSERT OR REPLACE
/// are SQLite extensions but not uniformly available in rusqdoltlite's surface).
pub fn set_user_version<E: CatalogEngine>(engine: &E, version: u32) -> Result<(), MigrationError> {
    engine.execute("DELETE FROM schema_meta", &[])?;
    engine.execute(
        "INSERT INTO schema_meta (user_version) VALUES (?1)",
        &[crate::engine::Value::Integer(i64::from(version))],
    )?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Migration entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Apply the schema-v4 DDL to `engine` (INDEX-PLAN §13, ID-5).
///
/// # Idempotence
///
/// This function is fully idempotent:
/// - **Check #1 (version guard):** if `schema_meta.user_version` already equals
///   [`SCHEMA_VERSION`] the function returns `Ok(())` immediately without
///   touching any table.
/// - **Check #2 (IF NOT EXISTS):** every CREATE TABLE and CREATE INDEX statement
///   in [`ddl::schema_v4_statements`] uses `IF NOT EXISTS`, so a partial prior
///   run that was interrupted cannot cause duplicate-object errors on retry.
///
/// # Rollback
///
/// Before executing any DDL, the runner creates a DoltLite branch named
/// `pre-migrate-v<N>` (e.g. `pre-migrate-v4`). If any DDL statement fails, the
/// runner checks out that branch (restoring the database to its pre-migration
/// state) and returns the error. On success the branch is kept as a permanent
/// rollback point (§13 says "checkout on failure"; it does not mandate deleting
/// the branch on success).
///
/// # Errors
///
/// Returns [`MigrationError::Engine`] if any DDL execution or versioning call
/// fails. On a DDL failure the rollback checkout is attempted before returning;
/// if the checkout itself fails, the checkout error is returned (the DDL error
/// is logged via `tracing::error!` rather than lost).
pub fn migrate_to_v4<E: VersioningEngine>(engine: &E) -> Result<(), MigrationError> {
    // ── Idempotence check #1 ──────────────────────────────────────────────────
    let current = current_user_version(engine)?;
    if current == SCHEMA_VERSION {
        tracing::debug!(
            version = SCHEMA_VERSION,
            "migrate_to_v4: already at target schema version, skipping"
        );
        return Ok(());
    }

    tracing::info!(
        from_version = current,
        to_version = SCHEMA_VERSION,
        "migrate_to_v4: beginning migration"
    );

    // ── Create rollback branch ────────────────────────────────────────────────
    let rollback_branch = BranchName(format!("pre-migrate-v{SCHEMA_VERSION}"));
    engine.dolt_branch_create(&rollback_branch)?;
    tracing::debug!(branch = %rollback_branch, "migrate_to_v4: rollback branch created");

    // ── Execute DDL statements ────────────────────────────────────────────────
    let statements = ddl::schema_v4_statements();
    for sql in &statements {
        if let Err(ddl_err) = engine.execute(sql, &[]) {
            // Attempt rollback: checkout the pre-migrate branch.
            tracing::error!(
                error = %ddl_err,
                failed_sql = %sql,
                "migrate_to_v4: DDL failed, attempting rollback checkout"
            );
            // Best-effort rollback. If checkout also fails, return the checkout
            // error (it is the more actionable one; the DDL error was logged).
            engine.dolt_checkout(&rollback_branch)?;
            return Err(MigrationError::Engine(ddl_err));
        }
    }

    // ── Write version marker ──────────────────────────────────────────────────
    set_user_version(engine, SCHEMA_VERSION)?;

    tracing::info!(
        version = SCHEMA_VERSION,
        "migrate_to_v4: migration complete"
    );

    Ok(())
}
