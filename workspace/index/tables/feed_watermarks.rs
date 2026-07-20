//! `feed_watermarks` — per-feed crawl cursor for the registryless discovery
//! pipeline (REGISTRYLESS-PLAN §5).
//!
//! ```text
//! feed_watermarks
//!   feed            TEXT PRIMARY KEY
//!   last_ref        TEXT NULL          -- opaque feed cursor (etag, page token, …)
//!   last_checked_at INTEGER NOT NULL   -- unix milliseconds
//!   last_error      TEXT NULL          -- last crawl error message, or NULL
//! ```

use crate::codec::{bind_optional_text, CodecError};
use crate::engine::{Row, Value};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "feed_watermarks";

/// Column names, in the canonical insert order used by [`FeedWatermarkRow::bind`].
pub mod columns {
    pub const FEED: &str = "feed";
    pub const LAST_REF: &str = "last_ref";
    pub const LAST_CHECKED_AT: &str = "last_checked_at";
    pub const LAST_ERROR: &str = "last_error";
}

/// A fully-typed `feed_watermarks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedWatermarkRow {
    /// `feed` — the feed identifier (URL, registry slug, …); primary key.
    pub feed: String,
    /// `last_ref` — opaque feed cursor from the last successful crawl (e.g. an
    /// ETag, a page token, or a git ref), or `NULL` on first use.
    pub last_ref: Option<String>,
    /// `last_checked_at` — wall-clock time of the most recent crawl attempt
    /// (unix milliseconds).
    pub last_checked_at: i64,
    /// `last_error` — the error message from the last failed crawl attempt, or
    /// `NULL` when the last crawl succeeded.
    pub last_error: Option<String>,
}

impl FeedWatermarkRow {
    /// The ordered column list matching [`FeedWatermarkRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::FEED,
        columns::LAST_REF,
        columns::LAST_CHECKED_AT,
        columns::LAST_ERROR,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Text(self.feed.clone()),
            bind_optional_text(self.last_ref.clone()),
            Value::Integer(self.last_checked_at),
            bind_optional_text(self.last_error.clone()),
        ]
    }

    /// Decode a `feed_watermarks` row read back in
    /// [`FeedWatermarkRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        Ok(Self {
            feed: row.get_text(0)?,
            last_ref: row.get_optional_text(1)?,
            last_checked_at: row.get_integer(2)?,
            last_error: row.get_optional_text(3)?,
        })
    }
}
