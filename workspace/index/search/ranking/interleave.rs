//! Cross-ecosystem SERP interleave.
//!
//! Unscoped package search ranks each ecosystem **independently** (so
//! popularity / downloads never compete on one global scale), then merges the
//! per-eco orders into a single SERP via deterministic round-robin.
//!
//! Scoped search skips this module and keeps a single [`super::policy::RankingPolicy`]
//! pass over the already-filtered candidate pool.

use heart::ecosystem::Language;

use super::cascade::Candidate;

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
	ranked_per_eco: Vec<(Language, Vec<Candidate<T>>)>,
	limit_hint: usize,
) -> Vec<Candidate<T>> {
	let _fairness_hint = limit_hint;

	// Key columns by the stable wire token (not `Language`'s derived `Ord`,
	// which is declaration order, not alphabetical) so column order here
	// matches every other ecosystem-keyed ordering in the codebase.
	let columns = ranked_per_eco
		.into_iter()
		.map(|(eco, list)| (eco.as_token(), list))
		.collect();

	interleave_columns(columns)
}

/// Round-robin merge of already-ranked, already-partitioned columns into one
/// list: columns are visited in ascending key order, one head taken from each
/// non-empty column per pass, until every column is exhausted.
///
/// This is the fairness-merge core behind [`interleave_by_ecosystem`], lifted
/// out and made generic over both the column key and the item type so other
/// ranking planes can reuse the exact same deterministic round-robin without
/// depending on the package plane's [`Candidate<T>`] shape. The semantic
/// (symbol) search plane's unscoped cross-language interleave
/// (`index::server::search::semantic`) is the other caller.
///
/// # Ordering
/// - Columns are ordered by `K`'s `Ord` (callers pass a stable, meaningful key
///   — e.g. [`Language::as_token`] — not an incidental one).
/// - Within each column, item order is preserved (caller already ranked it).
///
/// # Determinism
/// Pure and deterministic: same inputs always yield the same order (no RNG, no
/// timestamps, stable key order).
pub fn interleave_columns<K: Ord, T>(mut columns: Vec<(K, Vec<T>)>) -> Vec<T> {
	// Drop empty columns; sort remaining by key.
	columns.retain(|(_, list)| !list.is_empty());
	columns.sort_by(|(a, _), (b, _)| a.cmp(b));

	if columns.is_empty() {
		return Vec::new();
	}

	// Reverse each list so `pop()` yields the original head (best first).
	for (_, list) in &mut columns {
		list.reverse();
	}

	let total: usize = columns.iter().map(|(_, list)| list.len()).sum();
	let mut out = Vec::with_capacity(total);

	loop {
		let mut progressed = false;
		for (_, list) in &mut columns {
			if let Some(item) = list.pop() {
				out.push(item);
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

	// `interleave_columns` is the generic core other planes (the semantic
	// symbol search's unscoped cross-language merge) reuse directly, over
	// their own item type and key — exercised here with plain `&str` items
	// keyed by a `String` language token, standing in for
	// `Scored<SymbolId>` keyed by the qdrant `language` payload value, to
	// prove the fairness merge holds with no dependency on `Candidate<T>`.

	#[test]
	fn interleave_columns_empty_is_empty() {
		let out: Vec<&str> = interleave_columns(Vec::<(String, Vec<&str>)>::new());
		assert!(out.is_empty());
	}

	#[test]
	fn interleave_columns_round_robins_by_key_order_not_input_order() {
		// One language ("python") would dominate a raw score-sorted merge if
		// its items simply happened to embed closer to the query text; the
		// bucket-then-interleave contract guarantees every language gets a
		// head-of-column slot before any language's second item appears.
		let columns = vec![
			(
				"python".to_owned(),
				vec!["py_1", "py_2", "py_3", "py_4", "py_5"],
			),
			("rust".to_owned(), vec!["rs_1", "rs_2"]),
			("go".to_owned(), vec!["go_1"]),
		];
		let out = interleave_columns(columns);
		// go < python < rust by key order.
		assert_eq!(
			out,
			vec!["go_1", "py_1", "rs_1", "py_2", "rs_2", "py_3", "py_4", "py_5"]
		);
	}

	#[test]
	fn interleave_columns_preserves_within_column_order() {
		// Within a bucket, item order must be exactly what the caller ranked
		// it as (e.g. the qdrant score order for that language) — the merge
		// only ever reorders *across* buckets, never within one.
		let columns = vec![
			("a".to_owned(), vec![1, 2, 3]),
			("b".to_owned(), vec![10, 20]),
		];
		let out = interleave_columns(columns);
		assert_eq!(out, vec![1, 10, 2, 20, 3]);
	}

	#[test]
	fn interleave_columns_single_column_is_identity() {
		let columns = vec![("only".to_owned(), vec!["a", "b", "c"])];
		let out = interleave_columns(columns);
		assert_eq!(out, vec!["a", "b", "c"]);
	}

	#[test]
	fn interleave_by_ecosystem_matches_interleave_columns_on_tokens() {
		// Regression guard for the refactor: `interleave_by_ecosystem` must
		// still behave exactly as it did before it became a thin wrapper
		// around `interleave_columns`.
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
		assert_eq!(
			names(&out),
			vec!["serde", "lodash", "tokio", "axios"]
				.into_iter()
				.map(str::to_owned)
				.collect::<Vec<_>>()
		);
	}
}
