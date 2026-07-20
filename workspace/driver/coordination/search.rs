//! The search/read flow: route a read request to the right surface and assemble
//! the response.

#[allow(unused_imports)]
use crate::{registry};
use std::num::NonZeroUsize;

use futures::{Stream, StreamExt};
use heart::{SymbolId, Scored, Sourced, Symbol};

use registry::vector::EmbeddingModel;
use crate::Server;
use crate::authz::ReadCap;
use crate::error::{BadRequestReason, InternalError, ServerError};
use crate::search::planner::Plan;
use crate::search::query::{Query, Search};
use crate::search::semantic::SemanticSurface;
use crate::search::{SearchTarget, SearchPlanner, SymbolStore, merge_overlay_first};

impl<M: EmbeddingModel> Server<M> {
	/// Search for symbols. The caller must hold a [`ReadCap`] proving that
	/// authorization has already occurred at the HTTP boundary.
	pub async fn search_symbols<'a>(
		&'a self,
		_cap: &ReadCap,
		request: &'a Search<'a>,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send + 'a, ServerError> {
		let hits = match self.planner().plan(request) {
			Plan::Precise => self.precise_hits(request).await?,
			Plan::Semantic(gate) => self.semantic_hits(gate, request).await?,
		};
		Ok(futures::stream::iter(hits.into_iter().map(Ok)))
	}

	/// Resolve a symbol by id. The caller must hold a [`ReadCap`].
	pub async fn resolve_symbol(
		&self,
		_cap: &ReadCap,
		id: SymbolId,
	) -> Result<Option<Sourced<Symbol>>, ServerError> {
		for sourced in self.federation().in_precedence() {
			if let Some(symbol) = sourced.value.symbol_by_id(id).await? {
				return Ok(Some(sourced.map(|_| symbol)));
			}
		}
		Ok(None)
	}

	/// Expand graph neighbours of a symbol. The caller must hold a [`ReadCap`].
	pub async fn expand(
		&self,
		_cap: &ReadCap,
		hit: &Scored<Symbol>,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let mut groups = Vec::new();
		for sourced in self.federation().in_precedence() {
			groups.push(sourced.value.related_hits(hit).await?);
		}
		Ok(merge_overlay_first(groups, |symbol| symbol.id, usize::MAX))
	}

	fn planner(&self) -> &crate::search::SearchPlanner { &self.planner }

	/// The precise (tantivy) arm: query every source's text surface, merge
	/// overlay-over-base, rank by score, bound by the requested page.
	async fn precise_hits(&self, request: &Search<'_>) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let mut groups = Vec::new();
		for sourced in self.federation().in_precedence() {
			let hits = sourced.value.search(request).await?;
			futures::pin_mut!(hits);
			let mut collected = Vec::new();
			while let Some(hit) = hits.next().await {
				collected.push(hit?);
			}
			groups.push(collected);
		}
		Ok(merge_overlay_first(groups, |symbol| symbol.id, page_limit(request)))
	}

	/// The gated semantic (qdrant) arm: embed the query once (cache-first), fan
	/// the planner's authorization across the federation, hydrate the returned
	/// ids into symbols, and merge overlay-over-base.
	async fn semantic_hits(
		&self,
		gate: registry::vector::SemanticGate,
		request: &Search<'_>,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let Query::Abstract(query) = &request.query else {
			// The planner never plans a literal query semantically.
			return Err(InternalError::PlannerInvariantSemanticForLiteral.into());
		};
		let limit = NonZeroUsize::new(page_limit(request)).unwrap_or(NonZeroUsize::MIN);
		let after = request
			.page
			.cursor
			.as_deref()
			.map(|token| {
				heart::Cursor::<_, heart::Advisory>::decode(token)
					.map_err(|source| BadRequestReason::InvalidCursor {
				token: token.to_owned(),
				source,
			})
			})
			.transpose()?;

		// One planner issuance authorizes one user-visible query; fanning it out
		// across the federation re-issues per source under the same audit reason.
		let reason = gate.reason();
		let mut authorization = Some(gate);
		let mut groups = Vec::new();
		for sourced in self.federation().in_precedence() {
			let gate = authorization
				.take()
				.unwrap_or_else(|| SearchPlanner::extend_across_federation(reason));
			let surface = SemanticSurface::new(
				&sourced.value.semantics,
				&self.embedder,
				&self.embedding_cache,
			);
			let identities = surface.search(gate, query, limit, after.clone()).await?;
			futures::pin_mut!(identities);

			let mut collected = Vec::new();
			while let Some(scored_identity) = identities.next().await {
				let scored_identity = scored_identity?;
				if let Some(symbol) = sourced.value.symbol_by_id(scored_identity.value).await?
					&& request.filter.admits(&symbol) {
						collected.push(Scored::new(symbol, scored_identity.score));
					}
			}
			groups.push(collected);
		}
		Ok(merge_overlay_first(groups, |symbol| symbol.id, page_limit(request)))
	}
}

/// The requested page size, widened to the merge vocabulary.
fn page_limit(request: &Search<'_>) -> usize { request.page.limit as usize }
