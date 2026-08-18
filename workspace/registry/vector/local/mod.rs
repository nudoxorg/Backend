//! The qdrant-edge embedded store plane of the dual local/remote
//! vector-search architecture.
//!
//! This module owns every impure concern of the local plane:
//!
//! - [`actor`] — the single-writer store actor (09c §1.1).
//! - [`shard`] — shard lifecycle: `schema.json`-validated open-or-create.
//! - [`store`] — [`store::LocalShardStore`], the `VectorStore<JinaCodeV2>` implementation.
//! - [`fanout`] — the working set and raw-score cross-shard merge.
//! - [`pack`] / [`depshard`] — shard-artifact contract and install/evict flow.
//! - [`hotset`] — §20.4 budget-driven admission.
//! - [`compact`] — automatic optimize policy.
//! - [`lock`] — multi-window advisory file lock.

pub mod actor;
pub mod compact;
pub mod depshard;
pub mod fanout;
pub mod hotset;
pub mod lock;
pub mod pack;
pub mod shard;
pub mod store;

pub use actor::StoreHandle;
pub use compact::{
    COMPACT_DELETED_RATIO, COMPACT_IDLE, COMPACT_UPSERT_THRESHOLD, CompactPolicy, spawn_compactor,
};
pub use depshard::Error as InstallError;
pub use depshard::{
    ArtifactFetcher, DepManifestEntry, FetchError, InstallOutcome, RemoteRouteReason, evict,
    install, install_with_io,
};
pub use fanout::{SharedWorkingSet, WorkingSet, merge_hits};
pub use hotset::{AdmissionState, HotSetManager, InstallPlan, PackageStats, apply_plan, diff_plan};
pub use lock::{LOCK_FILE, ShardLock};
pub use pack::Error as PackError;
pub use pack::{pack_shard, pack_shard_to_file, unpack_shard};
pub use shard::{SCHEMA_FILE, VECTOR_NAME, open_or_create, schema_for};
pub use store::{LocalShardStore, upsert_raw};

use crate::vector::core::StoreError;

/// Wrap an Edge / IO failure as the trait-level backend error.
pub(crate) fn backend_error(err: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(err.to_string())
}

/// A shard whose on-disk state cannot be trusted (schema mismatch, torn
/// files). The message must be actionable (09c §1.1).
pub(crate) fn corrupt_error(msg: impl Into<String>) -> StoreError {
    StoreError::Corrupt(msg.into())
}

/// Another window / process owns the mutable shard (09c §1.1 multi-window).
pub(crate) fn locked_error(msg: impl Into<String>) -> StoreError {
    StoreError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        msg.into(),
    ))
}
