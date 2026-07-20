//! `advisories` — security advisory records linked to package stems and version
//! ranges (INDEX-PLAN §8, REGISTRYLESS §12 / OSV).
//!
//! ```text
//! advisories
//!   id           BLOB16 PK          -- AdvisoryId
//!   stem_id      BLOB16 NULL        -- PackageStemId (NULL = ecosystem-wide)
//!   version_range TEXT NULL
//!   severity     TEXT NULL
//!   summary      TEXT NULL
//!   url          TEXT NULL
//!   valid_from   INTEGER NOT NULL   -- unix milliseconds
//!   valid_to     INTEGER NULL       -- unix milliseconds, NULL = still active
//!   recorded_at  INTEGER NOT NULL   -- unix milliseconds
//! ```

use crate::codec::{bind_optional_integer, bind_optional_text, CodecError};
use crate::engine::{Row, Value};
use crate::ids::{AdvisoryId, PackageStemId};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "advisories";

/// Column names, in the canonical insert order used by [`AdvisoryRow::bind`].
pub mod columns {
    pub const ID: &str = "id";
    pub const STEM_ID: &str = "stem_id";
    pub const VERSION_RANGE: &str = "version_range";
    pub const SEVERITY: &str = "severity";
    pub const SUMMARY: &str = "summary";
    pub const URL: &str = "url";
    pub const VALID_FROM: &str = "valid_from";
    pub const VALID_TO: &str = "valid_to";
    pub const RECORDED_AT: &str = "recorded_at";
}

/// A fully-typed `advisories` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryRow {
    /// `id` — the advisory's unique identity (BLOB16 UUID).
    pub id: AdvisoryId,
    /// `stem_id` — the affected package stem, or `NULL` for an ecosystem-wide
    /// advisory.
    pub stem_id: Option<PackageStemId>,
    /// `version_range` — semver range expression (e.g. `>=1.0.0,<1.2.3`), or
    /// `NULL` when all versions are affected.
    pub version_range: Option<String>,
    /// `severity` — free-text severity label (e.g. `critical`, `high`), or
    /// `NULL` when not assigned.
    pub severity: Option<String>,
    /// `summary` — one-line description of the advisory, or `NULL`.
    pub summary: Option<String>,
    /// `url` — canonical advisory URL (OSV, NVD, …), or `NULL`.
    pub url: Option<String>,
    /// `valid_from` — start of the bitemporal validity interval (unix
    /// milliseconds).
    pub valid_from: i64,
    /// `valid_to` — end of the validity interval (unix milliseconds), `NULL`
    /// when the advisory is still active.
    pub valid_to: Option<i64>,
    /// `recorded_at` — wall-clock insert time (unix milliseconds).
    pub recorded_at: i64,
}

impl AdvisoryRow {
    /// The ordered column list matching [`AdvisoryRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::ID,
        columns::STEM_ID,
        columns::VERSION_RANGE,
        columns::SEVERITY,
        columns::SUMMARY,
        columns::URL,
        columns::VALID_FROM,
        columns::VALID_TO,
        columns::RECORDED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.id.to_blob().to_vec()),
            match self.stem_id {
                Some(s) => Value::Blob(s.to_blob().to_vec()),
                None => Value::Null,
            },
            bind_optional_text(self.version_range.clone()),
            bind_optional_text(self.severity.clone()),
            bind_optional_text(self.summary.clone()),
            bind_optional_text(self.url.clone()),
            Value::Integer(self.valid_from),
            bind_optional_integer(self.valid_to),
            Value::Integer(self.recorded_at),
        ]
    }

    /// Decode an `advisories` row read back in [`AdvisoryRow::INSERT_COLUMNS`]
    /// order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let id = AdvisoryId::from_blob(&row.get_blob(0)?)?;
        let stem_id = match row.get_optional_blob(1)? {
            Some(b) => Some(PackageStemId::from_blob(&b)?),
            None => None,
        };
        Ok(Self {
            id,
            stem_id,
            version_range: row.get_optional_text(2)?,
            severity: row.get_optional_text(3)?,
            summary: row.get_optional_text(4)?,
            url: row.get_optional_text(5)?,
            valid_from: row.get_integer(6)?,
            valid_to: row.get_optional_integer(7)?,
            recorded_at: row.get_integer(8)?,
        })
    }
}
