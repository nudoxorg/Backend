//! nDCG-based ranking regression tests.
//!
//! Each golden fixture is a JSON array of [`eval::GoldenQuery`] containing
//! human relevance judgments and synthetic retrieval signals.  The harness
//! replays each pool through the full five-stage ranking pipeline and scores
//! the result with nDCG@10.
//!
//! CI gate: every ecosystem must reach `mean_ndcg(k=10) >= 0.95`.  If a query
//! scores badly, the `println!` table in `all_ecosystems_mean_and_table` names
//! it so regressions are diagnosable without reading raw fixture JSON.

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

// ── Cross-ecosystem table ─────────────────────────────────────────────────────

/// Print a per-query nDCG table across all three ecosystems and assert on the
/// overall mean.  The `println!` output surfaces in `cargo test -- --nocapture`
/// so any single query causing a regression is immediately visible.
#[test]
fn all_ecosystems_mean_and_table() {
	let ecosystems: &[(&str, &str)] = &[
		("rust",       RUST_JSON),
		("typescript", TYPESCRIPT_JSON),
		("python",     PYTHON_JSON),
	];

	let mut all_scores: Vec<f64> = Vec::new();

	println!("\n{:<20} {:<40} {:>10}", "ecosystem", "query", "nDCG@10");
	println!("{}", "-".repeat(74));

	for &(name, json) in ecosystems {
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

// ── Fixture lint ──────────────────────────────────────────────────────────────

/// Every judged package name must appear in the pool for its query.
/// This prevents silent fixture bugs where a judgment names a package
/// that was never included in the candidate pool (and therefore can never
/// influence the ranking, making the ideal DCG unachievable).
#[test]
fn fixture_lint_all_judged_names_in_pool() {
	let all: &[(&str, &str)] = &[
		("rust",       RUST_JSON),
		("typescript", TYPESCRIPT_JSON),
		("python",     PYTHON_JSON),
	];

	let mut failures: Vec<String> = Vec::new();

	for &(eco, json) in all {
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
