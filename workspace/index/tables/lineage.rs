//! `repo_lineage` — git-based fork/mirror relationships between package stems
//! (REGISTRYLESS-PLAN §5, RL-16).
//!
//! ```text
//! repo_lineage
//!   stem_id       BLOB16 NOT NULL      -- PackageStemId (source)
//!   relation      TEXT NOT NULL        -- LineageRelation token
//!   target_stem   BLOB16 NOT NULL      -- PackageStemId (target)
//!   evidence      TEXT NOT NULL        -- LineageEvidence token
//!   fork_point_rev TEXT NULL           -- git rev of the detected merge-base
//!   overlap_ratio  REAL NULL           -- [0.0, 1.0] content-overlap score
//!   confidence    TEXT NOT NULL        -- AliasConfidence token
//!   recorded_at   INTEGER NOT NULL     -- unix milliseconds
//!   PRIMARY KEY (stem_id, relation, target_stem)
//! ```

use crate::codec::{bind_text_enum, bind_optional_text, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::{AliasConfidence, LineageEvidence, LineageRelation};
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "repo_lineage";

/// Column names, in the canonical insert order used by [`RepoLineageRow::bind`].
pub mod columns {
    pub const STEM_ID: &str = "stem_id";
    pub const RELATION: &str = "relation";
    pub const TARGET_STEM: &str = "target_stem";
    pub const EVIDENCE: &str = "evidence";
    pub const FORK_POINT_REV: &str = "fork_point_rev";
    pub const OVERLAP_RATIO: &str = "overlap_ratio";
    pub const CONFIDENCE: &str = "confidence";
    pub const RECORDED_AT: &str = "recorded_at";
}

/// A fully-typed `repo_lineage` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoLineageRow {
    /// `stem_id` — the source (derived) package stem (BLOB16). Part of the
    /// composite primary key.
    pub stem_id: PackageStemId,
    /// `relation` — how `stem_id` relates to `target_stem`. Part of the
    /// composite primary key.
    pub relation: LineageRelation,
    /// `target_stem` — the upstream package stem (BLOB16). Part of the composite
    /// primary key.
    pub target_stem: PackageStemId,
    /// `evidence` — the detection method used to establish the lineage.
    pub evidence: LineageEvidence,
    /// `fork_point_rev` — the git object id of the detected merge-base, or
    /// `NULL` for content-overlap evidence.
    pub fork_point_rev: Option<String>,
    /// `overlap_ratio` — content-overlap score in `[0.0, 1.0]`, or `NULL` for
    /// git-history evidence.
    pub overlap_ratio: Option<f64>,
    /// `confidence` — how trustworthy the lineage detection is.
    pub confidence: AliasConfidence,
    /// `recorded_at` — wall-clock time this lineage was recorded (unix
    /// milliseconds).
    pub recorded_at: i64,
}

impl RepoLineageRow {
    /// The ordered column list matching [`RepoLineageRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::STEM_ID,
        columns::RELATION,
        columns::TARGET_STEM,
        columns::EVIDENCE,
        columns::FORK_POINT_REV,
        columns::OVERLAP_RATIO,
        columns::CONFIDENCE,
        columns::RECORDED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.stem_id.to_blob().to_vec()),
            bind_text_enum(self.relation),
            Value::Blob(self.target_stem.to_blob().to_vec()),
            bind_text_enum(self.evidence),
            bind_optional_text(self.fork_point_rev.clone()),
            match self.overlap_ratio {
                Some(v) => Value::Real(v),
                None => Value::Null,
            },
            bind_text_enum(self.confidence),
            Value::Integer(self.recorded_at),
        ]
    }

    /// Decode a `repo_lineage` row read back in [`RepoLineageRow::INSERT_COLUMNS`]
    /// order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let stem_id = PackageStemId::from_blob(&row.get_blob(0)?)?;
        let relation = read_text_enum::<LineageRelation>(&row.get_text(1)?)?;
        let target_stem = PackageStemId::from_blob(&row.get_blob(2)?)?;
        let evidence = read_text_enum::<LineageEvidence>(&row.get_text(3)?)?;
        let confidence = read_text_enum::<AliasConfidence>(&row.get_text(6)?)?;
        Ok(Self {
            stem_id,
            relation,
            target_stem,
            evidence,
            fork_point_rev: row.get_optional_text(4)?,
            overlap_ratio: row.get_optional_real(5)?,
            confidence,
            recorded_at: row.get_integer(7)?,
        })
    }
}
