//! `repo_facts` — GitHub/VCS metadata fetched per package stem (INDEX-PLAN §8).
//!
//! ```text
//! repo_facts
//!   stem_id          BLOB16 PK        -- PackageStemId FK → packages
//!   stars            INTEGER          -- nullable star count
//!   last_activity_at INTEGER          -- unix milliseconds, nullable
//!   archived         INTEGER NOT NULL -- boolean 0/1
//!   default_branch   TEXT             -- nullable
//!   fetched_at       INTEGER NOT NULL -- unix milliseconds
//! ```

use crate::codec::{
    bind_bool, bind_optional_integer, bind_optional_text, read_bool, CodecError,
};
use crate::engine::{Row, Value};
use crate::ids::PackageStemId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "repo_facts";

/// Column names, in the canonical insert order used by [`RepoFactsRow::bind`].
pub mod columns {
    pub const STEM_ID: &str = "stem_id";
    pub const STARS: &str = "stars";
    pub const LAST_ACTIVITY_AT: &str = "last_activity_at";
    pub const ARCHIVED: &str = "archived";
    pub const DEFAULT_BRANCH: &str = "default_branch";
    pub const FETCHED_AT: &str = "fetched_at";
}

/// A fully-typed `repo_facts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFactsRow {
    /// `stem_id` — the package stem these repository facts belong to.
    pub stem_id: PackageStemId,
    /// `stars` — GitHub/VCS star count, when available.
    pub stars: Option<i64>,
    /// `last_activity_at` — most recent commit or push timestamp (unix milliseconds), if known.
    pub last_activity_at: Option<i64>,
    /// `archived` — whether the upstream repository has been archived.
    pub archived: bool,
    /// `default_branch` — the default branch name, when known.
    pub default_branch: Option<String>,
    /// `fetched_at` — when these facts were last fetched (unix milliseconds).
    pub fetched_at: i64,
}

impl RepoFactsRow {
    /// The ordered column list matching [`RepoFactsRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::STEM_ID,
        columns::STARS,
        columns::LAST_ACTIVITY_AT,
        columns::ARCHIVED,
        columns::DEFAULT_BRANCH,
        columns::FETCHED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.stem_id.to_blob().to_vec()),
            bind_optional_integer(self.stars),
            bind_optional_integer(self.last_activity_at),
            bind_bool(self.archived),
            bind_optional_text(self.default_branch.clone()),
            Value::Integer(self.fetched_at),
        ]
    }

    /// Decode a `repo_facts` row read back in [`RepoFactsRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let stem_id = PackageStemId::from_blob(&row.get_blob(0)?)?;
        Ok(Self {
            stem_id,
            stars: row.get_optional_integer(1)?,
            last_activity_at: row.get_optional_integer(2)?,
            archived: read_bool(row.get_integer(3)?),
            default_branch: row.get_optional_text(4)?,
            fetched_at: row.get_integer(5)?,
        })
    }
}
