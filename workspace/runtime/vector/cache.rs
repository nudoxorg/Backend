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
		let _ = capacity;
		todo!("build a moka::future::Cache with the given max capacity")
	}

	/// Look up the embedding for `text` under `embedder`'s model; on a miss,
	/// embed it (fallibly), populate the cache, and return the result.
	///
	/// `key` is passed explicitly so callers that already computed the text hash
	/// (e.g. from the symbol record) avoid re-hashing.
	pub async fn get_or_embed<E: Embedder<Model = M>>(
		&self,
		key: EmbeddingKey,
		embedder: &E,
		text: &str,
		purpose: EmbeddingPurpose,
	) -> Result<Embedding<M>, EmbedError> {
		let _ = (&self.inner, key, embedder, text, purpose);
		todo!("cache get_with: on miss call embedder.embed(text, purpose), store, return")
	}

	/// Best-effort peek without embedding — returns `None` on a miss.
	pub async fn get(&self, key: &EmbeddingKey) -> Option<Embedding<M>> {
		let _ = (&self.inner, key);
		todo!("cache.get(key)")
	}
}
