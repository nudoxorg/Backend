pub mod memory;

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
pub use memory::InMemoryBlobStore;
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobRef, BlobStore, Error, GlobalSymbolId, Result};
use object_store::{ObjectStore, local::LocalFileSystem, path::Path as ObjPath};

/// Blob store backed by `object_store`. v1 serializes `BlobInfo` as JSON.
///
/// Currently only the local-filesystem backend is wired up. An S3-backed
/// constructor is intended to land later behind a feature flag.
#[derive(Clone)]
pub struct ObjectStoreBlobStore {
	store: Arc<dyn ObjectStore>,
}

impl ObjectStoreBlobStore {
	/// Create a local-filesystem-backed blob store rooted at `root`.
	/// Creates the directory if it does not already exist.
	pub fn local(root: PathBuf) -> Result<Self> {
		std::fs::create_dir_all(&root)
			.map_err(|e| Error::BlobStore(format!("create_dir_all({}): {e}", root.display())))?;
		let fs = LocalFileSystem::new_with_prefix(&root)
			.map_err(|e| Error::BlobStore(format!("LocalFileSystem: {e}")))?;
		Ok(Self { store: Arc::new(fs) })
	}
}

fn path_for(blob_ref: &BlobRef) -> ObjPath { ObjPath::from(format!("blobs/{}.json", blob_ref.0)) }

#[async_trait]
impl BlobStore for ObjectStoreBlobStore {
	#[tracing::instrument(skip(self, info), fields(occurrence_id = %info.occurrence_id))]
	async fn put(&self, info: &BlobInfo) -> Result<BlobRef> {
		if info.metadata.blob_schema_version != BLOB_SCHEMA_VERSION {
			return Err(Error::BlobStore(format!(
				"schema version mismatch: expected {}, got {}",
				BLOB_SCHEMA_VERSION, info.metadata.blob_schema_version
			)));
		}
		let id = uuid::Uuid::new_v4().to_string();
		let blob_ref = BlobRef(id);
		let path = path_for(&blob_ref);
		let bytes =
			serde_json::to_vec(info).map_err(|e| Error::BlobStore(format!("serialize: {e}")))?;
		self.store.put(&path, bytes.into()).await.map_err(|e| Error::BlobStore(format!("put: {e}")))?;
		Ok(blob_ref)
	}

	#[tracing::instrument(skip(self), fields(blob_ref = %blob_ref))]
	async fn get(&self, blob_ref: &BlobRef) -> Result<BlobInfo> {
		let path = path_for(blob_ref);
		let get_result =
			self.store.get(&path).await.map_err(|e| Error::BlobStore(format!("get: {e}")))?;
		let bytes = get_result.bytes().await.map_err(|e| Error::BlobStore(format!("bytes: {e}")))?;
		let info: BlobInfo =
			serde_json::from_slice(&bytes).map_err(|e| Error::BlobStore(format!("deserialize: {e}")))?;
		if info.metadata.blob_schema_version != BLOB_SCHEMA_VERSION {
			return Err(Error::BlobStore(format!(
				"schema version mismatch on read: expected {}, got {}",
				BLOB_SCHEMA_VERSION, info.metadata.blob_schema_version
			)));
		}
		Ok(info)
	}

	#[tracing::instrument(skip(self), fields(blob_ref = %blob_ref, global_id = %global_id))]
	async fn update_resolution(&self, blob_ref: &BlobRef, global_id: GlobalSymbolId) -> Result<()> {
		let mut info = self.get(blob_ref).await?;
		info.resolved_global_id = Some(global_id);
		let path = path_for(blob_ref);
		let bytes =
			serde_json::to_vec(&info).map_err(|e| Error::BlobStore(format!("reserialize: {e}")))?;
		self
			.store
			.put(&path, bytes.into())
			.await
			.map_err(|e| Error::BlobStore(format!("put (update): {e}")))?;
		Ok(())
	}

	async fn list(&self) -> Result<Vec<BlobRef>> {
		let prefix = ObjPath::from("blobs");
		let result = self
			.store
			.list_with_delimiter(Some(&prefix))
			.await
			.map_err(|e| Error::BlobStore(format!("list: {e}")))?;
		let refs = result
			.objects
			.into_iter()
			.filter_map(|meta| {
				meta
					.location
					.filename()
					.and_then(|name| name.strip_suffix(".json"))
					.map(|uuid| BlobRef(uuid.to_string()))
			})
			.collect();
		Ok(refs)
	}
}

#[cfg(test)]
mod object_store_tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, Language, OccurrenceId, RepoId, SourceChunk, SymbolOrigin, TreesitterRepr};
	use tempfile::TempDir;

	use super::*;

	fn make_blob(symbol_name: &str, schema_version: u32) -> BlobInfo {
		BlobInfo {
			occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name:        symbol_name.into(),
			symbol_origin:      SymbolOrigin::Repo { repo_id: RepoId("test-repo".into()) },
			resolved_global_id: None,
			kind:               None,
			source:             SourceChunk {
				raw_code:        "fn x() {}".into(),
				treesitter_repr: TreesitterRepr(vec![]),
				symbol_span:     ByteSpan { start: 3, end: 4 },
			},
			embeddings:         vec![],
			metadata:           ChunkMetadata {
				repo_id:             RepoId("test-repo".into()),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan { start: 0, end: 9 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: schema_version,
			},
		}
	}

	#[tokio::test]
	async fn put_then_get_round_trips() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let info = make_blob("foo", BLOB_SCHEMA_VERSION);
		let expected_id = info.occurrence_id;
		let blob_ref = store.put(&info).await.unwrap();
		let got = store.get(&blob_ref).await.unwrap();
		assert_eq!(got.symbol_name, "foo");
		assert_eq!(got.occurrence_id.0, expected_id.0);
	}

	#[tokio::test]
	async fn update_resolution_persists() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let info = make_blob("bar", BLOB_SCHEMA_VERSION);
		let blob_ref = store.put(&info).await.unwrap();
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		store.update_resolution(&blob_ref, gid).await.unwrap();
		let got = store.get(&blob_ref).await.unwrap();
		assert_eq!(got.resolved_global_id.map(|g| g.0), Some(gid.0));
	}

	#[tokio::test]
	async fn get_missing_returns_err() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let result = store.get(&BlobRef("nonexistent".into())).await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn put_with_wrong_schema_version_rejected() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let bad = make_blob("baz", 999);
		let result = store.put(&bad).await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn list_returns_all_put_refs() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		assert!(store.list().await.unwrap().is_empty());

		let r1 = store.put(&make_blob("a", BLOB_SCHEMA_VERSION)).await.unwrap();
		let r2 = store.put(&make_blob("b", BLOB_SCHEMA_VERSION)).await.unwrap();
		let mut refs = store.list().await.unwrap();
		refs.sort_by_key(|r| r.0.clone());
		let mut expected = vec![r1, r2];
		expected.sort_by_key(|r| r.0.clone());
		assert_eq!(refs, expected);
	}

	#[tokio::test]
	async fn blobs_persist_across_store_reopen() {
		let tmp = TempDir::new().unwrap();
		let blob_ref;
		let occurrence_id;
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		{
			let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
			let info = make_blob("persistent_fn", BLOB_SCHEMA_VERSION);
			occurrence_id = info.occurrence_id;
			blob_ref = store.put(&info).await.unwrap();
			store.update_resolution(&blob_ref, gid).await.unwrap();
		}
		// Re-open at the same root — the new store should see the persisted JSON files.
		let store2 = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let got = store2.get(&blob_ref).await.unwrap();
		assert_eq!(got.symbol_name, "persistent_fn");
		assert_eq!(got.occurrence_id.0, occurrence_id.0);
		assert_eq!(got.resolved_global_id.map(|g| g.0), Some(gid.0));
		let refs = store2.list().await.unwrap();
		assert_eq!(refs.len(), 1);
		assert_eq!(refs[0], blob_ref);
	}
}
