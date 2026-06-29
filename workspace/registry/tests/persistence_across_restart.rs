//! Pipeline part: **registry persistence** (`registry::persist`).
//!
//! TDD specs for surviving a process restart durably, and rehydrating into a
//! clean, re-enqueueable state.

/// A published registry survives a restart.
///
/// Assert: after persisting and reloading, the same packages (and their ids)
///   are present.
#[tokio::test]
async fn registry_persists_across_restart() {
    todo!("assert packages + ids survive a reload");
}

/// Re-adding a persisted package returns the existing id, not a duplicate.
///
/// Assert: adding an identical `PackageKey` after reload returns the original
///   id rather than allocating a new one.
#[tokio::test]
async fn duplicate_add_after_reload_returns_existing_id() {
    todo!("assert dedup by package key across restart");
}

/// Rehydration clears transient sync progress.
///
/// Assert: a package that was mid-sync when persisted reloads with its sync
///   state reset to idle (so it can be safely re-enqueued).
#[tokio::test]
async fn rehydrate_clears_transient_sync_progress() {
    todo!("assert transient sync state resets on reload");
}
