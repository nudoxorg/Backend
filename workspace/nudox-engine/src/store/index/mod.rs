//! Derived, disposable indexes over one package's sealed IR.
//!
//! # Why separate index files
//!
//! Each index type answers a different query shape and has a different
//! performance envelope. Keeping them in separate files makes each one easy to
//! understand, test, and replace independently:
//!
//! * [`NameIndex`] — case-folded prefix lookup and fuzzy-ready storage.
//! * [`PostingList`] — sorted, deduplicated reverse postings (usages/mentions).
//! * [`AliasIndex`] — exact re-export alias path lookup (address scheme
//!   Stage 2, docs/MCP-SURFACE-PLAN.md §4.10).
//!
//! All index types are built once from a sealed [`IrView`] and are thereafter
//! immutable. They are never persisted (GD-34: disposable projections) — any
//! caller that needs them re-derives them from the view.

pub mod alias;
pub mod name;
pub mod posting;

pub use alias::AliasIndex;
pub use name::NameIndex;
pub use posting::PostingList;
