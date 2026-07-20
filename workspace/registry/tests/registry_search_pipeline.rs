//! Adversarial / enforcement tests for the single package-search pipeline
//! ([`registry::search::retrieve_and_rank`] /
//! [`registry::search::retrieve_and_rank_page`]).
//!
//! These prove that discovery always goes through structured parse + multi-stage
//! rank (exact-name bonus, ecosystem filter, synonym expansion, AllQuery filters,
//! keyset seamlessness) and cannot be bypassed by callers using the public API.

mod common;

use std::io::Write as _;

use heart::{Language, ResolutionState};
use registry::{
	GlobalPackage,
	metadata::SearchFacets,
	search::{
		PackageSearchDeps, PackageSearchRequest, SearchKey, retrieve_and_rank,
		retrieve_and_rank_page, search_page, tantivy::PackageIndex,
	},
};
use smol_str::SmolStr;

// ── Fixtures ────────────────────────────────────────────────────────────────

fn request(text: &str) -> PackageSearchRequest {
	PackageSearchRequest {
		text: text.to_owned(),
		ecosystem: None,
		limit: 10,
		after: None,
		semantic: Vec::new(),
	}
}

fn rust_record(name: &str) -> GlobalPackage {
	common::global_package(
		common::rust_package(name, "1.0.0"),
		ResolutionState::Unindexed { needed: false },
	)
}

fn rust_record_with_facets(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
	let package = common::rust_package(name, "1.0.0");
	let id = package.id();
	let facets = Some(SearchFacets {
		keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
		quality_ppm,
		..Default::default()
	});
	GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
}

fn record_with_facets_v3(
	package: registry::Package,
	description: &str,
	keywords: &[&str],
	deps: &[&str],
	license: Option<&str>,
) -> GlobalPackage {
	let id = package.id();
	let facets = Some(SearchFacets {
		keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
		quality_ppm: 600_000,
		description: Some(SmolStr::new(description)),
		dependencies: deps.iter().map(|&d| SmolStr::new(d)).collect(),
		license: license.map(SmolStr::new),
		..Default::default()
	});
	GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
}

fn searchable_index(directory: &common::TempDir, records: &[GlobalPackage]) -> PackageIndex {
	let mut index = PackageIndex::open(directory.path()).expect("a tempdir replica opens");
	index.absorb(records.iter(), 1).expect("records absorb into the replica");
	index
}

fn make_synonyms(csv: &str) -> registry::metadata::Synonyms {
	let dir = tempfile::tempdir().expect("tempdir for synonyms");
	let path = dir.path().join("tag-synonyms.csv");
	{
		let mut f = std::fs::File::create(&path).expect("create synonyms csv");
		f.write_all(csv.as_bytes()).expect("write synonyms csv");
	}
	// Synonyms owns its data; tempdir can drop after load.
	let synonyms = registry::metadata::Synonyms::new(dir.path()).expect("Synonyms::new");
	// Keep dir alive until after load by dropping after return path — data is owned.
	drop(dir);
	synonyms
}

// ── Exact name vs spam ──────────────────────────────────────────────────────

/// Exact-name match outranks a higher-BM25 keyword-spam / contains match.
///
/// Arrange: "tokio" (exact), "tokio-extended" (contains + keyword spam), "serde"
/// (high quality, unrelated). Query "tokio".
///
/// Assert: "tokio" ranks first via the pipeline name bonus.
#[tokio::test]
async fn pipeline_exact_name_wins_over_spam() {
	let directory = common::TempDir::new("pipeline-exact-name");
	let tokio_ext =
		rust_record_with_facets("tokio-extended", 600_000, &["async", "tokio", "runtime"]);
	let tokio_exact = rust_record_with_facets("tokio", 500_000, &["async", "runtime"]);
	let unrelated = rust_record_with_facets("serde", 900_000, &["serialization", "json"]);

	let index = searchable_index(&directory, &[tokio_ext, tokio_exact, unrelated]);

	let hits = retrieve_and_rank(
		&index,
		&request("tokio"),
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("pipeline executes");

	let names: Vec<&str> = hits
		.iter()
		.map(|s| s.value.package.coordinates.name.original())
		.collect();
	assert!(
		names.first().copied() == Some("tokio"),
		"exact-name match must rank first; order was: {names:?}"
	);

	let tokio_pos = names.iter().position(|&n| n == "tokio").unwrap_or(usize::MAX);
	let serde_pos = names.iter().position(|&n| n == "serde").unwrap_or(usize::MAX);
	assert!(
		tokio_pos < serde_pos,
		"exact-match 'tokio' (pos {tokio_pos}) must outrank unrelated 'serde' (pos {serde_pos})"
	);
}

// ── Ecosystem filter ────────────────────────────────────────────────────────

/// `lang:rust serde` / API ecosystem scope returns only Rust packages when the
/// index holds multiple ecosystems.
#[tokio::test]
async fn pipeline_lang_rust_filter_excludes_other_ecosystems() {
	let directory = common::TempDir::new("pipeline-lang-rust");
	let rust_serde = rust_record_with_facets("serde", 800_000, &["serialization", "serde"]);
	let py_serde = record_with_facets_v3(
		common::python_package("serde", "1.0.0"),
		"Python serde-like package",
		&["serialization", "serde"],
		&[],
		None,
	);
	let go_mux = record_with_facets_v3(
		common::go_package("github.com/gorilla/mux", "v1.8.1"),
		"HTTP router",
		&["http", "router", "mux"],
		&[],
		None,
	);
	let index =
		searchable_index(&directory, &[rust_serde.clone(), py_serde.clone(), go_mux]);

	// Inline lang: token
	let hits = retrieve_and_rank(
		&index,
		&request("lang:rust serde"),
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("lang:rust query executes");
	assert!(
		!hits.is_empty(),
		"lang:rust serde must recall the Rust serde package"
	);
	assert!(
		hits.iter().all(|h| h.value.package.coordinates.ecosystem() == Language::Rust),
		"all hits must be Rust: {:?}",
		hits.iter().map(|h| h.value.package.coordinates.ecosystem()).collect::<Vec<_>>()
	);
	assert!(
		hits.iter().any(|h| h.value.id == rust_serde.id),
		"Rust serde must appear"
	);
	assert!(
		!hits.iter().any(|h| h.value.id == py_serde.id),
		"Python serde must be excluded by lang:rust"
	);

	// API-level ecosystem scope
	let mut api_req = request("serde");
	api_req.ecosystem = Some(Language::Rust);
	let api_hits = retrieve_and_rank(
		&index,
		&api_req,
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("API-scoped query executes");
	assert!(
		api_hits.iter().all(|h| h.value.package.coordinates.ecosystem() == Language::Rust),
		"API ecosystem scope must restrict to Rust"
	);
}

// ── Synonyms ────────────────────────────────────────────────────────────────

/// With synonyms deps, an expanded query recalls a keyword-only package; without
/// synonyms the same query misses it.
#[tokio::test]
async fn pipeline_synonyms_expand_recalls_keyword_only_package() {
	let directory = common::TempDir::new("pipeline-synonyms");
	// Package named "reqwest" with keyword "reqwest" only — query "http-client"
	// has no direct name/keyword/description overlap unless expanded to "reqwest".
	// (Description must NOT contain http/client tokens or the subtoken AND tier
	// recalls without synonyms.)
	let reqwest_pkg = record_with_facets_v3(
		common::rust_package("reqwest", "0.12.0"),
		"ergonomic network library",
		&["reqwest"],
		&[],
		Some("mit"),
	);
	let unrelated = record_with_facets_v3(
		common::rust_package("rayon", "1.0.0"),
		"Data parallelism library",
		&["parallel", "rayon"],
		&[],
		None,
	);
	let index = searchable_index(&directory, &[reqwest_pkg.clone(), unrelated]);

	let synonyms = make_synonyms("http-client,reqwest,4\n");
	let req = request("http-client");

	// Without synonyms: should miss (no name/keyword token "http-client").
	let without = retrieve_and_rank(
		&index,
		&req,
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("plain query executes");
	assert!(
		!without.iter().any(|h| h.value.id == reqwest_pkg.id),
		"without synonyms, 'http-client' must not recall reqwest; hits: {:?}",
		without.iter().map(|h| h.value.package.coordinates.name.original()).collect::<Vec<_>>()
	);

	// With synonyms: expanded tier recalls via keyword "reqwest".
	let with = retrieve_and_rank(
		&index,
		&req,
		PackageSearchDeps { synonyms: Some(&synonyms), ..Default::default() },
	)
	.await
	.expect("synonym-expanded pipeline executes");
	assert!(
		with.iter().any(|h| h.value.id == reqwest_pkg.id),
		"with synonyms, 'http-client' → reqwest must recall the package; hits: {:?}",
		with.iter().map(|h| h.value.package.coordinates.name.original()).collect::<Vec<_>>()
	);
}

// ── Adversarial: filter-only / AllQuery ─────────────────────────────────────

/// Empty free terms with only `license:mit` (AllQuery + Must filter) must not
/// panic and must return MIT packages.
#[tokio::test]
async fn pipeline_license_only_allquery_does_not_panic() {
	let directory = common::TempDir::new("pipeline-license-only");
	let mit_pkg = record_with_facets_v3(
		common::rust_package("mit-only", "1.0.0"),
		"MIT licensed",
		&[],
		&[],
		Some("mit"),
	);
	let gpl_pkg = record_with_facets_v3(
		common::rust_package("gpl-only", "1.0.0"),
		"GPL licensed",
		&[],
		&[],
		Some("gpl-3.0"),
	);
	let index = searchable_index(&directory, &[mit_pkg.clone(), gpl_pkg.clone()]);

	let hits = retrieve_and_rank(
		&index,
		&request("license:mit"),
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("license-only AllQuery path must not panic");
	assert!(
		hits.iter().any(|h| h.value.id == mit_pkg.id),
		"MIT package must appear in license-only query"
	);
	assert!(
		!hits.iter().any(|h| h.value.id == gpl_pkg.id),
		"GPL package must be excluded"
	);
}

/// `dep:` filter with no matches returns empty, not an error.
#[tokio::test]
async fn pipeline_dep_filter_no_matches_returns_empty() {
	let directory = common::TempDir::new("pipeline-dep-empty");
	let pkg = record_with_facets_v3(
		common::rust_package("sync-worker", "1.0.0"),
		"Sync task worker",
		&["sync"],
		&["rayon"],
		None,
	);
	let index = searchable_index(&directory, &[pkg]);

	let hits = retrieve_and_rank(
		&index,
		&request("dep:nonexistent-crate-xyz"),
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("dep: with no matches must return Ok(empty), not Err");
	assert!(
		hits.is_empty(),
		"no-match dep: filter must yield empty hits, got {}",
		hits.len()
	);
}

// ── Pagination seamlessness ─────────────────────────────────────────────────

/// Page1 + page2 concat equals full order for a small multi-hit corpus.
#[tokio::test]
async fn pipeline_pagination_page1_plus_page2_equals_full_order() {
	let directory = common::TempDir::new("pipeline-pagination");

	// Small but multi-page corpus, shared keyword so all match.
	let records: Vec<GlobalPackage> = (0..12_u32)
		.map(|i| {
			let quality_ppm = (i * 80_000) % 1_000_000;
			let extra = match i % 3 {
				0 => "runtime",
				1 => "executor",
				_ => "futures",
			};
			rust_record_with_facets(&format!("pkg{i:02}"), quality_ppm, &["async", extra])
		})
		.collect();
	let index = searchable_index(&directory, &records);

	let full = retrieve_and_rank(
		&index,
		&PackageSearchRequest {
			text: "async".to_owned(),
			ecosystem: None,
			limit: 100,
			after: None,
			semantic: Vec::new(),
		},
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("full order");
	let full_ids: Vec<_> = full.iter().map(|s| s.value.id).collect();
	assert!(full_ids.len() >= 6, "need multi-page corpus; got {}", full_ids.len());

	// Page 1
	let page1 = retrieve_and_rank_page(
		&index,
		&PackageSearchRequest {
			text: "async".to_owned(),
			ecosystem: None,
			limit: 4,
			after: None,
			semantic: Vec::new(),
		},
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("page 1");
	assert_eq!(page1.items.len(), 4, "page 1 must fill limit");
	let page1_ids: Vec<_> = page1.items.iter().map(|s| s.value.id).collect();
	let next_token = page1.next.expect("page 1 must have a continuation");

	// Page 2
	let cursor = heart::Cursor::<SearchKey>::decode(&next_token).expect("cursor round-trips");
	let page2 = retrieve_and_rank_page(
		&index,
		&PackageSearchRequest {
			text: "async".to_owned(),
			ecosystem: None,
			limit: 4,
			after: Some(cursor),
			semantic: Vec::new(),
		},
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("page 2");
	let page2_ids: Vec<_> = page2.items.iter().map(|s| s.value.id).collect();

	let stitched: Vec<_> = page1_ids.iter().chain(page2_ids.iter()).copied().collect();
	assert_eq!(
		stitched,
		full_ids[..stitched.len()],
		"page1+page2 concat must equal the full-pipeline order prefix"
	);

	// Disjoint pages
	let mut seen = std::collections::HashSet::new();
	for id in &stitched {
		assert!(seen.insert(*id), "duplicate id across pages: {id:?}");
	}
}

/// Thin wrappers still share the pipeline (search_page ≡ retrieve_and_rank_page
/// with empty deps).
#[tokio::test]
async fn search_page_wrapper_matches_pipeline_page() {
	let directory = common::TempDir::new("pipeline-wrapper-parity");
	let serde = rust_record("serde");
	let tokio = rust_record("tokio");
	let index = searchable_index(&directory, &[serde.clone(), tokio]);

	let via_wrapper = search_page(&index, &request("serde"), None).await.expect("search_page");
	let via_pipeline = retrieve_and_rank_page(
		&index,
		&request("serde"),
		PackageSearchDeps { synonyms: None, ..Default::default() },
	)
	.await
	.expect("retrieve_and_rank_page");

	let w_ids: Vec<_> = via_wrapper.items.iter().map(|s| s.value.id).collect();
	let p_ids: Vec<_> = via_pipeline.items.iter().map(|s| s.value.id).collect();
	assert_eq!(w_ids, p_ids, "search_page must be a thin wrapper over the pipeline");
	assert_eq!(w_ids, vec![serde.id]);
}
