//! Index health snapshot for the replica-local package Tantivy index.
//!
//! Ops / readiness surfaces read a point-in-time [`PackageIndexHealth`] via
//! [`super::tantivy::PackageIndex::health_snapshot`] without re-opening the
//! index directory. This is intentionally distinct from the registry-wide
//! [`crate::health`] roll-up (postgres / object store / derived planes).

use std::path::PathBuf;

/// Point-in-time health of a replica-local package Tantivy index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageIndexHealth {
    /// Schema version the process is currently writing/reading (`SCHEMA_VERSION`).
    pub schema_version: u32,
    /// Number of live documents visible to the current searcher.
    pub num_docs: u64,
    /// Last absorbed postgres sync watermark position (micros, or 0 if never synced).
    pub watermark_position: i64,
    /// On-disk path of the index directory.
    pub path: PathBuf,
}
