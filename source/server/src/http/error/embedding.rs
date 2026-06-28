use thiserror::Error;
use tokio::task::JoinError;

#[derive(Debug, Error)]
pub enum EmbeddingError {
	#[error("missing text for embedding")]
	MissingText,

	#[error("missing generated vector")]
	MissingVector,

	#[error("missing required field: {field}")]
	MissingField { field: &'static str },

	#[error("invalid field type: {field}")]
	InvalidFieldType { field: &'static str },

	#[error("embedding task join failed")]
	TaskJoin {
		#[source]
		source: JoinError,
	},

	#[error("embedding provider request failed")]
	Provider {
		#[source]
		source: nudox_core::Error,
	},

	#[error("embedding provider returned an empty response")]
	EmptyResponse,
}
