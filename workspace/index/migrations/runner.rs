//! Migration runner for the schema-v4 catalog (INDEX-PLAN §13, ID-5).
//!
//! Protocol (§13):
//! 1. Read `schema_meta.user_version`; if already `SCHEMA_VERSION` return
//!    early.
//! 2. Create a `pre-migrate-v<N>` branch as a rollback point.
//! 3. Execute entity-derived DDL from [`super::ddl::schema_v4_statements`].
//! 4. On DDL failure: checkout the pre-migrate branch and surface the error.
//! 5. On success: write `user_version = SCHEMA_VERSION`.

use sea_orm::{
    ActiveValue::Set,
    DbBackend, EntityTrait, QuerySelect, QueryTrait,
    sea_query::{Query, SqliteQueryBuilder},
};

use crate::{
    SCHEMA_VERSION,
    engine::{self, BranchName, CatalogEngine, EngineError, VersioningEngine},
    entity::schema_meta,
};

use super::ddl;

/// Errors the migration runner can surface.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// An engine-level SQL or versioning failure.
    #[error("engine error during migration: {0}")]
    Engine(#[from] EngineError),

    /// The `schema_meta` table exists but contained no rows (corrupt state).
    #[error("schema_meta table exists but is empty")]
    EmptyMeta,
}

/// Read the `user_version` from `schema_meta`.
///
/// Returns `0` when the table is missing (cold catalog).
pub fn current_user_version<E: CatalogEngine>(engine: &E) -> Result<u32, MigrationError> {
    let stmt = schema_meta::Entity::find()
        .select_only()
        .column(schema_meta::Column::UserVersion)
        .limit(1)
        .build(DbBackend::Sqlite);

    let rows = engine::query(engine, stmt, &mut |row| row.get_integer(0));

    match rows {
        Err(_engine_err) => Ok(0),
        Ok(values) if values.is_empty() => Ok(0),
        Ok(values) => Ok(values[0] as u32),
    }
}

/// Overwrite `schema_meta.user_version` with `version` (single-row invariant).
pub fn set_user_version<E: CatalogEngine>(engine: &E, version: u32) -> Result<(), MigrationError> {
    let delete = Query::delete().from_table(schema_meta::Entity).to_owned();
    let (delete_sql, delete_values) = delete.build(SqliteQueryBuilder);
    engine.execute(&delete_sql, &engine::stmt::values_to_engine(&delete_values))?;

    let am = schema_meta::ActiveModel {
        user_version: Set(version as i32),
    };
    let insert = schema_meta::Entity::insert(am).build(DbBackend::Sqlite);
    engine::exec(engine, insert)?;
    Ok(())
}

/// Add `edges.optional` when a catalog was created before that column existed.
///
/// `CREATE TABLE IF NOT EXISTS` does not alter an existing table. A fresh
/// database already has the column from the entity DDL.
fn ensure_edge_optional<E: CatalogEngine>(engine: &E) -> Result<(), MigrationError> {
    let stmt = sea_orm::Statement::from_string(DbBackend::Sqlite, "PRAGMA table_info(edges)");
    let names = engine::query(engine, stmt, &mut |row| row.get_text(1))?;
    if names.iter().any(|name| name == "optional") {
        return Ok(());
    }
    engine.execute(
        "ALTER TABLE edges ADD COLUMN optional INTEGER NOT NULL DEFAULT 0",
        &[],
    )?;
    Ok(())
}

/// Apply the schema-v4 DDL to `engine` (INDEX-PLAN §13, ID-5).
pub fn migrate_to_v4<E: VersioningEngine>(engine: &E) -> Result<(), MigrationError> {
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

    let rollback_branch = BranchName(format!("pre-migrate-v{SCHEMA_VERSION}"));
    engine.dolt_branch_create(&rollback_branch)?;
    tracing::debug!(branch = %rollback_branch, "migrate_to_v4: rollback branch created");

    let statements = ddl::schema_v4_statements();
    for sql in &statements {
        if let Err(ddl_err) = engine.execute(sql, &[]) {
            tracing::error!(
                error = %ddl_err,
                failed_sql = %sql,
                "migrate_to_v4: DDL failed, attempting rollback checkout"
            );
            engine.dolt_checkout(&rollback_branch)?;
            return Err(MigrationError::Engine(ddl_err));
        }
    }

    ensure_edge_optional(engine)?;

    set_user_version(engine, SCHEMA_VERSION)?;

    tracing::info!(
        version = SCHEMA_VERSION,
        "migrate_to_v4: migration complete"
    );

    Ok(())
}
