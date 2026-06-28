//! Compound symbol-search engine combining full-text and vector indexes.
//!
//! # Wiring real backends
//!
//! [`SymbolSearcher`] accepts any implementation of the core traits, so it
//! works with [`crate::TantivySearchIndex`] as the `SearchQuery` backend and
//! [`crate::QdrantVectorIndex`] as the `VectorQuery` backend without any
//! additional glue code — just pass the `Arc`-wrapped backends to
//! [`SymbolSearcher::new`].

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use nudox_core::{BlobInfo, BlobRef, BlobStore, BodyQuery, ByteSpan, CombineMode, Criteria, Embedder, Embedding, EmbeddingPurpose, GlobalSymbolId, GlobalSymbolQuery, NamePattern, OccurrenceId, Result, Score, SearchQuery, SourceChunk, SymbolMatch, SymbolQuery, SymbolSearch, VectorQuery};

/// Compound symbol-search engine that combines full-text name search with
/// vector similarity search, then applies scope, kind, and occurrence-count
/// post-filters before returning ranked [`SymbolMatch`] results.
///
/// # Constructor
///
/// Use [`SymbolSearcher::new`] to build an instance with real or in-memory
/// backends. All backends are held behind `Arc<dyn …>` so they are cheap to
/// clone and can be shared with other subsystems.
pub struct SymbolSearcher {
	text:         Arc<dyn SearchQuery>,
	vector:       Arc<dyn VectorQuery>,
	blobs:        Arc<dyn BlobStore>,
	embedder:     Arc<dyn Embedder>,
	global_query: Option<Arc<dyn GlobalSymbolQuery>>,
}

impl SymbolSearcher {
	/// Create a new [`SymbolSearcher`].
	///
	/// - `text` — full-text search backend (e.g. `TantivySearchIndex`)
	/// - `vector` — vector similarity backend (e.g. `QdrantVectorIndex`)
	/// - `blobs` — blob store used to fetch [`BlobInfo`] for each hit
	/// - `embedder` — used to embed [`BodyQuery`] values into vectors
	/// - `global_query` — optional read handle for occurrence lists and
	///   count-based filtering; pass `None` if not available
	pub fn new(
		text: Arc<dyn SearchQuery>,
		vector: Arc<dyn VectorQuery>,
		blobs: Arc<dyn BlobStore>,
		embedder: Arc<dyn Embedder>,
		global_query: Option<Arc<dyn GlobalSymbolQuery>>,
	) -> Self {
		Self { text, vector, blobs, embedder, global_query }
	}

	/// Embed a [`BodyQuery`] into a raw vector using the configured embedder.
	async fn embed_body_query(&self, body: &BodyQuery) -> Result<Embedding> {
		let text = match body {
			BodyQuery::NaturalLanguage(t) | BodyQuery::CodeSnippet(t) => t,
		};
		let chunk = SourceChunk {
			raw_code:        text.as_str().into(),
			treesitter_repr: None,
			symbol_span:     ByteSpan::covering(0, text.len()),
		};
		self.embedder.embed(&chunk, EmbeddingPurpose::Code).await
	}
}

/// A scored reference collected during hit merging.
#[derive(Debug, Clone)]
struct ScoredRef {
	blob_ref:  BlobRef,
	global_id: Option<GlobalSymbolId>,
	score:     Score,
}

#[async_trait]
impl SymbolSearch for SymbolSearcher {
	async fn search(&self, query: &SymbolQuery) -> Result<Vec<SymbolMatch>> {
		let fetch_limit = query.limit.saturating_add(query.limit.get());

		// ── Collect hits from each active search arm ──────────────────────
		let mut name_hits: HashMap<BlobRef, ScoredRef> = HashMap::new();
		let mut body_hits: HashMap<BlobRef, ScoredRef> = HashMap::new();

		let (opt_name, opt_body, combine) = match &query.criteria {
			Criteria::Name(n) => (Some(n), None, CombineMode::Or),
			Criteria::Body(b) => (None, Some(b), CombineMode::Or),
			Criteria::Both { name, body, combine } => (Some(name), Some(body), *combine),
		};

		if let Some(NamePattern(pattern)) = opt_name {
			let raw_hits = if pattern.is_empty() {
				self.text.list_all(fetch_limit).await?
			} else {
				self.text.search(pattern, fetch_limit).await?
			};
			for h in raw_hits {
				let key = h.blob_ref.clone();
				name_hits.entry(key).or_insert(ScoredRef {
					blob_ref:  h.blob_ref,
					global_id: None,
					score:     h.score,
				});
			}
		}

		if let Some(body) = opt_body {
			let vec = self.embed_body_query(body).await?;
			let hits = self.vector.search(&vec, fetch_limit).await?;
			for h in hits {
				let key = h.blob_ref.clone();
				let entry = body_hits.entry(key).or_insert(ScoredRef {
					blob_ref:  h.blob_ref.clone(),
					global_id: Some(h.global_id),
					score:     h.score,
				});
				if h.score > entry.score {
					entry.score = h.score;
				}
				if entry.global_id.is_none() {
					entry.global_id = Some(h.global_id);
				}
			}
		}

		// ── Merge according to CombineMode ────────────────────────────────
		let merged: Vec<ScoredRef> = match combine {
			CombineMode::Or => {
				let mut union: HashMap<BlobRef, ScoredRef> = name_hits;
				for (key, body_hit) in body_hits {
					let entry = union.entry(key).or_insert(body_hit.clone());
					if body_hit.score > entry.score {
						entry.score = body_hit.score;
					}
					if entry.global_id.is_none() {
						entry.global_id = body_hit.global_id;
					}
				}
				union.into_values().collect()
			}
			CombineMode::And => name_hits
				.into_iter()
				.filter_map(|(key, name_hit)| {
					body_hits.get(&key).map(|body_hit| ScoredRef {
						blob_ref:  name_hit.blob_ref.clone(),
						global_id: body_hit.global_id.or(name_hit.global_id),
						score:     Score::new((name_hit.score.get() + body_hit.score.get()) / 2.0),
					})
				})
				.collect(),
		};

		// ── Score → truncate → fetch ──────────────────────────────────────
		// Sort candidates by score first, then fetch full BlobInfo (source text +
		// vectors) only until `limit` of them survive the post-filters. The old
		// path fetched *every* merged candidate up front and truncated afterward,
		// so O(merged) heavy blob reads were deserialized and discarded per query.
		// Processing in score order means the first `limit` survivors are exactly
		// the top `limit` — identical results, an order of magnitude fewer fetches.
		let mut merged = merged;
		merged.sort_by(|a, b| b.score.cmp(&a.score));

		let mut matches: Vec<SymbolMatch> = Vec::with_capacity(query.limit.get().min(merged.len()));

		for scored in merged {
			if matches.len() >= query.limit.get() {
				break;
			}

			let blob: BlobInfo = match self.blobs.get(&scored.blob_ref).await {
				Ok(b) => b,
				Err(_) => continue, // blob was deleted or unavailable; skip
			};

			// Scope filter
			if let Some(scope) = &query.scope {
				if let Some(lang) = scope.lang
					&& blob.metadata.lang != lang
				{
					continue;
				}
				if let Some(repo_id) = &scope.repo_id
					&& blob.metadata.repo_id != *repo_id
				{
					continue;
				}
			}

			// Kind filter — treat None kind as "unknown, passes all filters"
			if let Some(required_kind) = query.kind
				&& let Some(blob_kind) = blob.kind
				&& blob_kind != required_kind
			{
				continue;
			}

			// Occurrence filter (requires global_query)
			let occurrences: Vec<OccurrenceId> = if let Some(gq) = &self.global_query {
				match scored.global_id.or(blob.resolution.resolved_id()) {
					None => {
						// No global id — treat as 0 occurrences for the purpose
						// of the filter; still include if no min_count is set.
						if let Some(occ_filter) = &query.occurrence_filter
							&& occ_filter.min_count.is_some()
						{
							continue;
						}
						vec![]
					}
					Some(global_id) => {
						let occs = gq.get_occurrences(global_id).await?;
						if let Some(occ_filter) = &query.occurrence_filter {
							let count = occs.len();
							if let Some(min) = occ_filter.min_count
								&& count < min.get()
							{
								continue;
							}
							if let Some(max) = occ_filter.max_count
								&& count > max
							{
								continue;
							}
						}
						occs
					}
				}
			} else {
				vec![]
			};

			matches.push(SymbolMatch { blob, score: scored.score, occurrences });
		}

		Ok(matches)
	}
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::{collections::HashMap, num::NonZeroUsize, sync::Mutex};

	use blobstore::InMemoryBlobStore;
	use embed::MockEmbedder;
	use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobRef, BlobStore, ByteSpan, ChunkMetadata, CombineMode, Criteria, Embedding, EmbeddingPurpose, EmbeddingRecord, GlobalSymbolId, GlobalSymbolQuery, Language, ModelId, ModelType, NamePattern, OccurrenceFilter, OccurrenceId, RepoId, Result, ScopeFilter, SearchIndex, SourceChunk, SymbolKind, SymbolOrigin, SymbolQuery, SymbolSearch, VectorIndex};

	use super::*;
	use crate::memory::{InMemorySearchIndex, InMemoryVectorIndex};

	// ── InMemoryGlobalSymbolQuery ─────────────────────────────────────────

	/// Minimal in-memory implementation of [`GlobalSymbolQuery`] for tests.
	struct InMemoryGlobalSymbolQuery {
		map: Mutex<HashMap<GlobalSymbolId, Vec<OccurrenceId>>>,
	}

	impl InMemoryGlobalSymbolQuery {
		fn new() -> Self { Self { map: Mutex::new(HashMap::new()) } }

		fn insert(&self, global_id: GlobalSymbolId, occurrences: Vec<OccurrenceId>) {
			self.map.lock().unwrap().insert(global_id, occurrences);
		}
	}

	#[async_trait]
	impl GlobalSymbolQuery for InMemoryGlobalSymbolQuery {
		async fn get_occurrences(&self, global_id: GlobalSymbolId) -> Result<Vec<OccurrenceId>> {
			Ok(self.map.lock().unwrap().get(&global_id).cloned().unwrap_or_default())
		}
	}

	// ── Helpers ───────────────────────────────────────────────────────────

	fn make_blob(
		symbol_name: &str,
		repo_id: &str,
		lang: Language,
		kind: Option<SymbolKind>,
		global_id: Option<GlobalSymbolId>,
	) -> BlobInfo {
		use nudox_core::ResolutionState;
		BlobInfo {
			occurrence_id: OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name: symbol_name.to_string(),
			symbol_origin: SymbolOrigin::Repo { repo_id: RepoId::from(repo_id.to_string()) },
			resolution: match global_id {
				Some(gid) => ResolutionState::Resolved(gid),
				None => ResolutionState::Unresolved,
			},
			kind,
			source: SourceChunk {
				raw_code:        format!("fn {}() {{}}", symbol_name).into(),
				treesitter_repr: None,
				symbol_span:     ByteSpan::covering(3, symbol_name.len() + 3),
			},
			embeddings: vec![],
			metadata: ChunkMetadata {
				repo_id: RepoId::from(repo_id.to_string()),
				file_path: "src/lib.rs".into(),
				file_span: ByteSpan::covering(0, 20),
				parsed_at: chrono::Utc::now(),
				lang,
				lang_version: None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	/// Index a blob into both the search index and vector index, and store in the
	/// blob store. Returns the BlobRef assigned by the blob store.
	async fn index_blob(
		blob: &BlobInfo,
		vector: &[f32],
		global_id: GlobalSymbolId,
		search_idx: &InMemorySearchIndex,
		vector_idx: &InMemoryVectorIndex,
		blob_store: &InMemoryBlobStore,
	) -> BlobRef {
		let blob_ref = blob_store.put(blob).await.unwrap();
		search_idx.index(&blob_ref, blob).await.unwrap();
		vector_idx
			.upsert(&blob_ref, global_id, &[EmbeddingRecord {
				model_type: ModelType::Mock,
				model:      ModelId::new("mock"),
				purpose:    EmbeddingPurpose::Code,
				vector:     Embedding::new(vector.to_vec()).unwrap(),
			}])
			.await
			.unwrap();
		blob_ref
	}

	fn make_searcher(
		search_idx: Arc<InMemorySearchIndex>,
		vector_idx: Arc<InMemoryVectorIndex>,
		blob_store: Arc<InMemoryBlobStore>,
		global_query: Option<Arc<dyn GlobalSymbolQuery>>,
	) -> SymbolSearcher {
		let embedder = Arc::new(MockEmbedder::new(3));
		SymbolSearcher::new(search_idx, vector_idx, blob_store, embedder, global_query)
	}

	// ── Tests ─────────────────────────────────────────────────────────────

	#[tokio::test]
	async fn name_only_query() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		index_blob(
			&make_blob("alpha_fn", "repo1", Language::Rust, None, None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("beta_fn", "repo1", Language::Rust, None, None),
			&[0.0, 1.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("gamma_fn", "repo1", Language::Rust, None, None),
			&[0.0, 0.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("alpha".to_string())),
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert_eq!(results.len(), 1);
		assert_eq!(results[0].blob.symbol_name, "alpha_fn");
	}

	#[tokio::test]
	async fn body_only_query() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());

		let gid_a = GlobalSymbolId(uuid::Uuid::new_v4());
		let gid_b = GlobalSymbolId(uuid::Uuid::new_v4());

		// blob_a has a vector aligned with the query vector [0.1, 0.1, 0.1]
		// (MockEmbedder always returns [0.1, 0.1, 0.1]).
		// blob_b is orthogonal.
		index_blob(
			&make_blob("close_fn", "repo1", Language::Rust, None, None),
			&[1.0, 1.0, 1.0],
			gid_a,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("far_fn", "repo1", Language::Rust, None, None),
			&[0.0, 0.0, -1.0],
			gid_b,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Body(nudox_core::BodyQuery::NaturalLanguage(
				"compute something".to_string(),
			)),
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert!(!results.is_empty(), "should return at least one result");
		assert_eq!(results[0].blob.symbol_name, "close_fn", "most similar should be first");
	}

	#[tokio::test]
	async fn combine_and_returns_intersection() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		// Blob A: matches name "alpha" but has orthogonal vector
		index_blob(
			&make_blob("alpha_only", "repo1", Language::Rust, None, None),
			&[0.0, 0.0, -1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		// Blob B: does not match name "alpha", but has aligned vector [1,1,1]
		index_blob(
			&make_blob("body_only_fn", "repo1", Language::Rust, None, None),
			&[1.0, 1.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		// Blob C: matches both name "alpha_both" and has aligned vector
		index_blob(
			&make_blob("alpha_both", "repo1", Language::Rust, None, None),
			&[1.0, 1.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Both {
				name:    NamePattern("alpha".to_string()),
				body:    nudox_core::BodyQuery::NaturalLanguage("compute something".to_string()),
				combine: CombineMode::And,
			},
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		// Only "alpha_both" matches both the name pattern and has a high vector score.
		// "alpha_only" has negative cosine similarity with [0.1,0.1,0.1] query so it
		// would still appear in body results but with a very low score. However with
		// And, we need presence in *both* hit sets. "alpha_only" does appear in body
		// hits (just low score), so we need to verify "body_only_fn" is NOT in
		// results.
		let names: Vec<&str> = results.iter().map(|m| m.blob.symbol_name.as_str()).collect();
		assert!(!names.contains(&"body_only_fn"), "body_only_fn should be excluded (And mode)");
	}

	#[tokio::test]
	async fn combine_or_returns_union() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		// Blob A: name matches "alpha", orthogonal vector
		index_blob(
			&make_blob("alpha_only", "repo1", Language::Rust, None, None),
			&[0.0, 0.0, -1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		// Blob B: name does not match, but aligned vector
		index_blob(
			&make_blob("body_only_fn", "repo1", Language::Rust, None, None),
			&[1.0, 1.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		// Blob C: matches both
		index_blob(
			&make_blob("alpha_both", "repo1", Language::Rust, None, None),
			&[1.0, 1.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Both {
				name:    NamePattern("alpha".to_string()),
				body:    nudox_core::BodyQuery::NaturalLanguage("compute something".to_string()),
				combine: CombineMode::Or,
			},
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		let names: Vec<&str> = results.iter().map(|m| m.blob.symbol_name.as_str()).collect();
		assert!(names.contains(&"alpha_only"), "alpha_only should appear (Or)");
		assert!(names.contains(&"body_only_fn"), "body_only_fn should appear (Or)");
		assert!(names.contains(&"alpha_both"), "alpha_both should appear (Or)");
	}

	#[tokio::test]
	async fn scope_filter_by_repo() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		index_blob(
			&make_blob("fn_in_repo_a", "repo_a", Language::Rust, None, None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("fn_in_repo_b", "repo_b", Language::Rust, None, None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("fn_in".to_string())),
			scope:             Some(ScopeFilter { repo_id: Some(RepoId::from("repo_a")), lang: None }),
			limit:             NonZeroUsize::new(10).unwrap(),
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert_eq!(results.len(), 1);
		assert_eq!(results[0].blob.metadata.repo_id, RepoId::from("repo_a"));
	}

	#[tokio::test]
	async fn kind_filter() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		index_blob(
			&make_blob("my_function", "repo1", Language::Rust, Some(SymbolKind::Function), None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("MyStruct", "repo1", Language::Rust, Some(SymbolKind::Struct), None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("my".to_string())),
			kind:              Some(SymbolKind::Function),
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert_eq!(results.len(), 1);
		assert_eq!(results[0].blob.symbol_name, "my_function");
	}

	#[tokio::test]
	async fn occurrence_filter_min_count() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());

		let gid_a = GlobalSymbolId(uuid::Uuid::new_v4());
		let gid_b = GlobalSymbolId(uuid::Uuid::new_v4());

		let blob_a = make_blob("fn_a", "repo1", Language::Rust, None, Some(gid_a));
		let blob_b = make_blob("fn_b", "repo1", Language::Rust, None, Some(gid_b));

		index_blob(&blob_a, &[1.0, 0.0, 0.0], gid_a, &si, &vi, &bs).await;
		index_blob(&blob_b, &[1.0, 0.0, 0.0], gid_b, &si, &vi, &bs).await;

		// gid_a: 1 occurrence; gid_b: 3 occurrences
		let gq = Arc::new(InMemoryGlobalSymbolQuery::new());
		gq.insert(gid_a, vec![OccurrenceId(uuid::Uuid::new_v4())]);
		gq.insert(gid_b, vec![
			OccurrenceId(uuid::Uuid::new_v4()),
			OccurrenceId(uuid::Uuid::new_v4()),
			OccurrenceId(uuid::Uuid::new_v4()),
		]);

		let searcher = make_searcher(si, vi, bs, Some(gq as Arc<dyn GlobalSymbolQuery>));
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("fn_".to_string())),
			occurrence_filter: Some(OccurrenceFilter {
				min_count: Some(NonZeroUsize::new(2).unwrap()),
				max_count: None,
			}),
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			kind:              None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert_eq!(results.len(), 1);
		assert_eq!(results[0].blob.symbol_name, "fn_b");
	}

	#[tokio::test]
	async fn kind_only_query_no_name_pattern() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		index_blob(
			&make_blob("MyStruct", "repo1", Language::Rust, Some(SymbolKind::Struct), None),
			&[1.0, 0.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("my_function", "repo1", Language::Rust, Some(SymbolKind::Function), None),
			&[0.0, 1.0, 0.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;
		index_blob(
			&make_blob("MyEnum", "repo1", Language::Rust, Some(SymbolKind::Enum), None),
			&[0.0, 0.0, 1.0],
			gid,
			&si,
			&vi,
			&bs,
		)
		.await;

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("".to_string())),
			kind:              Some(SymbolKind::Struct),
			limit:             NonZeroUsize::new(10).unwrap(),
			scope:             None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert_eq!(results.len(), 1);
		assert_eq!(results[0].blob.symbol_name, "MyStruct");
	}

	#[tokio::test]
	async fn limit_is_respected() {
		let si = Arc::new(InMemorySearchIndex::new());
		let vi = Arc::new(InMemoryVectorIndex::new());
		let bs = Arc::new(InMemoryBlobStore::new());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());

		for i in 0..10 {
			index_blob(
				&make_blob(&format!("fn_{:02}", i), "repo1", Language::Rust, None, None),
				&[1.0, 0.0, 0.0],
				gid,
				&si,
				&vi,
				&bs,
			)
			.await;
		}

		let searcher = make_searcher(si, vi, bs, None);
		let query = SymbolQuery {
			criteria:          Criteria::Name(NamePattern("fn_".to_string())),
			limit:             NonZeroUsize::new(3).unwrap(),
			scope:             None,
			kind:              None,
			occurrence_filter: None,
		};
		let results = searcher.search(&query).await.unwrap();
		assert!(results.len() <= 3, "expected at most 3 results, got {}", results.len());
	}
}
