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
//! ([`MemoryWatermarkStore`]) backs tests and single-process embedded runs;
//! [`FileWatermarkStore`] is the durable file-backed implementation.
//!
//! These types mirror the column shape of `index::tables::feed_watermarks` and
//! `index::tables::git_watermarks` exactly so a future adapter is a field-copy.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ids::PackageStemId;

/// A per-feed crawl cursor — mirrors `feed_watermarks` (REGISTRYLESS-PLAN §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn git_watermark(&self, stem_id: PackageStemId) -> Result<Option<GitWatermark>, Error>;

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

    fn git_watermark(&self, stem_id: PackageStemId) -> Result<Option<GitWatermark>, Error> {
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

/// Durable [`WatermarkStore`] rooted at a directory (`feeds.json` / `gits.json`).
///
/// `put_*` fsyncs so a later [`FileWatermarkStore::open`] on the same directory
/// observes the values (docs/ISSUES.md L48-ic).
#[derive(Debug)]
pub struct FileWatermarkStore {
    dir: PathBuf,
    feeds: Mutex<BTreeMap<String, FeedWatermark>>,
    gits: Mutex<BTreeMap<[u8; 16], GitWatermark>>,
}

impl FileWatermarkStore {
    /// Open (or create) a durable store at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|err| Error::Backend(err.to_string()))?;
        let feeds = load_feeds(&dir.join("feeds.json"))?;
        let gits = load_gits(&dir.join("gits.json"))?;
        Ok(Self {
            dir,
            feeds: Mutex::new(feeds),
            gits: Mutex::new(gits),
        })
    }

    fn persist_feeds(&self, map: &BTreeMap<String, FeedWatermark>) -> Result<(), Error> {
        persist_json(&self.dir.join("feeds.json"), map)
    }

    fn persist_gits(&self, map: &BTreeMap<[u8; 16], GitWatermark>) -> Result<(), Error> {
        let rows: Vec<&GitWatermark> = map.values().collect();
        persist_json(&self.dir.join("gits.json"), &rows)
    }
}

impl WatermarkStore for FileWatermarkStore {
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
        self.persist_feeds(&map)
    }

    fn git_watermark(&self, stem_id: PackageStemId) -> Result<Option<GitWatermark>, Error> {
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
        self.persist_gits(&map)
    }
}

fn load_feeds(path: &Path) -> Result<BTreeMap<String, FeedWatermark>, Error> {
    Ok(load_json(path)?.unwrap_or_default())
}

fn load_gits(path: &Path) -> Result<BTreeMap<[u8; 16], GitWatermark>, Error> {
    let rows: Vec<GitWatermark> = load_json(path)?.unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|watermark| (watermark.stem_id.to_blob(), watermark))
        .collect())
}

fn load_json<T>(path: &Path) -> Result<Option<T>, Error>
where
    T: for<'de> Deserialize<'de>,
{
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| Error::Backend(err.to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Backend(err.to_string())),
    }
}

fn persist_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|err| Error::Backend(err.to_string()))?;
    let tmp = path.with_extension("tmp");
    {
        let mut file =
            std::fs::File::create(&tmp).map_err(|err| Error::Backend(err.to_string()))?;
        file.write_all(&bytes)
            .map_err(|err| Error::Backend(err.to_string()))?;
        file.sync_all()
            .map_err(|err| Error::Backend(err.to_string()))?;
    }
    std::fs::rename(&tmp, path).map_err(|err| Error::Backend(err.to_string()))?;
    if let Some(parent) = path.parent() {
        let dir = std::fs::File::open(parent).map_err(|err| Error::Backend(err.to_string()))?;
        dir.sync_all()
            .map_err(|err| Error::Backend(err.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod l48_durable_watermark_store {
    use super::*;
    use crate::ids::PackageStemId;

    /// docs/ISSUES.md L48-ic: `MemoryWatermarkStore` is the only
    /// `WatermarkStore`. A process restart must still observe the last ETag,
    /// so a file-backed impl has to exist.
    #[test]
    fn file_watermark_store_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("nudox-l48-watermarks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp watermark dir");

        let feed = FeedWatermark {
            feed: "https://example.test/feed".into(),
            last_ref: Some("\"etag-1\"".into()),
            last_checked_at: 1,
            last_error: None,
        };
        let stem = PackageStemId::from_uuid(uuid::Uuid::from_bytes([7u8; 16]));
        let git = GitWatermark {
            stem_id: stem,
            last_rev: Some("abc123".into()),
            last_checked_at: 2,
            last_error: None,
        };

        {
            let store = FileWatermarkStore::open(&dir).expect("open");
            store.put_feed_watermark(&feed).expect("put feed");
            store.put_git_watermark(&git).expect("put git");
        }

        let store = FileWatermarkStore::open(&dir).expect("reopen");
        let got_feed = store
            .feed_watermark("https://example.test/feed")
            .expect("read feed")
            .expect("feed must persist across reopen");
        assert_eq!(got_feed.last_ref.as_deref(), Some("\"etag-1\""));
        let got_git = store
            .git_watermark(stem)
            .expect("read git")
            .expect("git watermark must persist across reopen");
        assert_eq!(got_git.last_rev.as_deref(), Some("abc123"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
