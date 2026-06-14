use thiserror::Error;

/// The unified error type for all nudox-occurrences operations.
#[derive(Debug, Error)]
pub enum Error {
	/// An error originating from the blob store backend.
	#[error("blob store error: {0}")]
	BlobStore(String),
	/// An error originating from the global symbol store.
	#[error("global symbol store error: {0}")]
	GlobalStore(String),
	/// An error produced by an embedder implementation.
	#[error("embedder error: {0}")]
	Embedder(String),
	/// An error produced by the full-text search index.
	#[error("search index error: {0}")]
	Search(String),
	/// An error produced by the vector index.
	#[error("vector index error: {0}")]
	Vector(String),
	/// An error produced by a queue implementation.
	#[error("queue error: {0}")]
	Queue(String),
	/// An error produced during pipeline execution.
	#[error("pipeline error: {0}")]
	Pipeline(String),
	/// Any other error not covered by the variants above.
	#[error(transparent)]
	Other(#[from] anyhow::Error),
}

/// Convenience `Result` alias for [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;
