//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! # Single entry
//!
//! High-level discovery **must** go through [`pipeline::retrieve_and_rank`] /
//! [`pipeline::retrieve_and_rank_page`]. Wrappers cannot bypass structured parse
//! + intent-aware rank + (when unscoped) per-eco interleave.
//!
//! **LocalEnrichment** is desktop-only and is **not** applied on this path.

use heart::{PackageId, Scored, cursor::Cursor, ecosystem::Language, search::Page};

use crate::{GlobalPackage, error::SearchError, metadata::Synonyms};

pub mod dependents;
pub mod enrich;
pub mod entity;
pub mod eval;
pub mod gates;
pub mod health;
pub mod intent;
pub mod interleave;
pub mod listing_signals;
pub mod local_enrichment;
pub mod multi_parent;
pub mod pipeline;
pub mod policy;
pub mod popularity;
pub mod ranking;
pub mod rrf;
pub mod spell;
pub mod squat;
pub mod structured;
pub mod tantivy;

pub use health::PackageIndexHealth;
pub use local_enrichment::{
	DepRelation, EnrichedHit, HitSource, LocalContext, LocalEnrichment, LocalLabel,
	LocalOnlyPackage, RankedItem, UsageStat,
};
pub use pipeline::{
	PackageSearchDeps, PackageSearchRequest, retrieve_and_rank, retrieve_and_rank_page,
};
pub use popularity::assign_percentiles;
pub use structured::StructuredQuery;

/// The keyset a registry-search cursor advances over.
pub type SearchKey = (heart::Score, PackageId);

/// A single query against the registry search surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegistryQuery {
	pub text: String,
	pub ecosystem: Option<Language>,
	pub limit: usize,
	pub after: Option<Cursor<SearchKey>>,
}

/// Did-you-mean suggestions for free terms (does not re-query the index).
#[must_use]
pub fn dym_suggestions_for_query(
	vocab: impl IntoIterator<Item = impl AsRef<str>>,
	query: &RegistryQuery,
	limit: usize,
) -> Vec<String> {
	let sq = StructuredQuery::parse(&query.text, query.ecosystem);
	if sq.terms.is_empty() {
		return Vec::new();
	}
	spell::dym_suggestions(vocab, &sq.terms, limit)
}

/// Optional zero-hit auto-repair for bare free-text queries.
#[must_use]
pub fn maybe_repair_registry_query(
	spell_index: &spell::SpellIndex,
	query: &RegistryQuery,
) -> Option<RegistryQuery> {
	let sq = StructuredQuery::parse(&query.text, query.ecosystem);
	if sq.terms.is_empty() {
		return None;
	}
	if query.text.trim() != sq.terms.trim() {
		return None;
	}
	let repaired = spell::maybe_repair_query(spell_index, &sq.terms)?;
	Some(RegistryQuery {
		text: repaired,
		ecosystem: query.ecosystem,
		limit: query.limit,
		after: query.after.clone(),
	})
}

/// One page of results — thin wrapper over [`retrieve_and_rank_page`].
pub async fn search_page(
	index: &tantivy::PackageIndex,
	query: &RegistryQuery,
	synonyms: Option<&Synonyms>,
) -> Result<Page<GlobalPackage>, SearchError> {
	let req = PackageSearchRequest::from_registry(query, &[]);
	retrieve_and_rank_page(index, &req, PackageSearchDeps { synonyms }).await
}

/// Full ranked order — thin wrapper over [`retrieve_and_rank`].
pub async fn collect_ranked_hits(
	index: &tantivy::PackageIndex,
	query: &RegistryQuery,
	synonyms: Option<&Synonyms>,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
	collect_ranked_hits_hybrid(index, query, &[], synonyms).await
}

/// Hybrid BM25 + semantic RRF path.
pub async fn collect_ranked_hits_hybrid(
	index: &tantivy::PackageIndex,
	query: &RegistryQuery,
	semantic: &[PackageId],
	synonyms: Option<&Synonyms>,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
	let req = PackageSearchRequest::from_registry(query, semantic);
	retrieve_and_rank(index, &req, PackageSearchDeps { synonyms }).await
}

/// Clamp a raw tantivy score onto the provably-finite [`heart::Score`].
pub fn finite_score(raw: f32) -> heart::Score {
	heart::Score::try_new(raw)
		.unwrap_or_else(|_| heart::Score::try_new(0.0).expect("0.0 is finite"))
}
