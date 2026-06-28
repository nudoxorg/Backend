use std::{collections::HashMap, sync::{Arc, Mutex}};

use async_trait::async_trait;
use std::num::NonZeroUsize;

use nudox_core::{BlobInfo, BlobRef, Embedding, EmbeddingRecord, GlobalSymbolId, ModelId, OccurrenceId, Result, Score, SearchHit, SearchIndex, SearchQuery, VectorHit, VectorIndex, VectorQuery};

/// Indexed entry stored in the in-memory search index.
#[derive(Debug, Clone)]
pub struct SearchEntry {
	/// Reference to the blob this entry indexes.
	pub blob_ref:           BlobRef,
	/// The fully-qualified symbol name.
	pub symbol_name:        String,
	/// The occurrence identifier for this entry.
	pub occurrence_id:      OccurrenceId,
	/// Resolved global symbol identifier present at index time, if any.
	pub resolved_global_id: Option<GlobalSymbolId>,
}

/// In-memory search index for use in tests.
///
/// Keyed by [`OccurrenceId`] so that re-indexing a blob after deferred
/// resolution updates the existing entry rather than appending a duplicate.
#[derive(Clone)]
pub struct InMemorySearchIndex {
	entries: Arc<Mutex<HashMap<OccurrenceId, SearchEntry>>>,
}

impl InMemorySearchIndex {
	/// Create an empty index.
	pub fn new() -> Self { InMemorySearchIndex { entries: Arc::new(Mutex::new(HashMap::new())) } }

	/// Return all indexed entries (for test assertions).
	pub fn entries(&self) -> Vec<SearchEntry> {
		self.entries.lock().unwrap().values().cloned().collect()
	}
}

impl Default for InMemorySearchIndex {
	fn default() -> Self { Self::new() }
}

#[async_trait]
impl SearchIndex for InMemorySearchIndex {
	async fn index(&self, blob_ref: &BlobRef, info: &BlobInfo) -> Result<()> {
		let entry = SearchEntry {
			blob_ref:           blob_ref.clone(),
			symbol_name:        info.symbol_name.clone(),
			occurrence_id:      info.occurrence_id,
			resolved_global_id: info.resolution.resolved_id(),
		};
		self.entries.lock().unwrap().insert(info.occurrence_id, entry);
		Ok(())
	}
}

#[async_trait]
impl SearchQuery for InMemorySearchIndex {
	async fn search(&self, query: &str, limit: NonZeroUsize) -> Result<Vec<SearchHit>> {
		let q = query.to_lowercase();
		let map = self.entries.lock().unwrap();
		let mut hits: Vec<SearchHit> = map
			.values()
			.filter(|e| e.symbol_name.to_lowercase().contains(&q))
			.take(limit.get())
			.map(|e| SearchHit {
				blob_ref:      e.blob_ref.clone(),
				occurrence_id: e.occurrence_id,
				symbol_name:   e.symbol_name.clone(),
				score:         Score::new(1.0),
			})
			.collect();
		hits.sort_by(|a, b| a.symbol_name.cmp(&b.symbol_name));
		Ok(hits)
	}

	async fn find_by_global_id(
		&self,
		global_id: GlobalSymbolId,
		limit: NonZeroUsize,
	) -> Result<Vec<SearchHit>> {
		let map = self.entries.lock().unwrap();
		let hits = map
			.values()
			.filter(|e| e.resolved_global_id == Some(global_id))
			.take(limit.get())
			.map(|e| SearchHit {
				blob_ref:      e.blob_ref.clone(),
				occurrence_id: e.occurrence_id,
				symbol_name:   e.symbol_name.clone(),
				score:         Score::new(1.0),
			})
			.collect();
		Ok(hits)
	}

	async fn list_all(&self, limit: NonZeroUsize) -> Result<Vec<SearchHit>> {
		let map = self.entries.lock().unwrap();
		let mut hits: Vec<SearchHit> = map
			.values()
			.take(limit.get())
			.map(|e| SearchHit {
				blob_ref:      e.blob_ref.clone(),
				occurrence_id: e.occurrence_id,
				symbol_name:   e.symbol_name.clone(),
				score:         Score::new(1.0),
			})
			.collect();
		hits.sort_by(|a, b| a.symbol_name.cmp(&b.symbol_name));
		Ok(hits)
	}
}

/// In-memory vector index for use in tests.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct InMemoryVectorIndex {
	points: Arc<Mutex<HashMap<BlobRef, (GlobalSymbolId, Vec<EmbeddingRecord>)>>>,
}

impl InMemoryVectorIndex {
	/// Create an empty vector index.
	pub fn new() -> Self { InMemoryVectorIndex { points: Arc::new(Mutex::new(HashMap::new())) } }

	/// Return stored global_id for a blob_ref (for test assertions).
	pub fn get(&self, blob_ref: &BlobRef) -> Option<GlobalSymbolId> {
		self.points.lock().unwrap().get(blob_ref).map(|(id, _)| *id)
	}
}

impl Default for InMemoryVectorIndex {
	fn default() -> Self { Self::new() }
}

#[async_trait]
impl VectorIndex for InMemoryVectorIndex {
	async fn upsert(
		&self,
		blob_ref: &BlobRef,
		global_id: GlobalSymbolId,
		embeddings: &[EmbeddingRecord],
	) -> Result<()> {
		self.points.lock().unwrap().insert(blob_ref.clone(), (global_id, embeddings.to_vec()));
		Ok(())
	}
}

#[async_trait]
impl VectorQuery for InMemoryVectorIndex {
	async fn search(&self, vector: &[f32], limit: NonZeroUsize) -> Result<Vec<VectorHit>> {
		// Precompute the query norm once rather than per stored record.
		let query_norm = norm(vector);
		let map = self.points.lock().unwrap();
		let mut scored: Vec<(f32, BlobRef, GlobalSymbolId)> = map
			.iter()
			.flat_map(|(blob_ref_key, (global_id, records))| {
				records.iter().map(move |rec| {
					let score = cosine_with_query_norm(vector, query_norm, &rec.vector);
					(score, blob_ref_key.clone(), *global_id)
				})
			})
			.collect();

		scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
		scored.dedup_by_key(|(_, blob_ref, _)| blob_ref.clone());

		Ok(
			scored
				.into_iter()
				.take(limit.get())
				.map(|(score, blob_ref, global_id)| VectorHit { blob_ref, global_id, score: Score::new(score) })
				.collect(),
		)
	}
}

/// Cosine similarity using a SIMD inner loop and a precomputed query norm.
///
/// `query_norm` must be `norm(a)`. For each candidate `b` this computes
/// `dot(a, b)` and `‖b‖` in a single 8-lane pass; numerically equivalent to the
/// scalar formulation up to float rounding.
fn cosine_with_query_norm(a: &[f32], query_norm: f32, b: &[f32]) -> f32 {
	if a.len() != b.len() || a.is_empty() {
		return 0.0;
	}
	let (dot, norm_b_sq) = dot_and_norm_sq(a, b);
	let norm_b = norm_b_sq.sqrt();
	if query_norm == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (query_norm * norm_b) }
}

/// Euclidean norm `‖v‖` using an 8-lane SIMD reduction plus scalar remainder.
fn norm(v: &[f32]) -> f32 {
	use wide::f32x8;

	let chunks = v.len() / 8;
	let mut acc = f32x8::ZERO;
	for i in 0..chunks {
		let off = i * 8;
		let x = f32x8::new(v[off..off + 8].try_into().unwrap());
		acc += x * x;
	}
	let mut sum = acc.reduce_add();
	for &x in &v[chunks * 8..] {
		sum += x * x;
	}
	sum.sqrt()
}

/// Returns `(dot(a, b), Σ b_i²)` over equal-length slices, processed 8 lanes at
/// a time with a scalar tail for the remainder.
fn dot_and_norm_sq(a: &[f32], b: &[f32]) -> (f32, f32) {
	use wide::f32x8;

	let n = a.len();
	let chunks = n / 8;
	let mut dot_acc = f32x8::ZERO;
	let mut nb_acc = f32x8::ZERO;
	for i in 0..chunks {
		let off = i * 8;
		let va = f32x8::new(a[off..off + 8].try_into().unwrap());
		let vb = f32x8::new(b[off..off + 8].try_into().unwrap());
		dot_acc += va * vb;
		nb_acc += vb * vb;
	}
	let mut dot = dot_acc.reduce_add();
	let mut nb = nb_acc.reduce_add();
	for i in chunks * 8..n {
		dot += a[i] * b[i];
		nb += b[i] * b[i];
	}
	(dot, nb)
}

#[cfg(test)]
mod tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobRef, ByteSpan, ChunkMetadata, EmbeddingPurpose, EmbeddingRecord, GlobalSymbolId, Language, OccurrenceId, RepoId, SearchIndex, SourceChunk, SymbolOrigin, VectorIndex};
	use uuid::Uuid;

	use super::*;

	fn make_blob_info() -> (BlobRef, BlobInfo) {
		let blob_ref = BlobRef::from("test-blob-1");
		let info = BlobInfo {
			occurrence_id: OccurrenceId(Uuid::new_v4()),
			symbol_name:   "my_crate::MyStruct".to_string(),
			symbol_origin: SymbolOrigin::Repo { repo_id: RepoId::from("repo-abc") },
			resolution:    nudox_core::ResolutionState::Unresolved,
			kind:          None,
			source:        SourceChunk {
				raw_code:        "struct MyStruct {}".into(),
				treesitter_repr: None,
				symbol_span:     ByteSpan::covering(0, 18),
			},
			embeddings:    vec![],
			metadata:      ChunkMetadata {
				repo_id:             RepoId::from("repo-abc"),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan::covering(0, 18),
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		};
		(blob_ref, info)
	}

	#[tokio::test]
	async fn search_index_stores_entry() {
		let index = InMemorySearchIndex::new();
		let (blob_ref, info) = make_blob_info();

		index.index(&blob_ref, &info).await.unwrap();

		let entries = index.entries();
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].blob_ref, blob_ref);
		assert_eq!(entries[0].symbol_name, "my_crate::MyStruct");
		assert_eq!(entries[0].resolved_global_id, None);
	}

	#[tokio::test]
	async fn vector_index_upserts_and_retrieves() {
		let index = InMemoryVectorIndex::new();
		let blob_ref = BlobRef::from("vec-blob-1");
		let global_id = GlobalSymbolId(Uuid::new_v4());
		let embeddings = vec![EmbeddingRecord {
			model_type: nudox_core::ModelType::Mock,
			model:      ModelId::new("mock"),
			purpose:    EmbeddingPurpose::Code,
			vector:     Embedding::new(vec![0.1, 0.2, 0.3]).unwrap(),
		}];

		index.upsert(&blob_ref, global_id, &embeddings).await.unwrap();

		let stored = index.get(&blob_ref);
		assert_eq!(stored, Some(global_id));
	}

	#[tokio::test]
	async fn vector_index_upsert_overwrites() {
		let index = InMemoryVectorIndex::new();
		let blob_ref = BlobRef::from("vec-blob-2");
		let global_id_1 = GlobalSymbolId(Uuid::new_v4());
		let global_id_2 = GlobalSymbolId(Uuid::new_v4());
		let embeddings = vec![];

		index.upsert(&blob_ref, global_id_1, &embeddings).await.unwrap();
		index.upsert(&blob_ref, global_id_2, &embeddings).await.unwrap();

		assert_eq!(index.get(&blob_ref), Some(global_id_2));
	}

	#[tokio::test]
	async fn search_query_matches_by_symbol_name() {
		use nudox_core::SearchQuery;

		let index = InMemorySearchIndex::new();
		let (blob_ref, mut info) = make_blob_info();
		info.resolution = nudox_core::ResolutionState::Resolved(GlobalSymbolId(Uuid::new_v4()));
		index.index(&blob_ref, &info).await.unwrap();

		let hits = index.search("MyStruct", NonZeroUsize::new(10).unwrap()).await.unwrap();
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].blob_ref, blob_ref);
		assert_eq!(hits[0].symbol_name, "my_crate::MyStruct");
	}

	#[tokio::test]
	async fn search_query_find_by_global_id() {
		use nudox_core::SearchQuery;

		let index = InMemorySearchIndex::new();
		let (blob_ref, mut info) = make_blob_info();
		let gid = GlobalSymbolId(Uuid::new_v4());
		info.resolution = nudox_core::ResolutionState::Resolved(gid);
		index.index(&blob_ref, &info).await.unwrap();

		let hits = index.find_by_global_id(gid, NonZeroUsize::new(10).unwrap()).await.unwrap();
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].blob_ref, blob_ref);

		let not_found = index.find_by_global_id(GlobalSymbolId(Uuid::new_v4()), NonZeroUsize::new(10).unwrap()).await.unwrap();
		assert!(not_found.is_empty());
	}

	#[test]
	fn simd_cosine_matches_scalar_reference() {
		// Reference scalar cosine, matching the pre-SIMD implementation.
		fn scalar_cosine(a: &[f32], b: &[f32]) -> f32 {
			if a.len() != b.len() || a.is_empty() {
				return 0.0;
			}
			let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
			let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
			let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
			if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
		}

		// Lengths that exercise the 8-lane chunk plus a scalar remainder, an
		// exact multiple of 8, and the empty/short edge cases.
		for len in [0usize, 1, 7, 8, 13, 16, 33] {
			let a: Vec<f32> = (0..len).map(|i| (i as f32 * 0.37).sin()).collect();
			let b: Vec<f32> = (0..len).map(|i| (i as f32 * 0.91 + 1.0).cos()).collect();
			let qn = norm(&a);
			let simd = cosine_with_query_norm(&a, qn, &b);
			let scalar = scalar_cosine(&a, &b);
			assert!(
				(simd - scalar).abs() <= 1e-5,
				"len {len}: simd {simd} vs scalar {scalar}"
			);
		}
	}

	#[tokio::test]
	async fn vector_query_returns_top_k_by_cosine_similarity() {
		use nudox_core::VectorQuery;

		let index = InMemoryVectorIndex::new();
		let gid = GlobalSymbolId(Uuid::new_v4());

		// Two blobs: one close, one orthogonal.
		let close_ref = BlobRef::from("close");
		let far_ref = BlobRef::from("far");
		index
			.upsert(&close_ref, gid, &[EmbeddingRecord {
				model_type: nudox_core::ModelType::Mock,
				model:      ModelId::new("m"),
				purpose:    EmbeddingPurpose::Code,
				vector:     Embedding::new(vec![1.0, 0.0]).unwrap(),
			}])
			.await
			.unwrap();
		index
			.upsert(&far_ref, gid, &[EmbeddingRecord {
				model_type: nudox_core::ModelType::Mock,
				model:      ModelId::new("m"),
				purpose:    EmbeddingPurpose::Code,
				vector:     Embedding::new(vec![0.0, 1.0]).unwrap(),
			}])
			.await
			.unwrap();

		let hits = index.search(&[1.0, 0.0], NonZeroUsize::new(2).unwrap()).await.unwrap();
		assert_eq!(hits.len(), 2);
		assert_eq!(hits[0].blob_ref, close_ref, "most similar should be first");
		assert!(hits[0].score > hits[1].score);
	}
}
