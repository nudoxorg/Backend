//! Migration tests: schema v4 applies, is idempotent (running twice is a no-op),
//! and tracks the user_version.

mod common;

use common::migrated_writer;

use index::engine::{BranchName, Configured, VersioningEngine};
use index::migrations::runner::{current_user_version, migrate_to_v4};

#[test]
fn migrate_sets_user_version_to_schema_version() {
    let engine = Configured::open_in_memory().expect("open");
    migrate_to_v4(&engine).expect("migrate");
    assert_eq!(
        current_user_version(&engine).expect("read version"),
        index::SCHEMA_VERSION
    );
}

#[test]
fn migration_is_idempotent() {
    let engine = Configured::open_in_memory().expect("open");
    migrate_to_v4(&engine).expect("first migrate");
    // Running again must be a clean no-op (user_version check + IF NOT EXISTS).
    migrate_to_v4(&engine).expect("second migrate is a no-op");
    // A third run for good measure.
    migrate_to_v4(&engine).expect("third migrate is a no-op");
    assert_eq!(
        current_user_version(&engine).expect("version"),
        index::SCHEMA_VERSION
    );
}

#[test]
fn fresh_engine_reports_user_version_zero() {
    let engine = Configured::open_in_memory().expect("open");
    // Before migration, no schema_meta table exists; must report 0, not panic.
    assert_eq!(current_user_version(&engine).expect("version"), 0);
}

#[test]
fn migration_creates_the_pre_migrate_branch() {
    // §13: the runner creates a `pre-migrate-v<N>` snapshot branch before running
    // DDL, kept as a durable rollback point (rollback = checkout). Prove the
    // branch exists by checking it out after a successful migration.
    let engine = Configured::open_in_memory().expect("open");
    migrate_to_v4(&engine).expect("migrate");
    let branch = BranchName(format!("pre-migrate-v{}", index::SCHEMA_VERSION));
    engine
        .dolt_checkout(&branch)
        .expect("pre-migrate branch must exist and be checkout-able");
}

#[test]
fn migrated_writer_can_immediately_apply() {
    // Smoke: the harness's migrated writer accepts a write, proving the schema
    // and the writer wiring line up.
    let writer = migrated_writer();
    use index::store::MetaStore;
    let report = writer
        .apply_ops(&[index::protocol::CatalogOp::Refresh {
            stem: common::stem_id(7),
        }])
        .expect("refresh applies");
    assert_eq!(report.applied, 1);
}
