//! Generate — the "runs computer" half of the indexing flow: take a
//! materialized package and produce the resolutions that become a blob and the
//! graph documents:
//!
//! - [`cst`]: the concrete syntax tree (tree-sitter);
//! - [`surface`]: the API surface (`ir::Index`);
//! - [`tar`]: the condensed/tarred source;
//! - [`linked_data`]: RDF/JSON-LD for the graph store;
//! - [`blob_info`]: assembling the per-symbol records (incl. the
//!   code/treesitter hash) the registry/runtime sinks consume.
//!
//! IMPLEMENT HERE: orchestration over the `languages` lowering + `treesitter`.

pub mod blob_info;
pub mod cst;
pub mod linked_data;
pub mod source_archive;
pub mod surface;
pub mod tar;
