//! Precise (tantivy) symbol search — the DEFAULT search surface.

use futures::Stream;
use heart::{Scored, Symbol};

use crate::error::ServerError;
use crate::search::query::{LiteralQuery, Pagination};

pub struct SymbolTextSurface {}

impl SymbolTextSurface {
	pub async fn search(
		&self,
		query: &LiteralQuery,
		page: &Pagination,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
		let _ = (query, page);
		todo!("delegate to runtime::text::TextSearch, map TextError -> ServerError");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}
}
