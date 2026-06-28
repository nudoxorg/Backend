use serde::{Deserialize, Serialize};
use crate::{BlobRef, GlobalSymbolId, OccurrenceId};
use super::primitives::{ChunkMetadata, EmbeddingRecord, LibRef, SourceChunk, SymbolKind, SymbolOrigin};

/// The primary record stored per occurrence: all data about one symbol
/// occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobInfo {
	/// Unique identifier for this occurrence.
	pub occurrence_id:      OccurrenceId,
	/// Fully-qualified name of the symbol.
	pub symbol_name:        String,
	/// Where this symbol originates from.
	pub symbol_origin:      SymbolOrigin,
	/// Resolved global symbol identifier, if resolution has already occurred.
	pub resolved_global_id: Option<GlobalSymbolId>,
	/// Syntactic kind of this symbol, if known.
	#[serde(default)]
	pub kind:               Option<SymbolKind>,
	/// The extracted source chunk for this occurrence.
	pub source:             SourceChunk,
	/// All embedding vectors computed for this occurrence.
	pub embeddings:         Vec<EmbeddingRecord>,
	/// Contextual metadata about where and when this chunk was extracted.
	pub metadata:           ChunkMetadata,
}

/// The outcome of attempting to resolve a symbol occurrence to a global
/// identity.
#[derive(Debug, Clone)]
pub enum ResolutionOutcome {
	/// The symbol was successfully resolved to a known global identity.
	Resolved {
		/// The resolved global symbol identifier.
		global_id: GlobalSymbolId,
		/// Reference to the stored blob for this occurrence.
		blob_ref:  BlobRef,
	},
	/// Resolution could not be completed because the library has not been parsed
	/// yet.
	Deferred {
		/// The library whose parse is pending.
		lib:      LibRef,
		/// Reference to the stored blob to revisit after the library is parsed.
		blob_ref: BlobRef,
	},
}

/// Summary statistics from a resolve-library pass.
#[derive(Debug, Clone)]
pub struct ResolveLibReport {
	/// Total number of blobs examined during the pass.
	pub blobs_seen:     usize,
	/// Number of blobs successfully resolved.
	pub blobs_resolved: usize,
	/// Number of blobs skipped (e.g. already resolved or unresolvable).
	pub blobs_skipped:  usize,
}
