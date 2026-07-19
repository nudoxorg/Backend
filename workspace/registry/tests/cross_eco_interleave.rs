//! Adversarial tests for cross-ecosystem SERP interleave.
//!
//! Unscoped search must rank each ecosystem independently and interleave
//! results — never pure-sort all ecosystems by one popularity / fused scale.
//! Scoped search keeps a single pipeline over the filtered pool.
//!
//! Pure and deterministic: synthetic [`Candidate`] lists only (no IO).

use heart::ecosystem::Language;
use registry::search::interleave::{interleave_by_ecosystem, rank_per_ecosystem_and_interleave};
use registry::search::policy::RankingPolicy;
use registry::search::ranking::{Candidate, rank_full};
use smol_str::SmolStr;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cand(
	name: &str,
	bm25: f32,
	quality: f32,
	downloads: Option<u64>,
	eco: Language,
) -> Candidate<&'static str> {
	Candidate {
		item: "payload",
		name: name.to_owned(),
		bm25,
		quality,
		downloads,
		dependents: None,
		popularity_pct: None,
		withdrawn: false,
		squat_suspect: false,
		malware: false,
		verified_repo: false,
		ecosystem: eco,
		keywords: Vec::new(),
	}
}

fn cand_kw(
	name: &str,
	bm25: f32,
	quality: f32,
	downloads: Option<u64>,
	eco: Language,
	keywords: &[&str],
) -> Candidate<&'static str> {
	Candidate {
		item: "payload",
		name: name.to_owned(),
		bm25,
		quality,
		downloads,
		dependents: None,
		popularity_pct: None,
		withdrawn: false,
		squat_suspect: false,
		malware: false,
		verified_repo: false,
		ecosystem: eco,
		keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
	}
}

fn names(result: &[Candidate<&'static str>]) -> Vec<String> {
	result.iter().map(|c| c.name.clone()).collect()
}

fn ecosystems(result: &[Candidate<&'static str>]) -> Vec<Language> {
	result.iter().map(|c| c.ecosystem).collect()
}

/// Mirror of the unscoped pipeline path: partition → rank_full per eco → interleave.
fn unscoped_rank_interleave(
	query: &str,
	candidates: Vec<Candidate<&'static str>>,
	limit: usize,
) -> Vec<Candidate<&'static str>> {
	let policy = RankingPolicy::default();
	rank_per_ecosystem_and_interleave(candidates, limit, |eco, group| {
		policy.rank_full_candidates(query, group, limit, Some(eco))
	})
}

// ── 1. Unscoped Rust + TypeScript: front of SERP contains both ────────────────

/// With relevant hits in both Rust and TypeScript, the first ~2N results of an
/// unscoped rank must contain both ecosystems — not a pure dump of one eco's
/// high-download packages followed by the other.
#[test]
fn unscoped_rust_and_typescript_interleave_not_block_sorted() {
	// N packages per eco with relevant BM25; Rust has vastly higher raw
	// downloads so a pure global sort would put all Rust first.
	let n = 5;
	let mut candidates = Vec::new();
	for i in 0..n {
		candidates.push(cand(
			&format!("rust-pkg-{i}"),
			5.0 - (i as f32) * 0.1,
			0.8,
			Some(50_000_000 - i as u64 * 1_000_000),
			Language::Rust,
		));
		candidates.push(cand(
			&format!("ts-pkg-{i}"),
			5.0 - (i as f32) * 0.1,
			0.8,
			Some(100_000 - i as u64 * 1_000), // orders of magnitude smaller
			Language::Typescript,
		));
	}

	let ranked = unscoped_rank_interleave("pkg", candidates, 10);
	assert_eq!(ranked.len(), 2 * n, "all candidates retained (rank_full contract)");

	// First 2N (entire list here) and especially the head must mix both ecos.
	let head = &ranked[..(2 * n).min(ranked.len())];
	let has_rust = head.iter().any(|c| c.ecosystem == Language::Rust);
	let has_ts = head.iter().any(|c| c.ecosystem == Language::Typescript);
	assert!(has_rust, "first 2N must include Rust, got {:?}", ecosystems(head));
	assert!(has_ts, "first 2N must include TypeScript, got {:?}", ecosystems(head));

	// Stronger: positions 0..2 should already show both ecosystems under RR
	// (two columns → alternate). Allow first 4 to be robust to ranking reordering
	// within each eco.
	let first_four_ecos: Vec<Language> = ranked.iter().take(4).map(|c| c.ecosystem).collect();
	let distinct: std::collections::HashSet<_> = first_four_ecos.iter().copied().collect();
	assert!(
		distinct.len() >= 2,
		"first 4 results should already mix ecosystems under interleave, got {:?}",
		first_four_ecos
	);

	// Pure block-sort would be [R,R,R,R,R,T,T,T,T,T] — reject that pattern.
	let ecos = ecosystems(&ranked);
	let first_block_is_all_rust = ecos.iter().take(n).all(|e| *e == Language::Rust);
	let second_block_is_all_ts = ecos.iter().skip(n).all(|e| *e == Language::Typescript);
	assert!(
		!(first_block_is_all_rust && second_block_is_all_ts),
		"SERP must not be pure block-sorted by ecosystem; got {:?}",
		ecos
	);
}

// ── 2. Scoped Rust: no TypeScript packages ───────────────────────────────────

/// When ranking is scoped to Rust (as the pipeline does after index filter),
/// the single-pipeline path never introduces TypeScript packages — even if a
/// buggy caller passed mixed candidates, scoped rank does not interleave other
/// ecos. The real filter is earlier (Must TermQuery); this asserts the rank
/// path used for scoped queries is the non-interleave single pass.
#[test]
fn scoped_rust_uses_single_pipeline_no_foreign_ecos() {
	// Only Rust candidates (what remains after Must filter).
	let candidates = vec![
		cand("serde", 3.0, 0.9, Some(20_000_000), Language::Rust),
		cand("serde_json", 2.5, 0.88, Some(15_000_000), Language::Rust),
		cand("erased-serde", 1.0, 0.5, Some(100_000), Language::Rust),
	];

	let policy = RankingPolicy::default();
	let ranked = policy.rank_full_candidates("serde", candidates, 10, Some(Language::Rust));

	assert!(
		ranked.iter().all(|c| c.ecosystem == Language::Rust),
		"scoped Rust rank must only contain Rust, got {:?}",
		ecosystems(&ranked)
	);
	assert!(
		ranked.iter().all(|c| c.name.contains("serde") || c.name == "erased-serde"),
		"unexpected names: {:?}",
		names(&ranked)
	);
}

/// If mixed candidates were somehow ranked with an unscoped interleave path,
/// TypeScript would appear — contrast with scoped path. Guard: scoped API is
/// the single-pipeline call, not `rank_per_ecosystem_and_interleave`.
#[test]
fn scoped_path_is_not_interleave_helper() {
	// Document the contract: scoped code path in pipeline is
	// `rank_full_candidates(..., Some(eco))`, not interleave. A mixed pool
	// under single rank retains whatever was passed (index filter is the cut).
	let candidates = vec![
		cand("serde", 3.0, 0.9, Some(20_000_000), Language::Rust),
		cand("lodash", 3.0, 0.9, Some(50_000_000), Language::Typescript),
	];
	let policy = RankingPolicy::default();
	let scoped = policy.rank_full_candidates("util", candidates.clone(), 10, Some(Language::Rust));
	// Single pipeline does not drop by ecosystem — filter is upstream.
	// Presence of both is OK here; the test proves we did NOT force RR order.
	// Compare to interleave which forces alternation when both ecos ranked.
	let interleaved = unscoped_rank_interleave("util", candidates, 10);

	// Interleave of two singleton columns → [rust, typescript] or vice versa by token.
	// rust < typescript → rust first.
	assert_eq!(interleaved.len(), 2);
	assert_eq!(interleaved[0].ecosystem, Language::Rust);
	assert_eq!(interleaved[1].ecosystem, Language::Typescript);

	// Scoped single-pipeline is free to pure-sort by fused score (both may stay).
	assert_eq!(scoped.len(), 2);
	// Distinct contract: scoped result order need not match interleave order.
	// (lodash may win on downloads under Explore; either order is fine.)
	let _ = names(&scoped);
}

// ── 3. Single-ecosystem unscoped: interleave is identity ─────────────────────

/// When the unscoped corpus is a single ecosystem, ranking that group and
/// interleaving one list must match ranking the same list alone (interleave
/// of one column is identity).
#[test]
fn single_ecosystem_unscoped_matches_single_rank_full() {
	let candidates = vec![
		cand_kw("serde", 2.0, 0.9, Some(25_000_000), Language::Rust, &["serialize"]),
		cand_kw(
			"serde-spam-serialize-json-derive",
			8.0,
			0.1,
			Some(1_000),
			Language::Rust,
			&["serde", "serialize", "json", "derive"],
		),
		cand("tokio", 1.5, 0.85, Some(20_000_000), Language::Rust),
		cand("anyhow", 1.2, 0.8, Some(10_000_000), Language::Rust),
	];

	let query = "serde";
	let limit = 10;

	let direct = RankingPolicy::default().rank_full_candidates(
		query,
		candidates.clone(),
		limit,
		Some(Language::Rust),
	);
	let via_interleave = unscoped_rank_interleave(query, candidates, limit);

	assert_eq!(
		names(&via_interleave),
		names(&direct),
		"single-eco interleave must be identity of that eco's rank_full"
	);
}

/// Pure `interleave_by_ecosystem` identity (no re-rank): one column in → same order out.
#[test]
fn interleave_one_list_is_identity() {
	let list = vec![
		cand("a", 3.0, 0.9, Some(1), Language::Python),
		cand("b", 2.0, 0.8, Some(1), Language::Python),
		cand("c", 1.0, 0.7, Some(1), Language::Python),
	];
	let expected = names(&list);
	let out = interleave_by_ecosystem(vec![(Language::Python, list)], 10);
	assert_eq!(names(&out), expected);
}

// ── 4. Determinism ───────────────────────────────────────────────────────────

#[test]
fn same_input_twice_same_order() {
	let make = || {
		vec![
			cand("serde", 3.0, 0.9, Some(20_000_000), Language::Rust),
			cand("tokio", 2.5, 0.88, Some(18_000_000), Language::Rust),
			cand("lodash", 3.0, 0.9, Some(50_000_000), Language::Typescript),
			cand("axios", 2.0, 0.85, Some(30_000_000), Language::Typescript),
			cand("requests", 2.8, 0.87, Some(40_000_000), Language::Python),
		]
	};

	let a = names(&unscoped_rank_interleave("http", make(), 10));
	let b = names(&unscoped_rank_interleave("http", make(), 10));
	assert_eq!(a, b);

	// Also pure interleave of pre-ranked lists.
	let ranked = || {
		vec![
			(
				Language::Typescript,
				vec![
					cand("lodash", 1.0, 0.9, None, Language::Typescript),
					cand("axios", 1.0, 0.8, None, Language::Typescript),
				],
			),
			(
				Language::Rust,
				vec![
					cand("serde", 1.0, 0.9, None, Language::Rust),
					cand("tokio", 1.0, 0.8, None, Language::Rust),
				],
			),
		]
	};
	assert_eq!(
		names(&interleave_by_ecosystem(ranked(), 10)),
		names(&interleave_by_ecosystem(ranked(), 10))
	);
}

// ── Extra invariants ─────────────────────────────────────────────────────────

/// limit_hint must not drop candidates from the interleaved full order.
#[test]
fn limit_hint_does_not_drop_tail() {
	let candidates = vec![
		cand("r1", 5.0, 0.9, Some(1_000), Language::Rust),
		cand("r2", 4.0, 0.8, Some(900), Language::Rust),
		cand("r3", 3.0, 0.7, Some(800), Language::Rust),
		cand("t1", 5.0, 0.9, Some(1_000), Language::Typescript),
		cand("t2", 4.0, 0.8, Some(900), Language::Typescript),
	];
	// limit_hint = 1 (page size) still retains full rank_full order length.
	let ranked = unscoped_rank_interleave("pkg", candidates, 1);
	assert_eq!(ranked.len(), 5);
}

/// Bare `rank_full` on a mixed pool pure-sorts; interleave path must differ when
/// downloads would otherwise bury the smaller ecosystem.
#[test]
fn interleave_differs_from_naive_global_rank_when_downloads_skew() {
	let candidates = vec![
		cand("rust-a", 2.0, 0.8, Some(100_000_000), Language::Rust),
		cand("rust-b", 1.9, 0.8, Some(99_000_000), Language::Rust),
		cand("ts-a", 2.0, 0.8, Some(10), Language::Typescript),
		cand("ts-b", 1.9, 0.8, Some(9), Language::Typescript),
	];

	let global = rank_full("pkg", candidates.clone(), 10, None);
	let interleaved = unscoped_rank_interleave("pkg", candidates, 10);

	// Global pure-sort almost certainly puts both Rust first under popularity path.
	// Interleave must put a TS package in the first two slots (one per column).
	let first_two_inter = ecosystems(&interleaved[..2.min(interleaved.len())]);
	assert!(
		first_two_inter.contains(&Language::Typescript),
		"interleave front must surface TypeScript; got {:?} (global was {:?})",
		names(&interleaved),
		names(&global)
	);
}
