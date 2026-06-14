#![deny(missing_docs)]

//! Core types, traits, and error types for nudox-occurrences.

mod error;
mod ids;
mod traits;
mod types;

pub use error::{Error, Result};
pub use ids::{BlobRef, GlobalSymbolId, OccurrenceId, RepoId};
pub use traits::{BlobStore, Embedder, FutureParseQueue, GlobalSymbolQuery, GlobalSymbolStore, SearchIndex, SearchQuery, SymbolSearch, VectorIndex, VectorQuery};
pub use types::{BlobInfo, BodyQuery, ByteSpan, ChunkMetadata, CombineMode, EmbeddingPurpose, EmbeddingRecord, Language, LibRef, ModelType, OccurrenceFilter, ResolutionOutcome, ResolveLibReport, ScopeFilter, SearchHit, SourceChunk, SymbolKind, SymbolMatch, SymbolOrigin, SymbolQuery, TreesitterRepr, VectorHit};

/// Current BlobInfo schema version. Bump on any change to BlobInfo fields.
///
/// v2: `EmbeddingRecord` gained `model_type: ModelType`.
pub const BLOB_SCHEMA_VERSION: u32 = 2;
