//! Querying the tantivy text index — the default interface for searching through
//! items by name/signature.

use std::num::NonZeroUsize;

use futures::{Stream, TryFutureExt};
use serde::{Deserialize, Serialize};

use heart::{Cursor, Language, Score, Scored, Symbol, SymbolKind};

use tantivy::{
	IndexReader, TantivyDocument,
	collector::TopDocs,
	query::{BooleanQuery, Occur, Query, RegexQuery, TermQuery},
	schema::{Field, IndexRecordOption},
	Term,
};

use crate::{
	error::TextError,
	text::{
		TextCursorKey,
		index::{TextIndex, TextSchema, kind_token, snapshot_hash, symbol_from_document},
	},
};

/// The deepest a keyset resume will dig into the ranking before truncating.
const MAX_FETCH: usize = 10_000;

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
		// Everything the blocking task needs, owned — tantivy search is
		// CPU/disk-bound, so it must not run on the async worker threads.
		let reader = self.reader.clone();
		let schema = self.schema().clone();
		let query = query.clone();
		async move {
			let hits = tokio::task::spawn_blocking(move || {
				search_blocking(&reader, &schema, &query, limit, after)
			})
			.await
			.unwrap_or_else(|join_error| Err(TextError::Io(std::io::Error::other(join_error))))?;
			Ok(futures::stream::iter(hits.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}
}

/// The synchronous search body: reload, validate the cursor against the live
/// snapshot, rank, then resume strictly after the keyset key.
fn search_blocking(
	reader: &IndexReader,
	schema: &TextSchema,
	query: &TextQuery,
	limit: NonZeroUsize,
	after: Option<Cursor<TextCursorKey>>,
) -> Result<Vec<Scored<Symbol>>, TextError> {
	reader.reload().map_err(TextError::Engine)?;
	let searcher = reader.searcher();

	// A cursor minted against an older snapshot no longer addresses this
	// ranking; flag it so the caller can restart cleanly.
	if let Some(cursor) = &after
		&& cursor.snapshot != snapshot_hash(&searcher)
	{
		return Err(TextError::Cursor(heart::cursor::CursorError::Payload));
	}

	let parsed = build_query(schema, query)?;
	let target = limit.get();
	// Keyset resume over a score-ranked list: tantivy has no seek-to-key, so
	// resumed pages over-fetch (doubling, capped) and skip past the key.
	let mut fetch = match &after {
		None => target,
		Some(_) => (target * 2).min(MAX_FETCH),
	};
	loop {
		let ranked = searcher
			.search(&parsed, &TopDocs::with_limit(fetch))
			.map_err(TextError::Engine)?;
		let exhausted = ranked.len() < fetch;

		let mut hits = Vec::with_capacity(ranked.len());
		for (score, address) in ranked {
			let document: TantivyDocument =
				searcher.doc(address).map_err(TextError::Engine)?;
			hits.push(Scored::new(symbol_from_document(schema, &document)?, finite_score(score)));
		}
		// Stable keyset order: score descending, id ascending on ties.
		hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.id.cmp(&b.value.id)));

		let page: Vec<_> = hits
			.into_iter()
			.filter(|hit| {
				after.as_ref().is_none_or(|cursor| {
					let (score, id) = &cursor.after;
					hit.score < *score || (hit.score == *score && hit.value.id > *id)
				})
			})
			.take(target)
			.collect();

		if page.len() == target || exhausted || fetch >= MAX_FETCH {
			if page.len() < target && !exhausted && fetch >= MAX_FETCH {
				tracing::warn!(fetched = fetch, "text keyset resume truncated at fetch ceiling");
			}
			return Ok(page);
		}
		fetch = (fetch * 2).min(MAX_FETCH);
	}
}

/// A tantivy score is finite BM25 (or a constant for regex clauses); clamp the
/// pathological case rather than panic deep inside a search.
fn finite_score(raw: f32) -> Score {
	Score::try_new(raw)
		.unwrap_or_else(|_| Score::try_new(0.0).expect("zero is finite"))
}

/// Lower a [`TextQuery`] into the tantivy query tree:
/// `(exact-name OR name-contains OR path-contains) AND ecosystem? AND kinds?`.
/// Matching is case-insensitive via the lowercased shadow fields.
pub(crate) fn build_query(
	schema: &TextSchema,
	query: &TextQuery,
) -> Result<Box<dyn Query>, TextError> {
	let needle = query.terms.trim().to_lowercase();
	if needle.is_empty() {
		return Err(TextError::Query { message: "empty query terms".to_owned() });
	}

	let contains = |field: Field| -> Result<Box<dyn Query>, TextError> {
		RegexQuery::from_pattern(&format!(".*{}.*", escape_regex(&needle)), field)
			.map(|matched| Box::new(matched) as Box<dyn Query>)
			.map_err(|error| TextError::Query {
				message: format!("terms {needle:?} do not form a searchable pattern: {error}"),
			})
	};
	let exact = Box::new(TermQuery::new(
		Term::from_field_text(schema.name_lower, &needle),
		IndexRecordOption::Basic,
	)) as Box<dyn Query>;
	let names = BooleanQuery::new(vec![
		(Occur::Should, exact),
		(Occur::Should, contains(schema.name_lower)?),
		(Occur::Should, contains(schema.fq_lower)?),
	]);

	let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, Box::new(names))];
	if let Some(ecosystem) = query.ecosystem {
		clauses.push((
			Occur::Must,
			Box::new(TermQuery::new(
				Term::from_field_text(schema.ecosystem, ecosystem.as_token()),
				IndexRecordOption::Basic,
			)),
		));
	}
	if !query.kinds.is_empty() {
		let kinds = query
			.kinds
			.iter()
			.map(|kind| {
				(
					Occur::Should,
					Box::new(TermQuery::new(
						Term::from_field_text(schema.kind, &kind_token(*kind)),
						IndexRecordOption::Basic,
					)) as Box<dyn Query>,
				)
			})
			.collect::<Vec<_>>();
		clauses.push((Occur::Must, Box::new(BooleanQuery::new(kinds))));
	}
	Ok(Box::new(BooleanQuery::new(clauses)))
}

/// Escape the regex metacharacters of the term dictionary's pattern syntax so
/// user input always matches literally.
fn escape_regex(raw: &str) -> String {
	let mut escaped = String::with_capacity(raw.len());
	for character in raw.chars() {
		if matches!(
			character,
			'.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
		) {
			escaped.push('\\');
		}
		escaped.push(character);
	}
	escaped
}
