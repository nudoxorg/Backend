//! `popularity` — download and dependent counts per (ecosystem, stem) pair
//! (INDEX-PLAN §8).
//!
//! ```text
//! popularity
//!   ecosystem           TEXT NOT NULL    -- heart::Language token
//!   stem_id             BLOB16 NOT NULL  -- PackageStemId FK → packages
//!   downloads           INTEGER          -- nullable raw download count
//!   downloads_pct_ppm   INTEGER          -- nullable parts-per-million percentile
//!   dependents_pct_ppm  INTEGER          -- nullable parts-per-million percentile
//!   computed_at         INTEGER NOT NULL -- unix milliseconds
//!   PRIMARY KEY (ecosystem, stem_id)
//! ```

use heart::Language;

use crate::codec::{bind_optional_integer, CodecError};
use crate::engine::{Row, Value};
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "popularity";

/// Column names, in the canonical insert order used by [`PopularityRow::bind`].
pub mod columns {
    pub const ECOSYSTEM: &str = "ecosystem";
    pub const STEM_ID: &str = "stem_id";
    pub const DOWNLOADS: &str = "downloads";
    pub const DOWNLOADS_PCT_PPM: &str = "downloads_pct_ppm";
    pub const DEPENDENTS_PCT_PPM: &str = "dependents_pct_ppm";
    pub const COMPUTED_AT: &str = "computed_at";
}

/// A fully-typed `popularity` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopularityRow {
    /// `ecosystem` — the language/registry world this popularity measurement applies to.
    pub ecosystem: Language,
    /// `stem_id` — the package stem being measured.
    pub stem_id: PackageStemId,
    /// `downloads` — raw download count, when available from the registry.
    pub downloads: Option<i64>,
    /// `downloads_pct_ppm` — download count percentile rank in parts-per-million
    /// (0 = bottom, 1_000_000 = top), when computed.
    pub downloads_pct_ppm: Option<i64>,
    /// `dependents_pct_ppm` — dependent-count percentile rank in parts-per-million,
    /// when computed.
    pub dependents_pct_ppm: Option<i64>,
    /// `computed_at` — when these popularity figures were last computed (unix milliseconds).
    pub computed_at: i64,
}

impl PopularityRow {
    /// The ordered column list matching [`PopularityRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::ECOSYSTEM,
        columns::STEM_ID,
        columns::DOWNLOADS,
        columns::DOWNLOADS_PCT_PPM,
        columns::DEPENDENTS_PCT_PPM,
        columns::COMPUTED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Text(self.ecosystem.as_token().to_owned()),
            Value::Blob(self.stem_id.to_blob().to_vec()),
            bind_optional_integer(self.downloads),
            bind_optional_integer(self.downloads_pct_ppm),
            bind_optional_integer(self.dependents_pct_ppm),
            Value::Integer(self.computed_at),
        ]
    }

    /// Decode a `popularity` row read back in [`PopularityRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let ecosystem_token = row.get_text(0)?;
        let ecosystem = Language::from_token(&ecosystem_token).ok_or_else(|| {
            CodecError::Json(format!("unknown ecosystem token {ecosystem_token:?}"))
        })?;
        let stem_id = PackageStemId::from_blob(&row.get_blob(1)?)?;
        Ok(Self {
            ecosystem,
            stem_id,
            downloads: row.get_optional_integer(2)?,
            downloads_pct_ppm: row.get_optional_integer(3)?,
            dependents_pct_ppm: row.get_optional_integer(4)?,
            computed_at: row.get_integer(5)?,
        })
    }
}
