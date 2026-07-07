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

use crate::{
	error::EmbedError,
	vector::{
		embedding::{Embedder, Embedding, EmbeddingPurpose},
		model::{EmbeddingModel, ModelId},
	},
};

/// The cache key: which model produced the vector, and the content hash of the
/// exact text that was embedded. The package snapshot deliberately is *not* part
/// of the key — identical text embeds identically regardless of which package
/// snapshot it came from, which is what makes cross-package dedupe work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingKey {
	/// The model that produced (or would produce) the vector.
	pub model:     ModelId,
	/// The BLAKE3 hash of the embedded text.
	pub text_hash: ContentHash,
}

impl EmbeddingKey {
	/// Build a key for `text` under `model`, hashing the text.
	pub fn new(model: ModelId, text: &str) -> Self {
		Self { model, text_hash: ContentHash::of_bytes(text.as_bytes()) }
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
		purpose: EmbeddingPurpose,
	) -> Result<Embedding<M>, EmbedError> {
		if let Some(hit) = self.inner.get(&key).await {
			tracing::debug!(model = %key.model, "embedding cache hit");
			return Ok(hit);
		}
		let embedding = embedder.embed(text, purpose).await?;
		self.inner.insert(key, embedding.clone()).await;
		Ok(embedding)
	}

	/// Best-effort peek without embedding — returns `None` on a miss.
	pub async fn get(&self, key: &EmbeddingKey) -> Option<Embedding<M>> {
		self.inner.get(key).await
	}
}
