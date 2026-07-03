//! Per-ecosystem embedding-text shaping and language-scoped similarity.
//!
//! What you feed the embedder matters: raw source text embeds worse than a
//! normalized, decorated form (signature + doc + fully-qualified name, comments
//! stripped or kept per purpose). The shaping is ecosystem-specific — Rust,
//! TypeScript, and Python have different notions of "the signature" — so it is
//! isolated behind [`EmbeddingText`], which the indexer calls before embedding.
//!
//! Language *scoping* of a query (restricting hits to one ecosystem) is a
//! payload-level filter on the single shared collection, not a separate index.

use std::num::NonZeroUsize;

use heart::{AccessContext, Language, SymbolId, Scored, Symbol};

use crate::{
	error::VectorError,
	vector::{
		Embedding, SemanticLive,
		embedding::EmbeddingPurpose,
		gate::SemanticGate,
		model::EmbeddingModel,
	},
};

/// Shapes a [`Symbol`]'s durable fields into the exact text handed to the
/// embedder, per ecosystem and per [`EmbeddingPurpose`]. The output is what the
/// cache hashes, so it must be deterministic and stable.
pub trait EmbeddingText: Send + Sync {
	/// The ecosystem this shaper knows how to render.
	fn ecosystem(&self) -> Language;

	/// Produce the canonical embedding text for `symbol` under `purpose`. Must be
	/// deterministic: the same symbol always yields byte-identical text, so its
	/// content hash (the cache key) is stable across re-parses.
	fn shape(&self, symbol: &Symbol, purpose: EmbeddingPurpose) -> String;
}

/// Semantic search restricted to a single `ecosystem` via a payload filter on
/// the shared collection (no per-language collections).
pub async fn similar_within<M: EmbeddingModel>(
	store: &SemanticLive<M>,
	gate: SemanticGate,
	ecosystem: Language,
	query: &Embedding<M>,
	limit: NonZeroUsize,
	scope: &AccessContext,
) -> Result<Vec<Scored<SymbolId>>, VectorError> {
	let _ = (store, gate, ecosystem, query, limit, scope);
	todo!("k-NN search with a payload filter pinning ecosystem == the given one")
}
