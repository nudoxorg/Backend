//! The search/read flow: route a read request to the right surface and assemble
//! the response.
//!
//! The planner decides precise-vs-semantic and mints the [`SemanticGate`] only
//! when the heavy path is authorized; this flow never engages semantic search
//! implicitly.

use futures::Stream;
use heart::{AccessContext, GlobalSymbolId, Scored, Sourced, Symbol};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerError;
use crate::search::planner::Plan;
use crate::search::query::Search;

impl<M: EmbeddingModel> Server<M> {
	/// Answer a symbol search: plan it, dispatch to precise or (gated) semantic,
	/// query every federated source, and merge with overlay-override precedence.
	/// Access-scoped and keyset-paged throughout.
	pub async fn search_symbols<'a>(
		&'a self,
		request: &'a Search<'a>,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send + 'a, ServerError> {
		match self.planner().plan(request, request.scope) {
			// Both arms fan the query across `self.federation().in_precedence()`,
			// then merge so an overlay hit for a symbol shadows the base's.
			Plan::Precise => todo!("query each source's tantivy surface; merge overlay-over-base"),
			Plan::Semantic(_gate) => todo!("query each source's gated qdrant surface; merge overlay-over-base"),
		}
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}

	/// Resolve a single symbol across the federation with **override semantics**:
	/// walk sources in precedence order (overlays first, then the definitive
	/// base) and return the first that has it, tagged with which source provided
	/// it — so callers can tell an overlay override from a definitive record.
	pub async fn resolve_symbol(
		&self,
		id: GlobalSymbolId,
		ctx: &AccessContext,
	) -> Result<Option<Sourced<Symbol>>, ServerError> {
		let _ = (id, ctx);
		for (source_id, role, stores) in self.federation().in_precedence() {
			// The first source (highest precedence) that has `id` wins; an overlay
			// therefore shadows the base. Access is checked per source.
			let _ = (source_id, role, &stores.global_store);
			// if let Some(sym) = stores.lookup(id, ctx).await? { return Ok(Some(Sourced::new(sym, source_id, role))); }
		}
		todo!("federated resolve: overlay-override then definitive base")
	}

	/// Expand from a hit through graph relationships (session-driven exploration).
	pub async fn expand(
		&self,
		hit: &Scored<Symbol>,
		ctx: &AccessContext,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let _ = (hit, ctx);
		todo!("walk GraphStore relationships, score, merge into the caller's session")
	}

	/// The (lazily-built) search planner for this server.
	fn planner(&self) -> &crate::search::SearchPlanner { todo!("hold a planner on the Server") }
}
