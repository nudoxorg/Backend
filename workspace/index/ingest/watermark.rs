//! Watermark persistence: the seam between the ingestor's poll cadence and the
//! catalog's `feed_watermarks` / `git_watermarks` tables (INDEX-PLAN §8;
//! REGISTRYLESS-PLAN §5).
//!
//! # Why a trait here (and not `MetaStore` calls)
//!
//! The index crate *stores* both watermark tables, but its public `MetaStore` /
//! `Catalog` surface exposes **no read path** for either and **no write path**
//! for `feed_watermarks` (git watermarks are written as a side effect of the
//! `SourceMoved` / `Refresh` ops, but never read back). A follower cannot poll
//! incrementally without reading its last ETag, and the git monitor cannot diff
//! without reading `last_rev`. Rather than editing the index crate (out of this
//! workstream's scope), the driver depends on this small trait; a deployment
//! wires it to whichever store implements it. An in-memory implementation
//! ([`MemoryWatermarkStore`]) backs tests and single-process embedded runs.
//!
//! These types mirror the column shape of `index::tables::feed_watermarks` and
//! `index::tables::git_watermarks` exactly so a future adapter is a field-copy.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::ids::PackageStemId;

/// A per-feed crawl cursor — mirrors `feed_watermarks` (REGISTRYLESS-PLAN §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedWatermark {
    /// The feed identifier (a URL, a registry slug); the primary key.
    pub feed: String,
    /// The opaque cursor from the last successful crawl — an ETag for HTTP
    /// feeds, a page token, or a git ref. `None` on first use.
    pub last_ref: Option<String>,
    /// Wall-clock time of the most recent crawl attempt (unix milliseconds).
    pub last_checked_at: i64,
    /// The error message from the last failed crawl, or `None` on success.
    pub last_error: Option<String>,
}

/// A per-stem git high-water mark — mirrors `git_watermarks` (INDEX-PLAN §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWatermark {
    /// The stem this watermark tracks.
    pub stem_id: PackageStemId,
    /// The git revision last seen for this stem, if any poll succeeded.
    pub last_rev: Option<String>,
    /// When the last poll attempt was made (unix milliseconds).
    pub last_checked_at: i64,
    /// The error message from the most recent failed poll, if any.
    pub last_error: Option<String>,
}

/// Why a watermark read or write failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The backing store failed (IO, decode, engine).
    #[error("watermark store backend failed: {0}")]
    Backend(String),
}

/// Backwards-compatible alias: the watermark error (now [`Error`]).
pub use self::Error as WatermarkError;

/// The persistence seam for feed and git watermarks.
///
/// Implementations must be crash-consistent with the catalog write they gate:
/// the driver advances a watermark **only after** the corresponding op batch
/// has been committed, so a watermark can never claim progress the catalog does
/// not have. Re-reading a just-written watermark must observe it (read-after-
/// write within one process).
pub trait WatermarkStore: Send + Sync {
    /// Read a feed's watermark, or `None` if the feed has never been crawled.
    fn feed_watermark(&self, feed: &str) -> Result<Option<FeedWatermark>, Error>;

    /// Persist a feed's watermark (upsert on `feed`).
    fn put_feed_watermark(&self, watermark: &FeedWatermark) -> Result<(), Error>;

    /// Read a stem's git watermark, or `None` if never polled.
    fn git_watermark(
        &self,
        stem_id: PackageStemId,
    ) -> Result<Option<GitWatermark>, Error>;

    /// Persist a stem's git watermark (upsert on `stem_id`).
    fn put_git_watermark(&self, watermark: &GitWatermark) -> Result<(), Error>;
}

/// An in-memory [`WatermarkStore`] — tests and single-process embedded runs.
///
/// Never a durable product mode: state is lost on drop. The real deployment
/// wires the catalog's watermark tables behind [`WatermarkStore`] once the
/// index crate grows the read/write path (see report).
#[derive(Debug, Default)]
pub struct MemoryWatermarkStore {
    feeds: Mutex<BTreeMap<String, FeedWatermark>>,
    gits: Mutex<BTreeMap<[u8; 16], GitWatermark>>,
}

impl MemoryWatermarkStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl WatermarkStore for MemoryWatermarkStore {
    fn feed_watermark(&self, feed: &str) -> Result<Option<FeedWatermark>, Error> {
        let map = self
            .feeds
            .lock()
            .map_err(|_| Error::Backend("feed lock poisoned".to_owned()))?;
        Ok(map.get(feed).cloned())
    }

    fn put_feed_watermark(&self, watermark: &FeedWatermark) -> Result<(), Error> {
        let mut map = self
            .feeds
            .lock()
            .map_err(|_| Error::Backend("feed lock poisoned".to_owned()))?;
        map.insert(watermark.feed.clone(), watermark.clone());
        Ok(())
    }

    fn git_watermark(
        &self,
        stem_id: PackageStemId,
    ) -> Result<Option<GitWatermark>, Error> {
        let map = self
            .gits
            .lock()
            .map_err(|_| Error::Backend("git lock poisoned".to_owned()))?;
        Ok(map.get(&stem_id.to_blob()).cloned())
    }

    fn put_git_watermark(&self, watermark: &GitWatermark) -> Result<(), Error> {
        let mut map = self
            .gits
            .lock()
            .map_err(|_| Error::Backend("git lock poisoned".to_owned()))?;
        map.insert(watermark.stem_id.to_blob(), watermark.clone());
        Ok(())
    }
}
