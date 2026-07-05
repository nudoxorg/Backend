//! The semantic (qdrant) search surface — explicitly gated.

use std::num::NonZeroUsize;

use futures::Stream;
use heart::{SymbolId, Scored};
use runtime::vector::{Embedder, Embedding, EmbeddingModel, EmbeddingPurpose, SemanticGate};

use crate::error::ServerError;
use crate::search::query::{AbstractQuery, SymbolCursor};

pub struct SemanticSurface<'a, M: EmbeddingModel, E: Embedder<Model = M>> {
	_marker: std::marker::PhantomData<(&'a E, fn() -> M)>,
}

impl<'a, M: EmbeddingModel, E: Embedder<Model = M>> SemanticSurface<'a, M, E> {
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
		let _ = (gate, query, limit, after, EmbeddingPurpose::Code);
		todo!("shape query text, embed (cache-first), Semantic::search, map errors");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}

	fn purpose(_query: &AbstractQuery) -> EmbeddingPurpose { EmbeddingPurpose::Code }

	async fn embed(&self, _text: &str) -> Result<Embedding<M>, ServerError> {
		todo!("cache.get_or_embed(model, hash(text), embedder, text, purpose)")
	}
}
