//! The read-only search surfaces and the traits over them.
//!
//! Everything here is read-plane. A [`SearchTarget`] is any store answering a
//! [`Search`] with a *stream* of [`Scored`] hits (never a materialized `Vec`);
//! a [`SymbolStore`] additionally walks graph relationships. Streams are
//! keyset-paginated via the query's [`query::Page`] cursor.

pub mod planner;
pub mod query;
pub mod registry;
pub mod semantic;
pub mod symbolic;

use std::collections::HashSet;
use std::num::NonZeroUsize;

use futures::Stream;
use heart::{SymbolId, Scored, StoreError, Symbol};
use runtime::graph::{GraphStore, RelationKind, expansion::ExpansionBounds};
use runtime::vector::EmbeddingModel;

pub use planner::SearchPlanner;
pub use query::{AbstractQuery, Filter, Pagination, Query, Search, SymbolCursor};

use crate::SourceStores;
use crate::error::{ServerError, ServerResult};
use crate::search::symbolic::symbols::SymbolTextSurface;

/// A store/target that answers searches with a stream of scored results.
pub trait SearchTarget {
	/// What this target yields.
	type Item;

	/// Per-implementor failure mode.
	type Error: StoreError;

	/// Run a search, streaming scored results (keyset-paginated via the request's
	/// `page`). The stream is the contract — results are never collected here.
	async fn search(
		&self,
		request: &Search<'_>,
	) -> Result<impl Stream<Item = Result<Scored<Self::Item>, Self::Error>> + Send, Self::Error>;

	/// Fetch a single item by its durable global id.
	async fn get_by_id(&self, id: SymbolId) -> Result<Option<Self::Item>, Self::Error>;
}

/// A store of symbols supporting both precise and (gated) semantic search, plus
/// graph-relationship expansion. This is what the server's symbol-search surface
/// is generic over.
pub trait SymbolStore: SearchTarget<Item = Symbol> + GraphStore {
	/// Given a hit, walk its graph relationships and score the related symbols —
	/// the "expand from here" operation that powers session-based exploration.
	async fn related_hits(
		&self,
		hit: &Scored<Symbol>,
	) -> Result<Vec<Scored<Symbol>>, <Self as SearchTarget>::Error>;
}

/// Fold per-source result groups — supplied in federation **precedence order**
/// (overlays first, then the definitive base) — into one ranked page. The first
/// source to claim a key wins (overlay-override), then everything ranks by
/// score, bounded by `limit`.
pub fn merge_overlay_first<T, K>(
	groups: impl IntoIterator<Item = Vec<Scored<T>>>,
	key: impl Fn(&T) -> K,
	limit: usize,
) -> Vec<Scored<T>>
where
	K: Eq + std::hash::Hash,
{
	let mut claimed = HashSet::new();
	let mut merged = Vec::new();
	for group in groups {
		for hit in group {
			if claimed.insert(key(&hit.value)) {
				merged.push(hit);
			}
		}
	}
	merged.sort_by(|left, right| right.score.cmp(&left.score));
	merged.truncate(limit);
	merged
}

/// How far a single expand call walks: immediate neighbours, a bounded fan.
const EXPANSION_BOUNDS: ExpansionBounds = ExpansionBounds {
	depth: NonZeroUsize::new(1).expect("one is non-zero"),
	breadth: NonZeroUsize::new(16).expect("sixteen is non-zero"),
};

impl<M: EmbeddingModel> SourceStores<M> {
	/// Look one symbol up by its durable id within this source.
	pub async fn symbol_by_id(&self, id: SymbolId) -> ServerResult<Option<Symbol>> {
		SymbolTextSurface::over(&self.text).find(id).await
	}
}

// ── One federated source is itself a full symbol store: precise search over its
// text index, identity lookup, and graph walking. The server's federation logic
// composes these per-source stores overlay-first.

impl<M: EmbeddingModel> SearchTarget for SourceStores<M> {
	type Item = Symbol;
	type Error = ServerError;

	async fn search(
		&self,
		request: &Search<'_>,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
		// Whatever surface the request planned for, *this* target is the precise
		// one: an abstract query degrades to its raw text here.
		let literal = match &request.query {
			Query::Literal(literal) => literal.clone(),
			Query::Abstract(query) => query::LiteralQuery::parse(query.text())
				.map_err(|error| ServerError::BadRequest(error.to_string()))?,
		};
		let surface = SymbolTextSurface::over(&self.text);
		let hits = surface.collect(&literal, &request.page, &request.filter).await?;
		Ok(futures::stream::iter(hits.into_iter().map(Ok)))
	}

	async fn get_by_id(&self, id: SymbolId) -> Result<Option<Symbol>, ServerError> {
		self.symbol_by_id(id).await
	}
}

impl<M: EmbeddingModel> GraphStore for SourceStores<M> {
	type Error = runtime::error::GraphError;

	fn get_occurrences(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		self.graph.get_occurrences(item)
	}

	fn get_references(
		&self,
		item: SymbolId,
	) -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send {
		self.graph.get_references(item)
	}

	async fn are_related(
		&self,
		from: SymbolId,
		to: SymbolId,
	) -> Result<Option<RelationKind>, Self::Error> {
		self.graph.are_related(from, to).await
	}
}

impl<M: EmbeddingModel> SymbolStore for SourceStores<M> {
	async fn related_hits(
		&self,
		hit: &Scored<Symbol>,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		use futures::StreamExt;

		let edges = self.graph.expand(hit.value.id, EXPANSION_BOUNDS);
		futures::pin_mut!(edges);

		let mut related = Vec::new();
		let mut visited = HashSet::from([hit.value.id]);
		while let Some(edge) = edges.next().await {
			let edge = match edge {
				Ok(edge) => edge,
				// A symbol absent from this source's graph is an empty
				// neighbourhood, not a failure.
				Err(runtime::error::GraphError::NotFound) => break,
				Err(error) => return Err(ServerError::Runtime(error.into())),
			};
			if !visited.insert(edge.target.value) {
				continue;
			}
			if let Some(symbol) = self.symbol_by_id(edge.target.value).await? {
				related.push(Scored::new(symbol, edge.target.score));
			}
		}
		related.sort_by(|left, right| right.score.cmp(&left.score));
		Ok(related)
	}
}
