//! The outbox follower loop (INDEX-PLAN §11 loop 3): drain a sink's outbox rows,
//! project each into its derived store, and advance the sink watermark.
//!
//! # At-least-once delivery
//!
//! The catalog outbox rows are **plain INSERTs** written in the same transaction
//! as the business mutation ([`crate::store::MetaStore::apply_ops`], ID-3). A
//! follower drains them in three steps per row:
//!
//! 1. [`MetaStore::outbox_claim`](crate::store::MetaStore::outbox_claim) — read a
//!    batch strictly after the sink's durable watermark;
//! 2. [`SinkFollower::project`] — apply the row to the derived store;
//! 3. [`MetaStore::advance_sink_watermark`](crate::store::MetaStore::advance_sink_watermark)
//!    — move the durable watermark forward, but only after (2) succeeded.
//!
//! Because the watermark advances **after** projection, a crash between the
//! projection and the watermark advance leaves the row un-acked: the next drain
//! re-claims it and re-projects. Delivery is therefore **at-least-once**, never
//! at-most-once. There is no advisory lock and no exactly-once guarantee — the
//! former Postgres design leaned on `pg_advisory_lock` for single-drainer
//! exclusion and could still redeliver on failover; the catalog design drops the
//! lock (a single writer on `main` already serializes writes; the follower is a
//! single logical drainer per sink) and instead makes redelivery *correct* by
//! two complementary means:
//!
//! - **Idempotent projections.** Every [`SinkFollower::project`] implementation
//!   must be idempotent (upsert-by-id / replace-by-key / delete-missing-is-ok),
//!   so re-projecting a row already applied is a no-op. This is the primary
//!   defense and the contract [`SinkFollower`] documents.
//! - **Seq-skip fast path.** [`drain_once`] additionally skips any claimed row
//!   whose `seq` is `<=` the watermark it read at the start of the tick. Under
//!   normal operation `outbox_claim` already returns only rows above the
//!   watermark, so this never fires; it exists as an explicit, testable guard for
//!   the redelivery window and to make the at-least-once semantics legible at the
//!   call site (a redelivered row is *observably* skipped rather than silently
//!   re-run).
//!
//! The net contract: **a row is projected at least once, and a projection may run
//! more than once; correctness rests on projection idempotence, and the seq-skip
//! guard elides the common redelivery.**

use crate::enums::SinkKind;
use crate::store::{MetaError, MetaStore};
use crate::tables::outbox::OutboxRow;

/// Projects one drained [`OutboxRow`] into a derived store (tantivy text index,
/// vector plane, usage index, …).
///
/// # Idempotence contract
///
/// [`Self::project`] **must** be idempotent: the follower delivers at-least-once
/// (see the module docs), so the same row may be projected more than once across
/// a crash/redelivery window. Implementations satisfy this by keying every write
/// on a stable identity (upsert-by-id, replace-by-key) and treating a delete of
/// an already-absent row as success.
///
/// The follower is engine-agnostic and async-agnostic here: `project` is
/// synchronous because the catalog engine is synchronous. A follower whose
/// derived store is async wraps its await in a small runtime shim at the call
/// site (the server's poller does exactly this), keeping this trait — and the
/// whole `index` crate — free of an async runtime dependency.
pub trait SinkFollower {
    /// The sink this follower drains. The drain loop reads and advances the
    /// watermark for exactly this sink.
    fn sink(&self) -> SinkKind;

    /// Project one outbox row into the derived store. Must be idempotent.
    ///
    /// Errors abort the current drain tick **before** the watermark is advanced
    /// past this row, so the row is redelivered on the next tick (at-least-once).
    fn project(&mut self, row: &OutboxRow) -> Result<(), Error>;
}

/// Why a follower drain tick failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The catalog store (claim / watermark) failed.
    #[error(transparent)]
    Catalog(#[from] MetaError),

    /// The derived-store projection failed. Carries a boxed source so the trait
    /// stays free of every sink's concrete error type; the drain loop treats it
    /// as retryable (the row is redelivered next tick).
    #[error("outbox projection into the derived store failed")]
    Projection(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// Wrap any sink-specific projection error as a [`Error::Projection`].
    pub fn projection<E>(error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Error::Projection(Box::new(error))
    }
}

/// Compatibility alias for callers that named the drain error by its old
/// `FollowerError` spelling.
pub use self::Error as FollowerError;

/// The outcome of one [`drain_once`] tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrainReport {
    /// Rows claimed from the outbox this tick.
    pub claimed: usize,
    /// Rows actually projected (claimed minus [`Self::skipped`]).
    pub projected: usize,
    /// Rows skipped as already-consumed redeliveries (`seq <= watermark`). Under
    /// normal operation this is `0`; a non-zero value is the observable signature
    /// of a post-crash redelivery being elided by the seq-skip guard.
    pub skipped: usize,
    /// The watermark after this tick (unchanged when nothing new projected).
    pub watermark: i64,
}

/// Drain up to `batch` outbox rows for the follower's sink, projecting each and
/// advancing the durable watermark **after** every successful projection.
///
/// # Ordering and the watermark
///
/// Rows are claimed and projected in strictly increasing `seq` order. The
/// watermark is advanced **once, at the end**, to the highest `seq` projected
/// this tick — not per-row — so a mid-batch projection failure leaves the
/// watermark at its pre-tick value and the whole remaining suffix (including the
/// failed row) is redelivered next tick. Because projections are idempotent, the
/// already-projected prefix re-running on redelivery is harmless; advancing
/// per-row instead would be a valid alternative but would complicate the failure
/// story with no correctness gain under idempotence.
///
/// # Seq-skip guard (at-least-once made legible)
///
/// Before projecting, each claimed row is checked against `watermark_before`: a
/// row with `seq <= watermark_before` is a redelivery of something already
/// consumed and is counted in [`DrainReport::skipped`] instead of re-projected.
/// `outbox_claim` already filters to `seq > watermark`, so this guard is
/// belt-and-suspenders — but it is the single place the redelivery contract is
/// enforced and tested, so a redelivered row is provably not re-run.
pub fn drain_once<S, F>(
    store: &S,
    follower: &mut F,
    batch: usize,
) -> Result<DrainReport, Error>
where
    S: MetaStore,
    F: SinkFollower,
{
    let sink = follower.sink();
    let watermark_before = store.current_sink_watermark(sink)?;

    let rows = store.outbox_claim(sink, batch)?;
    let mut report = DrainReport {
        claimed: rows.len(),
        watermark: watermark_before,
        ..DrainReport::default()
    };

    let mut highest_projected: Option<i64> = None;
    for row in &rows {
        // At-least-once seq-skip: a redelivered row already below the watermark
        // is never re-projected. (Normally unreachable because `outbox_claim`
        // filters `seq > watermark`; kept as the explicit redelivery guard.)
        if row.seq <= watermark_before {
            report.skipped += 1;
            continue;
        }
        follower.project(row)?;
        report.projected += 1;
        highest_projected = Some(row.seq);
    }

    // Advance the durable watermark to the highest seq we actually projected.
    // A tick that projected nothing leaves the watermark untouched.
    if let Some(seq) = highest_projected {
        let updated_at = now_unix_millis();
        store.advance_sink_watermark(sink, seq, updated_at)?;
        report.watermark = seq;
    }

    Ok(report)
}

/// Wall-clock milliseconds since the Unix epoch, for the watermark's
/// `updated_at`. Falls back to `0` on a pre-epoch clock rather than panicking.
fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
