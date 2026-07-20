//! `outbox` — the only projection fan-out (INDEX-PLAN §8, ID-3).
//!
//! ```text
//! outbox
//!   seq INTEGER PRIMARY KEY AUTOINCREMENT
//!   version_id BLOB16
//!   gen_stamp BLOB32
//!   sink_kind TEXT NOT NULL      -- SinkKind
//!   op TEXT NOT NULL             -- OutboxOperation
//!   created_at INTEGER NOT NULL
//! ```
//!
//! Rows are written in the **same transaction** as the business mutation that
//! produced them; followers drain them via
//! [`MetaStore::outbox_claim`](crate::store::MetaStore::outbox_claim). `seq` is a
//! monotonic total order — the basis of both the sink watermark and the
//! [`CatalogCursor`](crate::store::CatalogCursor).

use crate::codec::{read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::{OutboxOperation, SinkKind, TextEnum};
use crate::ids::{version_id, GenerationStamp, PackageId};

/// The table name.
pub const TABLE: &str = "outbox";

/// Column names.
pub mod columns {
    /// Autoincrement sequence (assigned by the engine).
    pub const SEQ: &str = "seq";
    /// The affected version.
    pub const VERSION_ID: &str = "version_id";
    /// The affected generation.
    pub const GEN_STAMP: &str = "gen_stamp";
    /// The projection sink.
    pub const SINK_KIND: &str = "sink_kind";
    /// The projection operation.
    pub const OP: &str = "op";
    /// When the row was written.
    pub const CREATED_AT: &str = "created_at";
}

/// A fully-typed `outbox` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    /// `seq` — the monotonic order key (only meaningful on reads).
    pub seq: i64,
    /// `version_id` — the affected version, when the fan-out is version-scoped.
    pub version_id: Option<PackageId>,
    /// `gen_stamp` — the affected generation, when the fan-out is generation-scoped.
    pub gen_stamp: Option<GenerationStamp>,
    /// `sink_kind` — which projection follower must act.
    pub sink_kind: SinkKind,
    /// `op` — upsert or delete.
    pub op: OutboxOperation,
    /// `created_at` — the write instant.
    pub created_at: i64,
}

impl OutboxRow {
    /// Columns for INSERT (excludes the autoincrement `seq`).
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::VERSION_ID,
        columns::GEN_STAMP,
        columns::SINK_KIND,
        columns::OP,
        columns::CREATED_AT,
    ];

    /// Columns for SELECT (includes `seq` first).
    pub const SELECT_COLUMNS: &'static [&'static str] = &[
        columns::SEQ,
        columns::VERSION_ID,
        columns::GEN_STAMP,
        columns::SINK_KIND,
        columns::OP,
        columns::CREATED_AT,
    ];

    /// Bind an insert (no `seq`).
    pub fn bind_insert(
        version_id: Option<PackageId>,
        gen_stamp: Option<GenerationStamp>,
        sink_kind: SinkKind,
        op: OutboxOperation,
        created_at: i64,
    ) -> Vec<Value> {
        vec![
            match version_id {
                Some(id) => Value::Blob(version_id::to_blob(&id).to_vec()),
                None => Value::Null,
            },
            match gen_stamp {
                Some(stamp) => Value::Blob(stamp.to_blob().to_vec()),
                None => Value::Null,
            },
            Value::Text(sink_kind.as_token().to_owned()),
            Value::Text(op.as_token().to_owned()),
            Value::Integer(created_at),
        ]
    }

    /// Decode a row read back in [`OutboxRow::SELECT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        Ok(Self {
            seq: row.get_integer(0)?,
            version_id: match row.get_optional_blob(1)? {
                Some(bytes) => Some(version_id::from_blob(&bytes)?),
                None => None,
            },
            gen_stamp: match row.get_optional_blob(2)? {
                Some(bytes) => Some(GenerationStamp::from_blob(&bytes)?),
                None => None,
            },
            sink_kind: read_text_enum::<SinkKind>(&row.get_text(3)?)?,
            op: read_text_enum::<OutboxOperation>(&row.get_text(4)?)?,
            created_at: row.get_integer(5)?,
        })
    }
}
