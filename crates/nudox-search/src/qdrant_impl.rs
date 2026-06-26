//! Real Qdrant-backed vector index.
//!
//! Each [`EmbeddingRecord`] is upserted as one Qdrant point. The payload
//! carries enough metadata (model_type, model name, purpose, blob_ref,
//! global_id) to reconstruct provenance without round-tripping to the blob
//! store.
//!
//! Schema (v1 payload):
//!
//! | key            | type   |
//! |----------------|--------|
//! | `blob_ref`     | string |
//! | `global_id`    | string |
//! | `model_type`   | string |
//! | `model_name`   | string |
//! | `purpose`      | string |
//!
//! Point IDs are UUIDs derived deterministically from `(blob_ref, model_name,
//! purpose)` so re-upserting the same record overwrites in place.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use nudox_core::{BlobRef, EmbeddingPurpose, EmbeddingRecord, GlobalSymbolId, ModelType, Result, VectorError, VectorHit, VectorIndex, VectorQuery};
use qdrant_client::{Qdrant, qdrant::{CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder, UpsertPointsBuilder, Value, VectorParamsBuilder, vectors_config::Config}};

/// Qdrant-backed vector index. Connects via gRPC (default port 6334).
pub struct QdrantVectorIndex {
	client:     Arc<Qdrant>,
	collection: String,
}


fn model_type_label(mt: &ModelType) -> String {
	match mt {
		ModelType::Placeholder => "placeholder".into(),
		ModelType::Mock => "mock".into(),
		ModelType::InProcess => "in-process".into(),
		ModelType::Openai => "openai".into(),
		ModelType::SelfHosted => "self-hosted".into(),
		ModelType::Other(s) => format!("other:{s}"),
	}
}

fn purpose_label(p: &EmbeddingPurpose) -> String {
	match p {
		EmbeddingPurpose::Code => "code".into(),
		EmbeddingPurpose::Docstring => "docstring".into(),
		EmbeddingPurpose::Comment => "comment".into(),
		EmbeddingPurpose::Other(s) => format!("other:{s}"),
	}
}

/// Deterministic point ID built from `(blob_ref, model, purpose)` so repeated
/// upserts overwrite in place rather than accumulating duplicates.
fn point_id(blob_ref: &BlobRef, model: &str, purpose: &EmbeddingPurpose) -> String {
	let key = format!("{}|{}|{}", blob_ref.0, model, purpose_label(purpose));
	uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key.as_bytes()).to_string()
}

impl QdrantVectorIndex {
	/// Connect to a Qdrant instance at the given URL (e.g. `http://localhost:6334`)
	/// and bind to the given collection name. Does not create the collection —
	/// call [`Self::ensure_collection`] for that.
	pub async fn connect(url: impl Into<String>, collection: String) -> Result<Self> {
		let url = url.into();
		let client = Qdrant::from_url(&url).build().map_err(|e| VectorError::Connect(Box::new(e)))?;
		Ok(Self { client: Arc::new(client), collection })
	}

	/// Create the collection if it does not exist. Uses cosine distance and
	/// the given vector dimension.
	pub async fn ensure_collection(&self, vector_dim: u64) -> Result<()> {
		let exists = self.client.collection_exists(&self.collection).await.map_err(|e| VectorError::CollectionExists(Box::new(e)))?;
		if exists {
			return Ok(());
		}
		let req = CreateCollectionBuilder::new(&self.collection).vectors_config(Config::Params(
			VectorParamsBuilder::new(vector_dim, Distance::Cosine).build(),
		));
		self.client.create_collection(req).await.map_err(|e| VectorError::CreateCollection(Box::new(e)))?;
		Ok(())
	}

	/// Delete the bound collection. Useful for test teardown.
	pub async fn delete_collection(&self) -> Result<()> {
		self.client.delete_collection(&self.collection).await.map_err(|e| VectorError::DeleteCollection(Box::new(e)))?;
		Ok(())
	}

	/// Count points in the bound collection. Mostly useful for tests.
	pub async fn count_points(&self) -> Result<u64> {
		let resp = self
			.client
			.count(qdrant_client::qdrant::CountPointsBuilder::new(&self.collection).exact(true))
			.await
			.map_err(|e| VectorError::Count(Box::new(e)))?;
		Ok(resp.result.map(|r| r.count).unwrap_or(0))
	}
}

#[async_trait]
impl VectorIndex for QdrantVectorIndex {
	#[tracing::instrument(skip(self, embeddings), fields(blob_ref = %blob_ref.0, global_id = %global_id.0, count = embeddings.len()))]
	async fn upsert(
		&self,
		blob_ref: &BlobRef,
		global_id: GlobalSymbolId,
		embeddings: &[EmbeddingRecord],
	) -> Result<()> {
		if embeddings.is_empty() {
			return Ok(());
		}
		let mut points = Vec::with_capacity(embeddings.len());
		for rec in embeddings {
			let mut payload: HashMap<String, Value> = HashMap::new();
			payload.insert("blob_ref".into(), Value::from(blob_ref.0.clone()));
			payload.insert("global_id".into(), Value::from(global_id.0.to_string()));
			payload.insert("model_type".into(), Value::from(model_type_label(&rec.model_type)));
			payload.insert("model_name".into(), Value::from(rec.model.clone()));
			payload.insert("purpose".into(), Value::from(purpose_label(&rec.purpose)));

			let pid = point_id(blob_ref, &rec.model, &rec.purpose);
			points.push(PointStruct::new(pid, rec.vector.clone(), payload));
		}
		self
			.client
			.upsert_points(UpsertPointsBuilder::new(&self.collection, points).wait(true))
			.await
			.map_err(|e| VectorError::Upsert(Box::new(e)))?;
		Ok(())
	}
}

#[async_trait]
impl VectorQuery for QdrantVectorIndex {
	#[tracing::instrument(skip(self, vector), fields(collection = %self.collection, limit))]
	async fn search(&self, vector: &[f32], limit: usize) -> Result<Vec<VectorHit>> {
		let req =
			SearchPointsBuilder::new(&self.collection, vector.to_vec(), limit as u64).with_payload(true);

		let resp = self.client.search_points(req).await.map_err(|e| VectorError::Search(Box::new(e)))?;

		let hits = resp
			.result
			.into_iter()
			.map(|point| {
				let blob_ref = BlobRef(
					point.payload.get("blob_ref").and_then(|v| v.as_str()).cloned().unwrap_or_default(),
				);
				let gid_str =
					point.payload.get("global_id").and_then(|v| v.as_str()).cloned().unwrap_or_default();
				let global_id =
					GlobalSymbolId(uuid::Uuid::parse_str(&gid_str).unwrap_or_else(|_| uuid::Uuid::nil()));
				VectorHit { blob_ref, global_id, score: point.score }
			})
			.collect();

		Ok(hits)
	}
}

#[cfg(all(test, feature = "qdrant-integration"))]
mod tests {
	use std::time::{SystemTime, UNIX_EPOCH};

	use nudox_core::{EmbeddingPurpose, EmbeddingRecord, ModelType};

	use super::*;

	const QDRANT_URL: &str = "http://localhost:6334";

	fn unique_collection(name: &str) -> String {
		let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		format!("nudox_test_{name}_{ts}")
	}

	async fn fresh_index(collection: &str, dim: u64) -> QdrantVectorIndex {
		let ix = QdrantVectorIndex::connect(QDRANT_URL, collection.into())
			.await
			.expect("qdrant connect (is the container running on 6334?)");
		ix.ensure_collection(dim).await.expect("ensure_collection");
		ix
	}

	fn record(model_type: ModelType, model: &str, dim: usize, val: f32) -> EmbeddingRecord {
		EmbeddingRecord {
			model_type,
			model: model.into(),
			purpose: EmbeddingPurpose::Code,
			vector: vec![val; dim],
		}
	}

	#[tokio::test]
	async fn ensure_collection_is_idempotent() {
		let c = unique_collection("ensure");
		let ix = fresh_index(&c, 8).await;
		ix.ensure_collection(8).await.unwrap();
		ix.delete_collection().await.unwrap();
	}

	#[tokio::test]
	async fn upsert_inserts_one_point_per_record() {
		let c = unique_collection("upsert");
		let ix = fresh_index(&c, 8).await;
		let blob_ref = BlobRef(uuid::Uuid::new_v4().to_string());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		let recs = vec![
			record(ModelType::Mock, "mock", 8, 0.1),
			record(ModelType::Placeholder, "placeholder-v1", 8, 0.5),
		];
		ix.upsert(&blob_ref, gid, &recs).await.unwrap();
		let n = ix.count_points().await.unwrap();
		assert_eq!(n, 2);
		ix.delete_collection().await.unwrap();
	}

	#[tokio::test]
	async fn upsert_is_idempotent_per_model_purpose() {
		let c = unique_collection("idempotent");
		let ix = fresh_index(&c, 8).await;
		let blob_ref = BlobRef(uuid::Uuid::new_v4().to_string());
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		let recs = vec![record(ModelType::Mock, "mock", 8, 0.1)];
		ix.upsert(&blob_ref, gid, &recs).await.unwrap();
		ix.upsert(&blob_ref, gid, &recs).await.unwrap();
		let n = ix.count_points().await.unwrap();
		assert_eq!(n, 1, "re-upsert should overwrite, not duplicate");
		ix.delete_collection().await.unwrap();
	}

	#[tokio::test]
	async fn upsert_two_blobs_keeps_separate_points() {
		let c = unique_collection("twoblobs");
		let ix = fresh_index(&c, 8).await;
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		let rec = vec![record(ModelType::Mock, "mock", 8, 0.1)];
		ix.upsert(&BlobRef("blob-a".into()), gid, &rec).await.unwrap();
		ix.upsert(&BlobRef("blob-b".into()), gid, &rec).await.unwrap();
		assert_eq!(ix.count_points().await.unwrap(), 2);
		ix.delete_collection().await.unwrap();
	}

	#[tokio::test]
	async fn upsert_with_empty_embeddings_is_noop() {
		let c = unique_collection("empty");
		let ix = fresh_index(&c, 8).await;
		ix.upsert(&BlobRef("nope".into()), GlobalSymbolId(uuid::Uuid::new_v4()), &[]).await.unwrap();
		assert_eq!(ix.count_points().await.unwrap(), 0);
		ix.delete_collection().await.unwrap();
	}
}
