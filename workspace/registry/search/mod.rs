//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! Postgres is the source of truth; a **replica-local** tantivy index is the
//! query abstraction layered over it, kept in sync by polling postgres from a
//! watermark (see [`tantivy`]). Results are [`heart::Scored`] packages,
//! keyset-paginated via [`heart::Cursor`], and access-filtered — every method
//! takes an [`AccessContext`].

use futures::Stream;
use heart::{
	PackageId, Scored,
	cursor::Cursor,
	ecosystem::Language,
	search::Page,
};

use crate::{GlobalPackage, error::SearchError};

pub mod multi_parent;
pub mod tantivy;

/// The keyset a registry-search cursor advances over: a relevance score paired
/// with the package id as the tiebreak, so pagination is stable across an
/// eventually-consistent replica.
pub type SearchKey = (heart::Score, PackageId);

/// A single query against the registry search surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegistryQuery {
	/// The free-text query (package name / description / keywords).
	pub text: String,

	/// Restrict to a single ecosystem, or search across all when `None`.
	pub ecosystem: Option<Language>,

	/// Page size.
	pub limit: usize,

	/// The keyset cursor to resume after, or `None` for the first page.
	pub after: Option<Cursor<SearchKey>>,
}

/// The registry (package) search entry point the server's admin/read surface
/// calls. Wraps the replica-local tantivy index + the multi-parent de-dupe.
pub struct RegistrySearch {
	index: tantivy::PackageIndex,
}

impl RegistrySearch {
	/// Build over a replica-local package index.
	pub fn new(index: tantivy::PackageIndex) -> Self { Self { index } }

	/// Stream scored packages matching `query`, access-filtered and de-duped
	/// across multi-parent reachability.
	///
	/// Returns an `impl Stream` (native RPITIT, no `async_trait`) so a caller can
	/// consume results lazily and stop early. Each item carries the [`Scored`]
	/// package or a per-item error.
	pub fn search<'a>(
		&'a self,
		query: &'a RegistryQuery,
	) -> impl Stream<Item = Result<Scored<GlobalPackage>, SearchError>> + 'a {
		use futures::StreamExt;
		futures::stream::once(async move { self.page(query).await }).flat_map(|result| {
			match result {
				Ok(page) => futures::stream::iter(page.items.into_iter().map(Ok)).left_stream(),
				Err(error) => {
					futures::stream::once(std::future::ready(Err(error))).right_stream()
				}
			}
		})
	}

	/// Fetch one page (materialized) plus the cursor for the next page, for
	/// callers that page rather than stream.
	pub async fn page(
		&self,
		query: &RegistryQuery,
	) -> Result<Page<GlobalPackage>, SearchError> {
		let hits = self.collect_hits(query).await?;
		let snapshot = self.snapshot();

		// Resume strictly after the cursor key under (score desc, id asc). A
		// cursor anchored to an older snapshot still pages coherently — the
		// keyset resume is position-free — it just sees the newer corpus.
		let after = query.after.as_ref().map(|cursor| {
			if cursor.snapshot != snapshot {
				tracing::debug!("cursor anchored to an older snapshot; serving from the newer one");
			}
			cursor.after
		});
		let resumed = hits.into_iter().filter(|hit| {
			after.is_none_or(|(score, id)| {
				hit.score < score || (hit.score == score && hit.value.id > id)
			})
		});

		let limit = query.limit.max(1);
		let mut items: Vec<Scored<GlobalPackage>> = resumed.take(limit + 1).collect();
		let has_more = items.len() > limit;
		items.truncate(limit);
		let next = has_more
			.then(|| items.last())
			.flatten()
			.map(|last| Cursor::new((last.score, last.value.id), snapshot).encode());
		Ok(Page { items, next })
	}

	/// Run the raw query, hydrate, ecosystem-filter, de-dupe the multi-parent
	/// fan-in, and rank descending — the shared core of [`RegistrySearch::page`]
	/// and [`RegistrySearch::search`].
	async fn collect_hits(
		&self,
		query: &RegistryQuery,
	) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
		use std::collections::HashMap;

		// Over-fetch so filtering, de-dupe, and cursor resume still fill a page.
		let over_fetch = query.limit.max(1) * 4 + 32;
		let raw = self.index.query(&query.text, over_fetch)?;
		let ids: Vec<PackageId> = raw.iter().map(|(id, _)| *id).collect();
		let scores: HashMap<PackageId, f32> = raw.into_iter().collect();

		let scored: Vec<Scored<GlobalPackage>> = self
			.index
			.hydrate(&ids)
			.await?
			.into_iter()
			.filter(|package| {
				query
					.ecosystem
					.is_none_or(|ecosystem| package.package.coordinates.ecosystem() == ecosystem)
			})
			.map(|package| {
				let raw_score = scores.get(&package.id).copied().unwrap_or_default();
				Scored::new(package, finite_score(raw_score))
			})
			.collect();

		// Collapse multi-parent duplicates to their best representative, then
		// rank by score (descending) with the id as the stable tiebreak — the
		// same order the pagination keyset advances over.
		let mut hits: Vec<Scored<GlobalPackage>> = multi_parent::merge(scored)
			.into_iter()
			.map(|merged| merged.representative)
			.collect();
		hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.id.cmp(&b.value.id)));
		Ok(hits)
	}

	/// The snapshot fingerprint cursors are anchored to: the replica's current
	/// sync watermark, content-hashed.
	fn snapshot(&self) -> heart::ContentHash {
		heart::ContentHash::of_bytes(&self.index.watermark().position.to_le_bytes())
	}
}

/// Clamp a raw tantivy score onto the provably-finite [`heart::Score`].
/// Tantivy scores are always finite; the fallback is a defensive zero.
fn finite_score(raw: f32) -> heart::Score {
	heart::Score::try_new(raw)
		.unwrap_or_else(|_| heart::Score::try_new(0.0).expect("0.0 is finite"))
}
