//! The [`Follower`] trait: one upstream feed's incremental poll, cadence-gated
//! by its [`FeedWatermark`], emitting a batch of [`CatalogOp`]s
//! (INDEX-PLAN ID-4/ID-14; REGISTRYLESS-PLAN §7).
//!
//! This mirrors the intent of `registry/upstream::CatalogFollower` (read that
//! trait first — it defines the batch/cursor/exhausted rhythm) but diverges
//! deliberately in three ways, documented here so the divergence is a choice,
//! not drift:
//!
//! 1. **Emits `CatalogOp`, not `CatalogEvent`.** The legacy follower yields a
//!    registry-neutral `Published`/`Withdrawn` event that a *separate* driver
//!    translates into idempotent registration calls. The registryless plane's
//!    writer vocabulary **is** [`CatalogOp`] (INDEX-PLAN §8.1), so a follower
//!    here produces ops directly — there is no second translation stage, and
//!    the atomic-batch guarantee (ID-4) lives in the op layer, not above it.
//! 2. **Synchronous.** The legacy trait hand-rolls a boxed future to stay
//!    object-safe without `async_trait`. This crate keeps followers synchronous
//!    over a [`FeedTransport`](crate::transport::FeedTransport) so the pure
//!    poll logic is unit-testable with fixtures and the crate needs no async
//!    runtime; the binary picks the runtime and drives the blocking calls.
//! 3. **Watermark is the cursor.** The legacy `CatalogCursor` is an opaque JSON
//!    blob persisted next to tantivy files. Here the cursor is the typed
//!    [`FeedWatermark`] row (ETag / last ref), read and written through the
//!    catalog's watermark table via [`WatermarkStore`](crate::watermark::WatermarkStore).

use index::protocol::CatalogOp;

use crate::transport::TransportError;
use crate::watermark::FeedWatermark;

/// How often a follower wants to be polled — advisory to the driver's scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollCadence {
    /// Poll roughly every `seconds` when idle. The driver may poll sooner on
    /// demand (e.g. an add-by-URL request); this is the steady-state floor.
    EverySeconds(u64),
}

/// The outcome of one [`Follower::poll`].
#[derive(Debug)]
pub struct FollowerBatch {
    /// The ops to apply atomically (INDEX-PLAN ID-4). Empty is valid — a feed
    /// with nothing new emits no ops but may still advance its watermark
    /// (e.g. a fresh crawl time).
    pub ops: Vec<CatalogOp>,
    /// The watermark to persist **iff** the whole `ops` batch commits. On a
    /// `304 Not Modified` short-circuit this carries the unchanged `last_ref`
    /// with a bumped `last_checked_at` so the crawl clock advances without any
    /// catalog write.
    pub next_watermark: FeedWatermark,
    /// `true` when the feed is fully caught up; the driver may then back off to
    /// the follower's [`PollCadence`] instead of polling again immediately.
    pub caught_up: bool,
}

/// Why a follower poll failed.
#[derive(Debug, thiserror::Error)]
pub enum FollowerError {
    /// The feed transport (HTTP) failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The feed body could not be parsed into the expected shape.
    #[error("feed {feed} returned an unparseable body: {detail}")]
    Parse {
        /// The feed identifier.
        feed: String,
        /// What could not be parsed.
        detail: String,
    },
    /// A package name / repo URL in the feed could not be resolved to a stem.
    /// Non-fatal for the batch as a whole (the follower may skip the entry), but
    /// surfaced when the follower chooses to treat it as fatal.
    #[error("feed {feed} entry {entry} could not be mapped to a stem: {detail}")]
    Unmappable {
        /// The feed identifier.
        feed: String,
        /// The offending entry (formula/package name).
        entry: String,
        /// Why mapping failed.
        detail: String,
    },
}

/// One upstream feed's incremental follower.
///
/// A follower is **pure over its transport**: given the same watermark and the
/// same fetched bytes it produces the same batch, so fixture tests fully cover
/// it. It never touches the catalog or the watermark store directly — the
/// [`FollowerDriver`](crate::driver::FollowerDriver) owns that IO and the
/// atomic-commit discipline.
pub trait Follower: Send + Sync {
    /// A stable feed identifier — the `feed_watermarks` primary key
    /// (e.g. `"homebrew"`, `"osv-cpp"`). Never parsed; used for the watermark
    /// row and for logging.
    fn feed_id(&self) -> &str;

    /// The steady-state poll cadence (REGISTRYLESS-PLAN §15 rate limiting).
    fn cadence(&self) -> PollCadence;

    /// Poll the feed given the last persisted watermark (`None` on first crawl),
    /// producing the next batch. The follower fetches through the transport it
    /// was constructed with; the driver applies the returned ops atomically and
    /// only then persists `next_watermark`.
    fn poll(
        &self,
        previous: Option<&FeedWatermark>,
        now_unix_ms: i64,
    ) -> Result<FollowerBatch, FollowerError>;
}
