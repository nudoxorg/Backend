//! Shared test harness: build a migrated in-memory catalog writer.
//!
//! Every integration test opens a [`MemoryEngine`], migrates it to schema v4,
//! and wraps it in a [`CatalogWriter`]. The memory engine is the test-only
//! facade fake (never a product mode); versioning calls run against its honest
//! recorded commit log.
//!
//! Not every test binary uses every helper, so allow dead code here.
#![allow(dead_code)]

use index::engine::memory::MemoryEngine;
use index::migrations::runner::migrate_to_v4;
use index::store::writer::CatalogWriter;

/// Open a fresh, migrated in-memory catalog writer.
pub fn migrated_writer() -> CatalogWriter<MemoryEngine> {
    let engine = MemoryEngine::open_in_memory().expect("open in-memory engine");
    migrate_to_v4(&engine).expect("migrate to schema v4");
    CatalogWriter::new(engine)
}

/// A deterministic package stem id from a small seed.
pub fn stem_id(seed: u8) -> index::ids::PackageStemId {
    let mut bytes = [0u8; 16];
    bytes[0] = seed;
    index::ids::PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// A deterministic version (package) id from a small seed.
pub fn version_id(seed: u8) -> index::ids::PackageId {
    let mut bytes = [0u8; 16];
    bytes[15] = seed;
    index::ids::PackageId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// A deterministic generation stamp from a small seed.
pub fn gen_stamp(seed: u8) -> index::ids::GenerationStamp {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    index::ids::GenerationStamp::from_bytes(bytes)
}
