//! # Embeddings with Qdrant
//! this file describes the interface for using vector embeddings with qdrant.
//! We need to have an UUID, a payload, and the actual embeddings
//! the Payload will contain the uri of the document in the store and metadata
//! (for now just embedding model)
use std::collections::HashMap;

use qdrant_client::{Payload, qdrant::{PointStruct, Value}};

/// Info Like uri and embedding model for the Qdrant to store on the point
pub struct QdrantPayload {
	uri:             String,
	embedding_model: String,
}

impl Into<Payload> for QdrantPayload {
	fn into(self) -> Payload {
		let payload: Payload =
			vec![("uri", self.uri.into()), ("embedding_model", self.embedding_model.into())]
				.into_iter()
				.collect::<HashMap<&str, Value>>()
				.into();
		payload
	}
}

// ALSO into PointStruct::new(point_id, embedding, payload)
pub struct QdrantPoint {
	embeddings: Option<Vec<f32>>,
	payload:    QdrantPayload,
}

impl QdrantPoint {
	pub fn into_qdrant_point(self) -> Result<PointStruct, String> {
		if let Some(e) = self.embeddings {
			Ok(PointStruct::new(1_u64, e, self.payload))
		} else {
			Err(String::from("No Embeddings Generated"))
		}
	}
}
