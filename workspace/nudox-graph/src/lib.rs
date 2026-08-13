//! nudox-graph — the Trustfall query plane over the local IR corpus.
//!
//! This crate exposes a single [`CorpusAdapter`] that implements
//! [`AsyncAdapter`] over the [`Corpus`] store. Queries are expressed in
//! GraphQL against `schema.graphql` and executed with
//! `trustfall::execute_query_async`.
//!
//! # Design constraints (GUI-LOCAL-PLAN §L4)
//!
//! * LR-2: no `serde_json::Value` anywhere in the query plane.
//! * LR-6: no `unwrap`/`expect` in adapter resolution paths; every fallible
//!   operation returns `Result<_, GraphError>`.
//! * §L7.4: no `anyhow` in library code; errors are `thiserror` enums.
//! * `GraphError` and every public wire enum are `#[non_exhaustive]`.

pub mod adapter;
pub mod plan;
pub mod probe;
pub mod queries;
pub mod vertex;

pub use adapter::{CorpusAdapter, GraphError};
pub use plan::{PackagePlan, SymbolPlan};
pub use probe::{AdapterProbe, StoreProbe};
pub use vertex::Vertex;

/// The raw GraphQL SDL source of `schema.graphql`, for serving to agents.
///
/// Exposed so that `nudox-mcp` (and any other crate that needs to serve the
/// schema as text) can share a single `include_str!` rather than reaching
/// into this crate's source tree with their own path (LR-7: one schema).
///
/// Equivalent to the parsed form returned by [`schema()`]; both point at the
/// same file, and `tests/schema_source.rs` asserts they agree.
pub const SCHEMA_SDL: &str = include_str!("../schema.graphql");

/// Parse `schema.graphql` exactly once and return the singleton [`Schema`].
///
/// Parses on the first call and caches the result in a process-global
/// [`std::sync::OnceLock`]. Subsequent calls return the cached value at the
/// cost of a pointer load.
///
/// # Panics
///
/// Panics on the first call if the embedded `schema.graphql` fails to parse.
/// This is a programming error (the schema is `include_str!`'d at compile
/// time), not a runtime error, so `panic!` is the appropriate signal rather
/// than a `Result`.
pub fn schema() -> &'static trustfall::Schema {
    use std::sync::OnceLock;
    use trustfall::Schema;

    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        Schema::parse(include_str!("../schema.graphql"))
            .expect("embedded schema.graphql must be valid")
    })
}
