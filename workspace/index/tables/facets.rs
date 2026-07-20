//! `facets` — per-version quality metadata and keyword tags (INDEX-PLAN §8).
//!
//! ```text
//! facets
//!   version_id BLOB16 PK    -- heart PackageId
//!   keywords   TEXT NULL    -- whitespace-separated tag list, or NULL
//!   quality_ppm INTEGER NULL -- quality score in parts-per-million [0, 1_000_000]
//!   extras     TEXT NULL    -- free-form JSON extra metadata
//! ```

use heart::PackageId;

use crate::codec::{bind_optional_integer, bind_optional_text, CodecError};
use crate::engine::{Row, Value};
use crate::ids::version_id;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "facets";

/// Column names, in the canonical insert order used by [`FacetRow::bind`].
pub mod columns {
    pub const VERSION_ID: &str = "version_id";
    pub const KEYWORDS: &str = "keywords";
    pub const QUALITY_PPM: &str = "quality_ppm";
    pub const EXTRAS: &str = "extras";
}

/// A fully-typed `facets` row.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetRow {
    /// `version_id` — the version this facet record belongs to (heart PackageId,
    /// stored as BLOB16).
    pub version_id: PackageId,
    /// `keywords` — whitespace-separated tag list, or `NULL` when not available.
    pub keywords: Option<String>,
    /// `quality_ppm` — quality score in parts-per-million [0, 1 000 000], or
    /// `NULL` when not yet computed.
    pub quality_ppm: Option<i64>,
    /// `extras` — free-form JSON extra metadata, or `NULL` when absent.
    pub extras: Option<String>,
}

impl FacetRow {
    /// The ordered column list matching [`FacetRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::VERSION_ID,
        columns::KEYWORDS,
        columns::QUALITY_PPM,
        columns::EXTRAS,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(version_id::to_blob(&self.version_id).to_vec()),
            bind_optional_text(self.keywords.clone()),
            bind_optional_integer(self.quality_ppm),
            bind_optional_text(self.extras.clone()),
        ]
    }

    /// Decode a `facets` row read back in [`FacetRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let version_id = version_id::from_blob(&row.get_blob(0)?)?;
        Ok(Self {
            version_id,
            keywords: row.get_optional_text(1)?,
            quality_ppm: row.get_optional_integer(2)?,
            extras: row.get_optional_text(3)?,
        })
    }
}
