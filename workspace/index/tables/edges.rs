//! `edges` — dependency graph edges between package versions (INDEX-PLAN §8,
//! REGISTRYLESS RL-5).
//!
//! ```text
//! edges
//!   dependent_version BLOB16 NOT NULL  -- heart::PackageId FK → versions
//!   dep_ecosystem     TEXT NOT NULL    -- heart::Language token
//!   dep_name_canonical TEXT NOT NULL
//!   requirement       TEXT NOT NULL    -- version requirement string
//!   resolved_stem     BLOB16           -- PackageStemId FK → packages, nullable
//!   kind              TEXT NOT NULL    -- EdgeKind token
//!   source            TEXT NOT NULL    -- EdgeSource token
//!   PRIMARY KEY (dependent_version, dep_ecosystem, dep_name_canonical, kind)
//! ```

use heart::{Language, PackageId};

use crate::codec::{bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::{EdgeKind, EdgeSource};
use crate::ids::{version_id, PackageStemId};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "edges";

/// Column names, in the canonical insert order used by [`EdgeRow::bind`].
pub mod columns {
    pub const DEPENDENT_VERSION: &str = "dependent_version";
    pub const DEP_ECOSYSTEM: &str = "dep_ecosystem";
    pub const DEP_NAME_CANONICAL: &str = "dep_name_canonical";
    pub const REQUIREMENT: &str = "requirement";
    pub const RESOLVED_STEM: &str = "resolved_stem";
    pub const KIND: &str = "kind";
    pub const SOURCE: &str = "source";
}

/// A fully-typed `edges` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRow {
    /// `dependent_version` — the version that declares this dependency.
    pub dependent_version: PackageId,
    /// `dep_ecosystem` — the language/registry world the dependency lives in.
    pub dep_ecosystem: Language,
    /// `dep_name_canonical` — the normalized name of the depended-on package.
    pub dep_name_canonical: String,
    /// `requirement` — the version requirement string as declared in the manifest.
    pub requirement: String,
    /// `resolved_stem` — the resolved package stem, when known.
    pub resolved_stem: Option<PackageStemId>,
    /// `kind` — the dependency mechanism (runtime, build, CMake, git submodule, …).
    pub kind: EdgeKind,
    /// `source` — provenance of this edge fact (feed, manifest, git, detected).
    pub source: EdgeSource,
}

impl EdgeRow {
    /// The ordered column list matching [`EdgeRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::DEPENDENT_VERSION,
        columns::DEP_ECOSYSTEM,
        columns::DEP_NAME_CANONICAL,
        columns::REQUIREMENT,
        columns::RESOLVED_STEM,
        columns::KIND,
        columns::SOURCE,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(version_id::to_blob(&self.dependent_version).to_vec()),
            Value::Text(self.dep_ecosystem.as_token().to_owned()),
            Value::Text(self.dep_name_canonical.clone()),
            Value::Text(self.requirement.clone()),
            match self.resolved_stem {
                Some(id) => Value::Blob(id.to_blob().to_vec()),
                None => Value::Null,
            },
            bind_text_enum(self.kind),
            bind_text_enum(self.source),
        ]
    }

    /// Decode an `edges` row read back in [`EdgeRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let dependent_version = version_id::from_blob(&row.get_blob(0)?)?;
        let ecosystem_token = row.get_text(1)?;
        let dep_ecosystem = Language::from_token(&ecosystem_token).ok_or_else(|| {
            crate::codec::CodecError::Json(format!(
                "unknown ecosystem token {ecosystem_token:?}"
            ))
        })?;
        let resolved_stem = match row.get_optional_blob(4)? {
            Some(b) => Some(PackageStemId::from_blob(&b)?),
            None => None,
        };
        Ok(Self {
            dependent_version,
            dep_ecosystem,
            dep_name_canonical: row.get_text(2)?,
            requirement: row.get_text(3)?,
            resolved_stem,
            kind: read_text_enum::<EdgeKind>(&row.get_text(5)?)?,
            source: read_text_enum::<EdgeSource>(&row.get_text(6)?)?,
        })
    }
}
