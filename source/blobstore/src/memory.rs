use std::sync::{Arc, Mutex};
use rustc_hash::FxHashMap as HashMap;

use async_trait::async_trait;
use nudox_core::{BlobInfo, BlobRef, BlobStore, BlobStoreError, GlobalSymbolId, Result};

/// In-memory blob store for use in tests.
#[derive(Clone)]
pub struct InMemoryBlobStore {
	inner: Arc<Mutex<HashMap<BlobRef, BlobInfo>>>,
}

impl InMemoryBlobStore {
	/// Create an empty in-memory blob store.
	pub fn new() -> Self { Self { inner: Arc::new(Mutex::new(HashMap::default())) } }
}

impl Default for InMemoryBlobStore {
	fn default() -> Self { Self::new() }
}

#[async_trait]
impl BlobStore for InMemoryBlobStore {
	async fn put(&self, info: &BlobInfo) -> Result<BlobRef> {
		let blob_ref = BlobRef(uuid::Uuid::new_v4().to_string());
		let mut map = self.inner.lock().expect("mutex poisoned");
		map.insert(blob_ref.clone(), info.clone());
		Ok(blob_ref)
	}

	async fn get(&self, blob_ref: &BlobRef) -> Result<BlobInfo> {
		let map = self.inner.lock().expect("mutex poisoned");
		map.get(blob_ref).cloned().ok_or(nudox_core::Error::BlobStore(BlobStoreError::NotFound))
	}

	async fn update_resolution(&self, blob_ref: &BlobRef, global_id: GlobalSymbolId) -> Result<()> {
		let mut map = self.inner.lock().expect("mutex poisoned");
		let info =
			map.get_mut(blob_ref).ok_or(nudox_core::Error::BlobStore(BlobStoreError::NotFound))?;
		info.resolved_global_id = Some(global_id);
		Ok(())
	}

	async fn list(&self) -> Result<Vec<BlobRef>> {
		let map = self.inner.lock().expect("mutex poisoned");
		Ok(map.keys().cloned().collect())
	}
}

#[cfg(test)]
mod tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobStore, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, OccurrenceId, RepoId, SourceChunk, SymbolOrigin};

	use super::*;

	fn make_blob_info(symbol_name: &str) -> BlobInfo {
		BlobInfo {
			occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name:        symbol_name.to_string(),
			symbol_origin:      SymbolOrigin::Repo { repo_id: RepoId("test".into()) },
			resolved_global_id: None,
			kind:               None,
			source:             SourceChunk {
				raw_code:        "fn foo() {}".into(),
				treesitter_repr: None,
				symbol_span:     ByteSpan { start: 3, end: 6 },
			},
			embeddings:         vec![],
			metadata:           ChunkMetadata {
				repo_id:             RepoId("test".into()),
				file_path:           std::path::PathBuf::from("src/lib.rs"),
				file_span:           ByteSpan { start: 0, end: 11 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	#[tokio::test]
	async fn test_put_and_get_round_trip() {
		let store = InMemoryBlobStore::new();
		let info = make_blob_info("my_symbol");

		let blob_ref = store.put(&info).await.expect("put failed");
		let retrieved = store.get(&blob_ref).await.expect("get failed");

		assert_eq!(retrieved.symbol_name, "my_symbol");
	}

	#[tokio::test]
	async fn test_update_resolution_sets_global_id() {
		let store = InMemoryBlobStore::new();
		let info = make_blob_info("another_symbol");

		let blob_ref = store.put(&info).await.expect("put failed");
		assert!(store.get(&blob_ref).await.unwrap().resolved_global_id.is_none());

		let global_id = GlobalSymbolId(uuid::Uuid::new_v4());
		store.update_resolution(&blob_ref, global_id).await.expect("update_resolution failed");

		let updated = store.get(&blob_ref).await.expect("get after update failed");
		assert_eq!(updated.resolved_global_id, Some(global_id));
	}

	#[tokio::test]
	async fn test_get_missing_ref_returns_err() {
		let store = InMemoryBlobStore::new();
		let missing_ref = BlobRef("does-not-exist".into());

		let result = store.get(&missing_ref).await;
		assert!(result.is_err());
		match result.unwrap_err() {
			nudox_core::Error::BlobStore(nudox_core::BlobStoreError::NotFound) => {}
			nudox_core::Error::BlobStore(_) => panic!("expected NotFound"),
			other => panic!("unexpected error variant: {:?}", other),
		}
	}
}
