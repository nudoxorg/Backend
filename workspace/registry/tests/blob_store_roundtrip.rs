//! Pipeline part: **blob storage** (`registry::blob`).
//!
//! TDD specs for the content-addressed blob store: the immutable CST+IR+tar body
//! plus a mutable resolution sidecar. Mirrors the old `blobstore` guarantees.

/// A blob round-trips: what you put is what you get.
///
/// Act: `put(blob)` then `get(&ref)`.
/// Assert: the retrieved blob equals the stored one.
#[tokio::test]
async fn put_then_get_round_trips() {
    todo!("assert put/get round-trips a blob");
}

/// Storage is content-addressed and idempotent.
///
/// Assert: putting identical content twice yields the same `BlobRef` (a hash),
///   and `list()` shows a single entry.
#[tokio::test]
async fn put_is_content_addressed_and_idempotent() {
    todo!("assert identical content -> identical ref, list len 1");
}

/// The resolution state is excluded from the content address.
///
/// Assert: an `Unresolved` and a later-`Resolved` form of the same blob hash to
///   the same `BlobRef` (re-resolving never re-keys).
#[tokio::test]
async fn resolution_is_excluded_from_the_content_address() {
    todo!("assert resolution does not affect the blob ref");
}

/// Updating resolution writes the sidecar and leaves the body write-once.
///
/// Assert: `update_resolution(ref, gid)` makes `get` return `Resolved(gid)` while
///   the immutable body bytes are unchanged.
#[tokio::test]
async fn update_resolution_leaves_body_write_once() {
    todo!("assert sidecar overlay + write-once body");
}

/// A wrong blob schema version is rejected on put.
///
/// Assert: putting a blob whose `blob_schema_version` differs from the current
///   constant returns a `SchemaVersionMismatch`.
#[tokio::test]
async fn put_with_wrong_schema_version_is_rejected() {
    todo!("assert schema-version mismatch is rejected");
}

/// Getting an unknown ref returns NotFound.
#[tokio::test]
async fn get_missing_ref_returns_not_found() {
    todo!("assert NotFound for an unknown blob ref");
}

/// Blobs persist across a store reopen.
///
/// Assert: after dropping and reopening the store over the same dir, a
///   previously-put blob is still retrievable.
#[tokio::test]
async fn blobs_persist_across_reopen() {
    todo!("assert persistence across store reopen");
}
