//! The read-plane runtime salvage (§9a). Only the pieces the data layer needs
//! survive the re-layering:
//! - [`error`] — the per-area error enums (graph/vector/text/session/embed) the
//!   folded coordination + text poller chain through.
//! - [`text`] — the tantivy replica text index (added in the text tier).
//!
//! Pagination moved to `heart::page`; the session layer was deleted ("weird and
//! needless", §8); the graph/vector runtime error enums are retained only as
//! the classified error surface the folded code references.

pub mod error;
/// Keyset-pagination helper for score-ranked backends (kept over `heart::page`
/// because the text/vector call sites resume on a `(Score, SymbolId)` key).
pub(crate) mod pagination;
/// The replica-local tantivy text index + its catalog-outbox poller.
pub mod text;
