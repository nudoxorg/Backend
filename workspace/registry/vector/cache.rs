//! A content-addressed embedding cache.
//!
//! Embedding is the expensive, rate-limited step. Two facts make it cacheable:
//! the *same text under the same model* always yields the same vector, and the
//! same symbol text recurs constantly — across re-parses of a package and
//! across duplicate symbols in different packages (re-exports, vendored copies,
//! identical prelude items). Keying on `(ModelId, ContentHash)` — where the
//! hash is [`ContentHash::of_bytes`] over the exact text that was embedded —
//! lets us serve those hits without a second network call.
//!
//! Backed by [`moka`]'s async cache: bounded, concurrent, TTL-capable.

use heart::ContentHash;
use moka::future::Cache;

use vector_core::{EmbedError, EmbedRole, Embedder, Embedding, EmbeddingModel, ModelId};

/// The cache key: which model produced the vector, the retrieval role, and the
/// content hash of the exact text that was embedded.
///
/// `role` is part of the key because Voyage-class models produce different
/// vectors for the same text depending on whether it is a query or a document.
/// The package snapshot is deliberately *not* part of the key — identical text
/// embeds identically regardless of which package it came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingKey {
	/// The model that produced (or would produce) the vector.
	pub model:     ModelId,
	/// The retrieval role — query vs document encoding.
	pub role:      EmbedRole,
	/// The BLAKE3 hash of the embedded text.
	pub text_hash: ContentHash,
}

impl EmbeddingKey {
	/// Build a key for `text` under `model` and `role`, hashing the text.
	pub fn new(model: ModelId, role: EmbedRole, text: &str) -> Self {
		Self { model, role, text_hash: ContentHash::of_bytes(text.as_bytes()) }
	}
}

/// A `moka`-backed cache from [`EmbeddingKey`] to `Embedding<DIM>`, so
/// re-parses and cross-package duplicate symbols never re-embed.
pub struct EmbeddingCache<M: EmbeddingModel> {
	inner: Cache<EmbeddingKey, Embedding<M>>,
}

impl<M: EmbeddingModel> EmbeddingCache<M> {
	/// Create a cache holding up to `capacity` entries (LRU/TinyLFU eviction).
	pub fn new(capacity: u64) -> Self {
		Self { inner: Cache::new(capacity) }
	}

	/// Look up the embedding for `text` under `embedder`'s model; on a miss,
	/// embed it (fallibly), populate the cache, and return the result.
	///
	/// `key` is passed explicitly so callers that already computed the text hash
	/// (e.g. from the symbol record) avoid re-hashing.
	///
	/// Deliberately get-then-insert rather than moka's coalesced `try_get_with`:
	/// that API surfaces errors as `Arc<E>`, and [`EmbedError`] carries non-Clone
	/// sources. Two racing misses may embed the same text twice — harmless,
	/// since embedding is deterministic and last-write-wins stores equal values.
	pub async fn get_or_embed<E: Embedder<Model = M>>(
		&self,
		key: EmbeddingKey,
		embedder: &E,
		text: &str,
	) -> Result<Embedding<M>, EmbedError> {
		if let Some(hit) = self.inner.get(&key).await {
			tracing::debug!(model = %key.model, role = ?key.role, "embedding cache hit");
			return Ok(hit);
		}
		let role = key.role;
		let embedding = embedder.embed(text, role).await?;
		self.inner.insert(key, embedding.clone()).await;
		Ok(embedding)
	}

	/// Best-effort peek without embedding — returns `None` on a miss.
	pub async fn get(&self, key: &EmbeddingKey) -> Option<Embedding<M>> {
		self.inner.get(key).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use vector_core::{EmbeddingModel, JinaCodeV2};

	/// Verify that Query and Document roles produce distinct cache entries for
	/// identical text, so a query embedding never collides with a document
	/// embedding in the cache.
	#[test]
	fn embed_role_is_part_of_cache_key() {
		let model = JinaCodeV2::id();
		let text = "fn search(query: &str) -> Vec<Symbol>";

		let query_key = EmbeddingKey::new(model.clone(), EmbedRole::Query, text);
		let doc_key = EmbeddingKey::new(model, EmbedRole::Document, text);

		assert_ne!(query_key, doc_key, "Query and Document roles must produce distinct keys");
	}

	#[test]
	fn same_text_same_role_same_key() {
		let model = JinaCodeV2::id();
		let text = "fn search(query: &str) -> Vec<Symbol>";

		let k1 = EmbeddingKey::new(model.clone(), EmbedRole::Query, text);
		let k2 = EmbeddingKey::new(model, EmbedRole::Query, text);
		assert_eq!(k1, k2);
	}
}
