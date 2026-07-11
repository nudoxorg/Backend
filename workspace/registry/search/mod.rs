//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! Postgres is the source of truth; a **replica-local** tantivy index is the
//! query abstraction layered over it, kept in sync by polling postgres from a
//! watermark (see [`tantivy`]). Results are [`heart::Scored`] packages,
//! keyset-paginated via [`heart::Cursor`], and access-filtered — every method
//! takes an [`AccessContext`].
//!
//! # One order across all pages
//!
//! The full five-stage ranking pipeline (BM25 × quality kink, exact/contains
//! bonus, diversity, representative pull-up, downloads bubble) is a *positional*
//! reorder — it is not a monotonic function of any single per-item score. To
//! page over it with a keyset cursor without dropping or duplicating results at
//! the page boundary, [`collect_ranked_hits`] runs that pipeline **once** over
//! the whole over-fetched candidate set (via [`ranking::rank_full`]) and then
//! stamps each item with a strictly-descending **rank score** derived from its
//! final ordinal. Page 1 and page N are then slices of that one total order:
//! the `(score, id)` keyset cursor advances monotonically over the rank score,
//! so there is no scoring seam between pages — the first-page and resumed-page
//! code paths are identical.

use heart::{PackageId, Scored, cursor::Cursor, ecosystem::Language, search::Page};

use crate::{GlobalPackage, error::SearchError};

pub mod multi_parent;
pub mod ranking;
pub mod tantivy;

/// The quality assigned to a package with no extracted facets yet — a neutral
/// midpoint so the fusion multiplier neither erases (`0.0`) nor inflates such a
/// package relative to its BM25 relevance.
const NEUTRAL_QUALITY: f32 = 0.5;

/// The keyset a registry-search cursor advances over: a **rank score** paired
/// with the package id as the tiebreak, so pagination is stable across an
/// eventually-consistent replica.
///
/// The score component is *not* the raw fused BM25 relevance — it is a
/// strictly-descending value assigned by the item's final ordinal in the one
/// full-pipeline order (see [`collect_ranked_hits`]). Every item therefore has a
/// distinct score, the order is a strict total order under `score desc`, and the
/// `id` tiebreak is retained only defensively (it never fires). This is what lets
/// keyset resume slice the *same* order the pipeline produced, seam-free.
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
/// `PackageSearchIndex::page` (server crate) and the registry's own test suite.
/// Every page — first or keyset-resumed — is a slice of the *same* full
/// five-stage ranking order (BM25 × quality kink + exact/contains bonus +
/// diversity + representative pull-up + downloads bubble): [`collect_ranked_hits`]
/// runs that pipeline once and stamps each hit with a strictly-descending rank
/// score, so the `(score, id)` cursor advances monotonically over the identical
/// order regardless of page position. There is no first-page/resumed-page split
/// and no scoring seam at the page boundary.
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
/// fan-in, then rank the result set into **one** total order.
///
/// The full five-stage ranking pipeline runs once over the whole over-fetched
/// candidate set (independent of page position); each returned hit carries a
/// strictly-descending **rank score** derived from its final ordinal in that one
/// order. Callers then keyset-page over the result by `(score, id)` and, because
/// every item has a distinct rank score matching the pipeline order, page 1 and
/// page N are slices of the identical order — no drops or duplicates at the seam.
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

	// The page size only tunes the position-sensitive pipeline stages
	// (representative pull-up, bubble tail); it never truncates here — the whole
	// ranked order is materialized so the keyset cursor can slice any page out of
	// it. The order is thus a single function of `(query.text, ecosystem, snapshot)`
	// and *not* of page position: page 1 and page N resume over the same order.
	let limit = query.limit.max(1);
	let ranked = ranking::rank_full(&query.text, candidates, limit);

	// Stamp each item with a strictly-descending rank score keyed on its final
	// ordinal, so `(score, id)` is a strict total order matching the pipeline
	// order. `finite_score` clamps onto the provably-finite `heart::Score`.
	let total = ranked.len();
	let hits: Vec<Scored<GlobalPackage>> = ranked
		.into_iter()
		.enumerate()
		.map(|(ordinal, candidate)| {
			Scored::new(candidate.item, rank_score(ordinal, total))
		})
		.collect();

	Ok(hits)
}

/// Map a pipeline ordinal (`0` = best) onto a strictly-descending, provably
/// finite [`heart::Score`], so the one full-pipeline order becomes a strict
/// total order under `score desc` that the `(score, id)` keyset cursor can page
/// over with no seam.
///
/// Encoded as `(total - ordinal)`: rank 0 gets the largest score, each later
/// rank gets exactly one less, and every item's score is distinct. The values
/// stay well within `f32`'s exact-integer range for any realistic candidate set
/// (over-fetch is `limit*4 + 32`), so no two ordinals ever collide.
fn rank_score(ordinal: usize, total: usize) -> heart::Score {
	finite_score((total.saturating_sub(ordinal)) as f32)
}

/// Clamp a raw tantivy score onto the provably-finite [`heart::Score`].
/// Tantivy scores are always finite; the fallback is a defensive zero.
pub fn finite_score(raw: f32) -> heart::Score {
	heart::Score::try_new(raw)
		.unwrap_or_else(|_| heart::Score::try_new(0.0).expect("0.0 is finite"))
}
