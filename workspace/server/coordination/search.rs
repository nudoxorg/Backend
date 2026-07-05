//! The search/read flow: route a read request to the right surface and assemble
//! the response.

use futures::Stream;
use heart::{SymbolId, Scored, Sourced, Symbol};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerError;
use crate::search::planner::Plan;
use crate::search::query::Search;

impl<M: EmbeddingModel> Server<M> {
	pub async fn search_symbols<'a>(
		&'a self,
		request: &'a Search<'a>,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send + 'a, ServerError> {
		match self.planner().plan(request) {
			Plan::Precise => todo!("query each source's tantivy surface; merge overlay-over-base"),
			Plan::Semantic(_gate) => todo!("query each source's gated qdrant surface; merge overlay-over-base"),
		}
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}

	pub async fn resolve_symbol(
		&self,
		id: SymbolId,
	) -> Result<Option<Sourced<Symbol>>, ServerError> {
		let _ = id;
		for sourced in self.federation().in_precedence() {
			let _ = (sourced.source, sourced.role, &sourced.value.global_store);
		}
		todo!("federated resolve: overlay-override then definitive base")
	}

	pub async fn expand(
		&self,
		hit: &Scored<Symbol>,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let _ = hit;
		todo!("walk GraphStore relationships, score, merge into the caller's session")
	}

	fn planner(&self) -> &crate::search::SearchPlanner { todo!("hold a planner on the Server") }
}
