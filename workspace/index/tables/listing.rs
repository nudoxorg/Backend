//! `listing_events` — bitemporal listing lifecycle for a package version
//! (INDEX-PLAN §8).
//!
//! ```text
//! listing_events
//!   seq         INTEGER PRIMARY KEY AUTOINCREMENT
//!   version_id  BLOB16 NOT NULL    -- heart PackageId
//!   status      TEXT NOT NULL      -- ListingStatus token
//!   reason      TEXT NULL
//!   valid_from  INTEGER NOT NULL   -- unix milliseconds
//!   valid_to    INTEGER NULL       -- unix milliseconds, NULL = open interval
//!   recorded_at INTEGER NOT NULL   -- unix milliseconds
//! ```
//!
//! ## AUTOINCREMENT handling
//!
//! `seq` is engine-assigned on insert.  [`ListingEventRow::INSERT_COLUMNS`] and
//! [`ListingEventRow::bind`] therefore **omit** `seq`.  For reads (e.g. a full
//! table scan or a keyed SELECT), use [`ListingEventRow::SELECT_COLUMNS`] and
//! [`ListingEventRow::from_row`], which expect `seq` as column 0 and the
//! remaining fields at indices 1–6.

use heart::PackageId;

use crate::codec::{bind_optional_integer, bind_optional_text, bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::ListingStatus;
use crate::ids::version_id;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "listing_events";

/// Column names for inserts and selects.
pub mod columns {
    pub const SEQ: &str = "seq";
    pub const VERSION_ID: &str = "version_id";
    pub const STATUS: &str = "status";
    pub const REASON: &str = "reason";
    pub const VALID_FROM: &str = "valid_from";
    pub const VALID_TO: &str = "valid_to";
    pub const RECORDED_AT: &str = "recorded_at";
}

/// A fully-typed `listing_events` row, including the engine-assigned `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct ListingEventRow {
    /// `seq` — autoincrement primary key, assigned by the engine; present in
    /// reads via [`ListingEventRow::from_row`] but absent from inserts.
    pub seq: i64,
    /// `version_id` — the version this event belongs to (heart PackageId, stored
    /// as BLOB16).
    pub version_id: PackageId,
    /// `status` — the listing lifecycle state at this point in time.
    pub status: ListingStatus,
    /// `reason` — human-readable rationale for the status change, or `NULL`.
    pub reason: Option<String>,
    /// `valid_from` — start of the bitemporal validity interval (unix
    /// milliseconds).
    pub valid_from: i64,
    /// `valid_to` — end of the bitemporal validity interval (unix milliseconds),
    /// `NULL` for an open (current) interval.
    pub valid_to: Option<i64>,
    /// `recorded_at` — wall-clock insert time (unix milliseconds).
    pub recorded_at: i64,
}

impl ListingEventRow {
    /// Insert columns — **excludes** `seq`, which is engine-assigned.
    /// Pass this list when building an `INSERT` statement.
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::VERSION_ID,
        columns::STATUS,
        columns::REASON,
        columns::VALID_FROM,
        columns::VALID_TO,
        columns::RECORDED_AT,
    ];

    /// Select columns for full-row reads — **includes** `seq` first.
    /// Use this list when building a `SELECT` that feeds [`ListingEventRow::from_row`].
    pub const SELECT_COLUMNS: &'static [&'static str] = &[
        columns::SEQ,
        columns::VERSION_ID,
        columns::STATUS,
        columns::REASON,
        columns::VALID_FROM,
        columns::VALID_TO,
        columns::RECORDED_AT,
    ];

    /// Bind this row for an insert.  The returned slice matches
    /// [`ListingEventRow::INSERT_COLUMNS`] — `seq` is omitted.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(version_id::to_blob(&self.version_id).to_vec()),
            bind_text_enum(self.status),
            bind_optional_text(self.reason.clone()),
            Value::Integer(self.valid_from),
            bind_optional_integer(self.valid_to),
            Value::Integer(self.recorded_at),
        ]
    }

    /// Decode a `listing_events` row read back in
    /// [`ListingEventRow::SELECT_COLUMNS`] order (seq at index 0).
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let seq = row.get_integer(0)?;
        let version_id = version_id::from_blob(&row.get_blob(1)?)?;
        let status = read_text_enum::<ListingStatus>(&row.get_text(2)?)?;
        Ok(Self {
            seq,
            version_id,
            status,
            reason: row.get_optional_text(3)?,
            valid_from: row.get_integer(4)?,
            valid_to: row.get_optional_integer(5)?,
            recorded_at: row.get_integer(6)?,
        })
    }
}
