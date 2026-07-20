//! `symbols_proj` — IR-plane symbol identity projected into the search shape
//! (INDEX-PLAN §8).
//!
//! ```text
//! symbols_proj
//!   intro_id   BLOB32 NOT NULL    -- IntroIdHash
//!   version_id BLOB16 NOT NULL    -- heart PackageId
//!   gen_stamp  BLOB32 NOT NULL    -- GenerationStamp
//!   moniker    TEXT NOT NULL
//!   kind       TEXT NOT NULL      -- free symbol-kind token (not a TextEnum)
//!   PRIMARY KEY (gen_stamp, intro_id)
//! ```

use heart::PackageId;

use crate::codec::CodecError;
use crate::engine::{Row, Value};
use crate::ids::{GenerationStamp, IntroIdHash, version_id};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "symbols_proj";

/// Column names, in the canonical insert order used by
/// [`SymbolProjectionRow::bind`].
pub mod columns {
    pub const INTRO_ID: &str = "intro_id";
    pub const VERSION_ID: &str = "version_id";
    pub const GEN_STAMP: &str = "gen_stamp";
    pub const MONIKER: &str = "moniker";
    pub const KIND: &str = "kind";
}

/// A fully-typed `symbols_proj` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolProjectionRow {
    /// `intro_id` — IR-plane symbol identity (IntroIdHash, stored as BLOB32).
    /// The primary key is `(gen_stamp, intro_id)`.
    pub intro_id: IntroIdHash,
    /// `version_id` — the package version this projection belongs to (heart
    /// PackageId, stored as BLOB16).
    pub version_id: PackageId,
    /// `gen_stamp` — the generation that produced this projection (BLOB32).
    /// Part of the composite primary key.
    pub gen_stamp: GenerationStamp,
    /// `moniker` — the canonical display name of the symbol.
    pub moniker: String,
    /// `kind` — free symbol-kind token (e.g. `"function"`, `"struct"`).
    /// Stored as plain `TEXT`; not constrained to a [`crate::enums::TextEnum`]
    /// because the vocabulary is open-ended across languages.
    pub kind: String,
}

impl SymbolProjectionRow {
    /// The ordered column list matching [`SymbolProjectionRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::INTRO_ID,
        columns::VERSION_ID,
        columns::GEN_STAMP,
        columns::MONIKER,
        columns::KIND,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.intro_id.to_blob().to_vec()),
            Value::Blob(version_id::to_blob(&self.version_id).to_vec()),
            Value::Blob(self.gen_stamp.to_blob().to_vec()),
            Value::Text(self.moniker.clone()),
            Value::Text(self.kind.clone()),
        ]
    }

    /// Decode a `symbols_proj` row read back in
    /// [`SymbolProjectionRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let intro_id = IntroIdHash::from_blob(&row.get_blob(0)?)?;
        let version_id = version_id::from_blob(&row.get_blob(1)?)?;
        let gen_stamp = GenerationStamp::from_blob(&row.get_blob(2)?)?;
        Ok(Self {
            intro_id,
            version_id,
            gen_stamp,
            moniker: row.get_text(3)?,
            kind: row.get_text(4)?,
        })
    }
}
