//! `stores` — IR-VCS and ObjectPack store registrations (INDEX-PLAN §8).
//!
//! ```text
//! stores
//!   store_id  BLOB16 PK        -- StoreId
//!   kind      TEXT NOT NULL    -- StoreKind token
//!   endpoint  TEXT NOT NULL    -- connection string / path
//!   healthy   INTEGER NOT NULL -- boolean 0/1
//!   added_at  INTEGER NOT NULL -- unix milliseconds
//! ```

use crate::codec::{bind_bool, bind_text_enum, read_bool, read_text_enum, CodecError};
use crate::engine::{Row, Value};
use crate::enums::StoreKind;
use crate::ids::StoreId;

/// The table name as written in DDL and SQL.
pub const TABLE: &str = "stores";

/// Column names, in the canonical insert order used by [`StoreRow::bind`].
pub mod columns {
    pub const STORE_ID: &str = "store_id";
    pub const KIND: &str = "kind";
    pub const ENDPOINT: &str = "endpoint";
    pub const HEALTHY: &str = "healthy";
    pub const ADDED_AT: &str = "added_at";
}

/// A fully-typed `stores` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRow {
    /// `store_id` — the unique identifier of this store.
    pub store_id: StoreId,
    /// `kind` — the family of store (IR-VCS local/iroh, ObjectPack local/iroh).
    pub kind: StoreKind,
    /// `endpoint` — the connection string, local path, or iroh ticket for this store.
    pub endpoint: String,
    /// `healthy` — whether the last health check for this store succeeded.
    pub healthy: bool,
    /// `added_at` — when this store was registered (unix milliseconds).
    pub added_at: i64,
}

impl StoreRow {
    /// The ordered column list matching [`StoreRow::bind`].
    pub const INSERT_COLUMNS: &'static [&'static str] = &[
        columns::STORE_ID,
        columns::KIND,
        columns::ENDPOINT,
        columns::HEALTHY,
        columns::ADDED_AT,
    ];

    /// Bind this row to an ordered value slice for an insert/upsert.
    pub fn bind(&self) -> Vec<Value> {
        vec![
            Value::Blob(self.store_id.to_blob().to_vec()),
            bind_text_enum(self.kind),
            Value::Text(self.endpoint.clone()),
            bind_bool(self.healthy),
            Value::Integer(self.added_at),
        ]
    }

    /// Decode a `stores` row read back in [`StoreRow::INSERT_COLUMNS`] order.
    pub fn from_row(row: &dyn Row) -> Result<Self, CodecError> {
        let store_id = StoreId::from_blob(&row.get_blob(0)?)?;
        Ok(Self {
            store_id,
            kind: read_text_enum::<StoreKind>(&row.get_text(1)?)?,
            endpoint: row.get_text(2)?,
            healthy: read_bool(row.get_integer(3)?),
            added_at: row.get_integer(4)?,
        })
    }
}
