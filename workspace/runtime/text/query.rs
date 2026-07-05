//! Querying the tantivy text index — the default interface for searching through
//! items by name/signature.

use std::num::NonZeroUsize;

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{Cursor, Language, Scored, Symbol, SymbolKind};

use crate::{
	error::TextError,
	text::{index::TextIndex, TextCursorKey},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextQuery {
	pub terms: String,
	pub ecosystem: Option<Language>,
	pub kinds: Vec<SymbolKind>,
}

impl TextQuery {
	pub fn new(terms: impl Into<String>) -> Self {
		Self { terms: terms.into(), ecosystem: None, kinds: Vec::new() }
	}
}

impl TextIndex {
	pub fn search(
		&self,
		query: &TextQuery,
		limit: NonZeroUsize,
		after: Option<Cursor<TextCursorKey>>,
	) -> impl Stream<Item = Result<Scored<Symbol>, TextError>> + Send {
		let _ = (query, limit, after);
		todo!("spawn_blocking tantivy search from the cursor keyset, streamed");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
