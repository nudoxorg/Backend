pub mod memory;

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
pub use memory::InMemoryBlobStore;
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobRef, BlobStore, BlobStoreError, GlobalSymbolId, Result};
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
			.map_err(|e| BlobStoreError::CreateDir(Box::new(e)))?;
		let fs = LocalFileSystem::new_with_prefix(&root)
			.map_err(|e| BlobStoreError::Filesystem(Box::new(e)))?;
		Ok(Self { store: Arc::new(fs) })
	}
}

/// Path of the immutable, content-addressed blob body (binary `bincode`).
fn blob_path(blob_ref: &BlobRef) -> ObjPath { ObjPath::from(format!("blobs/{}.blob", blob_ref.0)) }

/// Path of the small mutable resolution sidecar holding the `GlobalSymbolId`.
/// Kept separate so the blob body itself is write-once.
fn resolution_path(blob_ref: &BlobRef) -> ObjPath {
	ObjPath::from(format!("blobs/{}.res", blob_ref.0))
}

impl ObjectStoreBlobStore {
	/// Write (or overwrite) the tiny resolution sidecar for a blob.
	async fn write_resolution(&self, blob_ref: &BlobRef, global_id: GlobalSymbolId) -> Result<()> {
		let bytes = bincode::serialize(&global_id)
			.map_err(|e| BlobStoreError::Serialize(Box::new(e)))?;
		self
			.store
			.put(&resolution_path(blob_ref), bytes.into())
			.await
			.map_err(|e| BlobStoreError::Put(Box::new(e)))?;
		Ok(())
	}

	/// Read the resolution sidecar, returning `None` when none has been written.
	async fn read_resolution(&self, blob_ref: &BlobRef) -> Result<Option<GlobalSymbolId>> {
		match self.store.get(&resolution_path(blob_ref)).await {
			Ok(result) => {
				let bytes = result.bytes().await.map_err(|e| BlobStoreError::Get(Box::new(e)))?;
				let global_id = bincode::deserialize(&bytes)
					.map_err(|e| BlobStoreError::Deserialize(Box::new(e)))?;
				Ok(Some(global_id))
			}
			Err(object_store::Error::NotFound { .. }) => Ok(None),
			Err(e) => Err(BlobStoreError::Get(Box::new(e)).into()),
		}
	}
}

#[async_trait]
impl BlobStore for ObjectStoreBlobStore {
	#[tracing::instrument(skip(self, info), fields(occurrence_id = %info.occurrence_id))]
	async fn put(&self, info: &BlobInfo) -> Result<BlobRef> {
		if info.metadata.blob_schema_version != BLOB_SCHEMA_VERSION {
			return Err(BlobStoreError::SchemaVersionMismatch {
				expected: BLOB_SCHEMA_VERSION,
				got:      info.metadata.blob_schema_version,
			}
			.into());
		}

		// The blob body is the *immutable* content. Resolution is mutable and lives
		// in the sidecar, so it is excluded from both the stored bytes and the
		// content hash — re-resolving a blob never rewrites or re-keys it.
		let mut canonical = info.clone();
		let resolution = canonical.resolved_global_id.take();

		let bytes = bincode::serialize(&canonical)
			.map_err(|e| BlobStoreError::Serialize(Box::new(e)))?;

		// Content address: identical content yields the same ref, so puts are
		// idempotent and the ref doubles as an integrity check.
		let blob_ref = BlobRef(blake3::hash(&bytes).to_hex().to_string());

		self
			.store
			.put(&blob_path(&blob_ref), bytes.into())
			.await
			.map_err(|e| BlobStoreError::Put(Box::new(e)))?;

		if let Some(global_id) = resolution {
			self.write_resolution(&blob_ref, global_id).await?;
		}

		Ok(blob_ref)
	}

	#[tracing::instrument(skip(self), fields(blob_ref = %blob_ref))]
	async fn get(&self, blob_ref: &BlobRef) -> Result<BlobInfo> {
		let result = match self.store.get(&blob_path(blob_ref)).await {
			Ok(result) => result,
			Err(object_store::Error::NotFound { .. }) => return Err(BlobStoreError::NotFound.into()),
			Err(e) => return Err(BlobStoreError::Get(Box::new(e)).into()),
		};
		let bytes = result.bytes().await.map_err(|e| BlobStoreError::Get(Box::new(e)))?;
		let mut info: BlobInfo =
			bincode::deserialize(&bytes).map_err(|e| BlobStoreError::Deserialize(Box::new(e)))?;
		if info.metadata.blob_schema_version != BLOB_SCHEMA_VERSION {
			return Err(BlobStoreError::SchemaVersionMismatch {
				expected: BLOB_SCHEMA_VERSION,
				got:      info.metadata.blob_schema_version,
			}
			.into());
		}

		// Overlay the mutable resolution sidecar, if one has been written.
		if let Some(global_id) = self.read_resolution(blob_ref).await? {
			info.resolved_global_id = Some(global_id);
		}

		Ok(info)
	}

	#[tracing::instrument(skip(self), fields(blob_ref = %blob_ref, global_id = %global_id))]
	async fn update_resolution(&self, blob_ref: &BlobRef, global_id: GlobalSymbolId) -> Result<()> {
		// Sidecar write only — no read-modify-write of the (large) blob body.
		self.write_resolution(blob_ref, global_id).await
	}

	async fn list(&self) -> Result<Vec<BlobRef>> {
		let prefix = ObjPath::from("blobs");
		let result = self
			.store
			.list_with_delimiter(Some(&prefix))
			.await
			.map_err(|e| BlobStoreError::List(Box::new(e)))?;
		let refs = result
			.objects
			.into_iter()
			.filter_map(|meta| {
				meta
					.location
					.filename()
					.and_then(|name| name.strip_suffix(".blob"))
					.map(|hash| BlobRef(hash.to_string()))
			})
			.collect();
		Ok(refs)
	}
}

#[cfg(test)]
mod object_store_tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, Language, OccurrenceId, RepoId, SourceChunk, SymbolOrigin};
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
				treesitter_repr: None,
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

	#[tokio::test]
	async fn put_is_content_addressed_and_idempotent() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let info = make_blob("dup", BLOB_SCHEMA_VERSION);

		let r1 = store.put(&info).await.unwrap();
		let r2 = store.put(&info).await.unwrap();

		assert_eq!(r1, r2, "identical content must yield the same blob ref");
		assert_eq!(r1.0.len(), 64, "ref is a blake3 hex digest");
		assert!(r1.0.bytes().all(|b| b.is_ascii_hexdigit()));
		assert_eq!(store.list().await.unwrap().len(), 1, "idempotent put must not duplicate");
	}

	#[tokio::test]
	async fn resolution_is_excluded_from_the_content_address() {
		// Same content with vs. without a precomputed resolution must hash the
		// same, so re-resolving a blob never re-keys it.
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let mut info = make_blob("res", BLOB_SCHEMA_VERSION);

		let unresolved = store.put(&info).await.unwrap();
		info.resolved_global_id = Some(GlobalSymbolId(uuid::Uuid::new_v4()));
		let resolved = store.put(&info).await.unwrap();

		assert_eq!(unresolved, resolved);
	}

	#[tokio::test]
	async fn update_resolution_leaves_the_blob_body_write_once() {
		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let blob_ref = store.put(&make_blob("body", BLOB_SCHEMA_VERSION)).await.unwrap();

		let body_before =
			store.store.get(&blob_path(&blob_ref)).await.unwrap().bytes().await.unwrap();
		assert!(
			store.store.get(&resolution_path(&blob_ref)).await.is_err(),
			"no sidecar before resolution"
		);

		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		store.update_resolution(&blob_ref, gid).await.unwrap();

		let body_after =
			store.store.get(&blob_path(&blob_ref)).await.unwrap().bytes().await.unwrap();
		assert_eq!(body_before, body_after, "the blob body must not be rewritten");
		let got = store.get(&blob_ref).await.unwrap();
		assert_eq!(got.resolved_global_id.map(|g| g.0), Some(gid.0));
	}

	#[tokio::test]
	async fn round_trips_a_blob_with_embeddings() {
		use nudox_core::{EmbeddingPurpose, EmbeddingRecord, ModelType};

		let tmp = TempDir::new().unwrap();
		let store = ObjectStoreBlobStore::local(tmp.path().to_path_buf()).unwrap();
		let mut info = make_blob("vecsym", BLOB_SCHEMA_VERSION);
		info.embeddings = vec![EmbeddingRecord {
			model_type: ModelType::Openai,
			model:      "text-embedding-3-small".to_owned(),
			purpose:    EmbeddingPurpose::Code,
			vector:     vec![0.1, -0.2, 0.333, 4.0],
		}];

		let blob_ref = store.put(&info).await.unwrap();
		let got = store.get(&blob_ref).await.unwrap();

		assert_eq!(got.embeddings.len(), 1);
		assert_eq!(got.embeddings[0].vector, vec![0.1, -0.2, 0.333, 4.0]);
		assert_eq!(got.embeddings[0].model, "text-embedding-3-small");
	}
}
