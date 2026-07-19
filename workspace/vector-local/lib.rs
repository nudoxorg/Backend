//! The qdrant-edge embedded store plane of the dual local/remote
//! vector-search architecture.
//!
//! This crate owns every impure concern of the local plane:
//!
//! - [`actor`] — the single-writer store actor (09c §1.1: `EdgeShard` is a
//!   synchronous engine that is not proven `Sync`; all access is serialized
//!   on one dedicated OS thread, and an Edge panic poisons the handle
//!   instead of killing the GUI process).
//! - [`shard`] — shard lifecycle: `schema.json`-validated open-or-create
//!   with the frozen client config (named vector `"sym"`, Cosine, on-disk
//!   vectors + payload, keyword facet indexes).
//! - [`store`] — [`store::LocalShardStore`], the
//!   `vector_core::VectorStore<JinaCodeV2>` implementation over the actor.
//! - [`fanout`] — the working set (mutable project shard ∪ admitted baked
//!   dep shards) and raw-score cross-shard merge (valid per 09-vector
//!   §20.6 because every quantized shard rescores to exact f32).
//! - [`pack`] / [`depshard`] — the shard-artifact contract shared with the
//!   server bakery (09-vector §20.3) and the client install/evict flow.
//! - [`hotset`] — §20.4 budget-driven admission applied to the working set.
//! - [`compact`] — automatic optimize policy (09-vector §13.4; 09c §1.1
//!   "no background optimizers" patch).
//! - [`lock`] — the multi-window advisory file lock (09c §1.1).
//!
//! Authoritative specs: `.research/librarification/09-vector/PLAN.md`
//! (§13, §20.3, §20.4, §20.6, §20.9) and
//! `09c-embeddings-runtime-adversarial.md` (§1.1, §8.1 P1/P5/P8).
//!
//! # vector-core integration points
//!
//! The sibling crate `vector-core` is authored in parallel against the same
//! frozen API. Every assumption this crate makes about vector-core's
//! concrete shapes is funneled through a small number of sites so an
//! integration mismatch is a one-line fix:
//!
//! - the [`StoreError`] constructors at the bottom of this file
//!   (`Backend`/`Corrupt` variants carrying a `String`, a unit `Closed`,
//!   and lock contention as `Io` with [`std::io::ErrorKind::WouldBlock`]),
//! - [`shard::schema_for`] (the only `ShardSchema` construction),
//! - [`store`]'s payload/filter converters,
//! - [`hotset::HotSetManager::plan`] (the only `admission::admit` call).

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
pub use depshard::{
	ArtifactFetcher, DepManifestEntry, FetchError, InstallError, InstallOutcome, RemoteRouteReason,
	evict, install,
};
pub use fanout::{SharedWorkingSet, WorkingSet, merge_hits};
pub use hotset::{AdmissionState, HotSetManager, InstallPlan, PackageStats, apply_plan, diff_plan};
pub use lock::{LOCK_FILE, ShardLock};
pub use pack::{PackError, pack_shard, pack_shard_to_file, unpack_shard};
pub use shard::{SCHEMA_FILE, VECTOR_NAME, open_or_create, schema_for};
pub use store::{LocalShardStore, upsert_raw};

use vector_core::StoreError;

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
///
/// vector-core has no dedicated lock variant, so contention is encoded as
/// [`StoreError::Io`] with [`std::io::ErrorKind::WouldBlock`] — programmatic
/// callers match on the kind, humans get the message.
pub(crate) fn locked_error(msg: impl Into<String>) -> StoreError {
	StoreError::Io(std::io::Error::new(std::io::ErrorKind::WouldBlock, msg.into()))
}
