//! One `thiserror` enum per runtime area, each classifying itself as
//! [`heart::Retryable`] so the retry/queue machinery is written exactly once, plus
//! the crate-level [`RuntimeError`] that aggregates them for callers that want a
//! single `#[from]` (the server).
//!
//! ## Conventions
//! - No field-less unit errors. A failure that is really "could not connect"
//!   reuses [`heart::ConnectError`] (which already carries [`heart::BackendKind`]
//!   + [`heart::ConnectFailure`], including `DimensionMismatch`).
//! - Underlying causes are chained via `#[source]` as their **concrete** backend
//!   types — no `Box<dyn Error>` — so the full error chain is preserved and typed.
//! - Each area's error implements [`heart::Retryable`] so transient backend
//!   faults (timeouts, 5xx, rate limits) retry uniformly while validation/auth
//!   failures do not.

use std::time::Duration;

use heart::{ConnectError, Retryable};

use crate::vector::CollectionNameError;

/// Classified reasons why a graph (Terminus/WOQL) query was rejected or failed
/// to execute. Carries structured data (never a bare string) so callers can
/// match explicitly and no diagnostic information is lost to formatting.
#[derive(Debug, thiserror::Error)]
pub enum GraphQueryError {
	/// Non-success HTTP status returned by the graph endpoint.
	#[error("graph endpoint returned HTTP {status}")]
	HttpStatus {
		status: reqwest::StatusCode,
		/// Truncated response body (to avoid logging giant error pages).
		body: String,
	},

	/// The submitted WOQL could not be parsed by the store.
	#[error("WOQL parse failed (query preview: {query_preview:?}): {details}")]
	ParseFailed {
		/// Prefix of the query for context (original not re-serialized to save space).
		query_preview: String,
		details: String,
	},

	/// Query used a feature the current runtime graph bindings do not support.
	#[error("unsupported WOQL feature: {feature}")]
	UnsupportedFeature { feature: &'static str },

	/// A symbol, package, or document referenced by the query does not exist.
	#[error("graph entity not found during query execution")]
	NotFound,

	/// A schema constraint, cardinality, or type check failed.
	#[error("graph schema/constraint violation: {reason}")]
	ConstraintViolation { reason: String },

	/// The query was rejected for authorization or credential reasons.
	#[error("graph query unauthorized: {reason}")]
	Unauthorized { reason: String },

	/// Store-side timeout while evaluating the query.
	#[error("graph query execution timed out")]
	Timeout,

	/// The query referenced an unknown or unsupported variable binding.
	#[error("unknown query variable binding: {name}")]
	UnknownBinding { name: String },

	/// Result cardinality or projection mismatch.
	#[error("graph query result shape unexpected: {details}")]
	ResultShapeMismatch { details: String },

	/// Catch-all for other graph rejections carrying the essential data.
	#[error("other graph query error: {details}")]
	Other { details: String },
}

/// Classified reasons a text (tantivy) query was rejected.
#[derive(Debug, thiserror::Error)]
pub enum TextQueryError {
	/// The query string was empty (or whitespace-only) after trimming.
	#[error("empty query terms")]
	Empty,

	/// Query string exceeded the accepted maximum length.
	#[error("text query too long ({len} > {max})")]
	TooLong { len: usize, max: usize },

	/// Query contained control characters which are disallowed.
	#[error("text query contains control characters")]
	ContainsControl,

	/// The terms could not be turned into a valid tantivy regex/term query.
	#[error("terms {needle:?} do not form a searchable pattern")]
	InvalidPattern {
		needle: String,
		#[source]
		cause: tantivy::TantivyError,
	},

	/// An ecosystem filter token was unknown.
	#[error("unknown ecosystem filter token in text query: {token}")]
	InvalidEcosystem { token: String },

	/// A symbol kind filter contained an unknown kind.
	#[error("unknown symbol kind filter in text query: {kind:?}")]
	InvalidKindFilter { kind: String },

	/// The query used an operator or syntax tantivy rejected.
	#[error("text query syntax error: {details}")]
	SyntaxError { details: String },

	/// Other text query malformation carrying the input data.
	#[error("malformed text query terms {terms:?}: {details}")]
	Malformed { terms: String, details: String },
}

/// Classified reasons an embedding request was rejected by the model backend.
#[derive(Debug, thiserror::Error)]
pub enum EmbedRejectionReason {
	/// Request JSON serialization failed (should be infallible for our shapes).
	#[error("failed to serialize embedding request")]
	Serialization(#[source] serde_json::Error),

	/// Non-success status (other than 429 which is RateLimited at EmbedError level).
	#[error("embedding provider returned {status}")]
	HttpStatus { status: reqwest::StatusCode, body: String },

	/// Response JSON could not be deserialized.
	#[error("malformed embedding response")]
	MalformedResponse(#[source] serde_json::Error),

	/// Returned vector count did not match number of input texts.
	#[error("embedding batch size mismatch: expected {expected}, received {received}")]
	BatchSizeMismatch { expected: usize, received: usize },

	/// The response data array was empty when at least one embedding was expected.
	#[error("empty embedding response batch")]
	EmptyResponse,

	/// The input text(s) were rejected as invalid by the embedding model.
	#[error("embedding input rejected: {reason}")]
	InputRejected { reason: String },

	/// Provider reported a dimension problem.
	#[error("embedding dimension problem reported by provider")]
	DimensionProblem,

	/// Authorization or API key problem specific to embedding.
	#[error("embedding authorization failed: {details}")]
	AuthFailed { details: String },

	/// Provider returned an internal server error body.
	#[error("embedding provider internal error: {details}")]
	ProviderError { details: String },

	/// Quota or other rate related but not the standard 429 path.
	#[error("embedding quota exceeded: {details}")]
	QuotaExceeded { details: String },

	/// The model id requested is not supported by the backend.
	#[error("unsupported embedding model: {model}")]
	UnsupportedModel { model: String },

	/// Other provider rejection preserving the key diagnostic data.
	#[error("other embedding rejection: {details}")]
	Other { details: String },
}

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
	#[error("graph query rejected: {0}")]
	Query(#[source] GraphQueryError),

	/// The graph responded but the JSON payload could not be decoded into the
	/// expected shape.
	#[error("malformed graph response")]
	Decode(#[source] serde_json::Error),

	/// A requested symbol/id was absent from the graph.
	#[error("symbol not present in graph")]
	NotFound,
}

impl Retryable for GraphError {
	fn is_retryable(&self) -> bool {
		match self {
			GraphError::Connect(e) => e.is_retryable(),
			GraphError::Transport(e) => e.is_timeout() || e.is_connect(),
			GraphError::Query(_) | GraphError::Decode(_) | GraphError::NotFound => false,
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
	Transport(#[source] qdrant_client::QdrantError),

	/// Embedding the query text failed on the gated path.
	#[error("query embedding failed")]
	Embed(#[source] EmbedError),

	/// A returned point's payload could not be decoded into a symbol identity.
	#[error("malformed point payload")]
	Payload(#[source] serde_json::Error),
}

impl Retryable for VectorError {
	fn is_retryable(&self) -> bool {
		match self {
			VectorError::Connect(e) => e.is_retryable(),
			VectorError::Embed(e) => e.is_retryable(),
			VectorError::Transport(_) => true,
			VectorError::Collection(_) | VectorError::Payload(_) => false,
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
	Engine(#[source] tantivy::TantivyError),

	/// The postgres poll that feeds the index failed.
	#[error("text index poll (postgres) failed")]
	Poll(#[source] sqlx::Error),

	/// A query could not be parsed into a tantivy query.
	#[error("malformed text query: {0}")]
	Query(#[source] TextQueryError),

	/// The pagination cursor could not be decoded / no longer matches the index.
	#[error("invalid text search cursor")]
	Cursor(#[source] heart::cursor::CursorError),
}

impl Retryable for TextError {
	fn is_retryable(&self) -> bool {
		match self {
			// A local-disk index blip or a transient postgres fault is worth a retry.
			TextError::Io(_) | TextError::Poll(_) => true,
			TextError::Engine(_) | TextError::Query(_) | TextError::Cursor(_) => false,
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
	Codec(#[source] serde_json::Error),

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
	Transport(#[source] reqwest::Error),

	/// The model rejected the request (bad input, unsupported purpose).
	#[error("embedding request rejected: {0}")]
	Rejected(#[source] EmbedRejectionReason),

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

/// The crate-level error: every runtime failure, aggregated so a caller (the
/// server) can `#[from]` one type instead of five. Direct `#[from]` of each area
/// enum — no boxing.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
	/// A graph-store failure.
	#[error(transparent)]
	Graph(#[from] GraphError),

	/// A vector-store failure.
	#[error(transparent)]
	Vector(#[from] VectorError),

	/// A text-index failure.
	#[error(transparent)]
	Text(#[from] TextError),

	/// A session-store failure.
	#[error(transparent)]
	Session(#[from] SessionError),

	/// An embedding failure.
	#[error(transparent)]
	Embed(#[from] EmbedError),
}

// Proper From wrappers so sub-errors can be directly converted / used with ?
// without losing their typed structure. These feed into the area error and
// then the top-level RuntimeError.
impl From<GraphQueryError> for GraphError {
	fn from(e: GraphQueryError) -> Self {
		GraphError::Query(e)
	}
}

impl From<GraphQueryError> for RuntimeError {
	fn from(e: GraphQueryError) -> Self {
		RuntimeError::Graph(GraphError::from(e))
	}
}

impl From<TextQueryError> for TextError {
	fn from(e: TextQueryError) -> Self {
		TextError::Query(e)
	}
}

impl From<TextQueryError> for RuntimeError {
	fn from(e: TextQueryError) -> Self {
		RuntimeError::Text(TextError::from(e))
	}
}

impl From<EmbedRejectionReason> for EmbedError {
	fn from(e: EmbedRejectionReason) -> Self {
		EmbedError::Rejected(e)
	}
}

impl From<EmbedRejectionReason> for RuntimeError {
	fn from(e: EmbedRejectionReason) -> Self {
		RuntimeError::Embed(EmbedError::from(e))
	}
}

impl Retryable for RuntimeError {
	fn is_retryable(&self) -> bool {
		match self {
			RuntimeError::Graph(e) => e.is_retryable(),
			RuntimeError::Vector(e) => e.is_retryable(),
			RuntimeError::Text(e) => e.is_retryable(),
			RuntimeError::Session(e) => e.is_retryable(),
			RuntimeError::Embed(e) => e.is_retryable(),
		}
	}

	fn retry_after(&self) -> Option<Duration> {
		match self {
			RuntimeError::Graph(e) => e.retry_after(),
			RuntimeError::Vector(e) => e.retry_after(),
			RuntimeError::Text(e) => e.retry_after(),
			RuntimeError::Session(e) => e.retry_after(),
			RuntimeError::Embed(e) => e.retry_after(),
		}
	}
}
