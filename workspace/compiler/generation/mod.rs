//! Generation — the "runs computer" half of the indexing flow: take a
//! materialized package and produce the three resolutions that become a blob
//! (CST + IR surface + tarred source), ready for `registry::blob` to store.
//!
//! IMPLEMENT HERE: orchestration that runs `treesitter`, the `languages`
//! lowering, and `tar`, then assembles the per-symbol blob information.

pub mod blob_info;
pub mod cst;
pub mod source_archive;
pub mod surface;
