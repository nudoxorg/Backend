//! The semantic (qdrant) search surface — explicitly gated.

#[allow(unused_imports)]
use crate::{registry, vector};
pub mod embedder;

use std::cmp::Ordering;
use std::num::NonZeroUsize;

use futures::Stream;
use heart::{Score, Scored, SymbolId};
use registry::vector::{
	EmbedRole, Embedder, Embedding, EmbeddingCache, EmbeddingKey, EmbeddingModel, Payload,
	PayloadValue, SearchFilter, SearchHit, SearchRequest, SemanticGate, VectorStore,
};
use vector::remote::store::RemoteStore;

use crate::error::ServerError;
use crate::search::query::{AbstractQuery, SymbolCursor};

/// The deepest a keyset resume will over-fetch before truncating a page. Qdrant
/// cannot filter on the *computed* similarity score, so resuming past a cursor
/// means fetching from the top and skipping client-side (capped here).
const MAX_FETCH: usize = 4096;

pub struct SemanticSurface<'a, M: EmbeddingModel, E: Embedder<Model = M>> {
	store: &'a RemoteStore<M>,
	embedder: &'a E,
	cache: &'a EmbeddingCache<M>,
}

impl<'a, M: EmbeddingModel, E: Embedder<Model = M>> SemanticSurface<'a, M, E> {
	/// Borrow a surface over one source's vector store, sharing the server-wide
	/// embedder + cache (embeddings are model-scoped, not source-scoped).
	pub fn new(
		store: &'a RemoteStore<M>,
		embedder: &'a E,
		cache: &'a EmbeddingCache<M>,
	) -> Self {
		Self { store, embedder, cache }
	}

	pub async fn search(
		&self,
		gate: SemanticGate,
		query: &AbstractQuery,
		limit: NonZeroUsize,
		after: Option<SymbolCursor>,
	) -> Result<
		impl Stream<Item = Result<Scored<SymbolId>, ServerError>> + Send,
		ServerError,
	> {
		// The gate is the capability token: holding it authorizes this search.
		// The new `VectorStore::search` takes no gate, so the check is here —
		// the store is only reached once the gate is proven.
		tracing::debug!(reason = gate.reason(), "semantic search authorized");

		let embedding = self.embed(query.text()).await?;

		// Keyset resume over the `(score, id)` cursor. Qdrant returns best-first
		// but cannot resume from an arbitrary score, so when a cursor is present
		// we over-fetch (capped) and skip past its key client-side; without one
		// we fetch exactly the page.
		let target = limit.get();
		let after_key = after.as_ref().map(|cursor| cursor.after);
		let fetch = if after_key.is_some() { MAX_FETCH.max(target) } else { target };

		let request = SearchRequest {
			vector: embedding,
			filter: SearchFilter::default(),
			limit: fetch,
			// A resuming page prunes everything strictly below the cursor score
			// server-side; ties on the exact score are broken client-side by id.
			score_threshold: after_key.map(|(score, _)| score.into_inner()),
		};

		let hits = self
			.store
			.search(request)
			.await
			.map_err(|error| ServerError::from(error))?;

		// Map each hit back to a scored symbol identity via the payload
		// (`symbol_id`); the derived point id is not the symbol uuid.
		let mut scored: Vec<Scored<SymbolId>> = Vec::with_capacity(hits.len());
		for hit in hits {
			scored.push(scored_symbol(&hit)?);
		}

		// Best-first, with a stable id tiebreak so a page boundary is
		// deterministic across the score_threshold prune.
		scored.sort_by(|a, b| {
			b.score
				.cmp(&a.score)
				.then_with(|| a.value.cmp(&b.value))
		});

		// Drop everything at or before the cursor key, then bound to the page.
		if let Some((after_score, after_id)) = after_key {
			scored.retain(|hit| match hit.score.cmp(&after_score) {
				Ordering::Less => true,
				Ordering::Equal => hit.value > after_id,
				Ordering::Greater => false,
			});
		}
		scored.truncate(target);

		Ok(futures::stream::iter(scored.into_iter().map(Ok)))
	}

	/// Embed the query text, cache-first: identical text under the same model
	/// never pays the network round-trip twice.
	///
	/// Queries always use [`EmbedRole::Query`] so Voyage-class models select the
	/// query encoder path.
	async fn embed(&self, text: &str) -> Result<Embedding<M>, ServerError> {
		self.cache
			.get_or_embed(EmbeddingKey::new(M::id(), EmbedRole::Query, text), self.embedder, text)
			.await
			.map_err(|error| ServerError::from(error))
	}
}

/// Recover a [`Scored<SymbolId>`] from a search hit: the [`SymbolId`] comes from
/// the `symbol_id` payload key (the store point id is a derived UUID, not the
/// symbol uuid), and the backend score is validated finite.
fn scored_symbol(hit: &SearchHit) -> Result<Scored<SymbolId>, ServerError> {
	let raw = payload_str(&hit.payload, "symbol_id").ok_or_else(|| {
		ServerError::Internal(crate::error::InternalError::Other {
			message: format!("semantic hit {} missing symbol_id payload", hit.id),
		})
	})?;
	let uuid = raw.parse::<uuid::Uuid>().map_err(|error| {
		ServerError::Internal(crate::error::InternalError::Other {
			message: format!("semantic hit symbol_id {raw:?} is not a uuid: {error}"),
		})
	})?;
	let score = Score::try_new(hit.score).map_err(|_| {
		ServerError::Internal(crate::error::InternalError::Other {
			message: format!("semantic hit {} has a non-finite score", hit.id),
		})
	})?;
	Ok(Scored::new(SymbolId::from_uuid(uuid), score))
}

/// Read a string payload value by key, or `None` if absent / not a string.
fn payload_str<'p>(payload: &'p Payload, key: &str) -> Option<&'p str> {
	match payload.get(key)? {
		PayloadValue::Str(value) => Some(value.as_str()),
		_ => None,
	}
}
