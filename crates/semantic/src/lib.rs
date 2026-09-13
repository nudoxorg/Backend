//! Canonical semantic relations and their admission laws.
//!
//! Semantic data is represented as typed relations over stable logical keys.
//! A key is an [`backend_version::ObjectKey`]; a value is an
//! [`backend_version::ObjectVersion`]. Coverage is carried by each facet and
//! is never inferred from an empty vector or an absent optional value.
#![deny(unsafe_code)]

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

pub mod ir_vocabulary;
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
