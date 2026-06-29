//! Pipeline part: **blob/registry coordination** (`registry::cordination`).
//!
//! TDD specs for recording the creation of a blob to the surrounding systems
//! (the global registry) so the index knows a package is now stored.

/// Recording a blob's creation registers it in the global index.
///
/// Act: `record_blob_creation(coord, blob_ref)`.
/// Assert: the global store now reports the package's symbols as registered,
///   with their canonical global ids.
#[tokio::test]
async fn recording_a_blob_registers_it_globally() {
    todo!("assert blob creation is recorded to the global registry");
}

/// Recording is idempotent.
///
/// Assert: recording the same blob twice does not double-register its symbols.
#[tokio::test]
async fn recording_is_idempotent() {
    todo!("assert duplicate recordings are no-ops");
}

/// Recording flips the package's resolution state toward `Stored`.
///
/// Assert: after coordination, the index entry reflects "loaded into the runtime
///   stores" rather than still pending.
#[tokio::test]
async fn recording_advances_resolution_state() {
    todo!("assert coordination advances the index lifecycle");
}
