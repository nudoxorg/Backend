//! Shared error scaffolding.
//!
//! Two things every backend in the system needs and should not re-invent:
//! a common bound for store errors ([`StoreError`]), a uniform way to ask "is
//! this failure worth retrying?" ([`Retryable`]), and a single, *context-rich*
//! connection-failure type ([`ConnectError`]) so that auth vs unreachable vs
//! schema-drift vs dimension-mismatch never collapse into one opaque bit.

use thiserror::Error;

/// A shared bound for the error type of any store/backend, so subsystem traits
/// can write `type Error: StoreError` without re-stating the
/// `std::error::Error + Send + Sync + 'static` litany every time.
pub trait StoreError: std::error::Error + Send + Sync + 'static {}
impl<T: std::error::Error + Send + Sync + 'static> StoreError for T {}

/// Uniform retry classification. The queue and [`crate::sink::Sink`] machinery
/// consult this instead of pattern-matching each backend's error enum, so the
/// retry policy is written exactly once.
pub trait Retryable {
	/// Whether retrying the operation that produced this error could plausibly
	/// succeed (transient: timeouts, 5xx, connection resets — not: auth,
	/// validation, not-found).
	fn is_retryable(&self) -> bool;

	/// A hint that the backend is asking us to slow down (e.g. HTTP 429 /
	/// `Retry-After`), so the caller can honour backpressure rather than hammer.
	fn retry_after(&self) -> Option<std::time::Duration> { None }
}

/// Which backing store a failure originated from — carried on [`ConnectError`]
/// so a single error type can describe every store without losing provenance.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, serde::Serialize, serde::Deserialize,
)]
pub enum BackendKind {
	/// The global index / relational spine.
	Postgres,
	/// The semantic vector store.
	Qdrant,
	/// The graph store.
	Terminus,
	/// The blob object store (S3 / GCS / local).
	ObjectStore,
	/// The full-text search index.
	Tantivy,
}

/// The specific way a connection attempt failed. Every variant preserves the
/// information an operator needs to distinguish "misconfigured" from "down"
/// from "incompatible".
#[derive(Debug, Error)]
pub enum ConnectFailure {
	/// The endpoint could not be reached (DNS, refused, network).
	#[error("endpoint unreachable")]
	Unreachable,

	/// Authentication or authorization was rejected.
	#[error("authentication rejected")]
	Auth,

	/// The connection came up but the schema/migration state is incompatible.
	#[error("schema not migrated or incompatible")]
	SchemaMismatch,

	/// A vector collection's dimension disagrees with the compiled-in `DIM`.
	#[error("vector dimension mismatch: store has {found}, expected {expected}")]
	DimensionMismatch { expected: usize, found: usize },

	/// The handshake timed out.
	#[error("connection timed out")]
	Timeout,

	/// Anything else, with its source preserved.
	#[error("connection failed")]
	Other(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Raised when a `Cold` store fails to come up. Carries which backend and how,
/// with the underlying cause chained via [`std::error::Error::source`].
#[derive(Debug, Error)]
#[error("failed to connect to {backend}: {kind}")]
pub struct ConnectError {
	/// Which backend failed to connect.
	pub backend: BackendKind,
	/// How it failed.
	#[source]
	pub kind: ConnectFailure,
}

impl ConnectError {
	/// Construct a connection error for a backend and failure mode.
	pub fn new(backend: BackendKind, kind: ConnectFailure) -> Self { Self { backend, kind } }
}

impl Retryable for ConnectError {
	fn is_retryable(&self) -> bool {
		matches!(self.kind, ConnectFailure::Unreachable | ConnectFailure::Timeout)
	}
}
