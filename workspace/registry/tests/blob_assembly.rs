//! Pipeline part: **blob assembly + emission** (`registry::blob::{creation, emit}`).
//!
//! TDD specs for building a `Blob` from the three compiler resolutions and
//! shipping it off for ingestion + signaling the rest of the pipeline.

/// A blob is assembled from the CST, the IR surface, and the tarred source.
///
/// Act: `creation::build_blob(cst_tree, ir_index, tarred_source)`.
/// Assert: the resulting `Blob` carries all three representations for one
///   package version.
#[test]
fn blob_assembles_all_three_resolutions() {
    todo!("assert a blob holds CST + IR + tarred source");
}

/// The tarred source is a real, readable tar archive.
///
/// Assert: the blob's `source_text` archive yields the package's source files
///   when its entries are read back.
#[test]
fn tarred_source_is_a_readable_archive() {
    todo!("assert the tar archive round-trips the source files");
}

/// Emitting a blob uploads it to object storage and signals downstream.
///
/// Act: `emit::emit_blob(&blob)`.
/// Assert: the blob lands in the (S3-like) object store and the global registry
///   is told the blob now exists (coordination).
#[tokio::test]
async fn emit_uploads_and_signals() {
    todo!("assert upload to object store + downstream signal");
}

/// Emission retries transient upload failures with backoff.
///
/// Assert: a transient failure is retried (per the `Sink` backoff) and
///   eventually succeeds, while a permanent failure is surfaced.
#[tokio::test]
async fn emit_retries_transient_failures() {
    todo!("assert transient retry vs permanent failure");
}
