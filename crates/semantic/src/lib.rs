//! Canonical semantic relations and their admission laws.
//!
//! Semantic data is represented as typed relations over stable logical keys.
//! A key is an [`backend_version::ObjectKey`]; a value is an
//! [`backend_version::ObjectVersion`]. Coverage is carried by each facet and
//! is never inferred from an empty vector or an absent optional value.
#![deny(unsafe_code)]

extern crate alloc;

mod canonical;
mod coverage;
mod error;
mod facets;
mod identity;
mod occurrence;
mod read_manifest;
mod recipes;
mod relations;
mod reuse;
mod schema;
mod support;

/// Durable, transactional carrier for verified index publication locators.
pub mod catalog;
/// Immutable graph/vector projections and their bounded leased edge.
pub mod graph_vector;
/// Borrowed immutable exact and lexical index segments.
pub mod index_core;
/// Allocation-free admission and reconciliation of observed index versions.
pub mod index_ingest;
/// Typed identities for the index capabilities that exist today.
pub mod index_vocabulary;
/// Canonical compiler IR fragments: encoding, validation, mapping, and borrowing.
pub mod ir;
pub mod ir_vocabulary;
/// Static dispatch from closed language-stage requests to concrete native compiler tools.
pub mod registry;
pub mod vocabulary;

pub use canonical::*;
pub use coverage::*;
pub use error::*;
pub use facets::*;
pub use identity::*;
pub use occurrence::*;
pub use read_manifest::*;
pub use recipes::*;
pub use relations::*;
pub use reuse::*;
pub use schema::*;

#[cfg(test)]
// Test fixtures intentionally fail fast when an invariant used by a law is
// accidentally weakened; production constructors remain fully fallible.
#[allow(clippy::expect_used)]
mod tests;

#[cfg(test)]
mod reuse_tests;
