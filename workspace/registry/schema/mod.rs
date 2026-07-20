//! The registry↔catalog data-mapping layer.
//!
//! The relational model itself now lives in the `index` crate (schema v4,
//! INDEX-PLAN §8) — this module holds only the pure codecs that translate
//! between runtime vocabulary and stored rows:
//!
//! - [`codec`] — token/JSON codecs for ecosystems, coordinates, toolchains,
//!   facets, and lifecycle states (engine-agnostic, pure).
//! - [`catalog_map`] — the frozen law lowering [`heart::ResolutionState`] and
//!   [`crate::metadata::SearchFacets`] onto catalog lifecycle/facet columns.
//!
//! The old postgres DDL (sea-query `Iden` enums + `schema_ddl`) is gone: the
//! catalog migrates itself (`index::migrations`), and scratch tables live in
//! `index::scratch`.

pub mod catalog_map;
pub mod codec;
