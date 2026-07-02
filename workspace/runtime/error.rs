//! One `thiserror` enum per runtime area, each classifying itself as
//! [`heart::Retryable`] so the retry/queue machinery is written exactly once.
//!
//! ## Conventions
//! - No field-less unit errors. A failure that is really "could not connect"
//!   reuses [`heart::ConnectError`] (which already carries [`heart::BackendKind`]
//!   + [`heart::ConnectFailure`], including `DimensionMismatch`).
//! - Underlying causes are chained via `#[source]` so the full error chain is
//!   preserved for operators.
//! - Each area's error implements [`heart::Retryable`] so transient backend
//!   faults (timeouts, 5xx, rate limits) retry uniformly while validation/auth
//!   failures do not.

use std::time::Duration;

use heart::{ConnectError, Retryable};

use crate::vector::CollectionNameError;

/// Failures from the graph store (terminus).
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
	/// The store was not reachable / not yet connected.
	#[error("graph store connection failed")]
	Connect(#[source] ConnectError),

	/// The underlying HTTP transport failed.
	#[error("graph transport error")]
	Transport(#[source] reqwest::Error),

	/// The graph returned a WOQL/document error (bad query, constraint, etc.).
	#[error("graph query rejected: {message}")]
	Query {
		/// A human-readable rendering of the terminus error body.
		message: String,
	},

	/// The graph responded but the payload could not be decoded into the
	/// expected shape.
	#[error("malformed graph response")]
	Decode(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// A requested symbol/id was absent from the graph.
	#[error("symbol not present in graph")]
	NotFound,

	/// The record's generation stamp disagreed with the queried generation —
	/// cross-store version skew, surfaced rather than silently mixed.
	#[error("graph generation skew: record is stale relative to the query")]
	GenerationSkew,
}

impl Retryable for GraphError {
	fn is_retryable(&self) -> bool {
		match self {
			GraphError::Connect(e) => e.is_retryable(),
			GraphError::Transport(e) => e.is_timeout() || e.is_connect(),
			GraphError::Query { .. }
			| GraphError::Decode(_)
			| GraphError::NotFound
			| GraphError::GenerationSkew => false,
		}
	}

	fn retry_after(&self) -> Option<Duration> {
		match self {
			GraphError::Connect(e) => e.retry_after(),
			_ => None,
		}
	}
}

/// Failures from the vector store (qdrant).
#[derive(Debug, thiserror::Error)]
pub enum VectorError {
	/// The store was not reachable / the collection dimension disagreed with the
	/// compiled-in `DIM` (carried as [`heart::ConnectFailure::DimensionMismatch`]).
	#[error("vector store connection failed")]
	Connect(#[source] ConnectError),

	/// A collection name failed validation.
	#[error("invalid collection name")]
	Collection(#[source] CollectionNameError),

	/// The qdrant client/transport failed.
	#[error("vector transport error")]
	Transport(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Embedding the query text failed on the gated path.
	#[error("query embedding failed")]
	Embed(#[source] EmbedError),

	/// A returned point's payload could not be decoded into a symbol identity.
	#[error("malformed point payload")]
	Payload(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// A page was requested against a newer index generation than its cursor was
	/// anchored to — the keyset position is no longer valid.
	#[error("vector cursor generation skew: index regenerated under the cursor")]
	GenerationSkew,
}

impl Retryable for VectorError {
	fn is_retryable(&self) -> bool {
		match self {
			VectorError::Connect(e) => e.is_retryable(),
			VectorError::Embed(e) => e.is_retryable(),
			VectorError::Transport(_) => true,
			VectorError::Collection(_)
			| VectorError::Payload(_)
			| VectorError::GenerationSkew => false,
		}
	}

	fn retry_after(&self) -> Option<Duration> {
		match self {
			VectorError::Connect(e) => e.retry_after(),
			VectorError::Embed(e) => e.retry_after(),
			_ => None,
		}
	}
}

/// Failures from the text index (tantivy) and its postgres poller.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
	/// Opening / creating the replica-local index directory failed.
	#[error("text index i/o failed")]
	Io(#[source] std::io::Error),

	/// A tantivy operation (open, write, commit, search) failed.
	#[error("text index engine error")]
	Engine(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// The postgres poll that feeds the index failed.
	#[error("text index poll (postgres) failed")]
	Poll(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// A query could not be parsed into a tantivy query.
	#[error("malformed text query: {message}")]
	Query {
		/// Why the query string was rejected.
		message: String,
	},

	/// The pagination cursor could not be decoded / no longer matches the index.
	#[error("invalid text search cursor")]
	Cursor(#[source] heart::cursor::CursorError),
}

impl Retryable for TextError {
	fn is_retryable(&self) -> bool {
		match self {
			// A local-disk index blip or a transient postgres fault is worth a retry.
			TextError::Io(_) | TextError::Poll(_) => true,
			TextError::Engine(_) | TextError::Query { .. } | TextError::Cursor(_) => false,
		}
	}
}

/// Failures from the per-session graph store.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
	/// Reading/writing the persisted session file failed.
	#[error("session persistence i/o failed")]
	Io(#[source] std::io::Error),

	/// A persisted session snapshot could not be (de)serialized.
	#[error("session snapshot codec error")]
	Codec(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// The referenced session id was not open.
	#[error("session not found")]
	NotFound,
}

impl Retryable for SessionError {
	fn is_retryable(&self) -> bool {
		matches!(self, SessionError::Io(_))
	}
}

/// Failures from an [`crate::vector::Embedder`] — a network model can fail,
/// time out, or rate-limit, so embedding is fallible and batched.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
	/// The embedding backend was unreachable / the transport failed.
	#[error("embedding transport error")]
	Transport(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// The model rejected the request (bad input, unsupported purpose).
	#[error("embedding request rejected: {message}")]
	Rejected {
		/// Why the model rejected the request.
		message: String,
	},

	/// The backend asked us to slow down (HTTP 429 / provider quota). Carries
	/// the honored backoff hint.
	#[error("embedding rate-limited")]
	RateLimited {
		/// How long the backend asked us to wait before retrying, if stated.
		retry_after: Option<Duration>,
	},

	/// A returned vector's length did not match the compiled-in `DIM`.
	#[error("embedding dimension mismatch: model returned {found}, expected {expected}")]
	DimensionMismatch {
		/// The `DIM` the caller compiled against.
		expected: usize,
		/// The length actually returned by the model.
		found: usize,
	},

	/// A `from_slice` conversion was handed the wrong number of elements.
	#[error("embedding length mismatch: got {found} values, expected {expected}")]
	LengthMismatch {
		/// The target `DIM`.
		expected: usize,
		/// The slice length supplied.
		found: usize,
	},
}

impl Retryable for EmbedError {
	fn is_retryable(&self) -> bool {
		matches!(self, EmbedError::Transport(_) | EmbedError::RateLimited { .. })
	}

	fn retry_after(&self) -> Option<Duration> {
		match self {
			EmbedError::RateLimited { retry_after } => *retry_after,
			_ => None,
		}
	}
}
