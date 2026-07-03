//! Precise text search (tantivy) over the indexed symbols — the **default**
//! search surface, returning symbols by name/signature without touching the
//! semantic layer.
//!
//! ## Replica-local, single-writer
//! Unlike the shared qdrant/terminus stores, the tantivy index is
//! **replica-local**: each server replica owns its own tantivy directory on
//! local disk, built independently from the durable postgres watermark. There is
//! exactly one writer per directory (the local [`Poller`]), so there is no
//! cross-replica write contention and no shared-index consistency problem — a
//! replica simply catches its local index up to postgres and serves from it.
//!
//! ## Blocking boundary
//! Tantivy indexing and search are CPU/disk-bound and synchronous; every such
//! call is dispatched to `spawn_blocking` (marked at each boundary) so the async
//! runtime's worker threads are never stalled.

pub mod index;
pub mod poll;
pub mod query;

pub use index::{TextIndex, TextSchema};
pub use poll::{Poller, Watermark};
pub use query::TextQuery;

/// The keyset key a text-search [`heart::Cursor`] resumes from: a hit's score
/// paired with its symbol id (score alone is not unique).
pub type TextCursorKey = (heart::Score, heart::SymbolId);
