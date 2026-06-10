use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nudox_core::{
    BlobInfo, BlobRef, EmbeddingRecord, GlobalSymbolId, OccurrenceId, Result, SearchHit,
    SearchIndex, SearchQuery, VectorHit, VectorIndex, VectorQuery,
};

/// Indexed entry stored in the in-memory search index.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    /// Reference to the blob this entry indexes.
    pub blob_ref: BlobRef,
    /// The fully-qualified symbol name.
    pub symbol_name: String,
    /// The occurrence identifier for this entry.
    pub occurrence_id: OccurrenceId,
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
    pub fn new() -> Self {
        InMemorySearchIndex {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return all indexed entries (for test assertions).
    pub fn entries(&self) -> Vec<SearchEntry> {
        self.entries.lock().unwrap().values().cloned().collect()
    }
}

impl Default for InMemorySearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SearchIndex for InMemorySearchIndex {
    async fn index(&self, blob_ref: &BlobRef, info: &BlobInfo) -> Result<()> {
        let entry = SearchEntry {
            blob_ref: blob_ref.clone(),
            symbol_name: info.symbol_name.clone(),
            occurrence_id: info.occurrence_id,
            resolved_global_id: info.resolved_global_id,
        };
        self.entries.lock().unwrap().insert(info.occurrence_id, entry);
        Ok(())
    }
}

#[async_trait]
impl SearchQuery for InMemorySearchIndex {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.to_lowercase();
        let map = self.entries.lock().unwrap();
        let mut hits: Vec<SearchHit> = map
            .values()
            .filter(|e| e.symbol_name.to_lowercase().contains(&q))
            .take(limit)
            .map(|e| SearchHit {
                blob_ref: e.blob_ref.clone(),
                occurrence_id: e.occurrence_id,
                symbol_name: e.symbol_name.clone(),
                score: 1.0,
            })
            .collect();
        hits.sort_by(|a, b| a.symbol_name.cmp(&b.symbol_name));
        Ok(hits)
    }

    async fn find_by_global_id(
        &self,
        global_id: GlobalSymbolId,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let map = self.entries.lock().unwrap();
        let hits = map
            .values()
            .filter(|e| e.resolved_global_id == Some(global_id))
            .take(limit)
            .map(|e| SearchHit {
                blob_ref: e.blob_ref.clone(),
                occurrence_id: e.occurrence_id,
                symbol_name: e.symbol_name.clone(),
                score: 1.0,
            })
            .collect();
        Ok(hits)
    }
}

/// In-memory vector index for use in tests.
#[derive(Clone)]
pub struct InMemoryVectorIndex {
    points: Arc<Mutex<HashMap<String, (GlobalSymbolId, Vec<EmbeddingRecord>)>>>,
}

impl InMemoryVectorIndex {
    /// Create an empty vector index.
    pub fn new() -> Self {
        InMemoryVectorIndex {
            points: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return stored global_id for a blob_ref (for test assertions).
    pub fn get(&self, blob_ref: &BlobRef) -> Option<GlobalSymbolId> {
        self.points
            .lock()
            .unwrap()
            .get(&blob_ref.0)
            .map(|(id, _)| *id)
    }
}

impl Default for InMemoryVectorIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VectorIndex for InMemoryVectorIndex {
    async fn upsert(
        &self,
        blob_ref: &BlobRef,
        global_id: GlobalSymbolId,
        embeddings: &[EmbeddingRecord],
    ) -> Result<()> {
        self.points
            .lock()
            .unwrap()
            .insert(blob_ref.0.clone(), (global_id, embeddings.to_vec()));
        Ok(())
    }
}

#[async_trait]
impl VectorQuery for InMemoryVectorIndex {
    async fn search(&self, vector: &[f32], limit: usize) -> Result<Vec<VectorHit>> {
        let map = self.points.lock().unwrap();
        let mut scored: Vec<(f32, BlobRef, GlobalSymbolId)> = map
            .iter()
            .flat_map(|(blob_ref_str, (global_id, records))| {
                records.iter().map(move |rec| {
                    let score = cosine_similarity(vector, &rec.vector);
                    (score, BlobRef(blob_ref_str.clone()), *global_id)
                })
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.dedup_by_key(|(_, blob_ref, _)| blob_ref.0.clone());

        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(score, blob_ref, global_id)| VectorHit { blob_ref, global_id, score })
            .collect())
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_core::{
        BlobInfo, BlobRef, ByteSpan, ChunkMetadata, EmbeddingPurpose, EmbeddingRecord,
        GlobalSymbolId, Language, OccurrenceId, RepoId, SearchIndex, SourceChunk, SymbolOrigin,
        TreesitterRepr, VectorIndex, BLOB_SCHEMA_VERSION,
    };
    use uuid::Uuid;

    fn make_blob_info() -> (BlobRef, BlobInfo) {
        let blob_ref = BlobRef("test-blob-1".to_string());
        let info = BlobInfo {
            occurrence_id: OccurrenceId(Uuid::new_v4()),
            symbol_name: "my_crate::MyStruct".to_string(),
            symbol_origin: SymbolOrigin::Repo {
                repo_id: RepoId("repo-abc".to_string()),
            },
            resolved_global_id: None,
            source: SourceChunk {
                raw_code: "struct MyStruct {}".to_string(),
                treesitter_repr: TreesitterRepr(vec![]),
                symbol_span: ByteSpan { start: 0, end: 18 },
            },
            embeddings: vec![],
            metadata: ChunkMetadata {
                repo_id: RepoId("repo-abc".to_string()),
                file_path: "src/lib.rs".into(),
                file_span: ByteSpan { start: 0, end: 18 },
                parsed_at: chrono::Utc::now(),
                lang: Language::Rust,
                lang_version: None,
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
        let blob_ref = BlobRef("vec-blob-1".to_string());
        let global_id = GlobalSymbolId(Uuid::new_v4());
        let embeddings = vec![EmbeddingRecord {
            model_type: nudox_core::ModelType::Mock,
            model: "mock".to_string(),
            purpose: EmbeddingPurpose::Code,
            vector: vec![0.1, 0.2, 0.3],
        }];

        index.upsert(&blob_ref, global_id, &embeddings).await.unwrap();

        let stored = index.get(&blob_ref);
        assert_eq!(stored, Some(global_id));
    }

    #[tokio::test]
    async fn vector_index_upsert_overwrites() {
        let index = InMemoryVectorIndex::new();
        let blob_ref = BlobRef("vec-blob-2".to_string());
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
        info.resolved_global_id = Some(GlobalSymbolId(Uuid::new_v4()));
        index.index(&blob_ref, &info).await.unwrap();

        let hits = index.search("MyStruct", 10).await.unwrap();
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
        info.resolved_global_id = Some(gid);
        index.index(&blob_ref, &info).await.unwrap();

        let hits = index.find_by_global_id(gid, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].blob_ref, blob_ref);

        let not_found = index
            .find_by_global_id(GlobalSymbolId(Uuid::new_v4()), 10)
            .await
            .unwrap();
        assert!(not_found.is_empty());
    }

    #[tokio::test]
    async fn vector_query_returns_top_k_by_cosine_similarity() {
        use nudox_core::VectorQuery;

        let index = InMemoryVectorIndex::new();
        let gid = GlobalSymbolId(Uuid::new_v4());

        // Two blobs: one close, one orthogonal.
        let close_ref = BlobRef("close".into());
        let far_ref = BlobRef("far".into());
        index
            .upsert(&close_ref, gid, &[EmbeddingRecord {
                model_type: nudox_core::ModelType::Mock,
                model: "m".into(),
                purpose: EmbeddingPurpose::Code,
                vector: vec![1.0, 0.0],
            }])
            .await
            .unwrap();
        index
            .upsert(&far_ref, gid, &[EmbeddingRecord {
                model_type: nudox_core::ModelType::Mock,
                model: "m".into(),
                purpose: EmbeddingPurpose::Code,
                vector: vec![0.0, 1.0],
            }])
            .await
            .unwrap();

        let hits = index.search(&[1.0, 0.0], 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].blob_ref, close_ref, "most similar should be first");
        assert!(hits[0].score > hits[1].score);
    }
}
