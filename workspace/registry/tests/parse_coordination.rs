//! Diagram: **postgres is in charge of load balancing, coordination, and the
//! status of every parse.**
//!
//! TDD specs (`todo!()`) for `registry::queue`.

/// Parse work is enqueued and drained in coordination order.
///
/// Assert: enqueued packages are handed out to workers and removed atomically
///   (no double-processing).
#[tokio::test]
async fn parse_work_is_enqueued_and_drained() {
    todo!("assert enqueue/drain without double-processing");
}

/// Parse status is tracked through its lifecycle.
///
/// Assert: a unit of work moves queued -> running -> done (or failed) in
///   postgres.
#[tokio::test]
async fn parse_status_is_tracked() {
    todo!("assert parse status lifecycle");
}

/// Concurrent parses are load-balanced under a bound.
///
/// Assert: no more than the configured number of parses run at once.
#[tokio::test]
async fn concurrent_parses_are_bounded() {
    todo!("assert bounded concurrent parses");
}

/// Stale (unfresh) libraries are re-queued by hash comparison.
///
/// Assert: when a library's stored hash differs from the remote, it is
///   re-enqueued for parsing.
#[tokio::test]
async fn stale_libraries_are_requeued() {
    todo!("assert stale libraries are re-queued");
}

/// Drain is scoped per library/version (tiers don't steal each other's work).
#[tokio::test]
async fn drain_is_scoped_per_library() {
    todo!("assert per-library drain scoping");
}
