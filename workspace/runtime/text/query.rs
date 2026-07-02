//! Querying the tantivy text index — the default interface for searching through
//! items by name/signature.
//!
//! Search is CPU-bound and synchronous, so it runs on `spawn_blocking`. Results
//! stream as [`Scored`] symbols, paginated by an opaque keyset [`Cursor`] and
//! access-scoped.

use std::num::NonZeroUsize;

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, Cursor, Ecosystem, Scored, Symbol, SymbolKind};

use crate::{
	error::TextError,
	text::{index::TextIndex, TextCursorKey},
};

/// A parsed text query: the raw terms plus optional structured filters that
/// narrow the result set before ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextQuery {
	/// The user's raw query text (matched against name + fq-name).
	pub terms: String,
	/// Restrict to a single ecosystem, if set.
	pub ecosystem: Option<Ecosystem>,
	/// Restrict to a set of symbol kinds, if non-empty.
	pub kinds: Vec<SymbolKind>,
}

impl TextQuery {
	/// A bare term query with no filters.
	pub fn new(terms: impl Into<String>) -> Self {
		Self { terms: terms.into(), ecosystem: None, kinds: Vec::new() }
	}
}

/// The searchable view over a [`TextIndex`]: the default query surface.
pub trait TextSearch: Send + Sync {
	/// The failure mode of a text search.
	type Error;

	/// Search the index, returning up to `limit` [`Scored`] symbols ranked by
	/// relevance, streamed. `after` resumes a previous page via the keyset
	/// cursor; results are access-scoped.
	fn search(
		&self,
		query: &TextQuery,
		limit: NonZeroUsize,
		scope: &AccessContext,
		after: Option<Cursor<TextCursorKey>>,
	) -> impl Stream<Item = Result<Scored<Symbol>, Self::Error>> + Send;
}

impl TextSearch for TextIndex {
	type Error = TextError;

	fn search(
		&self,
		query: &TextQuery,
		limit: NonZeroUsize,
		scope: &AccessContext,
		after: Option<Cursor<TextCursorKey>>,
	) -> impl Stream<Item = Result<Scored<Symbol>, Self::Error>> + Send {
		let _ = (query, limit, scope, after);
		// runs on spawn_blocking: parse the query, run the tantivy search from the
		// keyset position, access-filter, and stream the page.
		todo!("spawn_blocking tantivy search from the cursor keyset, access-filtered, streamed");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
