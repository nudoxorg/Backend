//! The catalog store: the [`MetaStore`] write surface and the [`Catalog`] read
//! surface (INDEX-PLAN §8.1), plus the single-writer discipline (ID-1) and the
//! outbox fan-out (ID-3).
//!
//! - [`writer::CatalogWriter`] owns the [`VersioningEngine`](crate::engine::VersioningEngine)
//!   on `main`; it is the *only* type that can mutate the catalog. Writes are
//!   staged by [`MetaStore::apply_ops`] (one transaction per batch, outbox rows
//!   included) and made durable by explicit
//!   [`writer::CatalogWriter::commit_batch`] heartbeat commits (ID-4).
//! - [`read`] implements the [`Catalog`] read trait against a plain
//!   [`CatalogEngine`](crate::engine::CatalogEngine), including the bitemporal
//!   [`Catalog::at`] view.

pub mod apply;
pub mod follower;
pub mod lifecycle;
pub mod read;
pub mod writer;

pub use follower::{DrainReport, FollowerError, SinkFollower, drain_once};

use serde::{Deserialize, Serialize};

use heart::query::AsOf;

use crate::codec::CodecError;
use crate::engine::EngineError;
use crate::enums::SinkKind;
use crate::ids::PackageId;
use crate::protocol::CatalogOp;
use crate::tables::outbox::OutboxRow;
use crate::tables::packages::PackageRow;

/// Why a catalog operation failed.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// The underlying engine failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A row could not be decoded into typed fields.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// An `AsOf::Time` read resolved to no commit (before the first commit).
    #[error("no catalog commit exists at or before the requested instant")]
    NoCommitAtInstant,
    /// A targeted write found no row to update (the caller's coordinates are
    /// stale or the row was never upserted).
    #[error("no {what} matched the write")]
    MissingRow {
        /// What was being written.
        what: &'static str,
    },
    /// A watermark advance would move a sink backwards (monotonicity guard).
    #[error("sink watermark for {sink} cannot regress from {current} to {requested}")]
    WatermarkRegression {
        /// The sink whose watermark was guarded.
        sink: SinkKind,
        /// The current, higher watermark.
        current: i64,
        /// The rejected, lower watermark.
        requested: i64,
    },
}

/// The outcome of applying a batch of [`CatalogOp`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// How many ops were applied.
    pub applied: usize,
    /// How many outbox rows were emitted in the same transaction (ID-3).
    pub outbox_rows: usize,
}

/// A generation registration (INDEX-PLAN §8 `generations`, ID-15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRegistration {
    /// The generation stamp.
    pub gen_stamp: crate::ids::GenerationStamp,
    /// The version this generation belongs to.
    pub version_id: PackageId,
    /// The IR channel tip, when sealed.
    pub channel_tip: Option<crate::ids::ChannelTip>,
    /// The producer job key.
    pub job_key: Option<crate::ids::JobKeyHash>,
    /// The producer toolchain reference.
    pub producer_toolchain: Option<String>,
    /// When the generation was sealed.
    pub sealed_at: Option<i64>,
    /// The IR status (always set).
    pub ir_status: crate::enums::IrStatus,
    /// Advisory resolution stats as a JSON string.
    pub resolution_stats: Option<String>,
}

/// A cursor into the catalog change stream (INDEX-PLAN §8.1 `changed_since`).
///
/// The outbox `seq` is a monotonic total order over catalog mutations; a cursor
/// is simply the last `seq` observed, so pagination is stable under interleaved
/// writes (new writes only ever get higher `seq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CatalogCursor {
    /// The last outbox `seq` already consumed; the next page starts strictly
    /// after it. Use `0` (the [`Default`]) to start from the beginning.
    pub after_seq: i64,
}

/// One page of catalog changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedPage {
    /// The outbox rows in this page, in `seq` order.
    pub rows: Vec<OutboxRow>,
    /// The cursor to pass for the next page (the last row's `seq`), or the input
    /// cursor unchanged when the page was empty.
    pub next: CatalogCursor,
}

/// Durable identity and source payload used to reconcile one stem's versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSnapshot {
    /// Stable version identity.
    pub version_id: crate::ids::PackageId,
    /// Canonical published version.
    pub version_canonical: String,
    /// Upstream source revision/checksum when present.
    pub source_rev: Option<String>,
}

/// The write surface of the catalog (INDEX-PLAN §8.1). Implemented by the
/// single [`writer::CatalogWriter`]; there is no other way to mutate the store.
pub trait MetaStore: Catalog {
    /// Apply a batch of ops in **one** transaction, writing the business rows
    /// and their outbox fan-out rows together (ID-3). Staged, not committed —
    /// durability is the caller's explicit batch heartbeat (ID-4).
    fn apply_ops(&self, ops: &[CatalogOp]) -> Result<ApplyReport, MetaError>;

    /// Register (or update) a generation row (ID-15). Emits no outbox rows by
    /// itself; symbol projection fan-out is driven separately.
    fn register_generation(&self, registration: GenerationRegistration) -> Result<(), MetaError>;

    /// Claim up to `limit` unconsumed outbox rows for a sink, in `seq` order,
    /// starting strictly after the sink's recorded watermark. Does not advance
    /// the watermark; the follower calls [`MetaStore::advance_sink_watermark`]
    /// after it has durably applied the projection.
    fn outbox_claim(&self, sink: SinkKind, limit: usize) -> Result<Vec<OutboxRow>, MetaError>;

    /// Advance a sink's watermark to `last_seq`. Rejects a regression
    /// ([`MetaError::WatermarkRegression`]) so a watermark can never move
    /// backwards under a late/duplicate follower.
    fn advance_sink_watermark(
        &self,
        sink: SinkKind,
        last_seq: i64,
        updated_at: i64,
    ) -> Result<(), MetaError>;

    /// Read a sink's current durable watermark `last_seq`, or `0` when the sink
    /// has never advanced. This is the read the follower loop measures its
    /// at-least-once redelivery guard against ([`follower::drain_once`]) and the
    /// server's watermark-lag observability poller reads.
    fn current_sink_watermark(&self, sink: SinkKind) -> Result<i64, MetaError>;

    /// The highest `seq` present in the outbox for `sink` (the drain head), or
    /// `0` when the sink has no rows. The lag a follower is behind is
    /// `outbox_head - current_sink_watermark`.
    fn outbox_head(&self, sink: SinkKind) -> Result<i64, MetaError>;
}

/// The read surface of the catalog (INDEX-PLAN §8.1).
pub trait Catalog: Send + Sync {
    /// Fetch a package stem by its version id's stem, or `None` if absent.
    fn get_package(&self, stem: crate::ids::PackageStemId)
    -> Result<Option<PackageRow>, MetaError>;

    /// Page the catalog change stream after `cursor` (INDEX-PLAN §8.1).
    fn changed_since(&self, cursor: CatalogCursor) -> Result<ChangedPage, MetaError>;

    /// Read the durable version identities and source revisions for a stem.
    fn version_snapshots(
        &self,
        stem: crate::ids::PackageStemId,
    ) -> Result<Vec<VersionSnapshot>, MetaError>;

    /// Resolve a read view as of a point in history (INDEX-PLAN §9). The
    /// returned handle pins a commit; a plain-SQLite fake resolves time against
    /// its honest fake commit log.
    fn at(&self, as_of: &AsOf) -> Result<read::CatalogAsOf, MetaError>;
}
