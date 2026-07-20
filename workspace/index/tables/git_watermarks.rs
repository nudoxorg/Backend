//! `git_watermarks` — per-stem git polling high-water marks (INDEX-PLAN §8).
//!
//! ```text
//! git_watermarks
//!   stem_id         BLOB16 PK -- PackageStemId FK → packages
//!   last_rev        TEXT      -- last-seen git rev, nullable
//!   last_checked_at INTEGER NOT NULL -- unix milliseconds
//!   last_error      TEXT      -- last poll error message, nullable
//! ```

use crate::codec::{bind_optional_text, CodecError};
use crate::engine::{Row, Value};
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "git_watermarks";

/// Column names, in the canonical insert order used by [`GitWatermarkRow::bind`].
pub mod columns {
    pub const STEM_ID: &str = "stem_id";
    pub const LAST_REV: &str = "last_rev";
    pub const LAST_CHECKED_AT: &str = "last_checked_at";
    pub const LAST_ERROR: &str = "last_error";
}

/// A fully-typed `git_watermarks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWatermarkRow {
    /// `stem_id` — the package stem this watermark tracks.
    pub stem_id: PackageStemId,
    /// `last_rev` — the git revision last seen for this stem, if any poll has succeeded.
    pub last_rev: Option<String>,
    /// `last_checked_at` — when the last poll attempt was made (unix milliseconds).
    pub last_checked_at: i64,
    /// `last_error` — the error message from the most recent failed poll, if any.
    pub last_error: Option<String>,
}

impl GitWatermarkRow {
    /// The ordered column list matching [`GitWatermarkRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::STEM_ID,
        columns::LAST_REV,
        columns::LAST_CHECKED_AT,
        columns::LAST_ERROR,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.stem_id.to_blob().to_vec()),
            bind_optional_text(self.last_rev.clone()),
            Value::Integer(self.last_checked_at),
            bind_optional_text(self.last_error.clone()),
        ]
    }

    /// Decode a `git_watermarks` row read back in
    /// [`GitWatermarkRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let stem_id = PackageStemId::from_blob(&row.get_blob(0)?)?;
        Ok(Self {
            stem_id,
            last_rev: row.get_optional_text(1)?,
            last_checked_at: row.get_integer(2)?,
            last_error: row.get_optional_text(3)?,
        })
    }
}
