//! Diagram: **REPRODUCIBILITY** — registry persists every store so the system is
//! rebuildable: qdrant persistence, tantivy persistence, terminus persistence,
//! and the blobs (parsed code) that everything else derives from.
//!
//! TDD specs (`todo!()`) for `registry::reproducibility`.

/// Blobs are the root of reproducibility.
///
/// Assert: persisted blobs survive a full wipe + restore of every other store.
#[tokio::test]
async fn blobs_are_the_durable_root() {
    todo!("assert blobs persist as the rebuild root");
}

/// The qdrant collection can be rebuilt from the blobs.
///
/// Assert: wiping qdrant then rebuilding from blobs restores semantic search
///   identically.
#[tokio::test]
async fn qdrant_rebuilds_from_blobs() {
    todo!("assert qdrant rebuild from blobs");
}

/// The tantivy indexes can be rebuilt from the blobs.
///
/// Assert: wiping tantivy then rebuilding restores precise search identically.
#[tokio::test]
async fn tantivy_rebuilds_from_blobs() {
    todo!("assert tantivy rebuild from blobs");
}

/// The terminus graph can be rebuilt from the blobs.
///
/// Assert: wiping terminus then rebuilding restores the graph identically.
#[tokio::test]
async fn terminus_rebuilds_from_blobs() {
    todo!("assert terminus rebuild from blobs");
}

/// A full snapshot + restore round-trips every store.
///
/// Assert: snapshot all four (qdrant/tantivy/terminus/blobs), restore into a
///   fresh deployment, and every search surface returns identical results.
#[tokio::test]
async fn full_snapshot_restore_round_trips() {
    todo!("assert whole-system snapshot/restore round-trip");
}
