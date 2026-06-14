use async_trait::async_trait;

use crate::{
    BlobInfo, BlobRef, EmbeddingPurpose, EmbeddingRecord, GlobalSymbolId, LibRef, ModelType,
    OccurrenceId, Result, SearchHit, SourceChunk, SymbolMatch, SymbolQuery, VectorHit,
};

/// Computes embedding vectors for source chunks.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Returns the canonical model identifier string for this embedder.
    fn model_id(&self) -> &str;

    /// Returns the [`ModelType`] family / provider this embedder belongs to.
    fn model_type(&self) -> ModelType;

    /// Embeds the given source chunk for the specified purpose, returning the raw vector.
    async fn embed(&self, chunk: &SourceChunk, purpose: EmbeddingPurpose) -> Result<Vec<f32>>;
}

/// Persistent store for [`BlobInfo`] records.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Persists a [`BlobInfo`] record and returns an opaque [`BlobRef`] for future retrieval.
    async fn put(&self, info: &BlobInfo) -> Result<BlobRef>;

    /// Retrieves the [`BlobInfo`] identified by the given [`BlobRef`].
    async fn get(&self, blob_ref: &BlobRef) -> Result<BlobInfo>;

    /// Updates the stored record to attach a resolved [`GlobalSymbolId`].
    async fn update_resolution(&self, blob_ref: &BlobRef, global_id: GlobalSymbolId)
        -> Result<()>;

    /// Returns all [`BlobRef`]s currently in the store.
    ///
    /// Order is unspecified and may vary between implementations. Blob storage
    /// is the system of record, so listing is the primary rebuild path.
    async fn list(&self) -> Result<Vec<BlobRef>>;
}

/// Bidirectional store that maps library symbols to their [`GlobalSymbolId`] and vice-versa.
#[async_trait]
pub trait GlobalSymbolStore: Send + Sync {
    /// Looks up the [`GlobalSymbolId`] for a symbol name within a library, returning `None` if unknown.
    async fn lookup(&self, lib: &LibRef, symbol_name: &str) -> Result<Option<GlobalSymbolId>>;

    /// Associates an occurrence with an existing global symbol identifier.
    async fn associate(&self, global_id: GlobalSymbolId, occurrence: OccurrenceId) -> Result<()>;
}

/// Queue of blobs whose symbol resolution must be retried after their library is parsed.
#[async_trait]
pub trait FutureParseQueue: Send + Sync {
    /// Enqueues a blob to be re-resolved once the given library is available.
    async fn enqueue(&self, lib: LibRef, blob_ref: BlobRef) -> Result<()>;

    /// Drains and returns all [`BlobRef`]s queued for the given library.
    async fn drain_for_lib(&self, lib: &LibRef) -> Result<Vec<BlobRef>>;
}

/// Full-text search index over [`BlobInfo`] records.
#[async_trait]
pub trait SearchIndex: Send + Sync {
    /// Indexes the given [`BlobInfo`] so it is discoverable by full-text search.
    async fn index(&self, blob_ref: &BlobRef, info: &BlobInfo) -> Result<()>;
}

/// Vector similarity index over embedding records.
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Upserts the provided embeddings for the given blob and global symbol identifier.
    async fn upsert(
        &self,
        blob_ref: &BlobRef,
        global_id: GlobalSymbolId,
        embeddings: &[EmbeddingRecord],
    ) -> Result<()>;
}

/// Read side of the full-text search index.
///
/// Kept separate from [`SearchIndex`] so ingestion and query paths can be
/// held behind different trait objects (e.g. a write-only indexer worker vs.
/// a read-only query server).
#[async_trait]
pub trait SearchQuery: Send + Sync {
    /// Free-text search across symbol names, library names, and repo ids.
    ///
    /// Returns at most `limit` hits ordered by descending relevance score.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>>;

    /// Enumerate all index entries for a specific resolved global symbol.
    async fn find_by_global_id(
        &self,
        global_id: GlobalSymbolId,
        limit: usize,
    ) -> Result<Vec<SearchHit>>;
}

/// Read side of the vector similarity index.
///
/// Kept separate from [`VectorIndex`] for the same reason as [`SearchQuery`].
#[async_trait]
pub trait VectorQuery: Send + Sync {
    /// Return the `limit` most similar stored vectors to the given query vector.
    ///
    /// Scores are cosine similarities in `[-1.0, 1.0]`.
    async fn search(&self, vector: &[f32], limit: usize) -> Result<Vec<VectorHit>>;
}

/// Read side of the global symbol store: enumerate occurrences of a resolved symbol.
///
/// Kept separate from [`GlobalSymbolStore`] (the write/lookup side) so that the
/// search path can take only a read handle.
#[async_trait]
pub trait GlobalSymbolQuery: Send + Sync {
    /// Return all [`OccurrenceId`]s associated with the given [`GlobalSymbolId`].
    async fn get_occurrences(&self, global_id: GlobalSymbolId) -> Result<Vec<OccurrenceId>>;
}

/// Combined high-level symbol search across name, body, scope, and kind.
///
/// Implementations are responsible for querying the appropriate indexes, merging
/// results, applying filters, and fetching [`BlobInfo`] for each hit.
#[async_trait]
pub trait SymbolSearch: Send + Sync {
    /// Execute the given [`SymbolQuery`] and return ranked [`SymbolMatch`] results.
    async fn search(&self, query: &SymbolQuery) -> Result<Vec<SymbolMatch>>;
}
