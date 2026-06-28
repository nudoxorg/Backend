use thiserror::Error;

#[derive(Debug, Error)]
pub enum QdrantError {
	#[error("failed to connect to Qdrant at `{endpoint}`")]
	ConnectionFailed {
		endpoint: String,
		#[source]
		source:   qdrant_client::QdrantError,
	},

	#[error("query failed on collection `{collection}`")]
	QueryFailed {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("failed to list collections with prefix `{prefix}`")]
	ListCollectionsFailed {
		prefix: String,
		#[source]
		source: qdrant_client::QdrantError,
	},

	#[error("upsert points to collection `{collection}` failed")]
	UpsertFailed {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("collection `{collection}` existence check failed")]
	CollectionExistenceCheck {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("collection `{collection}` creation failed")]
	CollectionCreation {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("deterministic point-id generation failed: {details}")]
	PointIdGeneration { details: String },
}
