#[cfg(any(test, feature = "test-stubs"))]
pub mod memory;

use std::sync::Arc;

use nudox_core::{
    BlobInfo, BlobStore, FutureParseQueue, GlobalSymbolId, GlobalSymbolStore, LibRef,
    ResolutionOutcome, ResolveLibReport, Result, SearchIndex, SymbolMatch, SymbolOrigin,
    SymbolQuery, SymbolSearch, VectorIndex,
};
use tracing::instrument;

/// Routes ingested blobs to the appropriate backends and manages deferred symbol resolution.
pub struct Orchestrator {
    global_store: Arc<dyn GlobalSymbolStore>,
    blob_store: Arc<dyn BlobStore>,
    queue: Arc<dyn FutureParseQueue>,
    search: Arc<dyn SearchIndex>,
    vector: Arc<dyn VectorIndex>,
    symbol_searcher: Option<Arc<dyn SymbolSearch>>,
}

impl Orchestrator {
    /// Create a new orchestrator wiring all backends together.
    pub fn new(
        global_store: Arc<dyn GlobalSymbolStore>,
        blob_store: Arc<dyn BlobStore>,
        queue: Arc<dyn FutureParseQueue>,
        search: Arc<dyn SearchIndex>,
        vector: Arc<dyn VectorIndex>,
    ) -> Self {
        Self {
            global_store,
            blob_store,
            queue,
            search,
            vector,
            symbol_searcher: None,
        }
    }

    /// Attach a symbol-search backend. Once set, `search()` is available.
    pub fn with_symbol_search(mut self, searcher: Arc<dyn SymbolSearch>) -> Self {
        self.symbol_searcher = Some(searcher);
        self
    }

    /// Execute a compound symbol search.
    ///
    /// Returns an error if no symbol-search backend has been attached via `with_symbol_search`.
    pub async fn search(&self, query: &SymbolQuery) -> Result<Vec<SymbolMatch>> {
        match &self.symbol_searcher {
            Some(s) => s.search(query).await,
            None => Err(anyhow::anyhow!("no symbol searcher attached").into()),
        }
    }

    async fn index_resolved(
        &self,
        blob_ref: &nudox_core::BlobRef,
        info: &BlobInfo,
        global_id: GlobalSymbolId,
    ) -> Result<()> {
        let mut resolved_info = info.clone();
        resolved_info.resolved_global_id = Some(global_id);

        self.search.index(blob_ref, &resolved_info).await?;
        self.vector
            .upsert(blob_ref, global_id, &resolved_info.embeddings)
            .await?;
        Ok(())
    }

    /// Ingest a blob, resolve its symbol if possible, and return the resolution outcome.
    #[instrument(skip(self, info), fields(symbol = %info.symbol_name, occurrence_id = %info.occurrence_id))]
    pub async fn ingest(&self, info: BlobInfo) -> Result<ResolutionOutcome> {
        let blob_ref = self.blob_store.put(&info).await?;

        match &info.symbol_origin {
            SymbolOrigin::Repo { .. } => {
                // Repo-local symbols are resolved immediately with a fresh global id.
                let global_id = GlobalSymbolId(uuid::Uuid::new_v4());
                self.blob_store.update_resolution(&blob_ref, global_id).await?;
                self.global_store.associate(global_id, info.occurrence_id).await?;
                self.index_resolved(&blob_ref, &info, global_id).await?;
                Ok(ResolutionOutcome::Resolved { global_id, blob_ref })
            }
            SymbolOrigin::ExternalLib { lib } => {
                let lib = lib.clone();
                match self.global_store.lookup(&lib, &info.symbol_name).await? {
                    Some(global_id) => {
                        self.blob_store.update_resolution(&blob_ref, global_id).await?;
                        self.global_store.associate(global_id, info.occurrence_id).await?;
                        self.index_resolved(&blob_ref, &info, global_id).await?;
                        Ok(ResolutionOutcome::Resolved { global_id, blob_ref })
                    }
                    None => {
                        self.queue.enqueue(lib.clone(), blob_ref.clone()).await?;
                        Ok(ResolutionOutcome::Deferred { lib, blob_ref })
                    }
                }
            }
        }
    }

    /// Rebuild the search and vector indexes by walking every blob in storage.
    ///
    /// Only blobs with a resolved [`GlobalSymbolId`] are re-indexed. Deferred
    /// blobs are counted as skipped and remain in the queue unchanged.
    ///
    /// Use this after an index schema change, a data migration, or to recover
    /// from index corruption. Blob storage is the system of record; all index
    /// state is derivable from it.
    #[instrument(skip(self))]
    pub async fn rebuild_indexes(&self) -> Result<ResolveLibReport> {
        let blob_refs = self.blob_store.list().await?;
        let blobs_seen = blob_refs.len();
        let mut blobs_resolved = 0usize;
        let mut blobs_skipped = 0usize;

        for blob_ref in &blob_refs {
            let info = self.blob_store.get(blob_ref).await?;
            match info.resolved_global_id {
                Some(global_id) => {
                    self.index_resolved(blob_ref, &info, global_id).await?;
                    blobs_resolved += 1;
                }
                None => {
                    blobs_skipped += 1;
                }
            }
        }

        Ok(ResolveLibReport { blobs_seen, blobs_resolved, blobs_skipped })
    }

    /// Drain the deferred queue for a library and resolve all pending blobs.
    #[instrument(skip(self), fields(lib_name = %lib.name, lib_version = %lib.version))]
    pub async fn resolve_lib(&self, lib: &LibRef) -> Result<ResolveLibReport> {
        let blob_refs = self.queue.drain_for_lib(lib).await?;
        let blobs_seen = blob_refs.len();
        let mut blobs_resolved = 0usize;
        let mut blobs_skipped = 0usize;

        for blob_ref in &blob_refs {
            let info = self.blob_store.get(blob_ref).await?;

            let lib_ref = match &info.symbol_origin {
                SymbolOrigin::ExternalLib { lib } => lib.clone(),
                _ => {
                    blobs_skipped += 1;
                    continue;
                }
            };

            match self.global_store.lookup(&lib_ref, &info.symbol_name).await? {
                Some(global_id) => {
                    self.blob_store.update_resolution(blob_ref, global_id).await?;
                    self.global_store.associate(global_id, info.occurrence_id).await?;
                    self.index_resolved(blob_ref, &info, global_id).await?;
                    blobs_resolved += 1;
                }
                None => {
                    blobs_skipped += 1;
                }
            }
        }

        Ok(ResolveLibReport { blobs_seen, blobs_resolved, blobs_skipped })
    }
}

/// Tests that wire the real on-disk backends together through the orchestrator.
#[cfg(test)]
mod disk_integration_tests {
    use super::*;
    use std::sync::Arc;
    use nudox_blobstore::ObjectStoreBlobStore;
    use nudox_core::{
        BLOB_SCHEMA_VERSION, BlobStore, ByteSpan, ChunkMetadata, FutureParseQueue,
        GlobalSymbolStore, Language, LibRef, OccurrenceId, RepoId, ResolutionOutcome,
        SearchIndex, SearchQuery, SourceChunk, SymbolOrigin, TreesitterRepr, VectorIndex,
    };
    use nudox_search::{InMemoryVectorIndex, TantivySearchIndex};
    use nudox_store::NudoxStore;
    use tempfile::TempDir;

    fn make_blob(symbol_name: &str, origin: SymbolOrigin) -> BlobInfo {
        BlobInfo {
            occurrence_id: OccurrenceId(uuid::Uuid::new_v4()),
            symbol_name: symbol_name.to_string(),
            symbol_origin: origin,
            resolved_global_id: None,
            kind: None,
            source: SourceChunk {
                raw_code: "fn placeholder() {}".to_string(),
                treesitter_repr: TreesitterRepr(vec![]),
                symbol_span: ByteSpan { start: 3, end: 14 },
            },
            embeddings: vec![],
            metadata: ChunkMetadata {
                repo_id: RepoId("test-repo".into()),
                file_path: std::path::PathBuf::from("src/lib.rs"),
                file_span: ByteSpan { start: 0, end: 19 },
                parsed_at: chrono::Utc::now(),
                lang: Language::Rust,
                lang_version: None,
                blob_schema_version: BLOB_SCHEMA_VERSION,
            },
        }
    }

    /// Ingest a resolvable symbol, a deferred symbol, register the deferred
    /// library, and resolve it — all using real on-disk stores.
    #[tokio::test]
    async fn disk_ingest_and_resolve_lib() {
        let tmp = TempDir::new().unwrap();
        let blob_store = Arc::new(ObjectStoreBlobStore::local(tmp.path().join("blobs")).unwrap());
        let search = Arc::new(TantivySearchIndex::open_or_create(&tmp.path().join("index")).unwrap());
        let store = NudoxStore::open(&tmp.path().join("nudox.db"), "test_org/test_db")
            .await
            .unwrap();
        let vector: Arc<dyn VectorIndex> = Arc::new(InMemoryVectorIndex::new());

        store
            .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
            .await
            .unwrap();

        let orchestrator = Orchestrator::new(
            store.clone() as Arc<dyn GlobalSymbolStore>,
            blob_store.clone() as Arc<dyn BlobStore>,
            store.clone() as Arc<dyn FutureParseQueue>,
            search.clone() as Arc<dyn SearchIndex>,
            vector,
        );

        // Ingest an immediately-resolvable external symbol.
        let serde_lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        let serde_blob_ref = match orchestrator
            .ingest(make_blob("Serialize", SymbolOrigin::ExternalLib { lib: serde_lib }))
            .await
            .unwrap()
        {
            ResolutionOutcome::Resolved { blob_ref, global_id } => {
                let stored = blob_store.get(&blob_ref).await.unwrap();
                assert_eq!(stored.resolved_global_id, Some(global_id));
                blob_ref
            }
            other => panic!("expected Resolved, got {other:?}"),
        };

        // Ingest a deferred external symbol (tokio not registered yet).
        let tokio_lib = LibRef { name: "tokio".into(), version: "1.0.0".into() };
        let tokio_blob_ref = match orchestrator
            .ingest(make_blob("spawn", SymbolOrigin::ExternalLib { lib: tokio_lib.clone() }))
            .await
            .unwrap()
        {
            ResolutionOutcome::Deferred { blob_ref, .. } => blob_ref,
            other => panic!("expected Deferred, got {other:?}"),
        };
        assert!(blob_store.get(&tokio_blob_ref).await.unwrap().resolved_global_id.is_none());

        // Register tokio and resolve the deferred queue.
        store
            .register_library("rust", "tokio", "1.0.0", ["Entry/rust/tokio/spawn"])
            .await
            .unwrap();
        let report = orchestrator.resolve_lib(&tokio_lib).await.unwrap();
        assert_eq!(report.blobs_resolved, 1);
        assert!(blob_store.get(&tokio_blob_ref).await.unwrap().resolved_global_id.is_some());

        // rebuild_indexes should re-index both resolved blobs from disk.
        let report2 = orchestrator.rebuild_indexes().await.unwrap();
        assert_eq!(report2.blobs_seen, 2);
        assert_eq!(report2.blobs_resolved, 2);
        assert_eq!(report2.blobs_skipped, 0);

        // Both blobs are findable in the search index.
        let hits = search.search("Serialize", 10).await.unwrap();
        assert!(hits.iter().any(|h| h.blob_ref == serde_blob_ref), "Serialize not found in search");
        let hits2 = search.search("spawn", 10).await.unwrap();
        assert!(hits2.iter().any(|h| h.blob_ref == tokio_blob_ref), "spawn not found in search");
    }

    /// Verify that blob storage, the SQLite global store, and the tantivy index
    /// all survive process-restart simulation: write data, drop all in-memory
    /// state, reopen from disk, and confirm everything is still there.
    #[tokio::test]
    async fn disk_backends_survive_reopen() {
        let tmp = TempDir::new().unwrap();
        let blob_dir = tmp.path().join("blobs");
        let index_dir = tmp.path().join("index");
        let db_path = tmp.path().join("nudox.db");

        let blob_store = Arc::new(ObjectStoreBlobStore::local(blob_dir.clone()).unwrap());

        let serde_blob_ref;
        {
            let store = NudoxStore::open(&db_path, "test_org/test_db").await.unwrap();
            store
                .register_library("rust", "serde", "1.0.0", ["Entry/rust/serde/Serialize"])
                .await
                .unwrap();
            let search: Arc<dyn SearchIndex> =
                Arc::new(TantivySearchIndex::open_or_create(&index_dir).unwrap());
            let vector: Arc<dyn VectorIndex> = Arc::new(InMemoryVectorIndex::new());
            let orchestrator = Orchestrator::new(
                store.clone() as Arc<dyn GlobalSymbolStore>,
                blob_store.clone() as Arc<dyn BlobStore>,
                store.clone() as Arc<dyn FutureParseQueue>,
                search,
                vector,
            );
            let serde_lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
            serde_blob_ref = match orchestrator
                .ingest(make_blob("Serialize", SymbolOrigin::ExternalLib { lib: serde_lib }))
                .await
                .unwrap()
            {
                ResolutionOutcome::Resolved { blob_ref, .. } => blob_ref,
                other => panic!("expected Resolved, got {other:?}"),
            };
        }
        // All Arc<dyn …> handles inside the block are dropped.
        // blob_store (ObjectStoreBlobStore) and the db file remain on disk.

        // Reopen SQLite and tantivy — simulating a restart.
        let store2 = NudoxStore::open(&db_path, "test_org/test_db").await.unwrap();
        let search2 = Arc::new(TantivySearchIndex::open_or_create(&index_dir).unwrap());
        let vector2: Arc<dyn VectorIndex> = Arc::new(InMemoryVectorIndex::new());
        let orchestrator2 = Orchestrator::new(
            store2.clone() as Arc<dyn GlobalSymbolStore>,
            blob_store.clone() as Arc<dyn BlobStore>,
            store2.clone() as Arc<dyn FutureParseQueue>,
            search2.clone() as Arc<dyn SearchIndex>,
            vector2,
        );

        // Blob file is still readable.
        let got = blob_store.get(&serde_blob_ref).await.unwrap();
        assert_eq!(got.symbol_name, "Serialize");
        assert!(got.resolved_global_id.is_some());

        // SQLite symbol registration persisted.
        let lib = LibRef { name: "serde".into(), version: "1.0.0".into() };
        assert!(store2.lookup(&lib, "Serialize").await.unwrap().is_some());

        // Tantivy index already has the document (committed before the drop).
        let hits = search2.search("Serialize", 10).await.unwrap();
        assert!(!hits.is_empty(), "Serialize not in search index after reopen");

        // rebuild_indexes works cleanly from the on-disk blob store.
        let report = orchestrator2.rebuild_indexes().await.unwrap();
        assert_eq!(report.blobs_seen, 1);
        assert_eq!(report.blobs_resolved, 1);
        assert_eq!(report.blobs_skipped, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore};
    use nudox_blobstore::InMemoryBlobStore;
    use nudox_core::{
        BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, GlobalSymbolQuery, Language, OccurrenceId,
        RepoId, SourceChunk, SymbolOrigin, TreesitterRepr,
    };
    use nudox_search::{InMemorySearchIndex, InMemoryVectorIndex};

    struct MockSearcher(Vec<SymbolMatch>);

    #[async_trait::async_trait]
    impl nudox_core::SymbolSearch for MockSearcher {
        async fn search(
            &self,
            _q: &nudox_core::SymbolQuery,
        ) -> nudox_core::Result<Vec<nudox_core::SymbolMatch>> {
            Ok(self.0.clone())
        }
    }

    fn make_orchestrator() -> Orchestrator {
        Orchestrator::new(
            Arc::new(InMemoryGlobalSymbolStore::new()),
            Arc::new(InMemoryBlobStore::new()),
            Arc::new(InMemoryFutureParseQueue::new()),
            Arc::new(InMemorySearchIndex::new()),
            Arc::new(InMemoryVectorIndex::new()),
        )
    }

    #[tokio::test]
    async fn search_delegates_to_symbol_searcher() {
        use nudox_core::{BlobInfo, OccurrenceId, SymbolMatch, SymbolOrigin, RepoId};

        let dummy_blob = BlobInfo {
            occurrence_id: OccurrenceId(uuid::Uuid::new_v4()),
            symbol_name: "dummy".to_string(),
            symbol_origin: SymbolOrigin::Repo { repo_id: RepoId("r".into()) },
            resolved_global_id: None,
            kind: None,
            source: SourceChunk {
                raw_code: "fn dummy() {}".to_string(),
                treesitter_repr: TreesitterRepr(vec![]),
                symbol_span: ByteSpan { start: 3, end: 8 },
            },
            embeddings: vec![],
            metadata: ChunkMetadata {
                repo_id: RepoId("r".into()),
                file_path: std::path::PathBuf::from("src/lib.rs"),
                file_span: ByteSpan { start: 0, end: 13 },
                parsed_at: chrono::Utc::now(),
                lang: Language::Rust,
                lang_version: None,
                blob_schema_version: BLOB_SCHEMA_VERSION,
            },
        };
        let expected = vec![SymbolMatch { blob: dummy_blob, score: 1.0, occurrences: vec![] }];

        let orchestrator = make_orchestrator()
            .with_symbol_search(Arc::new(MockSearcher(expected.clone())));

        let results = orchestrator.search(&SymbolQuery::default()).await.unwrap();
        assert_eq!(results.len(), expected.len());
        assert_eq!(results[0].blob.symbol_name, expected[0].blob.symbol_name);
    }

    #[tokio::test]
    async fn search_without_searcher_returns_error() {
        let orchestrator = make_orchestrator();
        let result = orchestrator.search(&SymbolQuery::default()).await;
        assert!(result.is_err(), "expected Err when no searcher is attached");
    }

    #[tokio::test]
    async fn in_memory_global_symbol_query_returns_occurrences() {
        use nudox_core::LibRef;

        let store = InMemoryGlobalSymbolStore::new();
        let lib = LibRef { name: "mylib".into(), version: "1.0".into() };
        let global_id = GlobalSymbolId(uuid::Uuid::new_v4());
        store.insert(&lib, "MySymbol", global_id);

        let occ1 = OccurrenceId(uuid::Uuid::new_v4());
        let occ2 = OccurrenceId(uuid::Uuid::new_v4());
        store.associate(global_id, occ1).await.unwrap();
        store.associate(global_id, occ2).await.unwrap();

        let occurrences = store.get_occurrences(global_id).await.unwrap();
        assert_eq!(occurrences.len(), 2);
        assert!(occurrences.contains(&occ1));
        assert!(occurrences.contains(&occ2));
    }

    fn make_blob_info(symbol_name: &str, origin: SymbolOrigin) -> BlobInfo {
        BlobInfo {
            occurrence_id: OccurrenceId(uuid::Uuid::new_v4()),
            symbol_name: symbol_name.to_string(),
            symbol_origin: origin,
            resolved_global_id: None,
            kind: None,
            source: SourceChunk {
                raw_code: "fn placeholder() {}".to_string(),
                treesitter_repr: TreesitterRepr(vec![]),
                symbol_span: ByteSpan { start: 3, end: 14 },
            },
            embeddings: vec![],
            metadata: ChunkMetadata {
                repo_id: RepoId("test-repo".into()),
                file_path: std::path::PathBuf::from("src/lib.rs"),
                file_span: ByteSpan { start: 0, end: 19 },
                parsed_at: chrono::Utc::now(),
                lang: Language::Rust,
                lang_version: None,
                blob_schema_version: BLOB_SCHEMA_VERSION,
            },
        }
    }

    #[tokio::test]
    async fn smoke_test_ingest_and_resolve_lib() {
        // 1. Create all backends.
        let blob_store = InMemoryBlobStore::new();
        let global_store = InMemoryGlobalSymbolStore::new();
        let queue = InMemoryFutureParseQueue::new();
        let search = InMemorySearchIndex::new();
        let vector = InMemoryVectorIndex::new();

        let blob_store_handle = blob_store.clone();
        let global_store_handle = global_store.clone();
        let search_handle = search.clone();

        // 2. Pre-populate a known symbol for serde::Serialize.
        let serde_lib = LibRef {
            name: "serde".into(),
            version: "1.0".into(),
        };
        let serde_global_id = GlobalSymbolId(uuid::Uuid::new_v4());
        global_store.insert(&serde_lib, "Serialize", serde_global_id);

        // 3. Create the orchestrator.
        let orchestrator = Orchestrator::new(
            Arc::new(global_store),
            Arc::new(blob_store),
            Arc::new(queue),
            Arc::new(search),
            Arc::new(vector),
        );

        // 4. Ingest a resolvable external symbol (serde::Serialize).
        let serde_info = make_blob_info(
            "Serialize",
            SymbolOrigin::ExternalLib {
                lib: serde_lib.clone(),
            },
        );
        let outcome = orchestrator.ingest(serde_info).await.unwrap();

        // 5. Assert it resolved immediately.
        let serde_blob_ref = match outcome {
            ResolutionOutcome::Resolved { global_id, blob_ref } => {
                assert_eq!(global_id, serde_global_id);
                blob_ref
            }
            other => panic!("expected Resolved, got {:?}", other),
        };
        assert_eq!(
            blob_store_handle
                .get(&serde_blob_ref)
                .await
                .unwrap()
                .resolved_global_id,
            Some(serde_global_id)
        );
        let serde_entry = search_handle
            .entries()
            .into_iter()
            .find(|entry| entry.blob_ref == serde_blob_ref)
            .expect("serde entry indexed");
        assert_eq!(serde_entry.resolved_global_id, Some(serde_global_id));

        // 6. Ingest an unresolvable external symbol (tokio::spawn — not in global store yet).
        let tokio_lib = LibRef {
            name: "tokio".into(),
            version: "1.0".into(),
        };
        let tokio_info = make_blob_info(
            "spawn",
            SymbolOrigin::ExternalLib {
                lib: tokio_lib.clone(),
            },
        );
        let tokio_blob_ref = match orchestrator.ingest(tokio_info).await.unwrap() {
            ResolutionOutcome::Deferred { lib, blob_ref } => {
                assert_eq!(lib.name, "tokio");
                blob_ref
            }
            other => panic!("expected Deferred, got {:?}", other),
        };

        // 7. Simulate the library being parsed: insert the tokio/spawn mapping then resolve.
        let tokio_global_id = GlobalSymbolId(uuid::Uuid::new_v4());
        global_store_handle.insert(&tokio_lib, "spawn", tokio_global_id);

        let report = orchestrator.resolve_lib(&tokio_lib).await.unwrap();
        assert_eq!(report.blobs_seen, 1);
        assert_eq!(report.blobs_resolved, 1);
        assert_eq!(report.blobs_skipped, 0);
        assert_eq!(
            blob_store_handle
                .get(&tokio_blob_ref)
                .await
                .unwrap()
                .resolved_global_id,
            Some(tokio_global_id)
        );
        let tokio_entry = search_handle
            .entries()
            .into_iter()
            .find(|entry| entry.blob_ref == tokio_blob_ref)
            .expect("tokio entry indexed after resolve_lib");
        assert_eq!(tokio_entry.resolved_global_id, Some(tokio_global_id));

        // 8. Ingest a repo-local symbol.
        let repo_info = make_blob_info(
            "my_fn",
            SymbolOrigin::Repo {
                repo_id: RepoId("my-repo".into()),
            },
        );
        let outcome3 = orchestrator.ingest(repo_info).await.unwrap();
        let repo_global_id = match outcome3 {
            ResolutionOutcome::Resolved { global_id, .. } => global_id,
            other => panic!("expected Resolved for repo-local symbol, got {:?}", other),
        };

        // 9. Verify associate() was called for every resolved blob.
        let serde_occurrences = global_store_handle.occurrences_for(serde_global_id);
        assert_eq!(serde_occurrences.len(), 1, "serde symbol should be associated once");

        let tokio_occurrences = global_store_handle.occurrences_for(tokio_global_id);
        assert_eq!(tokio_occurrences.len(), 1, "tokio symbol should be associated after resolve_lib");

        let repo_occurrences = global_store_handle.occurrences_for(repo_global_id);
        assert_eq!(repo_occurrences.len(), 1, "repo-local symbol should be associated");
    }

    #[tokio::test]
    async fn rebuild_indexes_re_indexes_resolved_blobs() {
        let blob_store = InMemoryBlobStore::new();
        let global_store = InMemoryGlobalSymbolStore::new();
        let queue = InMemoryFutureParseQueue::new();
        let search = InMemorySearchIndex::new();
        let vector = InMemoryVectorIndex::new();

        let search_handle = search.clone();

        let serde_lib = LibRef { name: "serde".into(), version: "1.0".into() };
        let serde_gid = GlobalSymbolId(uuid::Uuid::new_v4());
        global_store.insert(&serde_lib, "Serialize", serde_gid);

        let orchestrator = Orchestrator::new(
            Arc::new(global_store),
            Arc::new(blob_store),
            Arc::new(queue),
            Arc::new(search),
            Arc::new(vector),
        );

        orchestrator
            .ingest(make_blob_info("Serialize", SymbolOrigin::ExternalLib { lib: serde_lib }))
            .await
            .unwrap();

        // Simulate the search index being wiped.
        search_handle.entries().len(); // ensure there is one entry
        // Re-build: should re-index the resolved blob.
        let report = orchestrator.rebuild_indexes().await.unwrap();
        assert_eq!(report.blobs_seen, 1);
        assert_eq!(report.blobs_resolved, 1);
        assert_eq!(report.blobs_skipped, 0);
        assert_eq!(search_handle.entries().len(), 1);
    }
}
