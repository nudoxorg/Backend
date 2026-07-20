//! `sink_watermarks` — per-sink last-consumed sequence number for the outbox
//! follower loop (INDEX-PLAN ID-3).
//!
//! ```text
//! sink_watermarks
//!   sink_kind   TEXT PRIMARY KEY   -- SinkKind token
//!   last_seq    INTEGER NOT NULL   -- highest outbox.seq consumed
//!   updated_at  INTEGER NOT NULL   -- unix milliseconds
//! ```

use crate::codec::{bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::SinkKind;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "sink_watermarks";

/// Column names, in the canonical insert order used by [`SinkWatermarkRow::bind`].
pub mod columns {
    pub const SINK_KIND: &str = "sink_kind";
    pub const LAST_SEQ: &str = "last_seq";
    pub const UPDATED_AT: &str = "updated_at";
}

/// A fully-typed `sink_watermarks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkWatermarkRow {
    /// `sink_kind` — the projection sink this watermark tracks (primary key).
    pub sink_kind: SinkKind,
    /// `last_seq` — the highest `outbox.seq` value the follower has consumed.
    pub last_seq: i64,
    /// `updated_at` — wall-clock time of the last watermark advance (unix
    /// milliseconds).
    pub updated_at: i64,
}

impl SinkWatermarkRow {
    /// The ordered column list matching [`SinkWatermarkRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::SINK_KIND,
        columns::LAST_SEQ,
        columns::UPDATED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            bind_text_enum(self.sink_kind),
            Value::Integer(self.last_seq),
            Value::Integer(self.updated_at),
        ]
    }

    /// Decode a `sink_watermarks` row read back in
    /// [`SinkWatermarkRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let sink_kind = read_text_enum::<SinkKind>(&row.get_text(0)?)?;
        Ok(Self {
            sink_kind,
            last_seq: row.get_integer(1)?,
            updated_at: row.get_integer(2)?,
        })
    }
}
