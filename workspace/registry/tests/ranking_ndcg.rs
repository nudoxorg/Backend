//! nDCG-based ranking regression tests.
//!
//! Each golden fixture is a JSON array of [`eval::GoldenQuery`] containing
//! human relevance judgments and synthetic retrieval signals.  The harness
//! replays each pool through the full five-stage ranking pipeline and scores
//! the result with nDCG@10.
//!
//! CI gate: every primary ecosystem (rust / typescript / python) must reach
//! `mean_ndcg(k=10) >= 0.95`.  Go and Java use the same gate once their
//! fixtures are winnable; if an experimental ecosystem dips, give it a named
//! lower gate rather than silently weakening the primary set.
//!
//! If a query scores badly, the `println!` table in `all_ecosystems_mean_and_table`
//! names it so regressions are diagnosable without reading raw fixture JSON.
//!
//! Authoring guide (query classes, gain scale, spam-never-gain-3):
//! `tests/fixtures/golden_queries/README.md`.

use registry::search::eval::{GoldenQuery, mean_ndcg, evaluate};

// ── Fixture loading ───────────────────────────────────────────────────────────

fn load(json: &str) -> Vec<GoldenQuery> {
	serde_json::from_str(json).expect("fixture must deserialise as Vec<GoldenQuery>")
}

static RUST_JSON: &str =
	include_str!("fixtures/golden_queries/rust.json");
static TYPESCRIPT_JSON: &str =
	include_str!("fixtures/golden_queries/typescript.json");
static PYTHON_JSON: &str =
	include_str!("fixtures/golden_queries/python.json");
static GO_JSON: &str =
	include_str!("fixtures/golden_queries/go.json");
static JAVA_JSON: &str =
	include_str!("fixtures/golden_queries/java.json");

/// Primary ecosystems that must hold the hard 0.95 mean nDCG@10 gate.
const PRIMARY: &[(&str, &str)] = &[
	("rust",       RUST_JSON),
	("typescript", TYPESCRIPT_JSON),
	("python",     PYTHON_JSON),
];

/// Additional ecosystems included in the multi-eco suite (same gate while
/// fixtures remain winnable; demote to a lower experimental gate if needed).
const EXTENDED: &[(&str, &str)] = &[
	("go",   GO_JSON),
	("java", JAVA_JSON),
];

fn all_ecosystems() -> Vec<(&'static str, &'static str)> {
	PRIMARY.iter().chain(EXTENDED.iter()).copied().collect()
}

// ── Per-ecosystem gate tests ──────────────────────────────────────────────────

/// Rust fixture: mean nDCG@10 must be at least 0.95.
#[test]
fn rust_mean_ndcg_gate() {
	let queries = load(RUST_JSON);
	let score = mean_ndcg(&queries, 10);
	println!("rust mean nDCG@10 = {score:.3}");
	assert!(
		score >= 0.95,
		"rust mean nDCG@10 = {score:.3} < 0.95 — ranking regressed or fixture is unwinnable"
	);
}

/// TypeScript fixture: mean nDCG@10 must be at least 0.95.
#[test]
fn typescript_mean_ndcg_gate() {
	let queries = load(TYPESCRIPT_JSON);
	let score = mean_ndcg(&queries, 10);
	println!("typescript mean nDCG@10 = {score:.3}");
	assert!(
		score >= 0.95,
		"typescript mean nDCG@10 = {score:.3} < 0.95 — ranking regressed or fixture is unwinnable"
	);
}

/// Python fixture: mean nDCG@10 must be at least 0.95.
#[test]
fn python_mean_ndcg_gate() {
	let queries = load(PYTHON_JSON);
	let score = mean_ndcg(&queries, 10);
	println!("python mean nDCG@10 = {score:.3}");
	assert!(
		score >= 0.95,
		"python mean nDCG@10 = {score:.3} < 0.95 — ranking regressed or fixture is unwinnable"
	);
}

/// Go fixture: mean nDCG@10 must be at least 0.95 (same bar as primary ecos
/// while the synthetic pool stays winnable).
#[test]
fn go_mean_ndcg_gate() {
	let queries = load(GO_JSON);
	let score = mean_ndcg(&queries, 10);
	println!("go mean nDCG@10 = {score:.3}");
	assert!(
		score >= 0.95,
		"go mean nDCG@10 = {score:.3} < 0.95 — ranking regressed or fixture is unwinnable"
	);
}

/// Java fixture: mean nDCG@10 must be at least 0.95.
#[test]
fn java_mean_ndcg_gate() {
	let queries = load(JAVA_JSON);
	let score = mean_ndcg(&queries, 10);
	println!("java mean nDCG@10 = {score:.3}");
	assert!(
		score >= 0.95,
		"java mean nDCG@10 = {score:.3} < 0.95 — ranking regressed or fixture is unwinnable"
	);
}

// ── Cross-ecosystem table ─────────────────────────────────────────────────────

/// Print a per-query nDCG table across all ecosystems and assert on the
/// overall mean.  The `println!` output surfaces in `cargo test -- --nocapture`
/// so any single query causing a regression is immediately visible.
#[test]
fn all_ecosystems_mean_and_table() {
	let ecosystems = all_ecosystems();

	let mut all_scores: Vec<f64> = Vec::new();

	println!("\n{:<20} {:<40} {:>10}", "ecosystem", "query", "nDCG@10");
	println!("{}", "-".repeat(74));

	for (name, json) in &ecosystems {
		let queries = load(json);
		for q in &queries {
			let score = evaluate(q, 10);
			all_scores.push(score);
			println!("{:<20} {:<40} {:>10.3}", name, q.query, score);
		}
		let eco_mean = mean_ndcg(&queries, 10);
		println!("{:<20} {:<40} {:>10.3}  ← ecosystem mean", name, "(mean)", eco_mean);
		println!();
	}

	let overall = all_scores.iter().sum::<f64>() / all_scores.len() as f64;
	println!("overall mean nDCG@10 across all ecosystems = {overall:.3}");

	assert!(
		overall >= 0.95,
		"overall mean nDCG@10 = {overall:.3} < 0.95"
	);
}

// ── Fixture lints ─────────────────────────────────────────────────────────────

/// Every judged package name must appear in the pool for its query.
/// This prevents silent fixture bugs where a judgment names a package
/// that was never included in the candidate pool (and therefore can never
/// influence the ranking, making the ideal DCG unachievable).
#[test]
fn fixture_lint_all_judged_names_in_pool() {
	let mut failures: Vec<String> = Vec::new();

	for (eco, json) in all_ecosystems() {
		let queries = load(json);
		for query in &queries {
			let pool_names: std::collections::HashSet<&str> =
				query.pool.iter().map(|p| p.name.as_str()).collect();
			for judgment in &query.judgments {
				if !pool_names.contains(judgment.name.as_str()) {
					failures.push(format!(
						"[{eco}] query {:?}: judged name {:?} not in pool",
						query.query, judgment.name
					));
				}
			}
		}
	}

	if !failures.is_empty() {
		panic!(
			"fixture lint failed — judged names missing from pool:\n{}",
			failures.join("\n")
		);
	}
}

/// Spam / land-grab packages must never carry gain 3 in judgments.
///
/// Encode the quality invariant at the fixture layer: a package that is the
/// land-grab (low quality, keyword-stuffed, or named for the query without
/// being the real package) must not be graded as the canonical answer.
#[test]
fn fixture_lint_spam_never_gets_gain_three() {
	// Heuristic: names containing "spam" or "landgrab" (case-insensitive) are
	// fixture-authored distractors and must have gain 0 (or at most 1 if they
	// are merely weak alternatives — never the canonical gain 3).
	let mut failures: Vec<String> = Vec::new();

	for (eco, json) in all_ecosystems() {
		let queries = load(json);
		for query in &queries {
			for judgment in &query.judgments {
				let lower = judgment.name.to_ascii_lowercase();
				let is_spam_marker =
					lower.contains("spam") || lower.contains("landgrab");
				if is_spam_marker && judgment.gain >= 3 {
					failures.push(format!(
						"[{eco}] query {:?}: spam/landgrab {:?} has gain {}",
						query.query, judgment.name, judgment.gain
					));
				}
			}
		}
	}

	if !failures.is_empty() {
		panic!(
			"fixture lint failed — spam must not receive gain 3:\n{}",
			failures.join("\n")
		);
	}
}

/// Explore-style multiword queries should include at least one gain-3 blessed
/// package and at least one gain-0 distractor so the quality-kink path is
/// exercised by the nDCG gate (not only by unit tests).
#[test]
fn fixture_lint_explore_queries_have_blessed_and_spam() {
	let explore_markers = ["http client", "yaml parser", "web framework"];
	let mut failures: Vec<String> = Vec::new();

	for (eco, json) in all_ecosystems() {
		let queries = load(json);
		for query in &queries {
			let q_lower = query.query.to_ascii_lowercase();
			if !explore_markers.iter().any(|m| q_lower == *m) {
				continue;
			}
			let has_blessed = query.judgments.iter().any(|j| j.gain == 3);
			let has_spam = query.judgments.iter().any(|j| j.gain == 0);
			if !has_blessed {
				failures.push(format!(
					"[{eco}] explore query {:?} missing gain-3 blessed package",
					query.query
				));
			}
			if !has_spam {
				failures.push(format!(
					"[{eco}] explore query {:?} missing gain-0 spam/distractor judgment",
					query.query
				));
			}
		}
	}

	if !failures.is_empty() {
		panic!(
			"fixture lint failed — explore queries need blessed + spam:\n{}",
			failures.join("\n")
		);
	}
}
