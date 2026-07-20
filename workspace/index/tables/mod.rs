//! Typed table modules — one file per schema-v4 table family (INDEX-PLAN §8)
//! plus the three registryless additions (REGISTRYLESS-PLAN §5).
//!
//! Each module holds:
//! - a fully-typed row struct (BLOB16 → uuid newtypes, BLOB32 → hash newtypes,
//!   TEXT enums → real Rust enums, JSON columns → typed payloads),
//! - the table + column name constants used by both the DDL builder
//!   ([`crate::migrations`]) and the store's SQL,
//! - `bind_*` helpers producing the ordered [`Value`](crate::engine::Value)
//!   slice for an insert/upsert, and a `from_row` decoder returning
//!   [`CodecError`](crate::codec::CodecError) on any shape mismatch.
//!
//! Table structs never touch the engine directly; the store layer
//! ([`crate::store`]) owns statement text and transactions.

pub mod advisories;
pub mod aliases;
pub mod compile_cache;
pub mod edgepack;
pub mod edges;
pub mod facets;
pub mod feed_watermarks;
pub mod generations;
pub mod git_watermarks;
pub mod lineage;
pub mod listing;
pub mod locations;
pub mod outbox;
pub mod overlays;
pub mod packages;
pub mod popularity;
pub mod repo_facts;
pub mod sink_watermarks;
pub mod stores;
pub mod symbols_proj;
pub mod versions;

/// Every catalog table name schema v4 authors. Used to validate a table name
/// before it is ever placed into SQL text (e.g. the historical `dolt_at_<table>`
/// read path), so a crafted table string can never reach the statement.
pub const ALL_TABLE_NAMES: &[&str] = &[
    packages::TABLE,
    versions::TABLE,
    generations::TABLE,
    stores::TABLE,
    locations::GENERATION_LOCATIONS_TABLE,
    locations::OBJECT_LOCATIONS_TABLE,
    repo_facts::TABLE,
    git_watermarks::TABLE,
    popularity::TABLE,
    facets::TABLE,
    listing::TABLE,
    advisories::TABLE,
    symbols_proj::TABLE,
    outbox::TABLE,
    sink_watermarks::TABLE,
    overlays::TABLE,
    compile_cache::TABLE,
    edgepack::TABLE,
    edges::TABLE,
    aliases::TABLE,
    lineage::TABLE,
    feed_watermarks::TABLE,
];

/// Whether `name` is a known schema-v4 catalog table (validation gate for any
/// code path that must place a table name into SQL text).
pub fn is_known_table(name: &str) -> bool {
    ALL_TABLE_NAMES.contains(&name)
}
