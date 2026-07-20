//! `compile_cache` — producer job-key → result cache for IR and ObjectPack
//! compile stages (INDEX-PLAN §8).
//!
//! ```text
//! compile_cache
//!   job_key      BLOB32 PRIMARY KEY    -- JobKeyHash
//!   kind         TEXT NOT NULL         -- CompileCacheKind token
//!   gen_stamp    BLOB32 NULL           -- GenerationStamp (NULL until sealed)
//!   object_id    BLOB32 NULL           -- ObjectPackHash (NULL for IR-only cache)
//!   image_digest TEXT NULL             -- OCI image digest, when kind = golden
//!   updated_at   INTEGER NOT NULL      -- unix milliseconds
//! ```

use crate::codec::{bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::CompileCacheKind;
use crate::ids::{GenerationStamp, JobKeyHash, ObjectPackHash};

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "compile_cache";

/// Column names, in the canonical insert order used by [`CompileCacheRow::bind`].
pub mod columns {
    pub const JOB_KEY: &str = "job_key";
    pub const KIND: &str = "kind";
    pub const GEN_STAMP: &str = "gen_stamp";
    pub const OBJECT_ID: &str = "object_id";
    pub const IMAGE_DIGEST: &str = "image_digest";
    pub const UPDATED_AT: &str = "updated_at";
}

/// A fully-typed `compile_cache` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileCacheRow {
    /// `job_key` — the producer job key digest (BLOB32, primary key).
    pub job_key: JobKeyHash,
    /// `kind` — which cache tier this entry belongs to.
    pub kind: CompileCacheKind,
    /// `gen_stamp` — the IR generation stamp, once sealed; `NULL` while pending.
    pub gen_stamp: Option<GenerationStamp>,
    /// `object_id` — the ObjectPack root hash for L1-stage entries; `NULL` for
    /// L0 IR tips and golden entries.
    pub object_id: Option<ObjectPackHash>,
    /// `image_digest` — OCI image digest for `golden` entries; `NULL` otherwise.
    pub image_digest: Option<String>,
    /// `updated_at` — wall-clock time of the last cache write (unix milliseconds).
    pub updated_at: i64,
}

impl CompileCacheRow {
    /// The ordered column list matching [`CompileCacheRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::JOB_KEY,
        columns::KIND,
        columns::GEN_STAMP,
        columns::OBJECT_ID,
        columns::IMAGE_DIGEST,
        columns::UPDATED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.job_key.to_blob().to_vec()),
            bind_text_enum(self.kind),
            match self.gen_stamp {
                Some(g) => Value::Blob(g.to_blob().to_vec()),
                None => Value::Null,
            },
            match self.object_id {
                Some(o) => Value::Blob(o.to_blob().to_vec()),
                None => Value::Null,
            },
            match &self.image_digest {
                Some(d) => Value::Text(d.clone()),
                None => Value::Null,
            },
            Value::Integer(self.updated_at),
        ]
    }

    /// Decode a `compile_cache` row read back in
    /// [`CompileCacheRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let job_key = JobKeyHash::from_blob(&row.get_blob(0)?)?;
        let kind = read_text_enum::<CompileCacheKind>(&row.get_text(1)?)?;
        let gen_stamp = match row.get_optional_blob(2)? {
            Some(b) => Some(GenerationStamp::from_blob(&b)?),
            None => None,
        };
        let object_id = match row.get_optional_blob(3)? {
            Some(b) => Some(ObjectPackHash::from_blob(&b)?),
            None => None,
        };
        Ok(Self {
            job_key,
            kind,
            gen_stamp,
            object_id,
            image_digest: row.get_optional_text(4)?,
            updated_at: row.get_integer(5)?,
        })
    }
}
