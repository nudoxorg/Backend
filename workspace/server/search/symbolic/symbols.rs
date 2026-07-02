//! Precise (tantivy) symbol search — the DEFAULT search surface.
//!
//! Delegates to the replica-local [`runtime::text`] index. This is what a bare
//! query hits; the semantic path is only taken when the planner engages it.

use futures::Stream;
use heart::{AccessContext, Scored, Symbol};

use crate::error::ServerError;
use crate::search::query::{LiteralQuery, Page};

/// The default precise-search surface over symbols.
pub struct SymbolTextSurface {
	// TODO: borrowed handle to runtime::text (TextSearch impl) from the Server.
}

impl SymbolTextSurface {
	/// Precise search over symbol names/signatures, streaming scored symbols,
	/// keyset-paged and access-scoped.
	pub async fn search(
		&self,
		query: &LiteralQuery,
		page: &Page,
		scope: &AccessContext,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
		let _ = (query, page, scope);
		todo!("delegate to runtime::text::TextSearch, map TextError -> ServerError");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}
}
