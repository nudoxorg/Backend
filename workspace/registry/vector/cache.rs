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

use crate::vector::{EmbedError, EmbedRole, Embedder, Embedding, EmbeddingModel, ModelId};

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
    pub model: ModelId,
    /// The retrieval role — query vs document encoding.
    pub role: EmbedRole,
    /// The BLAKE3 hash of the embedded text.
    pub text_hash: ContentHash,
}

impl EmbeddingKey {
    /// Build a key for `text` under `model` and `role`, hashing the text.
    pub fn new(model: ModelId, role: EmbedRole, text: &str) -> Self {
        Self {
            model,
            role,
            text_hash: ContentHash::of_bytes(text.as_bytes()),
        }
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
        Self {
            inner: Cache::new(capacity),
        }
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

    /// Resolve `texts` in order. Cache hits are not embedded. Identical misses
    /// share one vector. The embedder sees only the distinct misses, in chunks
    /// of its `max_batch`.
    pub async fn get_or_embed_batch<E: Embedder<Model = M>>(
        &self,
        embedder: &E,
        role: EmbedRole,
        texts: &[&str],
    ) -> Result<Vec<Embedding<M>>, EmbedError> {
        use std::collections::HashMap;

        let model = M::id();
        let mut slots: Vec<Option<Embedding<M>>> = Vec::with_capacity(texts.len());
        let mut missing: Vec<(EmbeddingKey, String)> = Vec::new();
        let mut missing_at: Vec<Option<usize>> = Vec::with_capacity(texts.len());
        let mut seen: HashMap<EmbeddingKey, usize> = HashMap::new();

        for text in texts {
            let key = EmbeddingKey::new(model.clone(), role, text);
            if let Some(hit) = self.inner.get(&key).await {
                slots.push(Some(hit));
                missing_at.push(None);
                continue;
            }
            slots.push(None);
            if let Some(&index) = seen.get(&key) {
                missing_at.push(Some(index));
                continue;
            }
            let index = missing.len();
            seen.insert(key.clone(), index);
            missing.push((key, (*text).to_owned()));
            missing_at.push(Some(index));
        }

        if !missing.is_empty() {
            let max_batch = embedder.runtime().max_batch.max(1);
            let mut embedded: Vec<Embedding<M>> = Vec::with_capacity(missing.len());
            for chunk in missing.chunks(max_batch) {
                let chunk_texts: Vec<&str> = chunk.iter().map(|(_, text)| text.as_str()).collect();
                let batch = embedder.embed_batch(&chunk_texts, role).await?;
                if batch.len() != chunk.len() {
                    return Err(EmbedError::Backend(format!(
                        "embed_batch returned {} vectors for {} texts",
                        batch.len(),
                        chunk.len()
                    )));
                }
                for ((key, _), embedding) in chunk.iter().zip(batch) {
                    self.inner.insert(key.clone(), embedding.clone()).await;
                    embedded.push(embedding);
                }
            }
            for (slot, miss) in slots.iter_mut().zip(missing_at) {
                if let Some(index) = miss {
                    *slot = Some(embedded[index].clone());
                }
            }
        }

        let mut resolved = Vec::with_capacity(slots.len());
        for slot in slots {
            let Some(embedding) = slot else {
                return Err(EmbedError::Backend(
                    "an embedding slot was left empty".to_owned(),
                ));
            };
            resolved.push(embedding);
        }
        Ok(resolved)
    }

    /// Best-effort peek without embedding — returns `None` on a miss.
    pub async fn get(&self, key: &EmbeddingKey) -> Option<Embedding<M>> {
        self.inner.get(key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{EmbeddingModel, JinaCodeV2};

    /// Verify that Query and Document roles produce distinct cache entries for
    /// identical text, so a query embedding never collides with a document
    /// embedding in the cache.
    #[test]
    fn embed_role_is_part_of_cache_key() {
        let model = JinaCodeV2::id();
        let text = "fn search(query: &str) -> Vec<Symbol>";

        let query_key = EmbeddingKey::new(model.clone(), EmbedRole::Query, text);
        let doc_key = EmbeddingKey::new(model, EmbedRole::Document, text);

        assert_ne!(
            query_key, doc_key,
            "Query and Document roles must produce distinct keys"
        );
    }

    #[test]
    fn same_text_same_role_same_key() {
        let model = JinaCodeV2::id();
        let text = "fn search(query: &str) -> Vec<Symbol>";

        let k1 = EmbeddingKey::new(model.clone(), EmbedRole::Query, text);
        let k2 = EmbeddingKey::new(model, EmbedRole::Query, text);
        assert_eq!(k1, k2);
    }

    /// A repeated miss is one embed. A text already cached is not embedded.
    /// The returned order matches the input, including the repeat.
    #[tokio::test]
    async fn batch_embeds_only_distinct_misses() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use async_trait::async_trait;

        use crate::vector::{EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding};

        struct Counting {
            calls: AtomicUsize,
            texts: AtomicUsize,
        }

        #[async_trait]
        impl Embedder for Counting {
            type Model = JinaCodeV2;

            async fn embed(
                &self,
                text: &str,
                role: EmbedRole,
            ) -> Result<Embedding<JinaCodeV2>, EmbedError> {
                let mut batch = self.embed_batch(&[text], role).await?;
                Ok(batch.pop().expect("one vector"))
            }

            async fn embed_batch(
                &self,
                texts: &[&str],
                _role: EmbedRole,
            ) -> Result<Vec<Embedding<JinaCodeV2>>, EmbedError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.texts.fetch_add(texts.len(), Ordering::SeqCst);
                texts
                    .iter()
                    .map(|text| {
                        let mut values = vec![0.0; JinaCodeV2::DIMENSIONS];
                        values[0] = 1.0;
                        values[1] = text.len() as f32;
                        Embedding::from_vec(values)
                    })
                    .collect()
            }

            fn runtime(&self) -> EmbedRuntimeInfo {
                EmbedRuntimeInfo {
                    model_id: JinaCodeV2::id(),
                    accel: crate::vector::AccelKind::Cpu,
                    durable_canonical: false,
                    max_batch: 8,
                    max_seq_len: 8,
                    weights_sha256: None,
                    ort_package_id: "counting".into(),
                }
            }
        }

        let cache = EmbeddingCache::<JinaCodeV2>::new(16);
        let embedder = Counting {
            calls: AtomicUsize::new(0),
            texts: AtomicUsize::new(0),
        };
        let cached = EmbeddingKey::new(JinaCodeV2::id(), EmbedRole::Document, "cached");
        cache
            .get_or_embed(cached, &embedder, "cached")
            .await
            .expect("seed");
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);

        let vectors = cache
            .get_or_embed_batch(
                &embedder,
                EmbedRole::Document,
                &["fresh", "cached", "fresh"],
            )
            .await
            .expect("batch");
        assert_eq!(vectors.len(), 3);
        assert_eq!(vectors[0], vectors[2]);
        assert_ne!(vectors[0], vectors[1]);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
        assert_eq!(embedder.texts.load(Ordering::SeqCst), 2);
    }
}
