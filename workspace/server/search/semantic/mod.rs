//! The semantic (qdrant) search surface — explicitly gated.

pub mod embedder;

use std::num::NonZeroUsize;

use futures::Stream;
use heart::{Live, SymbolId, Scored};
use registry::vector::{
	EmbedRole, Embedder, Embedding, EmbeddingCache, EmbeddingKey, EmbeddingModel, EmbeddingPurpose,
	Semantic, SemanticGate,
};

use crate::error::ServerError;
use crate::search::query::{AbstractQuery, SymbolCursor};

pub struct SemanticSurface<'a, M: EmbeddingModel, E: Embedder<Model = M>> {
	store: &'a Semantic<M, Live>,
	embedder: &'a E,
	cache: &'a EmbeddingCache<M>,
}

impl<'a, M: EmbeddingModel, E: Embedder<Model = M>> SemanticSurface<'a, M, E> {
	/// Borrow a surface over one source's vector store, sharing the server-wide
	/// embedder + cache (embeddings are model-scoped, not source-scoped).
	pub fn new(
		store: &'a Semantic<M, Live>,
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
		use futures::StreamExt;

		tracing::debug!(reason = gate.reason(), "semantic search authorized");
		let embedding = self.embed(query.text(), Self::purpose(query)).await?;
		let hits = self
			.store
			.search(gate, &embedding, limit, after)
			.await
			.map_err(|error| ServerError::Runtime(error.into()))?;

		// The store's stream captures the borrowed query embedding (RPIT 2024
		// lifetime rules), so the limit-bounded page is drained here and re-yielded
		// as an owned stream.
		futures::pin_mut!(hits);
		let mut page = Vec::new();
		while let Some(hit) = hits.next().await {
			page.push(hit.map_err(|error| ServerError::Runtime(error.into()))?);
		}
		Ok(futures::stream::iter(page.into_iter().map(Ok)))
	}

	/// What the query text should be embedded *as*: prose reads against the
	/// documentation regime, code against the code regime.
	fn purpose(query: &AbstractQuery) -> EmbeddingPurpose {
		match query {
			AbstractQuery::NaturalLanguage(_) => EmbeddingPurpose::Documentation,
			AbstractQuery::CodeSnippet { .. } => EmbeddingPurpose::Code,
		}
	}

	/// Embed the query text, cache-first: identical text under the same model
	/// never pays the network round-trip twice.
	///
	/// Queries always use [`EmbedRole::Query`] so Voyage-class models select the
	/// query encoder path.
	async fn embed(
		&self,
		text: &str,
		purpose: EmbeddingPurpose,
	) -> Result<Embedding<M>, ServerError> {
		self.cache
			.get_or_embed(
				EmbeddingKey::new(M::id(), EmbedRole::Query, text),
				self.embedder,
				text,
				purpose,
			)
			.await
			.map_err(|error| ServerError::Runtime(error.into()))
	}
}
