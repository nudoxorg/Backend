//! The semantic (qdrant) search surface — explicitly gated.
//!
//! Reachable only with a [`SemanticGate`] minted by the [`crate::search::planner`].
//! This surface embeds the query (via the configured [`Embedder`]), consulting
//! the embedding cache, then streams nearest neighbours from qdrant. It is never
//! the default and never invoked implicitly by the precise path.

use std::num::NonZeroUsize;

use futures::Stream;
use heart::{AccessContext, SymbolId, Scored};
use runtime::vector::{Embedder, Embedding, EmbeddingModel, EmbeddingPurpose, SemanticGate};

use crate::error::ServerError;
use crate::search::query::{AbstractQuery, SymbolCursor};

/// The gated semantic surface, generic over the embedding-model brand `M` and an
/// embedder that produces vectors for that same model.
pub struct SemanticSurface<'a, M: EmbeddingModel, E: Embedder<Model = M>> {
	// TODO: borrowed Semantic<M, Live> + embedder + cache from the Server.
	_marker: std::marker::PhantomData<(&'a E, fn() -> M)>,
}

impl<'a, M: EmbeddingModel, E: Embedder<Model = M>> SemanticSurface<'a, M, E> {
	/// Run a gated semantic search. The `gate` is consumed — it cannot be reused.
	/// The query is embedded (cache-first), then top-`limit` neighbours stream
	/// back, keyset-paged and access-scoped.
	pub async fn search(
		&self,
		gate: SemanticGate,
		query: &AbstractQuery,
		limit: NonZeroUsize,
		scope: &AccessContext,
		after: Option<SymbolCursor>,
	) -> Result<
		impl Stream<Item = Result<Scored<SymbolId>, ServerError>> + Send,
		ServerError,
	> {
		let _ = (gate, query, limit, scope, after, EmbeddingPurpose::Code);
		todo!("shape query text, embed (cache-first), Semantic::search, map errors");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}

	/// The purpose an abstract query embeds under.
	fn purpose(_query: &AbstractQuery) -> EmbeddingPurpose { EmbeddingPurpose::Code }

	/// Embed a query string, checking the cache first.
	async fn embed(&self, _text: &str) -> Result<Embedding<M>, ServerError> {
		todo!("cache.get_or_embed(model, hash(text), embedder, text, purpose)")
	}
}
