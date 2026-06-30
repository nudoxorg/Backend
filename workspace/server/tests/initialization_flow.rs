//! Diagram flow: **initialization** (`server::coordination::initialization`).
//!
//! "if index (otherwise let index) · tracks the usage of a library, coordinating
//! tiers, pulling down if unfresh · updates postgres status · loads into pg ·
//! ensure init (if not send req) · return responses".

/// An uninitialized library is indexed on first use.
///
/// Act: initialize a library that has never been parsed.
/// Assert: indexing is kicked off (or deferred to an indexer) and postgres
///   status moves off "unindexed".
#[tokio::test]
async fn first_use_triggers_indexing() {
    todo!("assert first use indexes (or defers) the library");
}

/// An already-initialized, fresh library is served without re-indexing.
///
/// Assert: when the stored code/treesitter hash matches, no re-parse happens.
#[tokio::test]
async fn fresh_library_is_served_without_reindex() {
    todo!("assert fresh libraries skip re-indexing");
}

/// A stale (unfresh) library is pulled down again.
///
/// Assert: when the remote hash differs from the stored one, the library is
///   re-fetched and re-indexed, coordinating its tier.
#[tokio::test]
async fn unfresh_library_is_pulled_down_again() {
    todo!("assert stale libraries are refreshed");
}

/// `ensure init` returns immediately when ready, else requests initialization.
///
/// Assert: a ready library returns responses directly; a not-ready one returns a
///   "request sent / pending" response rather than blocking.
#[tokio::test]
async fn ensure_init_returns_or_requests() {
    todo!("assert ensure-init returns-when-ready / requests-when-not");
}

/// Initialization loads the library into postgres (pg).
///
/// Assert: after init, postgres holds the package's metadata + status.
#[tokio::test]
async fn initialization_loads_into_postgres() {
    todo!("assert init loads metadata into pg");
}

/// Library usage is tracked to coordinate tiers.
///
/// Assert: repeated use updates usage so hotter libraries can be tiered/kept
///   fresh.
#[tokio::test]
async fn usage_is_tracked_for_tiering() {
    todo!("assert usage tracking feeds tier coordination");
}
