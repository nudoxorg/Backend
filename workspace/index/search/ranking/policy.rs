//! Search / ranking policy: the enforced entry point that bundles
//! [`RankingConfig`] with intent-derived weight adjustments.
//!
//! Call sites that rank candidates should prefer
//! [`RankingPolicy::rank_candidates`] / [`RankingPolicy::rank_full_candidates`]
//! so intent classification and config tuning cannot be skipped.

use heart::ecosystem::Language;

use super::intent::{QueryIntent, classify_intent};
use super::cascade::{Candidate, RankingConfig, rank_full_with, rank_with};

/// Bundled ranking knobs + intent-aware helpers.
///
/// Holds a base [`RankingConfig`] and derives per-intent configs so Navigate
/// (stronger exact / contains) and Explore (stronger quality/popularity path)
/// stay centralized.
#[derive(Debug, Clone, Default)]
pub struct RankingPolicy {
	/// Base config (Navigate-aligned defaults). Intent adjustments clone and
	/// tweak this rather than mutating shared state.
	pub config: RankingConfig,
}

/// Alias used by search orchestration — same type as [`RankingPolicy`].
pub type SearchPolicy = RankingPolicy;

impl RankingPolicy {
	pub fn new(config: RankingConfig) -> Self {
		Self { config }
	}

	/// Derive a [`RankingConfig`] tuned for `intent`.
	///
	/// - **Navigate** — stronger exact-name bonus; full contains-specificity
	///   factors so package-id lookups float exact matches.
	/// - **Explore** — weaker exact / contains path; stronger quality×popularity
	///   additive path so high-signal packages beat keyword-spam names.
	pub fn config_for_intent(&self, intent: QueryIntent) -> RankingConfig {
		let mut cfg = self.config.clone();
		match intent {
			QueryIntent::Navigate => {
				// Preserve (or slightly strengthen) exact match dominance.
				cfg.exact_name_bonus = self.config.exact_name_bonus.max(10.0);
				cfg.contains_specificity_when_specific = 0.9;
				cfg.contains_specificity_when_generic = 0.15;
				cfg.quality_popularity_path_scale = 0.0;
			}
			QueryIntent::Explore => {
				// Soften exact/contains so spammy name hits don't dominate prose queries.
				cfg.exact_name_bonus = self.config.exact_name_bonus * 0.35;
				cfg.contains_specificity_when_specific = 0.35;
				cfg.contains_specificity_when_generic = 0.05;
				// Quality kink engages earlier so quality multiplies BM25 more often.
				cfg.quality_kink_threshold =
					self.config.quality_kink_threshold.min(0.25);
				// Additive quality × log-popularity path (see fuse_scores).
				cfg.quality_popularity_path_scale = 2.5;
			}
		}
		cfg
	}

	/// Classify intent from `query` + ecosystem scope, then rank (truncated).
	///
	/// This is the preferred single-page entry point from search orchestration.
	pub fn rank_candidates<T>(
		&self,
		query: &str,
		candidates: Vec<Candidate<T>>,
		limit: usize,
		ecosystem_scope: Option<Language>,
	) -> Vec<Candidate<T>> {
		let intent = self.intent_for(query, ecosystem_scope);
		let cfg = self.config_for_intent(intent);
		rank_with(&cfg, query, candidates, limit, ecosystem_scope)
	}

	/// Classify intent, then run the full (non-truncating) pipeline.
	///
	/// Preferred pagination entry point — used by `collect_ranked_hits`.
	pub fn rank_full_candidates<T>(
		&self,
		query: &str,
		candidates: Vec<Candidate<T>>,
		limit: usize,
		ecosystem_scope: Option<Language>,
	) -> Vec<Candidate<T>> {
		let intent = self.intent_for(query, ecosystem_scope);
		let cfg = self.config_for_intent(intent);
		rank_full_with(&cfg, query, candidates, limit, ecosystem_scope)
	}

	/// Classify intent with ecosystem norms when the scope is known.
	pub fn intent_for(&self, query: &str, ecosystem_scope: Option<Language>) -> QueryIntent {
		let norms = ecosystem_scope.map(|lang| {
			use ecosystem::LanguageExt;
			lang.spec().search_norms()
		});
		classify_intent(query, ecosystem_scope, norms)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn navigate_keeps_strong_exact_bonus() {
		let policy = RankingPolicy::default();
		let cfg = policy.config_for_intent(QueryIntent::Navigate);
		assert!(cfg.exact_name_bonus >= 10.0);
		assert_eq!(cfg.quality_popularity_path_scale, 0.0);
	}

	#[test]
	fn explore_softens_exact_and_boosts_quality_path() {
		let policy = RankingPolicy::default();
		let base = policy.config.exact_name_bonus;
		let cfg = policy.config_for_intent(QueryIntent::Explore);
		assert!(cfg.exact_name_bonus < base);
		assert!(cfg.quality_popularity_path_scale > 0.0);
		assert!(cfg.quality_kink_threshold <= 0.25);
	}
}
