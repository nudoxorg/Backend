use serde::{Deserialize, Serialize};

use crate::{BlobRef, GlobalSymbolId, OccurrenceId, RepoId};

/// Identifies an external library by name and version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibRef {
    /// The library's package name.
    pub name: String,
    /// The library's version string.
    pub version: String,
}

/// Describes where a symbol originates — either from an indexed repo or an external library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolOrigin {
    /// The symbol originates from an indexed repository.
    Repo {
        /// The repository identifier.
        repo_id: RepoId,
    },
    /// The symbol originates from an external library dependency.
    ExternalLib {
        /// The external library reference.
        lib: LibRef,
    },
}

/// Opaque tree-sitter representation of a source chunk as raw bytes.
///
/// The concrete format is decided by the pipeline implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreesitterRepr(pub Vec<u8>);

/// A byte range within a source file or buffer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ByteSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

/// A chunk of source code together with its tree-sitter representation and the span of the primary symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceChunk {
    /// The raw source code text of this chunk.
    pub raw_code: String,
    /// The tree-sitter representation of this chunk.
    pub treesitter_repr: TreesitterRepr,
    /// The byte span of the primary symbol within `raw_code`.
    pub symbol_span: ByteSpan,
}

/// Describes the semantic purpose for which an embedding was generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmbeddingPurpose {
    /// Embedding captures the code structure and semantics.
    Code,
    /// Embedding captures a docstring attached to the symbol.
    Docstring,
    /// Embedding captures an inline or block comment.
    Comment,
    /// Embedding captures some other user-defined purpose.
    Other(String),
}

/// Identifies the kind of embedding backend that produced a vector.
///
/// Carried with every [`EmbeddingRecord`] so that downstream consumers (indexes,
/// reranking, debugging) can distinguish vectors from different providers even
/// when their model names happen to collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelType {
    /// Deterministic placeholder embedder used for development.
    Placeholder,
    /// Deterministic constant-vector mock embedder used in tests.
    Mock,
    /// In-process model (e.g. candle, ort).
    InProcess,
    /// Remote HTTP embedding service hosted by OpenAI or similar.
    Openai,
    /// Self-hosted remote embedding service.
    SelfHosted,
    /// Any other model type identified by a free-form string.
    Other(String),
}

/// A single embedding vector produced by a specific model for a specific purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    /// Family / provider of the model that produced this vector.
    pub model_type: ModelType,
    /// The model's identifier (e.g. `"text-embedding-3-small"`).
    pub model: String,
    /// The semantic purpose for which this embedding was generated.
    pub purpose: EmbeddingPurpose,
    /// The raw embedding vector.
    pub vector: Vec<f32>,
}

/// Programming language of a source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    /// The Rust programming language.
    Rust,
}

/// Metadata that describes the context in which a source chunk was extracted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMetadata {
    /// The repository from which this chunk was extracted.
    pub repo_id: RepoId,
    /// Path to the source file within the repository.
    pub file_path: std::path::PathBuf,
    /// Byte span of this chunk within the source file.
    pub file_span: ByteSpan,
    /// Timestamp at which this chunk was parsed.
    pub parsed_at: chrono::DateTime<chrono::Utc>,
    /// The programming language of this chunk.
    pub lang: Language,
    /// Optional toolchain / language version string (e.g. `"1.78.0"` for Rust).
    pub lang_version: Option<String>,
    /// Schema version of the [`BlobInfo`] format.
    pub blob_schema_version: u32,
}

/// The primary record stored per occurrence: all data about one symbol occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobInfo {
    /// Unique identifier for this occurrence.
    pub occurrence_id: OccurrenceId,
    /// Fully-qualified name of the symbol.
    pub symbol_name: String,
    /// Where this symbol originates from.
    pub symbol_origin: SymbolOrigin,
    /// Resolved global symbol identifier, if resolution has already occurred.
    pub resolved_global_id: Option<GlobalSymbolId>,
    /// The extracted source chunk for this occurrence.
    pub source: SourceChunk,
    /// All embedding vectors computed for this occurrence.
    pub embeddings: Vec<EmbeddingRecord>,
    /// Contextual metadata about where and when this chunk was extracted.
    pub metadata: ChunkMetadata,
}

/// The outcome of attempting to resolve a symbol occurrence to a global identity.
#[derive(Debug, Clone)]
pub enum ResolutionOutcome {
    /// The symbol was successfully resolved to a known global identity.
    Resolved {
        /// The resolved global symbol identifier.
        global_id: GlobalSymbolId,
        /// Reference to the stored blob for this occurrence.
        blob_ref: BlobRef,
    },
    /// Resolution could not be completed because the library has not been parsed yet.
    Deferred {
        /// The library whose parse is pending.
        lib: LibRef,
        /// Reference to the stored blob to revisit after the library is parsed.
        blob_ref: BlobRef,
    },
}

/// Summary statistics from a resolve-library pass.
#[derive(Debug, Clone)]
pub struct ResolveLibReport {
    /// Total number of blobs examined during the pass.
    pub blobs_seen: usize,
    /// Number of blobs successfully resolved.
    pub blobs_resolved: usize,
    /// Number of blobs skipped (e.g. already resolved or unresolvable).
    pub blobs_skipped: usize,
}

/// A single hit returned from the full-text search index.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Reference to the indexed blob.
    pub blob_ref: BlobRef,
    /// Occurrence identifier for the matching record.
    pub occurrence_id: OccurrenceId,
    /// Fully-qualified symbol name of the matching record.
    pub symbol_name: String,
    /// Relevance score (higher is more relevant; scale is index-dependent).
    pub score: f32,
}

/// A single hit returned from the vector similarity index.
#[derive(Debug, Clone)]
pub struct VectorHit {
    /// Reference to the indexed blob.
    pub blob_ref: BlobRef,
    /// Global symbol identifier associated with this vector point.
    pub global_id: GlobalSymbolId,
    /// Cosine similarity score in `[-1.0, 1.0]` (higher is more similar).
    pub score: f32,
}
