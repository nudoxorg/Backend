#![deny(missing_docs)]

//! Core types, traits, and error types for nudox-occurrences.

mod error;
mod ids;
mod traits;
mod types;

pub use error::{
    BlobStoreError, EmbedderError, Error, GlobalStoreError, PipelineError, QueueError, Result,
    SearchError, VectorError,
};
pub use ids::{BlobRef, GlobalSymbolId, OccurrenceId, RepoId};
pub use traits::{BlobStore, Embedder, FutureParseQueue, GlobalSymbolQuery, GlobalSymbolStore, SearchIndex, SearchQuery, SymbolSearch, VectorIndex, VectorQuery};
pub use types::*;

/// Current BlobInfo schema version. Bump on any change to BlobInfo fields.
///
/// v2: `EmbeddingRecord` gained `model_type: ModelType`.
pub const BLOB_SCHEMA_VERSION: u32 = 2;
