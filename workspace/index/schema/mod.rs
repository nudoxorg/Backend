//! The registry↔catalog data-mapping layer (salvaged from the deleted
//! `registry::schema`, §9a). The relational model itself lives in this crate
//! (schema v4, INDEX-PLAN §8) — this module holds only the pure codecs that
//! translate between runtime vocabulary and stored rows:
//!
//! - [`codec`] — token/JSON codecs for ecosystems, coordinates, toolchains,
//!   facets, and lifecycle states (engine-agnostic, pure).
//! - [`catalog_map`] — the frozen law lowering [`heart::ResolutionState`] and
//!   [`crate::metadata::SearchFacets`] onto catalog lifecycle/facet columns.

pub mod catalog_map;
pub mod codec;
