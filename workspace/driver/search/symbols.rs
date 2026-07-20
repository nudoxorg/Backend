//! Precise (tantivy) symbol search — the DEFAULT search surface.

#[allow(unused_imports)]
use crate::{registry};
use std::num::NonZeroUsize;

use futures::{Stream, StreamExt};
use heart::{Cursor, Enforced, Scored, Symbol, SymbolId, SymbolKind};
use registry::runtime::text::{TextIndex, TextQuery};

use heart::PageSpecification;

use crate::error::{BadRequestReason, ServerError};
use crate::search::query::{Filter, LiteralQuery};

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
		page: &PageSpecification,
	) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
		let limit = page_limit(page);
		let after = self.decode_cursor(page)?;
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
		page: &PageSpecification,
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
	pub async fn find(&self, id: SymbolId) -> Result<Option<Symbol>, ServerError> {
		self.index.find_by_id(id).await.map_err(|error| ServerError::Runtime(error.into()))
	}

	/// Decode the opaque resume token, if any, as an [`Enforced`] cursor.
	///
	/// The token's policy tag is verified (an advisory token is rejected), and
	/// the [`Enforced`] decode path re-checks the cursor's snapshot against the
	/// index's *live* snapshot right here — so obtaining the branded cursor
	/// re-proves freshness at decode time rather than trusting the erased brand.
	/// The text paginator performs the same snapshot comparison again downstream
	/// (a cheap defence-in-depth check against a commit racing this decode).
	fn decode_cursor(
		&self,
		page: &PageSpecification,
	) -> Result<Option<Cursor<registry::runtime::text::TextCursorKey, Enforced>>, ServerError> {
		let Some(token) = page.cursor.as_deref() else {
			return Ok(None);
		};
		let live = self
			.index
			.snapshot()
			.map_err(|error| ServerError::Runtime(error.into()))?;
		Cursor::<registry::runtime::text::TextCursorKey, Enforced>::decode(token, live)
			.map(Some)
			.map_err(|source| {
				ServerError::from(BadRequestReason::InvalidCursor {
					token: token.to_owned(),
					source,
				})
			})
	}
}

/// Whether a symbol's kind survives a kind allowlist (empty = unbounded).
pub fn kind_admits(kinds: &[SymbolKind], kind: SymbolKind) -> bool {
	kinds.is_empty() || kinds.contains(&kind)
}

/// The page size as the index's `NonZeroUsize` vocabulary.
fn page_limit(page: &PageSpecification) -> NonZeroUsize {
	NonZeroUsize::new(page.limit as usize).unwrap_or(NonZeroUsize::MIN)
}

