//! Pipeline part: **package admin API** (`server::http`).
//!
//! TDD specs for the write/admin plane: tracking a package kicks off a
//! background sync and the request returns immediately.

/// Adding a package returns immediately while sync runs in the background.
///
/// Act: `POST /api/packages`.
/// Assert: responds 202 with `sync_status = "queued"` / `health = "pending"`,
///   then a poll reaches a terminal healthy/idle state.
#[tokio::test]
async fn add_package_returns_immediately_then_syncs() {
    todo!("assert 202 queued -> terminal healthy");
}

/// Adding a duplicate package returns the existing record, no re-queue.
///
/// Assert: a second add of the same `PackageKey` returns 200 with the original
///   id and does not enqueue another sync.
#[tokio::test]
async fn duplicate_add_returns_existing() {
    todo!("assert dedup add returns existing id");
}

/// Listing and getting reflect tracked packages.
///
/// Assert: `GET /api/packages` lists the tracked package and
///   `GET /api/packages/{id}` returns its snapshot.
#[tokio::test]
async fn list_and_get_reflect_tracked_packages() {
    todo!("assert list/get endpoints");
}

/// An unsupported language is rejected with 400.
#[tokio::test]
async fn unsupported_language_is_rejected() {
    todo!("assert 400 for unsupported language");
}

/// The read plane never holds a handle the write plane mutates.
///
/// Assert: a query (`QueryState`) cannot reach the registry's mutating handle
///   (`AdminState`) — the read/write split is enforced.
#[tokio::test]
async fn read_and_write_planes_are_separated() {
    todo!("assert read/write state separation");
}
