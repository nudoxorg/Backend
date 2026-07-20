//! Graph = Trustfall over IR memory + a disposable reverse-position index (INDEX-PLAN §5.5).
//! No Ladybug, no Terminus.
//!
//! The graph module provides two orthogonal layers:
//!
//! 1. [`reverse_index`] — a disposable projection rebuilt from an [`ir::IrView`]
//!    at a known `(channel_tip, schema_version)` key. It materialises the reverse
//!    occurrence and type-reference postings so look-ups are O(log n) instead of a
//!    linear scan over all occurrences.
//!
//! 2. [`trustfall_adapter`] — an [`IrTrustfallAdapter`] that wraps an `IrView` (plus an
//!    optional `ReversePositionIndex` for the fast path) and exposes plain-Rust neighbour
//!    methods that a Trustfall schema can call once the full schema wiring is completed in
//!    a later wave. [`execute_graph_query`] is present as a correctly-typed stub that
//!    returns [`GraphQueryError::Unsupported`] until the schema is wired.
//!
//! HTTP routes (`GET /v1/symbols/:ref/usages`, etc.) are a later wave — this module
//! exposes library functions only.

pub mod reverse_index;
pub mod trustfall_adapter;

pub use reverse_index::{ReverseIndexKey, ReversePositionIndex, SCHEMA_VERSION};
pub use trustfall_adapter::{
    execute_graph_query, GraphQueryError, GraphVertex, IrTrustfallAdapter,
};
