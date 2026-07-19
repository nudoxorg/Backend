//! Ranking-quality evaluation harness.
//!
//! Every boost/constant change becomes a measured experiment instead of vibes.
//! The module is **pure and deterministic**: no IO, no RNG, no timestamps.
//! All computation is over the serde types defined here and the pipeline in
//! [`super::ranking`].
//!
//! # Workflow
//!
//! 1. Author [`GoldenQuery`] fixtures (human relevance judgments + synthetic
//!    retrieval signals) in JSON.
//! 2. Call [`mean_ndcg`] over a fixture set — the returned value is your CI
//!    gate number.
//! 3. Adjust ranking constants; re-run; confirm the gate number did not drop.

use heart::ecosystem::Language;
use smol_str::SmolStr;

use super::ranking::{Candidate, rank_full};

// ── Public types ──────────────────────────────────────────────────────────────

/// One judged result for a golden query: the package name and its graded
/// relevance (0 = irrelevant … 3 = the canonical answer).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Judgment {
	pub name: String,
	pub gain: u8,
}

/// A golden query with human relevance judgments, plus the synthetic retrieval
/// signals the harness replays through the ranking pipeline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoldenQuery {
	pub query: String,
	/// Ecosystem token ("rust", "typescript", "python", …) or absent for
	/// unscoped. The token is parsed via [`Language::from_token`]; unknown
	/// tokens are treated as unscoped.
	#[serde(default)]
	pub ecosystem: Option<String>,
	pub judgments: Vec<Judgment>,
	/// The candidate pool replayed through the pipeline: every judged package
	/// plus distractors, each with its synthetic retrieval signals.
	pub pool: Vec<PoolEntry>,
}

/// One package in the candidate pool for a golden query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoolEntry {
	pub name: String,
	pub bm25: f32,
	pub quality: f32,
	#[serde(default)]
	pub downloads: Option<u64>,
	#[serde(default)]
	pub dependents: Option<u32>,
	#[serde(default)]
	pub withdrawn: bool,
	#[serde(default)]
	pub keywords: Vec<String>,
}

// ── nDCG ─────────────────────────────────────────────────────────────────────

/// nDCG@k over a ranked name list against graded judgments (unjudged = gain 0).
///
/// Standard log2 discount: position 1 has discount log2(2) = 1, position 2 has
/// log2(3), etc.  Ideal DCG is the maximum achievable DCG given the judgment
/// gains (sorted descending).  Returns `1.0` for a query whose ideal DCG is 0
/// (nothing judged relevant) — there is nothing to penalise.
pub fn ndcg_at_k(ranked: &[&str], judgments: &[Judgment], k: usize) -> f64 {
	use std::collections::HashMap;

	let gain_map: HashMap<&str, u8> = judgments.iter().map(|j| (j.name.as_str(), j.gain)).collect();

	let dcg: f64 = ranked
		.iter()
		.take(k)
		.enumerate()
		.map(|(i, name)| {
			let gain = gain_map.get(*name).copied().unwrap_or(0) as f64;
			gain / (i as f64 + 2.0_f64).log2()
		})
		.sum();

	// Ideal: top-k gains sorted descending.
	let mut ideal_gains: Vec<u8> = judgments.iter().map(|j| j.gain).collect();
	ideal_gains.sort_unstable_by(|a, b| b.cmp(a));
	let ideal_dcg: f64 = ideal_gains
		.iter()
		.take(k)
		.enumerate()
		.map(|(i, &gain)| gain as f64 / (i as f64 + 2.0_f64).log2())
		.sum();

	if ideal_dcg == 0.0 {
		1.0
	} else {
		dcg / ideal_dcg
	}
}

// ── Evaluate ──────────────────────────────────────────────────────────────────

/// Replay one golden query through the full five-stage pipeline
/// ([`ranking::rank_full`]) and score it with nDCG@k.
///
/// Builds `Candidate<String>` (item = name) from each [`PoolEntry`], resolves
/// the ecosystem scope via [`Language::from_token`], runs `rank_full`, extracts
/// the top-k names, and calls [`ndcg_at_k`].
pub fn evaluate(query: &GoldenQuery, k: usize) -> f64 {
	let scope: Option<Language> = query
		.ecosystem
		.as_deref()
		.and_then(Language::from_token);

	// Default ecosystem for pool entries when no scope is given.
	let default_eco = scope.unwrap_or(Language::Rust);

	let candidates: Vec<Candidate<String>> = query
		.pool
		.iter()
		.map(|entry| {
			// Resolve the per-entry ecosystem (use the query scope as fallback).
			let ecosystem = default_eco;
			Candidate {
				item: entry.name.clone(),
				name: entry.name.clone(),
				bm25: entry.bm25,
				quality: entry.quality,
				downloads: entry.downloads,
				ecosystem,
				keywords: entry.keywords.iter().map(SmolStr::new).collect(),
				dependents: entry.dependents,
				popularity_pct: None,
				withdrawn: entry.withdrawn,
				squat_suspect: false,
				malware: false,
				verified_repo: false,
			}
		})
		.collect();

	let ranked = rank_full(&query.query, candidates, k, scope);
	let names: Vec<&str> = ranked.iter().map(|c| c.name.as_str()).collect();
	ndcg_at_k(&names, &query.judgments, k)
}

/// Mean nDCG@k over a set of golden queries (the CI gate number).
///
/// Returns `1.0` for an empty set (vacuously perfect).
pub fn mean_ndcg(queries: &[GoldenQuery], k: usize) -> f64 {
	if queries.is_empty() {
		return 1.0;
	}
	let sum: f64 = queries.iter().map(|q| evaluate(q, k)).sum();
	sum / queries.len() as f64
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	fn j(name: &str, gain: u8) -> Judgment {
		Judgment { name: name.to_owned(), gain }
	}

	// ── nDCG correctness ─────────────────────────────────────────────────────

	#[test]
	fn ndcg_perfect_order_is_one() {
		// ranked in exactly the right order → nDCG = 1.0
		let ranked = vec!["a", "b", "c"];
		let judgments = vec![j("a", 3), j("b", 2), j("c", 1)];
		let score = ndcg_at_k(&ranked, &judgments, 3);
		assert!(
			(score - 1.0).abs() < 1e-9,
			"perfect order must give nDCG=1.0, got {score}"
		);
	}

	#[test]
	fn ndcg_reversed_graded_order_less_than_one() {
		// worst possible order → nDCG < 1.0
		let ranked = vec!["c", "b", "a"];
		let judgments = vec![j("a", 3), j("b", 2), j("c", 1)];
		let score = ndcg_at_k(&ranked, &judgments, 3);
		assert!(score < 1.0, "reversed order must give nDCG < 1.0, got {score}");
		assert!(score > 0.0, "reversed order must still give nDCG > 0.0, got {score}");
	}

	#[test]
	fn ndcg_unjudged_only_returns_one() {
		// no judged items in results → ideal DCG = 0 → defined to be 1.0
		let ranked = vec!["x", "y", "z"];
		let judgments: Vec<Judgment> = vec![]; // nothing judged
		let score = ndcg_at_k(&ranked, &judgments, 3);
		assert!(
			(score - 1.0).abs() < 1e-9,
			"unjudged-only ranking must give nDCG=1.0, got {score}"
		);
	}

	#[test]
	fn ndcg_k_truncation() {
		// Only the first k items count. With k=1, only the top item matters.
		// top item is "a" (gain 3) → ideal is also "a" (gain 3) → nDCG@1 = 1.0
		let ranked = vec!["a", "c", "b"];
		let judgments = vec![j("a", 3), j("b", 2), j("c", 1)];
		let score = ndcg_at_k(&ranked, &judgments, 1);
		assert!(
			(score - 1.0).abs() < 1e-9,
			"k=1 with top item correct must give nDCG=1.0, got {score}"
		);

		// k=2: ranked ["a", "c"] vs ideal ["a", "b"].
		// DCG@2   = 3/log2(2) + 1/log2(3) = 3.0 + 0.631..
		// IDCG@2  = 3/log2(2) + 2/log2(3) = 3.0 + 1.261..
		// nDCG@2  = (3.0 + 0.631) / (3.0 + 1.261) ≈ 0.852
		let score2 = ndcg_at_k(&ranked, &judgments, 2);
		assert!(score2 < 1.0 && score2 > 0.8, "k=2 partial score out of range: {score2}");
	}

	#[test]
	fn ndcg_unjudged_items_contribute_zero_gain() {
		// Some ranked items aren't judged at all — they contribute 0 gain.
		let ranked = vec!["distractor", "b", "a"];
		let judgments = vec![j("a", 3), j("b", 2)];
		let score_with_distractor = ndcg_at_k(&ranked, &judgments, 3);
		// "distractor" at position 1 contributes 0; "b" at 2 is gain 2; "a" at 3 is gain 3.
		// DCG   = 0/log2(2) + 2/log2(3) + 3/log2(4) = 0 + 1.261 + 1.5 = 2.761
		// IDCG  = 3/log2(2) + 2/log2(3) = 3.0 + 1.261 = 4.261
		// nDCG  ≈ 0.648
		assert!(
			score_with_distractor < 1.0,
			"distractor at top must hurt nDCG, got {score_with_distractor}"
		);
		assert!(
			score_with_distractor > 0.5,
			"score should still be reasonable, got {score_with_distractor}"
		);
	}

	// ── evaluate: hand-built case where exact-name match must land #1 ────────

	#[test]
	fn evaluate_exact_name_match_lands_first() {
		// "tokio" exact match must beat a distractor with higher bm25.
		let query = GoldenQuery {
			query: "tokio".to_owned(),
			ecosystem: Some("rust".to_owned()),
			judgments: vec![j("tokio", 3), j("tokio-runtime", 1)],
			pool: vec![
				PoolEntry {
					name: "tokio-runtime".to_owned(),
					bm25: 5.0,    // higher raw BM25
					quality: 0.5,
					downloads: Some(500_000),
					dependents: None,
					withdrawn: false,
					keywords: vec!["async".to_owned(), "runtime".to_owned()],
				},
				PoolEntry {
					name: "tokio".to_owned(),
					bm25: 1.0,    // lower raw BM25 but exact name bonus fires
					quality: 0.9,
					downloads: Some(10_000_000),
					dependents: Some(50_000),
					withdrawn: false,
					keywords: vec!["async".to_owned(), "runtime".to_owned()],
				},
				PoolEntry {
					name: "async-std".to_owned(),
					bm25: 0.3,
					quality: 0.7,
					downloads: Some(1_000_000),
					dependents: None,
					withdrawn: false,
					keywords: vec!["async".to_owned()],
				},
			],
		};
		let score = evaluate(&query, 10);
		// With tokio at #1 and tokio-runtime at #2 (or later), nDCG is perfect or near-perfect.
		assert!(score > 0.95, "exact-match query must score very high, got {score}");
	}

	// ── mean_ndcg ────────────────────────────────────────────────────────────

	#[test]
	fn mean_ndcg_empty_set_is_one() {
		assert!((mean_ndcg(&[], 10) - 1.0).abs() < 1e-9);
	}

	#[test]
	fn mean_ndcg_single_perfect_query() {
		let query = GoldenQuery {
			query: "serde".to_owned(),
			ecosystem: Some("rust".to_owned()),
			judgments: vec![j("serde", 3)],
			pool: vec![PoolEntry {
				name: "serde".to_owned(),
				bm25: 10.0,
				quality: 0.9,
				downloads: Some(5_000_000),
				dependents: Some(80_000),
				withdrawn: false,
				keywords: vec!["serialization".to_owned()],
			}],
		};
		let score = mean_ndcg(&[query], 10);
		assert!(score > 0.99, "single perfect candidate must give near-1.0 mean_ndcg, got {score}");
	}
}
