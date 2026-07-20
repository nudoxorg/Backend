//! `packages` — the stem (name-level) identity of a package (INDEX-PLAN §8).
//!
//! ```text
//! packages
//!   stem_id BLOB16 PK
//!   ecosystem TEXT NOT NULL
//!   name_struct TEXT NOT NULL       -- purl / StructuredName canonical wire
//!   name_canonical TEXT NOT NULL
//!   name_original TEXT NOT NULL
//!   repo_url TEXT
//!   created_at INTEGER NOT NULL
//!   UNIQUE(ecosystem, name_canonical)
//! ```
//!
//! This module is the reference shape every other table module follows.

use heart::Language;

use crate::codec::CodecError;
use crate::engine::{Row, Value};
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "packages";

/// Column names, in the canonical insert order used by [`PackageRow::bind`].
pub mod columns {
    pub const STEM_ID: &str = "stem_id";
    pub const ECOSYSTEM: &str = "ecosystem";
    pub const NAME_STRUCT: &str = "name_struct";
    pub const NAME_CANONICAL: &str = "name_canonical";
    pub const NAME_ORIGINAL: &str = "name_original";
    pub const REPO_URL: &str = "repo_url";
    pub const CREATED_AT: &str = "created_at";
}

/// A fully-typed `packages` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRow {
    /// `stem_id` — the deterministic stem identity.
    pub stem_id: PackageStemId,
    /// `ecosystem` — the language/registry world (stored as its lowercase token).
    pub ecosystem: Language,
    /// `name_struct` — the structured-name canonical wire (purl-like).
    pub name_struct: String,
    /// `name_canonical` — the normalized name used for uniqueness + lookup.
    pub name_canonical: String,
    /// `name_original` — the name exactly as published.
    pub name_original: String,
    /// `repo_url` — upstream repository, when known.
    pub repo_url: Option<String>,
    /// `created_at` — first-seen instant (unix milliseconds).
    pub created_at: i64,
}

impl PackageRow {
    /// The ordered column list matching [`PackageRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::STEM_ID,
        columns::ECOSYSTEM,
        columns::NAME_STRUCT,
        columns::NAME_CANONICAL,
        columns::NAME_ORIGINAL,
        columns::REPO_URL,
        columns::CREATED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.stem_id.to_blob().to_vec()),
            Value::Text(self.ecosystem.as_token().to_owned()),
            Value::Text(self.name_struct.clone()),
            Value::Text(self.name_canonical.clone()),
            Value::Text(self.name_original.clone()),
            match &self.repo_url {
                Some(url) => Value::Text(url.clone()),
                None => Value::Null,
            },
            Value::Integer(self.created_at),
        ]
    }

    /// Decode a `packages` row read back in [`PackageRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let stem_id = PackageStemId::from_blob(&row.get_blob(0)?)?;
        let ecosystem_token = row.get_text(1)?;
        let ecosystem = Language::from_token(&ecosystem_token).ok_or_else(|| {
            CodecError::Json(format!("unknown ecosystem token {ecosystem_token:?}"))
        })?;
        Ok(Self {
            stem_id,
            ecosystem,
            name_struct: row.get_text(2)?,
            name_canonical: row.get_text(3)?,
            name_original: row.get_text(4)?,
            repo_url: row.get_optional_text(5)?,
            created_at: row.get_integer(6)?,
        })
    }
}
