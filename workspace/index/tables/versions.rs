//! `versions` — one row per (stem, version) instance (INDEX-PLAN §8).
//!
//! ```text
//! versions
//!   id BLOB16 PK             -- heart::PackageId
//!   stem_id BLOB16 NOT NULL  -- PackageStemId FK → packages
//!   version_canonical TEXT NOT NULL
//!   version_original  TEXT NOT NULL
//!   published_at      INTEGER          -- unix milliseconds, nullable
//!   toolchain         TEXT             -- JSON payload, nullable
//!   license_spdx      TEXT             -- nullable
//!   yanked_upstream   INTEGER NOT NULL -- boolean 0/1
//!   parse_state       TEXT NOT NULL    -- ParseState token
//!   parse_phase       TEXT             -- nullable free-text phase label
//!   attempts          INTEGER NOT NULL
//!   failure           TEXT             -- JSON payload, nullable
//!   source_kind       TEXT NOT NULL    -- SourceKind token
//!   source_pack       BLOB32           -- ObjectPackHash, nullable
//!   source_rev        TEXT             -- nullable git rev
//!   registry_checksum TEXT             -- nullable
//!   registry_package_uri TEXT          -- nullable
//! ```

use heart::PackageId;

use crate::codec::{
    bind_bool, bind_optional_integer, bind_optional_text, bind_text_enum, read_bool,
    read_text_enum, CodecError,
};
use crate::engine::{Row, Value};
use crate::enums::{ParseState, SourceKind};
use crate::ids::{version_id, ObjectPackHash, PackageStemId};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "versions";

/// Column names, in the canonical insert order used by [`VersionRow::bind`].
pub mod columns {
    pub const ID: &str = "id";
    pub const STEM_ID: &str = "stem_id";
    pub const VERSION_CANONICAL: &str = "version_canonical";
    pub const VERSION_ORIGINAL: &str = "version_original";
    pub const PUBLISHED_AT: &str = "published_at";
    pub const TOOLCHAIN: &str = "toolchain";
    pub const LICENSE_SPDX: &str = "license_spdx";
    pub const YANKED_UPSTREAM: &str = "yanked_upstream";
    pub const PARSE_STATE: &str = "parse_state";
    pub const PARSE_PHASE: &str = "parse_phase";
    pub const ATTEMPTS: &str = "attempts";
    pub const FAILURE: &str = "failure";
    pub const SOURCE_KIND: &str = "source_kind";
    pub const SOURCE_PACK: &str = "source_pack";
    pub const SOURCE_REV: &str = "source_rev";
    pub const REGISTRY_CHECKSUM: &str = "registry_checksum";
    pub const REGISTRY_PACKAGE_URI: &str = "registry_package_uri";
}

/// A fully-typed `versions` row.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionRow {
    /// `id` — the version instance identity (heart::PackageId), stored as BLOB16.
    pub id: PackageId,
    /// `stem_id` — the parent package stem this version belongs to.
    pub stem_id: PackageStemId,
    /// `version_canonical` — the normalized version string used for comparisons.
    pub version_canonical: String,
    /// `version_original` — the version string exactly as published.
    pub version_original: String,
    /// `published_at` — registry publication timestamp (unix milliseconds), if known.
    pub published_at: Option<i64>,
    /// `toolchain` — JSON payload describing the toolchain requirement, if any.
    pub toolchain: Option<String>,
    /// `license_spdx` — SPDX license expression, if known.
    pub license_spdx: Option<String>,
    /// `yanked_upstream` — whether the upstream registry has yanked this version.
    pub yanked_upstream: bool,
    /// `parse_state` — pipeline lifecycle state of this version.
    pub parse_state: ParseState,
    /// `parse_phase` — free-text label for the current parse sub-phase, if set.
    pub parse_phase: Option<String>,
    /// `attempts` — number of parse/acquisition attempts made so far.
    pub attempts: i64,
    /// `failure` — JSON payload describing the last failure, if any.
    pub failure: Option<String>,
    /// `source_kind` — where the source bytes came from.
    pub source_kind: SourceKind,
    /// `source_pack` — ObjectPack root hash, when source is sealed as a pack.
    pub source_pack: Option<ObjectPackHash>,
    /// `source_rev` — git revision used for the source checkout, if applicable.
    pub source_rev: Option<String>,
    /// `registry_checksum` — checksum as published by the registry, if any.
    pub registry_checksum: Option<String>,
    /// `registry_package_uri` — canonical URI of the registry artifact, if any.
    pub registry_package_uri: Option<String>,
}

impl VersionRow {
    /// The ordered column list matching [`VersionRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::ID,
        columns::STEM_ID,
        columns::VERSION_CANONICAL,
        columns::VERSION_ORIGINAL,
        columns::PUBLISHED_AT,
        columns::TOOLCHAIN,
        columns::LICENSE_SPDX,
        columns::YANKED_UPSTREAM,
        columns::PARSE_STATE,
        columns::PARSE_PHASE,
        columns::ATTEMPTS,
        columns::FAILURE,
        columns::SOURCE_KIND,
        columns::SOURCE_PACK,
        columns::SOURCE_REV,
        columns::REGISTRY_CHECKSUM,
        columns::REGISTRY_PACKAGE_URI,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(version_id::to_blob(&self.id).to_vec()),
            Value::Blob(self.stem_id.to_blob().to_vec()),
            Value::Text(self.version_canonical.clone()),
            Value::Text(self.version_original.clone()),
            bind_optional_integer(self.published_at),
            bind_optional_text(self.toolchain.clone()),
            bind_optional_text(self.license_spdx.clone()),
            bind_bool(self.yanked_upstream),
            bind_text_enum(self.parse_state),
            bind_optional_text(self.parse_phase.clone()),
            Value::Integer(self.attempts),
            bind_optional_text(self.failure.clone()),
            bind_text_enum(self.source_kind),
            match self.source_pack {
                Some(h) => Value::Blob(h.to_blob().to_vec()),
                None => Value::Null,
            },
            bind_optional_text(self.source_rev.clone()),
            bind_optional_text(self.registry_checksum.clone()),
            bind_optional_text(self.registry_package_uri.clone()),
        ]
    }

    /// Decode a `versions` row read back in [`VersionRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let id = version_id::from_blob(&row.get_blob(0)?)?;
        let stem_id = PackageStemId::from_blob(&row.get_blob(1)?)?;
        let source_pack = match row.get_optional_blob(13)? {
            Some(b) => Some(ObjectPackHash::from_blob(&b)?),
            None => None,
        };
        Ok(Self {
            id,
            stem_id,
            version_canonical: row.get_text(2)?,
            version_original: row.get_text(3)?,
            published_at: row.get_optional_integer(4)?,
            toolchain: row.get_optional_text(5)?,
            license_spdx: row.get_optional_text(6)?,
            yanked_upstream: read_bool(row.get_integer(7)?),
            parse_state: read_text_enum::<ParseState>(&row.get_text(8)?)?,
            parse_phase: row.get_optional_text(9)?,
            attempts: row.get_integer(10)?,
            failure: row.get_optional_text(11)?,
            source_kind: read_text_enum::<SourceKind>(&row.get_text(12)?)?,
            source_pack,
            source_rev: row.get_optional_text(14)?,
            registry_checksum: row.get_optional_text(15)?,
            registry_package_uri: row.get_optional_text(16)?,
        })
    }
}
