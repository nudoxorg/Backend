//! Cross-ecosystem SERP interleave.
//!
//! Unscoped package search ranks each ecosystem **independently** (so
//! popularity / downloads never compete on one global scale), then merges the
//! per-eco orders into a single SERP via deterministic round-robin.
//!
//! Scoped search skips this module and keeps a single [`super::policy::RankingPolicy`]
//! pass over the already-filtered candidate pool.

use heart::ecosystem::Language;

use super::ranking::Candidate;

/// Round-robin interleave of per-ecosystem ranked candidate lists into one SERP.
///
/// # Ordering
/// - Ecosystem columns are ordered by [`Language::as_token`] (stable wire token).
/// - Within each ecosystem, candidate order is preserved (caller already ranked).
/// - Round-robin: take one head from each non-empty column per pass until all
///   columns are exhausted.
///
/// # `limit_hint`
/// Used only as a fairness sizing hint for how deep each ecosystem should be
/// allowed to contribute early in the merge. The final list is **not** truncated
/// to `limit_hint` — full retention matches the `rank_full` / keyset-pagination
/// contract (the tail is still interleaved, not pure-sorted).
///
/// Currently each ecosystem contributes its full ranked list via fair
/// round-robin; `limit_hint` is accepted so callers can pass the page-size
/// tuning value without a second API, and so a future quality-aware interleave
/// can bias take-depth without changing the call site.
///
/// # Determinism
/// Pure and deterministic: same inputs always yield the same order (no RNG, no
/// timestamps, stable ecosystem key order).
pub fn interleave_by_ecosystem<T>(
	mut ranked_per_eco: Vec<(Language, Vec<Candidate<T>>)>,
	limit_hint: usize,
) -> Vec<Candidate<T>> {
	let _fairness_hint = limit_hint;

	// Drop empty columns; sort remaining by stable wire token.
	ranked_per_eco.retain(|(_, list)| !list.is_empty());
	ranked_per_eco.sort_by(|(a, _), (b, _)| a.as_token().cmp(b.as_token()));

	if ranked_per_eco.is_empty() {
		return Vec::new();
	}

	// Reverse each list so `pop()` yields the original head (best first).
	for (_, list) in &mut ranked_per_eco {
		list.reverse();
	}

	let total: usize = ranked_per_eco.iter().map(|(_, list)| list.len()).sum();
	let mut out = Vec::with_capacity(total);

	loop {
		let mut progressed = false;
		for (_, list) in &mut ranked_per_eco {
			if let Some(candidate) = list.pop() {
				out.push(candidate);
				progressed = true;
			}
		}
		if !progressed {
			break;
		}
	}

	out
}

/// Partition candidates by ecosystem, rank each group with `rank_group`, then
/// interleave. Empty groups are skipped. Ecosystem order after interleave is
/// determined solely by [`interleave_by_ecosystem`].
///
/// `rank_group` receives `(ecosystem, candidates_for_eco)` and must return a
/// fully ordered list for that ecosystem (typically
/// [`super::policy::RankingPolicy::rank_full_candidates`] with
/// `ecosystem_scope = Some(eco)`).
pub fn rank_per_ecosystem_and_interleave<T, F>(
	candidates: Vec<Candidate<T>>,
	limit_hint: usize,
	mut rank_group: F,
) -> Vec<Candidate<T>>
where
	F: FnMut(Language, Vec<Candidate<T>>) -> Vec<Candidate<T>>,
{
	use std::collections::HashMap;

	if candidates.is_empty() {
		return Vec::new();
	}

	let mut by_eco: HashMap<Language, Vec<Candidate<T>>> = HashMap::new();
	for candidate in candidates {
		by_eco.entry(candidate.ecosystem).or_default().push(candidate);
	}

	let ranked_per_eco: Vec<(Language, Vec<Candidate<T>>)> = by_eco
		.into_iter()
		.map(|(eco, group)| (eco, rank_group(eco, group)))
		.collect();

	interleave_by_ecosystem(ranked_per_eco, limit_hint)
}

#[cfg(test)]
mod tests {
	use super::*;
	use smol_str::SmolStr;

	fn cand(name: &str, eco: Language) -> Candidate<&'static str> {
		Candidate {
			item: "payload",
			name: name.to_owned(),
			bm25: 1.0,
			quality: 0.5,
			downloads: None,
			dependents: None,
			popularity_pct: None,
			withdrawn: false,
			squat_suspect: false,
			malware: false,
			verified_repo: false,
			ecosystem: eco,
			keywords: Vec::<SmolStr>::new(),
		}
	}

	fn names(result: &[Candidate<&'static str>]) -> Vec<String> {
		result.iter().map(|c| c.name.clone()).collect()
	}

	#[test]
	fn empty_input_yields_empty() {
		let out = interleave_by_ecosystem::<()>(vec![], 10);
		assert!(out.is_empty());
	}

	#[test]
	fn single_ecosystem_is_identity() {
		let rust = vec![
			cand("serde", Language::Rust),
			cand("tokio", Language::Rust),
			cand("anyhow", Language::Rust),
		];
		let expected: Vec<String> = rust.iter().map(|c| c.name.clone()).collect();
		let out = interleave_by_ecosystem(vec![(Language::Rust, rust)], 10);
		assert_eq!(names(&out), expected);
	}

	#[test]
	fn round_robin_alternates_ecosystems() {
		// csharp < go < rust < typescript by as_token.
		let ranked = vec![
			(
				Language::Typescript,
				vec![cand("lodash", Language::Typescript), cand("axios", Language::Typescript)],
			),
			(
				Language::Rust,
				vec![cand("serde", Language::Rust), cand("tokio", Language::Rust)],
			),
		];
		let out = interleave_by_ecosystem(ranked, 10);
		// rust token < typescript → rust first column.
		assert_eq!(
			names(&out),
			vec!["serde", "lodash", "tokio", "axios"]
				.into_iter()
				.map(str::to_owned)
				.collect::<Vec<_>>()
		);
	}

	#[test]
	fn longer_column_drains_after_shorter_exhausted() {
		let ranked = vec![
			(
				Language::Rust,
				vec![
					cand("a", Language::Rust),
					cand("b", Language::Rust),
					cand("c", Language::Rust),
				],
			),
			(Language::Python, vec![cand("x", Language::Python)]),
		];
		// python < rust by token → python first each round.
		let out = interleave_by_ecosystem(ranked, 5);
		assert_eq!(
			names(&out),
			vec!["x", "a", "b", "c"]
				.into_iter()
				.map(str::to_owned)
				.collect::<Vec<_>>()
		);
	}

	#[test]
	fn ecosystem_order_is_by_as_token_not_input_order() {
		let ranked = vec![
			(Language::Typescript, vec![cand("ts1", Language::Typescript)]),
			(Language::Go, vec![cand("go1", Language::Go)]),
			(Language::Rust, vec![cand("rs1", Language::Rust)]),
		];
		// go < rust < typescript
		let out = interleave_by_ecosystem(ranked, 3);
		assert_eq!(
			names(&out),
			vec!["go1", "rs1", "ts1"]
				.into_iter()
				.map(str::to_owned)
				.collect::<Vec<_>>()
		);
	}

	#[test]
	fn limit_hint_does_not_truncate_final() {
		let ranked = vec![
			(
				Language::Rust,
				vec![cand("r1", Language::Rust), cand("r2", Language::Rust), cand("r3", Language::Rust)],
			),
			(
				Language::Go,
				vec![cand("g1", Language::Go), cand("g2", Language::Go)],
			),
		];
		// limit_hint = 1 must still return all 5 candidates (rank_full contract).
		let out = interleave_by_ecosystem(ranked, 1);
		assert_eq!(out.len(), 5);
	}

	#[test]
	fn determinism_same_input_twice() {
		let make = || {
			vec![
				(
					Language::Typescript,
					vec![cand("t1", Language::Typescript), cand("t2", Language::Typescript)],
				),
				(
					Language::Rust,
					vec![cand("r1", Language::Rust), cand("r2", Language::Rust)],
				),
			]
		};
		let a = names(&interleave_by_ecosystem(make(), 10));
		let b = names(&interleave_by_ecosystem(make(), 10));
		assert_eq!(a, b);
	}
}
