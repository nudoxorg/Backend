use qdrant_client::{Qdrant, qdrant::{CreateCollectionBuilder, Distance, PointStruct, UpsertPointsBuilder, VectorParamsBuilder}};
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::{config::VectorDistance, error::{AppError, QdrantError}};

/// Configuration for connecting to a Qdrant instance.
///
/// For local development, `endpoint` will usually be:
/// - http://localhost:6334
///
/// The Rust client uses the gRPC endpoint for its main operations.
#[derive(Clone, Debug)]
pub struct QdrantConfig {
	pub endpoint:        Url,
	pub collection_name: String,
	pub vector_size:     u64,
	pub distance:        VectorDistance,
}

/// Ensure the target collection exists before upload.
///
/// If the collection does not exist, create it with the configured vector size
/// and distance metric. If it already exists, leave it as-is.
#[instrument(skip_all, fields(collection = %config.collection_name))]
pub async fn ensure_collection(config: &QdrantConfig) -> Result<(), AppError> {
	let client = Qdrant::from_url(config.endpoint.as_str())
		.build()
		.map_err(|source| AppError::Qdrant(QdrantError::ConnectionFailed {
			endpoint: config.endpoint.to_string(),
			source,
		}))?;

	let exists = client
		.collection_exists(&config.collection_name)
		.await
		.map_err(|source| AppError::Qdrant(QdrantError::CollectionExistenceCheck {
			collection: config.collection_name.clone(),
			source,
		}))?;

	if exists {
		debug!(collection = %config.collection_name, "qdrant collection already exists");
		return Ok(());
	}

	info!(
		collection = %config.collection_name,
		vector_size = config.vector_size,
		distance = ?config.distance,
		"creating qdrant collection"
	);

	let qdrant_distance = match config.distance {
		VectorDistance::Cosine => Distance::Cosine,
		VectorDistance::Dot => Distance::Dot,
		VectorDistance::Euclid => Distance::Euclid,
		VectorDistance::Manhattan => Distance::Manhattan,
	};
	client
		.create_collection(
			CreateCollectionBuilder::new(&config.collection_name)
				.vectors_config(VectorParamsBuilder::new(config.vector_size, qdrant_distance)),
		)
		.await
		.map_err(|source| AppError::Qdrant(QdrantError::CollectionCreation {
			collection: config.collection_name.clone(),
			source,
		}))?;

	info!(collection = %config.collection_name, "qdrant collection created");
	Ok(())
}

/// Upload points to a Qdrant collection using upsert semantics.
///
/// The collection is created first if it does not already exist.
#[instrument(skip_all, fields(collection = %config.collection_name))]
pub async fn upload_points(config: &QdrantConfig, points: Vec<PointStruct>) -> Result<(), AppError> {
	if points.is_empty() {
		warn!("no qdrant points to upload");
		return Ok(());
	}

	ensure_collection(config).await?;

	let client = Qdrant::from_url(config.endpoint.as_str())
		.build()
		.map_err(|source| AppError::Qdrant(QdrantError::ConnectionFailed {
			endpoint: config.endpoint.to_string(),
			source,
		}))?;

	info!(
		collection = %config.collection_name,
		count = points.len(),
		"uploading qdrant points"
	);

	let response = client
		.upsert_points(UpsertPointsBuilder::new(&config.collection_name, points))
		.await
		.map_err(|source| AppError::Qdrant(QdrantError::UpsertFailed {
			collection: config.collection_name.clone(),
			source,
		}))?;

	debug!(status = ?response.result, "qdrant upsert completed");

	Ok(())
}
