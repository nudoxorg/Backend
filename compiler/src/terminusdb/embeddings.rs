//! # Embeddings with Qdrant
//! this file describes the interface for using vector embeddings with qdrant.
//! We need to have an UUID, a payload, and the actual embeddings
//! the Payload will contain the uri of the document in the store and metadata
//! (for now just embedding model)
use qdrant_client::Payload;
use serde_json::Value;

// TODO implement into Payload type from the rust type
pub struct QdrantPayload {
	uri:             String,
	embedding_model: String,
}

// impl Into<> for QdrantPayload {
//
//}

// ALSO into PointStruct::new(point_id, embedding, payload)
pub struct QdrantPoint {
	payload: QdrantPayload,
}
