//! `overlays` — registered overlay branches for the DoltLite catalog
//! (INDEX-PLAN ID-9).
//!
//! ```text
//! overlays
//!   name                TEXT PRIMARY KEY
//!   remote_endpoint     TEXT NULL
//!   branch              TEXT NOT NULL
//!   precedence          INTEGER NOT NULL
//!   last_merged_commit  TEXT NULL
//!   added_at            INTEGER NOT NULL   -- unix milliseconds
//! ```

use crate::codec::{bind_optional_text, CodecError};
use crate::engine::{Row, Value};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "overlays";

/// Column names, in the canonical insert order used by [`OverlayRow::bind`].
pub mod columns {
    pub const NAME: &str = "name";
    pub const REMOTE_ENDPOINT: &str = "remote_endpoint";
    pub const BRANCH: &str = "branch";
    pub const PRECEDENCE: &str = "precedence";
    pub const LAST_MERGED_COMMIT: &str = "last_merged_commit";
    pub const ADDED_AT: &str = "added_at";
}

/// A fully-typed `overlays` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRow {
    /// `name` — the human-readable overlay identifier (primary key).
    pub name: String,
    /// `remote_endpoint` — the iroh/HTTP endpoint to pull from, or `NULL` for
    /// a local-only branch.
    pub remote_endpoint: Option<String>,
    /// `branch` — the DoltLite branch name this overlay maps to.
    pub branch: String,
    /// `precedence` — merge priority; lower values win conflicts first.
    pub precedence: i64,
    /// `last_merged_commit` — the commit hash of the last successful merge into
    /// `main`, or `NULL` when never merged.
    pub last_merged_commit: Option<String>,
    /// `added_at` — wall-clock time this overlay was registered (unix
    /// milliseconds).
    pub added_at: i64,
}

impl OverlayRow {
    /// The ordered column list matching [`OverlayRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::NAME,
        columns::REMOTE_ENDPOINT,
        columns::BRANCH,
        columns::PRECEDENCE,
        columns::LAST_MERGED_COMMIT,
        columns::ADDED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Text(self.name.clone()),
            bind_optional_text(self.remote_endpoint.clone()),
            Value::Text(self.branch.clone()),
            Value::Integer(self.precedence),
            bind_optional_text(self.last_merged_commit.clone()),
            Value::Integer(self.added_at),
        ]
    }

    /// Decode an `overlays` row read back in [`OverlayRow::INSERT_COLUMNS`]
    /// order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        Ok(Self {
            name: row.get_text(0)?,
            remote_endpoint: row.get_optional_text(1)?,
            branch: row.get_text(2)?,
            precedence: row.get_integer(3)?,
            last_merged_commit: row.get_optional_text(4)?,
            added_at: row.get_integer(5)?,
        })
    }
}
