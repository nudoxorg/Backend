//! `edgepack_artifacts` — built edgepack bundles keyed by their recipe digest
//! (INDEX-PLAN §8).
//!
//! ```text
//! edgepack_artifacts
//!   edgepack_key_digest BLOB PRIMARY KEY    -- EdgepackKeyDigest (BLOB32)
//!   version_id          BLOB16 NOT NULL     -- heart PackageId
//!   recipe_fingerprint  TEXT NOT NULL
//!   artifact_id         BLOB NULL           -- opaque artifact ref (variable width)
//!   ram_estimate        INTEGER NULL        -- bytes
//!   published_at        INTEGER NULL        -- unix milliseconds
//! ```

use heart::PackageId;

use crate::codec::{bind_optional_integer, CodecError};
use crate::engine::{Row, Value};
use crate::ids::{EdgepackKeyDigest, version_id};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "edgepack_artifacts";

/// Column names, in the canonical insert order used by
/// [`EdgepackArtifactRow::bind`].
pub mod columns {
    pub const EDGEPACK_KEY_DIGEST: &str = "edgepack_key_digest";
    pub const VERSION_ID: &str = "version_id";
    pub const RECIPE_FINGERPRINT: &str = "recipe_fingerprint";
    pub const ARTIFACT_ID: &str = "artifact_id";
    pub const RAM_ESTIMATE: &str = "ram_estimate";
    pub const PUBLISHED_AT: &str = "published_at";
    /// Bake lifecycle: `claimed` | `ready` | `failed` (single-winner claim
    /// rows double as the bake ledger; INDEX-PLAN §8 + bakery).
    pub const STATUS: &str = "status";
    /// Last transition instant (unix milliseconds) — drives stale-claim reaps.
    pub const UPDATED_AT: &str = "updated_at";
}

/// A fully-typed `edgepack_artifacts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgepackArtifactRow {
    /// `edgepack_key_digest` — the 32-byte BLAKE3 digest of the edgepack recipe
    /// key (BLOB32 primary key).
    pub edgepack_key_digest: EdgepackKeyDigest,
    /// `version_id` — the package version this edgepack was built for (heart
    /// PackageId, stored as BLOB16).
    pub version_id: PackageId,
    /// `recipe_fingerprint` — a deterministic fingerprint of the build recipe
    /// inputs (e.g. feature flags, target triple).
    pub recipe_fingerprint: String,
    /// `artifact_id` — opaque byte reference to the stored artifact (variable
    /// width), or `NULL` when the artifact has not yet been written.
    pub artifact_id: Option<Vec<u8>>,
    /// `ram_estimate` — estimated peak resident memory in bytes during
    /// execution, or `NULL` when not yet profiled.
    pub ram_estimate: Option<i64>,
    /// `published_at` — wall-clock time the artifact was published (unix
    /// milliseconds), or `NULL` while building.
    pub published_at: Option<i64>,
}

impl EdgepackArtifactRow {
    /// The ordered column list matching [`EdgepackArtifactRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::EDGEPACK_KEY_DIGEST,
        columns::VERSION_ID,
        columns::RECIPE_FINGERPRINT,
        columns::ARTIFACT_ID,
        columns::RAM_ESTIMATE,
        columns::PUBLISHED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.edgepack_key_digest.to_blob().to_vec()),
            Value::Blob(version_id::to_blob(&self.version_id).to_vec()),
            Value::Text(self.recipe_fingerprint.clone()),
            match &self.artifact_id {
                Some(b) => Value::Blob(b.clone()),
                None => Value::Null,
            },
            bind_optional_integer(self.ram_estimate),
            bind_optional_integer(self.published_at),
        ]
    }

    /// Decode an `edgepack_artifacts` row read back in
    /// [`EdgepackArtifactRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let edgepack_key_digest = EdgepackKeyDigest::from_blob(&row.get_blob(0)?)?;
        let version_id = version_id::from_blob(&row.get_blob(1)?)?;
        Ok(Self {
            edgepack_key_digest,
            version_id,
            recipe_fingerprint: row.get_text(2)?,
            artifact_id: row.get_optional_blob(3)?,
            ram_estimate: row.get_optional_integer(4)?,
            published_at: row.get_optional_integer(5)?,
        })
    }
}
