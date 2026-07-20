//! `generation_locations` and `object_locations` — store-presence tracking
//! for IR generations and ObjectPacks (INDEX-PLAN §8).
//!
//! ```text
//! generation_locations
//!   gen_stamp BLOB32 NOT NULL  -- GenerationStamp FK → generations
//!   store_id  BLOB16 NOT NULL  -- StoreId FK → stores
//!   status    TEXT NOT NULL    -- LocationStatus token
//!   PRIMARY KEY (gen_stamp, store_id)
//!
//! object_locations
//!   object_id BLOB32 NOT NULL  -- ObjectPackHash
//!   store_id  BLOB16 NOT NULL  -- StoreId FK → stores
//!   status    TEXT NOT NULL    -- LocationStatus token
//!   PRIMARY KEY (object_id, store_id)
//! ```

use crate::codec::{bind_text_enum, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::LocationStatus;
use crate::ids::{GenerationStamp, ObjectPackHash, StoreId};

// ─────────────────────────────────────────────────────────────────────────────
// generation_locations
// ─────────────────────────────────────────────────────────────────────────────

/// The `generation_locations` table name as written in DDL and SQL.
pub const GENERATION_LOCATIONS_TABLE: &str = "generation_locations";

/// Column names for `generation_locations`, in the canonical insert order used
/// by [`GenerationLocationRow::bind`].
pub mod generation_columns {
    pub const GEN_STAMP: &str = "gen_stamp";
    pub const STORE_ID: &str = "store_id";
    pub const STATUS: &str = "status";
}

/// A fully-typed `generation_locations` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationLocationRow {
    /// `gen_stamp` — the generation whose presence at a store is recorded here.
    pub gen_stamp: GenerationStamp,
    /// `store_id` — the store at which the generation is (or was) held.
    pub store_id: StoreId,
    /// `status` — current presence state at this store.
    pub status: LocationStatus,
}

impl GenerationLocationRow {
    /// The ordered column list matching [`GenerationLocationRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        generation_columns::GEN_STAMP,
        generation_columns::STORE_ID,
        generation_columns::STATUS,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.gen_stamp.to_blob().to_vec()),
            Value::Blob(self.store_id.to_blob().to_vec()),
            bind_text_enum(self.status),
        ]
    }

    /// Decode a `generation_locations` row read back in
    /// [`GenerationLocationRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let gen_stamp = GenerationStamp::from_blob(&row.get_blob(0)?)?;
        let store_id = StoreId::from_blob(&row.get_blob(1)?)?;
        Ok(Self {
            gen_stamp,
            store_id,
            status: read_text_enum::<LocationStatus>(&row.get_text(2)?)?,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// object_locations
// ─────────────────────────────────────────────────────────────────────────────

/// The `object_locations` table name as written in DDL and SQL.
pub const OBJECT_LOCATIONS_TABLE: &str = "object_locations";

/// Column names for `object_locations`, in the canonical insert order used by
/// [`ObjectLocationRow::bind`].
pub mod object_columns {
    pub const OBJECT_ID: &str = "object_id";
    pub const STORE_ID: &str = "store_id";
    pub const STATUS: &str = "status";
}

/// A fully-typed `object_locations` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectLocationRow {
    /// `object_id` — the ObjectPack whose presence at a store is recorded here.
    pub object_id: ObjectPackHash,
    /// `store_id` — the store at which the ObjectPack is (or was) held.
    pub store_id: StoreId,
    /// `status` — current presence state at this store.
    pub status: LocationStatus,
}

impl ObjectLocationRow {
    /// The ordered column list matching [`ObjectLocationRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        object_columns::OBJECT_ID,
        object_columns::STORE_ID,
        object_columns::STATUS,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.object_id.to_blob().to_vec()),
            Value::Blob(self.store_id.to_blob().to_vec()),
            bind_text_enum(self.status),
        ]
    }

    /// Decode an `object_locations` row read back in
    /// [`ObjectLocationRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let object_id = ObjectPackHash::from_blob(&row.get_blob(0)?)?;
        let store_id = StoreId::from_blob(&row.get_blob(1)?)?;
        Ok(Self {
            object_id,
            store_id,
            status: read_text_enum::<LocationStatus>(&row.get_text(2)?)?,
        })
    }
}
