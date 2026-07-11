//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! Postgres is the source of truth; a **replica-local** tantivy index is the
//! query abstraction layered over it, kept in sync by polling postgres from a
//! watermark (see [`tantivy`]). Results are [`heart::Scored`] packages,
//! keyset-paginated via [`heart::Cursor`], and access-filtered — every method
//! takes an [`AccessContext`].

use heart::{PackageId, Scored, cursor::Cursor, ecosystem::Language, search::Page};

use crate::{GlobalPackage, error::SearchError};

pub mod multi_parent;
pub mod ranking;
pub mod tantivy;

/// The quality assigned to a package with no extracted facets yet — a neutral
/// midpoint so the fusion multiplier neither erases (`0.0`) nor inflates such a
/// package relative to its BM25 relevance.
const NEUTRAL_QUALITY: f32 = 0.5;

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

/// Fetch one page of results for `query` from the replica-local `index`.
///
/// This is the shared implementation backing both the server's
/// `PackageSearchIndex::page` (server crate) and the registry's own test
/// suite.  First pages (`query.after.is_none()`) apply the full five-stage
/// ranking pipeline (BM25 × quality kink + exact/contains bonus + diversity +
/// representative pull-up + downloads bubble).  Keyset-resumed pages apply
/// only the pagination-safe fused score so the `(score, id)` cursor remains
/// monotonic.
pub async fn search_page(
	index: &tantivy::PackageIndex,
	query: &RegistryQuery,
) -> Result<Page<GlobalPackage>, SearchError> {
	let hits = collect_ranked_hits(index, query).await?;

	let snapshot = heart::ContentHash::of_bytes(&index.watermark().position.to_le_bytes());

	// Resume strictly after the cursor key under (score desc, id asc).
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

/// Run the raw query, hydrate, ecosystem-filter, de-dup the multi-parent
/// fan-in, then rank the result set.
///
/// For first pages the full five-stage ranking pipeline runs; for keyset-resumed
/// pages only the pagination-safe fused score is applied so the cursor remains
/// monotonic.
pub async fn collect_ranked_hits(
	index: &tantivy::PackageIndex,
	query: &RegistryQuery,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
	use std::collections::HashMap;

	// Over-fetch so filtering, de-dup, and cursor resume still fill a page.
	let over_fetch = query.limit.max(1) * 4 + 32;
	let raw = index.query(&query.text, over_fetch)?;
	let ids: Vec<PackageId> = raw.iter().map(|(id, _)| *id).collect();
	let scores: HashMap<PackageId, f32> = raw.into_iter().collect();

	let scored: Vec<Scored<GlobalPackage>> = index
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

	// Collapse multi-parent duplicates to their best representative.
	let representatives: Vec<GlobalPackage> = multi_parent::merge(scored)
		.into_iter()
		.map(|merged| merged.representative.value)
		.collect();

	// Build ranking candidates.
	let candidates: Vec<ranking::Candidate<GlobalPackage>> = representatives
		.into_iter()
		.map(|package| {
			let bm25 = scores.get(&package.id).copied().unwrap_or_default();
			// A package whose rich metadata hasn't been extracted yet has no
			// quality signal. Treat "unknown" as neutral, not zero — otherwise
			// `bm25 × 0` would erase its relevance.
			let (quality, keywords) = package
				.facets
				.as_ref()
				.map(|facets| (facets.quality(), facets.keywords.clone()))
				.unwrap_or((NEUTRAL_QUALITY, Vec::new()));
			let name = package.package.coordinates.name.canonical().to_string();
			ranking::Candidate { item: package, name, bm25, quality, downloads: 0, keywords }
		})
		.collect();

	let is_first_page = query.after.is_none();
	let limit = query.limit.max(1);

	let hits: Vec<Scored<GlobalPackage>> = if is_first_page {
		// First page: apply the full five-stage ranking pipeline (diversity pass,
		// representative pull-up, downloads bubble).  The result is ordered by the
		// pipeline rather than by a monotonic score, so it cannot be resumed with a
		// keyset cursor — the caller must restart for page 2.
		ranking::rank(&query.text, candidates, limit)
			.into_iter()
			.map(|c| Scored::new(c.item, finite_score(c.bm25)))
			.collect()
	} else {
		// Subsequent pages: pagination-safe fused score only so the `(score, id)`
		// keyset cursor stays monotonic.
		let fused = ranking::fused_scores(&query.text, &candidates);
		let mut page_hits: Vec<Scored<GlobalPackage>> = candidates
			.into_iter()
			.zip(fused)
			.map(|(candidate, score)| Scored::new(candidate.item, finite_score(score)))
			.collect();
		page_hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.id.cmp(&b.value.id)));
		page_hits
	};

	Ok(hits)
}

/// Clamp a raw tantivy score onto the provably-finite [`heart::Score`].
/// Tantivy scores are always finite; the fallback is a defensive zero.
pub fn finite_score(raw: f32) -> heart::Score {
	heart::Score::try_new(raw)
		.unwrap_or_else(|_| heart::Score::try_new(0.0).expect("0.0 is finite"))
}
