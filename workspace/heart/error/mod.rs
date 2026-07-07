pub mod connect;
pub mod failure;

pub use connect::{ConnectError, ConnectFailure};
pub use failure::{Failure, FailureKind, Phase, ResolutionState};

pub trait StoreError: std::error::Error + Send + Sync + 'static {}
impl<T: std::error::Error + Send + Sync + 'static> StoreError for T {}

/// Uniform retry classification. The queue and sink machinery
/// consult this instead of pattern-matching each backend's error enum, so the
/// retry policy is written exactly once.
pub trait Retryable {
    /// Whether retrying the operation that produced this error could plausibly
    /// succeed (transient: timeouts, 5xx, connection resets. Nothing fatal.
    fn is_retryable(&self) -> bool;

    /// A hint that the backend is asking us to slow down (e.g. HTTP 429 /
    /// `Retry-After`), so the caller can honour backpressure rather than hammer.
    fn retry_after(&self) -> Option<std::time::Duration> {
        None
    }
}

/// Which backing store a failure originated from
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

