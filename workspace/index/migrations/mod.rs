//! Schema-v4 migrations for the versioned catalog (INDEX-PLAN ID-5, §13).
//!
//! # Protocol (§13 — pre-migrate branch + rollback = checkout)
//!
//! 1. Read `schema_meta.user_version`. If it already equals
//!    [`crate::SCHEMA_VERSION`], return early (fully idempotent).
//! 2. Create a DoltLite branch `pre-migrate-v<N>` as a durable rollback point.
//! 3. Execute all DDL statements from [`ddl::schema_v4_statements`] in order.
//!    Every statement uses `IF NOT EXISTS`, so retrying a partial migration is
//!    safe (idempotence guarantee #2).
//! 4. On any DDL failure: `dolt_checkout(pre-migrate-v<N>)` to restore the
//!    database, then surface the error.
//! 5. On success: write `user_version = SCHEMA_VERSION` to `schema_meta` and
//!    return `Ok(())`.
//!
//! # Modules
//!
//! - [`ddl`] — `schema_v4_statements()` returning all CREATE TABLE / CREATE
//!   INDEX strings, one per sea-query builder call.
//! - [`runner`] — `migrate_to_v4`, `current_user_version`, `set_user_version`.

pub mod ddl;
pub mod runner;

pub use runner::{MigrationError, current_user_version, migrate_to_v4, set_user_version};
