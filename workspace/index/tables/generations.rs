//! `generations` — one row per (version, compiler-invocation) IR generation
//! (INDEX-PLAN §8, ID-15).
//!
//! ```text
//! generations
//!   gen_stamp          BLOB32 PK        -- GenerationStamp
//!   version_id         BLOB16 NOT NULL  -- heart::PackageId FK → versions
//!   channel_tip        BLOB32           -- ChannelTip, nullable until seal
//!   job_key            BLOB32           -- JobKeyHash, nullable
//!   producer_toolchain TEXT             -- nullable free-text toolchain label
//!   sealed_at          INTEGER          -- unix milliseconds, nullable
//!   ir_status          TEXT NOT NULL    -- IrStatus token
//!   resolution_stats   TEXT             -- JSON payload, nullable
//! ```

use heart::PackageId;

use crate::codec::{
    bind_optional_integer, bind_optional_text, bind_text_enum, read_text_enum, CodecError,
};
use crate::engine::{Row, Value};
use crate::enums::IrStatus;
use crate::ids::{version_id, ChannelTip, GenerationStamp, JobKeyHash};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "generations";

/// Column names, in the canonical insert order used by [`GenerationRow::bind`].
pub mod columns {
    pub const GEN_STAMP: &str = "gen_stamp";
    pub const VERSION_ID: &str = "version_id";
    pub const CHANNEL_TIP: &str = "channel_tip";
    pub const JOB_KEY: &str = "job_key";
    pub const PRODUCER_TOOLCHAIN: &str = "producer_toolchain";
    pub const SEALED_AT: &str = "sealed_at";
    pub const IR_STATUS: &str = "ir_status";
    pub const RESOLUTION_STATS: &str = "resolution_stats";
}

/// A fully-typed `generations` row.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationRow {
    /// `gen_stamp` — the BLAKE3 commit-gated stamp that uniquely identifies this generation.
    pub gen_stamp: GenerationStamp,
    /// `version_id` — the version instance this generation was produced from.
    pub version_id: PackageId,
    /// `channel_tip` — the IR channel tip hash, set once the generation is sealed.
    pub channel_tip: Option<ChannelTip>,
    /// `job_key` — the producer job key, when this generation was enqueued.
    pub job_key: Option<JobKeyHash>,
    /// `producer_toolchain` — free-text label for the compiler/toolchain used.
    pub producer_toolchain: Option<String>,
    /// `sealed_at` — timestamp when the IR was sealed (unix milliseconds), if sealed.
    pub sealed_at: Option<i64>,
    /// `ir_status` — current IR seal state; always set.
    pub ir_status: IrStatus,
    /// `resolution_stats` — JSON payload with resolution statistics, if computed.
    pub resolution_stats: Option<String>,
}

impl GenerationRow {
    /// The ordered column list matching [`GenerationRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::GEN_STAMP,
        columns::VERSION_ID,
        columns::CHANNEL_TIP,
        columns::JOB_KEY,
        columns::PRODUCER_TOOLCHAIN,
        columns::SEALED_AT,
        columns::IR_STATUS,
        columns::RESOLUTION_STATS,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.gen_stamp.to_blob().to_vec()),
            Value::Blob(version_id::to_blob(&self.version_id).to_vec()),
            match self.channel_tip {
                Some(h) => Value::Blob(h.to_blob().to_vec()),
                None => Value::Null,
            },
            match self.job_key {
                Some(h) => Value::Blob(h.to_blob().to_vec()),
                None => Value::Null,
            },
            bind_optional_text(self.producer_toolchain.clone()),
            bind_optional_integer(self.sealed_at),
            bind_text_enum(self.ir_status),
            bind_optional_text(self.resolution_stats.clone()),
        ]
    }

    /// Decode a `generations` row read back in [`GenerationRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let gen_stamp = GenerationStamp::from_blob(&row.get_blob(0)?)?;
        let version_id = version_id::from_blob(&row.get_blob(1)?)?;
        let channel_tip = match row.get_optional_blob(2)? {
            Some(b) => Some(ChannelTip::from_blob(&b)?),
            None => None,
        };
        let job_key = match row.get_optional_blob(3)? {
            Some(b) => Some(JobKeyHash::from_blob(&b)?),
            None => None,
        };
        Ok(Self {
            gen_stamp,
            version_id,
            channel_tip,
            job_key,
            producer_toolchain: row.get_optional_text(4)?,
            sealed_at: row.get_optional_integer(5)?,
            ir_status: read_text_enum::<IrStatus>(&row.get_text(6)?)?,
            resolution_stats: row.get_optional_text(7)?,
        })
    }
}
