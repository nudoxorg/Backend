//! `package_aliases` — name-alias mappings used by the registryless resolver
//! (REGISTRYLESS-PLAN §5).
//!
//! ```text
//! package_aliases
//!   ecosystem   TEXT NOT NULL         -- heart::Language token
//!   alias_kind  TEXT NOT NULL         -- free classifier (e.g. "import", "purl")
//!   alias       TEXT NOT NULL         -- the alias string itself
//!   stem_id     BLOB16 NOT NULL       -- PackageStemId
//!   confidence  TEXT NOT NULL         -- AliasConfidence token
//!   recorded_at INTEGER NOT NULL      -- unix milliseconds
//!   PRIMARY KEY (ecosystem, alias_kind, alias)
//! ```

use heart::Language;

use crate::codec::{bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::AliasConfidence;
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "package_aliases";

/// Column names, in the canonical insert order used by [`PackageAliasRow::bind`].
pub mod columns {
    pub const ECOSYSTEM: &str = "ecosystem";
    pub const ALIAS_KIND: &str = "alias_kind";
    pub const ALIAS: &str = "alias";
    pub const STEM_ID: &str = "stem_id";
    pub const CONFIDENCE: &str = "confidence";
    pub const RECORDED_AT: &str = "recorded_at";
}

/// A fully-typed `package_aliases` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAliasRow {
    /// `ecosystem` — the language/registry world this alias belongs to (stored
    /// as its lowercase token). Part of the composite primary key.
    pub ecosystem: Language,
    /// `alias_kind` — free classifier for the alias namespace (e.g. `"import"`,
    /// `"purl"`, `"crate"`). Part of the composite primary key.
    pub alias_kind: String,
    /// `alias` — the alias string itself (e.g. an import path or alternate name).
    /// Part of the composite primary key.
    pub alias: String,
    /// `stem_id` — the canonical package stem this alias resolves to (BLOB16).
    pub stem_id: PackageStemId,
    /// `confidence` — how trustworthy the mapping is.
    pub confidence: AliasConfidence,
    /// `recorded_at` — wall-clock time this alias was registered (unix
    /// milliseconds).
    pub recorded_at: i64,
}

impl PackageAliasRow {
    /// The ordered column list matching [`PackageAliasRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::ECOSYSTEM,
        columns::ALIAS_KIND,
        columns::ALIAS,
        columns::STEM_ID,
        columns::CONFIDENCE,
        columns::RECORDED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Text(self.ecosystem.as_token().to_owned()),
            Value::Text(self.alias_kind.clone()),
            Value::Text(self.alias.clone()),
            Value::Blob(self.stem_id.to_blob().to_vec()),
            bind_text_enum(self.confidence),
            Value::Integer(self.recorded_at),
        ]
    }

    /// Decode a `package_aliases` row read back in
    /// [`PackageAliasRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let ecosystem_token = row.get_text(0)?;
        let ecosystem = Language::from_token(&ecosystem_token).ok_or_else(|| {
            CodecError::Json(format!("unknown ecosystem token {ecosystem_token:?}"))
        })?;
        let stem_id = PackageStemId::from_blob(&row.get_blob(3)?)?;
        let confidence = read_text_enum::<AliasConfidence>(&row.get_text(4)?)?;
        Ok(Self {
            ecosystem,
            alias_kind: row.get_text(1)?,
            alias: row.get_text(2)?,
            stem_id,
            confidence,
            recorded_at: row.get_integer(5)?,
        })
    }
}
