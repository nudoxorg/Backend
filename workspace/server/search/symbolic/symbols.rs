//! Precise (tantivy) symbol search — the DEFAULT search surface.

use std::num::NonZeroUsize;

use futures::{Stream, StreamExt, TryStreamExt};
use heart::{Cursor, Scored, Symbol, SymbolId, SymbolKind};
use runtime::text::{TextIndex, TextQuery};

use crate::error::ServerError;
use crate::search::query::{Filter, LiteralQuery, Pagination};

/// The thin adapter from the server's query vocabulary onto one replica-local
/// [`TextIndex`], owning the `TextError` → [`ServerError`] translation.
pub struct SymbolTextSurface<'a> {
	index: &'a TextIndex,
}

impl<'a> SymbolTextSurface<'a> {
	/// Borrow a surface over one source's text index.
	pub fn over(index: &'a TextIndex) -> Self { Self { index } }

	pub async fn search(
		&self,
		query: &LiteralQuery,
		page: &Pagination,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
		let limit = page_limit(page);
		let after = decode_cursor(page)?;
		let text_query = TextQuery::new(query.as_str());

		// The index's stream captures the borrowed query (RPIT 2024 lifetime
		// rules), so the limit-bounded page is drained here and re-yielded owned.
		let hits = self.index.search(&text_query, limit, after);
		futures::pin_mut!(hits);
		let mut page = Vec::new();
		while let Some(hit) = hits.next().await {
			page.push(hit.map_err(|error| ServerError::Runtime(error.into()))?);
		}
		Ok(futures::stream::iter(page.into_iter().map(Ok)))
	}

	/// One materialized, filter-checked page — what the federated merge consumes.
	pub(crate) async fn collect(
		&self,
		query: &LiteralQuery,
		page: &Pagination,
		filter: &Filter,
	) -> Result<Vec<Scored<Symbol>>, ServerError> {
		let limit = page_limit(page).get();
		let hits = self.search(query, page).await?;
		futures::pin_mut!(hits);

		let mut collected = Vec::new();
		while let Some(hit) = hits.next().await {
			let hit = hit?;
			if filter.admits(&hit.value) {
				collected.push(hit);
			}
			if collected.len() == limit {
				break;
			}
		}
		Ok(collected)
	}

	/// Fetch one symbol by its durable id.
	///
	/// The text index exposes no term-level get, so this issues a literal search
	/// on the id's canonical string (the id is an indexed field) and confirms the
	/// exact id on the way out — the closest join available today.
	pub async fn find(&self, id: SymbolId) -> Result<Option<Symbol>, ServerError> {
		let query = TextQuery::new(id.to_string());
		let limit = NonZeroUsize::new(8).expect("eight is non-zero");
		let hits = self.index.search(&query, limit, None);
		futures::pin_mut!(hits);
		while let Some(hit) = hits.next().await {
			let hit = hit.map_err(|error| ServerError::Runtime(error.into()))?;
			if hit.value.id == id {
				return Ok(Some(hit.value));
			}
		}
		Ok(None)
	}
}

/// Whether a symbol's kind survives a kind allowlist (empty = unbounded).
pub fn kind_admits(kinds: &[SymbolKind], kind: SymbolKind) -> bool {
	kinds.is_empty() || kinds.contains(&kind)
}

/// The page size as the index's `NonZeroUsize` vocabulary.
fn page_limit(page: &Pagination) -> NonZeroUsize {
	NonZeroUsize::new(page.limit.get() as usize).unwrap_or(NonZeroUsize::MIN)
}

/// Decode the opaque resume token, if any.
fn decode_cursor(
	page: &Pagination,
) -> Result<Option<Cursor<runtime::text::TextCursorKey>>, ServerError> {
	page.after
		.as_deref()
		.map(|token| {
			Cursor::decode(token)
				.map_err(|error| ServerError::BadRequest(format!("invalid cursor: {error}")))
		})
		.transpose()
}
