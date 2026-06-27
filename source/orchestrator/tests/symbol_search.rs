//! End-to-end integration tests for the full symbol-search pipeline.
//!
//! Each test drives real data through:
//!   Pipeline::process() → Orchestrator::ingest() → SymbolSearcher::search()
//!
//! We use PlaceholderEmbedder (content-derived, deterministic) so vector search
//! produces meaningful results without an external embedding service: same
//! source text → same hash → same vector → cosine similarity = 1.0.

use std::sync::Arc;

use nudox_blobstore::InMemoryBlobStore;
use nudox_core::{BLOB_SCHEMA_VERSION, BlobStore, BodyQuery, ByteSpan, ChunkMetadata, GlobalSymbolId, GlobalSymbolQuery, Language, LibRef, OccurrenceFilter, RepoId, ResolutionOutcome, ScopeFilter, SearchIndex, SearchQuery, SymbolOrigin, SymbolQuery, SymbolSearch, VectorIndex, VectorQuery};
use nudox_embed::PlaceholderEmbedder;
use nudox_pipeline::{Pipeline, PipelineConfig, PipelineInput};
use nudox_search::{InMemorySearchIndex, InMemoryVectorIndex, SymbolSearcher};

use crate::{Orchestrator, memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore}};

const EMBED_DIM: usize = 64;

// ── Stack builder
// ─────────────────────────────────────────────────────────────

struct TestStack {
	pipeline:     Pipeline,
	blob_store:   Arc<InMemoryBlobStore>,
	search:       Arc<InMemorySearchIndex>,
	vector:       Arc<InMemoryVectorIndex>,
	global_store: Arc<InMemoryGlobalSymbolStore>,
	queue:        Arc<InMemoryFutureParseQueue>,
	orchestrator: Orchestrator,
}

impl TestStack {
	fn new() -> Self {
		let search = Arc::new(InMemorySearchIndex::new());
		let vector = Arc::new(InMemoryVectorIndex::new());
		let blob_store = Arc::new(InMemoryBlobStore::new());
		let global_store = Arc::new(InMemoryGlobalSymbolStore::new());
		let queue = Arc::new(InMemoryFutureParseQueue::new());

		let pipeline = Pipeline::new(
			vec![Box::new(PlaceholderEmbedder::new("placeholder-v1", EMBED_DIM))],
			PipelineConfig::default(),
		);

		let orchestrator = Orchestrator::new(
			Arc::clone(&global_store) as Arc<dyn nudox_core::GlobalSymbolStore>,
			Arc::clone(&blob_store) as Arc<dyn nudox_core::BlobStore>,
			Arc::clone(&queue) as Arc<dyn nudox_core::FutureParseQueue>,
			Arc::clone(&search) as Arc<dyn SearchIndex>,
			Arc::clone(&vector) as Arc<dyn VectorIndex>,
		);

		TestStack { pipeline, blob_store, search, vector, global_store, queue, orchestrator }
	}

	fn searcher(&self) -> SymbolSearcher {
		SymbolSearcher::new(
			Arc::clone(&self.search) as Arc<dyn SearchQuery>,
			Arc::clone(&self.vector) as Arc<dyn VectorQuery>,
			Arc::clone(&self.blob_store) as Arc<dyn BlobStore>,
			Arc::new(PlaceholderEmbedder::new("placeholder-v1", EMBED_DIM)),
			Some(Arc::clone(&self.global_store) as Arc<dyn GlobalSymbolQuery>),
		)
	}

	/// Process and ingest a repo-local symbol. Returns the resolved BlobRef.
	async fn ingest_repo(&self, repo: &RepoId, file: &str, code: &str, symbol: &str) {
		let input = make_input(repo, file, code, symbol, SymbolOrigin::Repo { repo_id: repo.clone() });
		let blob = self.pipeline.process(input).await.unwrap();
		self.orchestrator.ingest(blob).await.unwrap();
	}

	/// Process and ingest an external-lib symbol.
	async fn ingest_lib(&self, repo: &RepoId, file: &str, code: &str, symbol: &str, lib: LibRef) {
		let input = make_input(repo, file, code, symbol, SymbolOrigin::ExternalLib { lib });
		let blob = self.pipeline.process(input).await.unwrap();
		self.orchestrator.ingest(blob).await.unwrap();
	}
}

fn make_input(
	repo_id: &RepoId,
	file: &str,
	raw_code: &str,
	symbol_name: &str,
	origin: SymbolOrigin,
) -> PipelineInput {
	let start = raw_code.find(symbol_name).unwrap_or(0);
	PipelineInput {
		raw_code:      raw_code.to_string(),
		symbol_name:   symbol_name.to_string(),
		symbol_span:   ByteSpan { start, end: start + symbol_name.len() },
		symbol_origin: origin,
		metadata:      ChunkMetadata {
			repo_id:             repo_id.clone(),
			file_path:           file.into(),
			file_span:           ByteSpan { start: 0, end: raw_code.len() },
			parsed_at:           chrono::Utc::now(),
			lang:                Language::Rust,
			lang_version:        None,
			blob_schema_version: BLOB_SCHEMA_VERSION,
		},
		docstring:     None,
	}
}

// ── Test 1: name search roundtrip through full pipeline
// ───────────────────────

#[tokio::test]
async fn name_search_roundtrip() {
	let s = TestStack::new();
	let repo = RepoId("repo-a".into());

	// Three Rust functions with distinct names.
	let symbols = [
		(
			"fn compute_checksum(data: &[u8]) -> u32 { data.iter().fold(0u32, |a, &b| a.wrapping_add(b as u32)) }",
			"compute_checksum",
		),
		(
			"fn parse_config(src: &str) -> Option<String> { Some(src.trim().to_owned()) }",
			"parse_config",
		),
		(
			"fn render_template(tmpl: &str, ctx: &str) -> String { tmpl.replace(\"{}\", ctx) }",
			"render_template",
		),
	];
	for (code, name) in &symbols {
		s.ingest_repo(&repo, "src/lib.rs", code, name).await;
	}

	let searcher = s.searcher();

	// Each name pattern matches exactly one symbol.
	for (pattern, expected) in
		[("checksum", "compute_checksum"), ("parse", "parse_config"), ("render", "render_template")]
	{
		let hits = searcher
			.search(&SymbolQuery {
				name_pattern: Some(pattern.to_string()),
				limit: 10,
				..Default::default()
			})
			.await
			.unwrap();
		assert_eq!(hits.len(), 1, "'{pattern}' should match exactly 1 symbol");
		assert_eq!(hits[0].blob.symbol_name, expected, "wrong symbol for pattern '{pattern}'");
	}

	// "fn_" (embedded in all three names indirectly via the underscore convention)
	// — actually all three names contain underscores; check we get all 3.
	let hits = searcher
		.search(&SymbolQuery { name_pattern: Some("_".to_string()), limit: 10, ..Default::default() })
		.await
		.unwrap();
	assert_eq!(hits.len(), 3, "all 3 symbols contain underscores");
}

// ── Test 2: vector search finds exact code match (PlaceholderEmbedder)
// ────────

#[tokio::test]
async fn vector_search_exact_code_match() {
	// PlaceholderEmbedder is hash-based: same text → same vector → cosine sim =
	// 1.0. tree-sitter extracts the enclosing function_item, which for a
	// self-contained function equals the full raw_code text.  So pipeline
	// embedding and query embedding use identical inputs → perfect match.
	let s = TestStack::new();
	let repo = RepoId("repo-vec".into());

	let target = "fn add_numbers(a: i32, b: i32) -> i32 { a + b }";
	let noise = "fn completely_unrelated_xyzabc(q: u64) -> bool { q == 0 }";

	s.ingest_repo(&repo, "src/math.rs", target, "add_numbers").await;
	s.ingest_repo(&repo, "src/other.rs", noise, "completely_unrelated_xyzabc").await;

	let searcher = s.searcher();
	let hits = searcher
		.search(&SymbolQuery {
			body_query: Some(BodyQuery::CodeSnippet(target.to_string())),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();

	assert!(!hits.is_empty(), "vector search should return at least one hit");
	assert_eq!(hits[0].blob.symbol_name, "add_numbers", "top hit should be the target function");
	assert!(hits[0].score > 0.9, "score for exact code match should be > 0.9, got {}", hits[0].score);
}

// ── Test 3: occurrence counting through full ingest + GlobalSymbolQuery
// ────────

#[tokio::test]
async fn occurrence_counting_three_usages_of_same_lib_symbol() {
	let s = TestStack::new();
	let repo = RepoId("repo-occ".into());
	let serde_lib = LibRef { name: "serde".into(), version: "1.0".into() };

	// Pre-register serde::Serialize so all 3 ingests resolve immediately.
	let serde_gid = GlobalSymbolId(uuid::Uuid::new_v4());
	s.global_store.insert(&serde_lib, "Serialize", serde_gid);

	// Ingest 3 distinct usages of Serialize from 3 different files.
	for (i, file) in ["src/user.rs", "src/product.rs", "src/order.rs"].iter().enumerate() {
		let code =
			format!("use serde::Serialize;\n#[derive(Serialize)]\nstruct Model{i} {{ id: u64 }}");
		s.ingest_lib(&repo, file, &code, "Serialize", serde_lib.clone()).await;
	}

	let searcher = s.searcher();

	// All 3 occurrences should be findable by name.
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("Serialize".into()),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	assert_eq!(hits.len(), 3, "all 3 Serialize occurrences should be indexed");

	// Every hit knows about all 3 occurrences (they share the same global_id).
	for m in &hits {
		assert_eq!(
			m.occurrences.len(),
			3,
			"each SymbolMatch should report 3 occurrences for this global symbol"
		);
	}

	// min_count = 3 → all 3 pass.
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("Serialize".into()),
			occurrence_filter: Some(OccurrenceFilter { min_count: Some(3), max_count: None }),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	assert_eq!(hits.len(), 3, "min_count=3 should pass all 3 occurrences");

	// min_count = 4 → nothing (only 3 occurrences exist).
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("Serialize".into()),
			occurrence_filter: Some(OccurrenceFilter { min_count: Some(4), max_count: None }),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	assert!(hits.is_empty(), "min_count=4 should find nothing (only 3 occurrences)");
}

// ── Test 4: deferred lib symbol becomes searchable after resolve_lib
// ───────────

#[tokio::test]
async fn deferred_lib_becomes_searchable_after_resolve() {
	let s = TestStack::new();
	let repo = RepoId("repo-defer".into());
	let tokio_lib = LibRef { name: "tokio".into(), version: "1.0".into() };

	// Ingest tokio::spawn before registering the library → deferred.
	let code = "tokio::spawn(async move { println!(\"hello\"); });";
	let input = make_input(&repo, "src/main.rs", code, "spawn", SymbolOrigin::ExternalLib {
		lib: tokio_lib.clone(),
	});
	let blob = s.pipeline.process(input).await.unwrap();
	let outcome = s.orchestrator.ingest(blob).await.unwrap();
	assert!(matches!(outcome, ResolutionOutcome::Deferred { .. }), "should be deferred");

	let searcher = s.searcher();

	// Deferred blobs are not indexed — text search returns nothing.
	let hits = searcher
		.search(&SymbolQuery { name_pattern: Some("spawn".into()), limit: 10, ..Default::default() })
		.await
		.unwrap();
	assert!(hits.is_empty(), "deferred symbol must not appear in search before resolution");

	// Register tokio and resolve the deferred queue.
	let tokio_gid = GlobalSymbolId(uuid::Uuid::new_v4());
	s.global_store.insert(&tokio_lib, "spawn", tokio_gid);
	let report = s.orchestrator.resolve_lib(&tokio_lib).await.unwrap();
	assert_eq!(report.blobs_resolved, 1);

	// Now it must be searchable.
	let hits = searcher
		.search(&SymbolQuery { name_pattern: Some("spawn".into()), limit: 10, ..Default::default() })
		.await
		.unwrap();
	assert_eq!(hits.len(), 1, "spawn should be findable after resolve_lib");
	assert_eq!(hits[0].blob.symbol_name, "spawn");
}

// ── Test 5: repo scope filter isolates results by repository
// ──────────────────

#[tokio::test]
async fn scope_filter_by_repo_isolates_results() {
	let s = TestStack::new();
	let repo_ui = RepoId("repo-ui".into());
	let repo_api = RepoId("repo-api".into());

	s.ingest_repo(
		&repo_ui,
		"src/ui.rs",
		"fn render_widget(id: u32) -> String { format!(\"w-{}\", id) }",
		"render_widget",
	)
	.await;
	s.ingest_repo(
		&repo_api,
		"src/api.rs",
		"fn render_response(code: u16) -> String { format!(\"r-{}\", code) }",
		"render_response",
	)
	.await;

	let searcher = s.searcher();

	// No scope → both repos.
	let hits = searcher
		.search(&SymbolQuery { name_pattern: Some("render".into()), limit: 10, ..Default::default() })
		.await
		.unwrap();
	assert_eq!(hits.len(), 2, "unscoped search should return symbols from both repos");

	// Scope to repo-ui.
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("render".into()),
			scope: Some(ScopeFilter { repo_id: Some(repo_ui.clone()), lang: None }),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].blob.symbol_name, "render_widget");

	// Scope to repo-api.
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("render".into()),
			scope: Some(ScopeFilter { repo_id: Some(repo_api.clone()), lang: None }),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].blob.symbol_name, "render_response");
}

// ── Test 6: combine-mode Or vs And over real pipeline data
// ────────────────────

#[tokio::test]
async fn combine_or_and_with_pipeline_data() {
	// We need 3 blobs:
	//   A — name matches "alpha", but low vector similarity to query
	//   B — name does NOT match "alpha", but high vector similarity
	//   C — name matches "alpha" AND high similarity
	//
	// With PlaceholderEmbedder the vector is derived from the extracted snippet.
	// We use the exact snippet text as the BodyQuery::CodeSnippet so we get
	// cosine sim = 1.0 for the intended blob, near-zero for others.

	let s = TestStack::new();
	let repo = RepoId("repo-combine".into());

	let snippet_b = "fn high_similarity_target(x: f64) -> f64 { x * 2.0 }";
	let snippet_c = "fn alpha_and_similar(x: f64) -> f64 { x + 1.0 }";

	// Blob A: name contains "alpha", unrelated vector.
	s.ingest_repo(&repo, "a.rs", "fn alpha_unrelated() -> u8 { 42 }", "alpha_unrelated").await;
	// Blob B: name unrelated, use snippet_b as the body-query target.
	s.ingest_repo(&repo, "b.rs", snippet_b, "high_similarity_target").await;
	// Blob C: name contains "alpha", use snippet_c as the body-query target.
	s.ingest_repo(&repo, "c.rs", snippet_c, "alpha_and_similar").await;

	let searcher = s.searcher();

	// OR: name="alpha" ∪ body=snippet_b → A (name), B (body), C (name+body)
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("alpha".into()),
			body_query: Some(BodyQuery::CodeSnippet(snippet_b.to_string())),
			combine: nudox_core::CombineMode::Or,
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	let names: Vec<&str> = hits.iter().map(|h| h.blob.symbol_name.as_str()).collect();
	assert!(names.contains(&"alpha_unrelated"), "Or: A should be included (name match)");
	assert!(names.contains(&"high_similarity_target"), "Or: B should be included (body match)");
	assert!(names.contains(&"alpha_and_similar"), "Or: C should be included (name match)");

	// AND: name="alpha" ∩ body=snippet_c → only C (the only blob matching both)
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("alpha".into()),
			body_query: Some(BodyQuery::CodeSnippet(snippet_c.to_string())),
			combine: nudox_core::CombineMode::And,
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();
	let names: Vec<&str> = hits.iter().map(|h| h.blob.symbol_name.as_str()).collect();
	assert!(!names.contains(&"high_similarity_target"), "And: B should be excluded (no name match)");
	assert!(names.contains(&"alpha_and_similar"), "And: C must appear (matches both)");
}

// ── Test 7: result limit is enforced after full pipeline ingest
// ────────────────

#[tokio::test]
async fn limit_enforced_with_pipeline_data() {
	let s = TestStack::new();
	let repo = RepoId("repo-limit".into());

	for i in 0..8 {
		let code = format!("fn target_fn_{i:02}(x: u32) -> u32 {{ x + {i} }}");
		s.ingest_repo(&repo, &format!("src/f{i}.rs"), &code, &format!("target_fn_{i:02}")).await;
	}

	let searcher = s.searcher();
	let hits = searcher
		.search(&SymbolQuery {
			name_pattern: Some("target_fn_".into()),
			limit: 3,
			..Default::default()
		})
		.await
		.unwrap();
	assert!(hits.len() <= 3, "limit=3 must cap results, got {}", hits.len());
	assert_eq!(hits.len(), 3, "should return exactly 3 results when 8 are available");
}

// ── Test 8: Orchestrator::search() delegates through with_symbol_search
// ────────

#[tokio::test]
async fn orchestrator_search_delegates_to_symbol_searcher() {
	let s = TestStack::new();
	let repo = RepoId("repo-orch".into());

	s.ingest_repo(
		&repo,
		"src/lib.rs",
		"fn orchestrate_work(n: u32) -> u32 { n * 2 }",
		"orchestrate_work",
	)
	.await;

	let searcher = s.searcher();
	let orchestrator_with_search = s.orchestrator.with_symbol_search(Arc::new(searcher));

	let hits = orchestrator_with_search
		.search(&SymbolQuery {
			name_pattern: Some("orchestrate".into()),
			limit: 10,
			..Default::default()
		})
		.await
		.unwrap();

	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0].blob.symbol_name, "orchestrate_work");
}
