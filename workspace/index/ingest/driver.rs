//! [`FollowerDriver`]: batches a source's [`CatalogOp`]s through the single
//! catalog writer and commits once per batch (INDEX-PLAN ID-4), advancing the
//! watermark **only** after the commit succeeds.
//!
//! # The atomicity contract (adversarial requirement)
//!
//! A batch is all-or-nothing:
//!
//! 1. `apply_ops` runs the whole `ops` slice in **one** engine transaction
//!    (INDEX-PLAN ID-3): if any op fails, every staged row rolls back and the
//!    call returns `Err`.
//! 2. On that error the driver returns without calling `commit_batch` and
//!    **without advancing the watermark**, so the catalog is untouched and the
//!    feed re-delivers the batch on the next poll (ops are idempotent upserts).
//! 3. Only after a successful `apply_ops` **and** a successful `commit_batch`
//!    does the driver persist `next_watermark`. A crash between commit and
//!    watermark-persist re-delivers one batch — safe, because the ops upsert.
//!
//! The driver is generic over the versioning engine `E` rather than naming
//! [`crate::engine::Configured`], so it is written against the *contract* and
//! not against whichever engine a build selected. Every build and every test
//! instantiates it with the real DoltLite writer.

use crate::engine::VersioningEngine;
use crate::store::MetaStore;
use crate::store::writer::CatalogWriter;

use crate::ingest::follower::{Follower, FollowerError};
use crate::ingest::watermark::{WatermarkError, WatermarkStore};

/// What one drive step did — for logging and test assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOutcome {
    /// The batch committed; `applied` ops and a watermark advance were persisted.
    Committed {
        /// How many ops were applied in the batch.
        applied: usize,
        /// Whether the feed reported itself caught up.
        caught_up: bool,
    },
    /// The feed answered `304`/no-change: no catalog write, watermark clock
    /// advanced (crawl time only).
    NoChange,
}

/// Why a drive step failed.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    /// The follower's poll failed (transport / parse / mapping).
    #[error(transparent)]
    Follower(#[from] FollowerError),
    /// Applying or committing the op batch failed. The watermark was **not**
    /// advanced; the batch re-delivers next poll.
    #[error("catalog batch for feed {feed} failed to commit: {message}")]
    Commit {
        /// The feed whose batch failed.
        feed: String,
        /// The underlying store/engine error message.
        message: String,
    },
    /// Reading or persisting the watermark failed.
    #[error(transparent)]
    Watermark(#[from] WatermarkError),
}

/// Drives followers against a catalog writer with per-batch commits (ID-4).
pub struct FollowerDriver<'writer, Engine, Watermarks>
where
    Engine: VersioningEngine + Send + Sync,
    Watermarks: WatermarkStore,
{
    writer: &'writer CatalogWriter<Engine>,
    watermarks: &'writer Watermarks,
}

impl<'writer, Engine, Watermarks> FollowerDriver<'writer, Engine, Watermarks>
where
    Engine: VersioningEngine + Send + Sync,
    Watermarks: WatermarkStore,
{
    /// Wrap a writer handle and a watermark store.
    pub fn new(
        writer: &'writer CatalogWriter<Engine>,
        watermarks: &'writer Watermarks,
    ) -> Self {
        Self { writer, watermarks }
    }

    /// Drive one poll of `follower`: read its watermark, poll, apply the batch
    /// atomically, commit, then persist the watermark. See the module contract.
    pub fn drive_once(
        &self,
        follower: &dyn Follower,
        now_unix_ms: i64,
    ) -> Result<DriveOutcome, DriveError> {
        let feed = follower.feed_id().to_owned();
        let previous = self.watermarks.feed_watermark(&feed)?;

        let batch = follower.poll(previous.as_ref(), now_unix_ms)?;

        if batch.ops.is_empty() {
            // No-change / caught-up-with-nothing-new: advance only the crawl
            // clock. No catalog transaction, so nothing to commit.
            self.watermarks.put_feed_watermark(&batch.next_watermark)?;
            return Ok(DriveOutcome::NoChange);
        }

        // ── Atomic catalog batch (ID-3/ID-4) ─────────────────────────────────
        // apply_ops stages the whole batch in one transaction; a single bad op
        // rolls it all back and we bail *before* committing or advancing the
        // watermark.
        let report = self
            .writer
            .apply_ops(&batch.ops)
            .map_err(|error| DriveError::Commit { feed: feed.clone(), message: error.to_string() })?;

        self.writer
            .commit_batch(&format!("ingestor: {feed} batch ({} ops)", report.applied))
            .map_err(|error| DriveError::Commit { feed: feed.clone(), message: error.to_string() })?;

        // Only now that the batch is durable do we advance the watermark.
        self.watermarks.put_feed_watermark(&batch.next_watermark)?;

        Ok(DriveOutcome::Committed {
            applied: report.applied,
            caught_up: batch.caught_up,
        })
    }
}
