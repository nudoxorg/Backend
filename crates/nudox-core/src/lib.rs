#![deny(missing_docs)]

//! Core types, traits, and error types for nudox-occurrences.

mod error;
mod ids;
mod traits;
mod types;

pub use error::{Error, Result};
pub use ids::{BlobRef, GlobalSymbolId, OccurrenceId, RepoId};
pub use traits::{BlobStore, Embedder, FutureParseQueue, GlobalSymbolStore, SearchIndex, SearchQuery, VectorIndex, VectorQuery};
pub use types::{
    BlobInfo, ByteSpan, ChunkMetadata, EmbeddingPurpose, EmbeddingRecord, Language,
    LibRef, ModelType, ResolutionOutcome, ResolveLibReport, SearchHit, SourceChunk, SymbolOrigin,
    TreesitterRepr, VectorHit,
};

/// Current BlobInfo schema version. Bump on any change to BlobInfo fields.
///
/// v2: `EmbeddingRecord` gained `model_type: ModelType`.
pub const BLOB_SCHEMA_VERSION: u32 = 2;
