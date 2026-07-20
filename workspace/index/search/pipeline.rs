//! Single retrieval + rank entry for package discovery.
//!
//! All high-level package search (server, registry tests, hybrid) **must** go
//! through [`retrieve_and_rank`] / [`retrieve_and_rank_page`]. There is no second
//! ranking path that bypasses structured parse + multi-stage rank.
//!
//! # Pipeline stages
//! 1. Parse raw text into [`StructuredQuery`] (ecosystem / namespace / deps /
//!    license / phrases / free terms).
//! 2. Optionally expand free terms via [`PackageSearchDeps::synonyms`].
//! 3. Over-fetch from the tantivy replica (`query_structured`).
//! 4. Optionally RRF-fuse with semantic package ids.
//! 5. Hydrate → multi-parent merge → entity dedup (unscoped) →
//!    intent-aware rank on free terms (`sq.terms`):
//!    - **scoped** → single [`policy::RankingPolicy::rank_full_candidates`]
//!    - **unscoped** → per-ecosystem rank then
//!      [`interleave::rank_per_ecosystem_and_interleave`]
//! 6. Stamp strictly-descending rank scores so keyset pagination is seam-free.
//!
//! # Not applied here
//! **LocalEnrichment** (desktop used-before / in-project soft boosts and local
//! path/fork package injection) is intentionally **not** applied in this
//! pipeline. It is a desktop-only future sidecar and must never run on the
//! server/registry path.

use std::collections::HashMap;

use heart::{PackageId, Scored, cursor::Cursor, ecosystem::Language, search::Page};

use crate::{GlobalPackage, error::SearchError, metadata::Synonyms};

use super::{SearchKey, finite_score, ranking, structured::StructuredQuery, tantivy::PackageIndex};
use super::ranking::{entity, interleave, multi_parent, policy, popularity, rrf};

/// The quality assigned to a package with no extracted facets yet — a neutral
/// midpoint so the fusion multiplier neither erases (`0.0`) nor inflates such a
/// package relative to its BM25 relevance.
const NEUTRAL_QUALITY: f32 = 0.5;

/// Canonical high-level package search request.
///
/// Built once by callers and fed exclusively into [`retrieve_and_rank`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSearchRequest {
	/// Free-text query (package name / description / keywords), may include
	/// structured tokens (`lang:`, `dep:`, `license:`, `scope:`, …).
	pub text: String,

	/// API-level ecosystem scope. Wins over any inline `lang:`/`ecosystem:`
	/// token when present (see [`StructuredQuery::parse`]).
	pub ecosystem: Option<Language>,

	/// Page size (also tunes position-sensitive rank stages; the full order is
	/// still materialised for keyset resume).
	pub limit: usize,

	/// Keyset cursor to resume after, or `None` for the first page. Only
	/// consumed by [`retrieve_and_rank_page`]; ignored by [`retrieve_and_rank`].
	pub after: Option<Cursor<SearchKey>>,

	/// Optional semantic (vector) ranking ids, best-first. Empty = pure BM25.
	pub semantic: Vec<PackageId>,
}

/// Optional runtime deps for the retrieval pipeline.
///
/// Spell-check index will land here later; keep the struct open for that.
#[derive(Clone, Copy, Default)]
pub struct PackageSearchDeps<'a> {
	/// When `Some`, free terms are synonym-expanded before the tantivy query
	/// (populates [`StructuredQuery::expanded_terms`]).
	pub synonyms: Option<&'a Synonyms>,

	/// When `Some`, a `cpp`-scoped bare-token query is alias-expanded to its
	/// canonical stem name before the tantivy query (REGISTRYLESS §9, P8 — the
	/// "users never type slugs" hook). Catalog-backed; injected by the server,
	/// which holds both the `MetaStore` and this pipeline. `None` disables
	/// expansion (the default; every non-cpp path is unaffected regardless).
	pub alias_expander: Option<&'a dyn super::alias::AliasExpander>,
}

/// **The only high-level entry** for package discovery.
///
/// Builds a [`StructuredQuery`], expands synonyms when provided, over-fetches
/// from `index`, hydrates, collapses multi-parent fan-in, entity-dedups unscoped
/// results, ranks via [`policy::RankingPolicy`] on free terms (intent-aware),
/// and stamps rank scores for seam-free keyset pagination.
///
/// LocalEnrichment is **not** applied (see module docs).
pub async fn retrieve_and_rank(
	index: &PackageIndex,
	req: &PackageSearchRequest,
	deps: PackageSearchDeps<'_>,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
	// Over-fetch so filtering, de-dup, and cursor resume still fill a page.
	let over_fetch = req.limit.max(1) * 4 + 32;
	let mut sq = StructuredQuery::parse(&req.text, req.ecosystem);
	// P8: a cpp-scoped bare token (`zlib`) is rewritten to its canonical stem
	// name (`github.com/madler/zlib`) before search, so users never type slugs.
	// A miss, a non-cpp scope, or a non-bare token is a pass-through.
	if let Some(expander) = deps.alias_expander {
		super::alias::expand_cpp_bare_token(sq.ecosystem, &mut sq.terms, expander);
	}
	if let Some(synonyms) = deps.synonyms {
		sq.expand_synonyms(synonyms);
	}
	let raw = index.query_structured(&sq, over_fetch)?;

	// Build the id order and score map from BM25 results.
	let bm25_ids: Vec<PackageId> = raw.iter().map(|(id, _)| *id).collect();
	let bm25_scores: HashMap<PackageId, f32> = raw.into_iter().collect();

	// Determine the final id order and per-id relevance score.
	// When semantic is empty this is a no-op (pure BM25 path).
	let (hydrate_ids, scores): (Vec<PackageId>, HashMap<PackageId, f32>) =
		if req.semantic.is_empty() {
			(bm25_ids.clone(), bm25_scores)
		} else {
			// RRF-fuse BM25 order with semantic order.
			let fused =
				rrf::fuse(&[bm25_ids.as_slice(), req.semantic.as_slice()], rrf::DEFAULT_RRF_K);

			// Rescale RRF scores into BM25 magnitude so downstream stages (quality
			// kink, exact/contains bonus) keep their calibration. The rescaling
			// factor is max_bm25 / max_rrf; when no BM25 hits exist the RRF scores
			// stand as-is.
			let max_bm25 = bm25_scores.values().cloned().fold(0.0_f32, f32::max);
			let max_rrf = fused.first().map(|(_, s)| *s).unwrap_or(1.0_f32);
			let rescale = if max_rrf > 0.0 && max_bm25 > 0.0 {
				max_bm25 / max_rrf
			} else {
				1.0
			};

			let all_ids: Vec<PackageId> = fused.iter().map(|(id, _)| *id).collect();
			let score_map: HashMap<PackageId, f32> =
				fused.into_iter().map(|(id, s)| (id, s * rescale)).collect();
			(all_ids, score_map)
		};

	let scored: Vec<Scored<GlobalPackage>> = index
		.hydrate(&hydrate_ids)
		.await?
		.into_iter()
		// Ecosystem filtering happens in the index (Must TermQuery, Q4); this
		// assert is a belt-and-braces rollout guard only, never a filter.
		.inspect(|package| {
			debug_assert!(
				req.ecosystem
					.is_none_or(|eco| package.package.coordinates.ecosystem() == eco),
				"tantivy ecosystem Must-filter missed a doc — index may need rebuild"
			);
		})
		.map(|package| {
			let raw_score = scores.get(&package.id).copied().unwrap_or_default();
			Scored::new(package, finite_score(raw_score))
		})
		.collect();

	// Collapse multi-parent duplicates to their best representative.
	let mut representatives: Vec<GlobalPackage> = multi_parent::merge(scored)
		.into_iter()
		.map(|merged| merged.representative.value)
		.collect();

	// Cross-ecosystem entity dedup for unscoped queries: when the user hasn't
	// narrowed to a single ecosystem, deduplicate "the same project" (e.g. a
	// Rust crate + its npm wasm shim) by repository slug, keeping the best-ranked
	// representative only.
	// Use the *resolved* structured ecosystem (API scope or inline `lang:`) so
	// `lang:rust serde` does not re-merge across ecosystems that the index already
	// pruned.
	if sq.ecosystem.is_none() {
		representatives = entity::dedup_by_repo(representatives, |pkg| {
			pkg.facets.as_ref().and_then(|f| f.repo_slug.as_deref())
		});
	}

	// Build ranking candidates. Downloads calibration + percentile go through
	// `PopularitySignals::from_facets_full` so scale/pct cannot be forgotten.
	// Gate flags come from facets; verified_repo is computed if unset.
	let candidates: Vec<ranking::Candidate<GlobalPackage>> = representatives
		.into_iter()
		.map(|package| {
			use ecosystem::LanguageExt;
			let bm25 = scores.get(&package.id).copied().unwrap_or_default();
			let (
				quality,
				keywords,
				raw_downloads,
				dependents,
				withdrawn,
				popularity_pct,
				squat_suspect,
				malware,
				facets_verified_repo,
				repo_slug,
			) = package
				.facets
				.as_ref()
				.map(|facets| {
					(
						facets.quality(),
						facets.keywords.clone(),
						facets.downloads,
						facets.dependents,
						facets.withdrawn,
						facets.popularity_pct_f32(),
						facets.squat_suspect,
						facets.malware,
						facets.verified_repo,
						facets.repo_slug.as_deref(),
					)
				})
				.unwrap_or((
					NEUTRAL_QUALITY,
					Vec::new(),
					None,
					None,
					false,
					None,
					false,
					false,
					false,
					None,
				));
			let ecosystem = package.package.coordinates.ecosystem();
			let scale = ecosystem.spec().search_norms().downloads_scale;
			let popularity = popularity::PopularitySignals::from_facets_full(
				raw_downloads,
				dependents,
				scale,
				popularity_pct,
			);
			let name = package.package.coordinates.name.canonical().to_string();
			let verified_repo =
				facets_verified_repo || super::ranking::gates::verified_repo(&name, repo_slug);
			ranking::Candidate {
				item: package,
				name,
				bm25,
				quality,
				downloads: popularity.downloads,
				dependents: popularity.dependents,
				popularity_pct: popularity.popularity_pct,
				withdrawn,
				squat_suspect,
				malware,
				verified_repo,
				ecosystem,
				keywords,
			}
		})
		.collect();

	// Rank on FREE terms via RankingPolicy. Unscoped: per-eco rank + interleave
	// so crates.io download scale never pure-sorts the whole multi-eco SERP.
	let limit = req.limit.max(1);
	let ranked = if sq.ecosystem.is_some() {
		policy::RankingPolicy::default().rank_full_candidates(
			&sq.terms,
			candidates,
			limit,
			sq.ecosystem,
		)
	} else {
		let policy = policy::RankingPolicy::default();
		interleave::rank_per_ecosystem_and_interleave(candidates, limit, |eco, group| {
			policy.rank_full_candidates(&sq.terms, group, limit, Some(eco))
		})
	};

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

/// Keyset-paginated form of [`retrieve_and_rank`].
///
/// Runs the full pipeline once, then slices the stamped total order with
/// `req.after` / `req.limit`. Page 1 and page N are always slices of the same
/// order — no first-page/resumed-page split and no scoring seam at the boundary.
///
/// LocalEnrichment is **not** applied (see module docs).
pub async fn retrieve_and_rank_page(
	index: &PackageIndex,
	req: &PackageSearchRequest,
	deps: PackageSearchDeps<'_>,
) -> Result<Page<GlobalPackage>, SearchError> {
	let hits = retrieve_and_rank(index, req, deps).await?;

	let snapshot = heart::ContentHash::of_bytes(&index.watermark().position.to_le_bytes());

	// Resume strictly after the cursor key under (score desc, id asc).
	let after = req.after.as_ref().map(|cursor| {
		if cursor.snapshot != snapshot {
			tracing::debug!("cursor anchored to an older snapshot; serving from the newer one");
		}
		cursor.after
	});
	let resumed = hits
		.into_iter()
		.filter(|hit| super::keyset_is_after(after, hit.score, hit.value.id));

	let limit = req.limit.max(1);
	let mut items: Vec<Scored<GlobalPackage>> = resumed.take(limit + 1).collect();
	let has_more = items.len() > limit;
	items.truncate(limit);
	let next = has_more
		.then(|| items.last())
		.flatten()
		.map(|last| Cursor::new((last.score, last.value.id), snapshot).encode());
	Ok(Page { items, next })
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
