//! The server's error taxonomy and its mapping onto HTTP responses.
//!
//! Internal subsystem errors (registry, runtime) are carried as their **concrete**
//! typed sources via `#[from]` — never boxed/erased — so the whole chain survives
//! to structured logs and retry classification can delegate straight to the
//! subsystem's own [`Retryable`]. Only the outward [`ServerError::status`]
//! projection decides what the client sees.

use heart::{ConnectError, Retryable};

/// The single error type every handler and coordination flow returns.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// A backing store failed to connect during assembly/health.
    #[error(transparent)]
    Connect(#[from] ConnectError),

    /// The registry tier failed.
    #[error(transparent)]
    Registry(#[from] registry::RegistryError),

    /// The runtime serving tier failed.
    #[error(transparent)]
    Runtime(#[from] runtime::RuntimeError),

    /// The request was malformed or violated an invariant (→ 4xx).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// The caller is not permitted to perform the action (→ 403).
    #[error("forbidden: {0}")]
    Forbidden(&'static str),

    /// The requested resource does not exist (→ 404).
    #[error("not found")]
    NotFound,

    /// Configuration failed to resolve.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    /// An internal invariant broke (→ 500).
    #[error("internal error: {0}")]
    Internal(String),
}

impl ServerError {
    /// The HTTP status this error projects to. The single place the internal →
    /// external mapping is decided.
    pub fn status(&self) -> http::StatusCode {
        use http::StatusCode;
        match self {
            ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServerError::NotFound => StatusCode::NOT_FOUND,
            ServerError::Connect(_) | ServerError::Runtime(_) | ServerError::Registry(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ServerError::Config(_) | ServerError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl Retryable for ServerError {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            ServerError::Connect(_) | ServerError::Runtime(_) | ServerError::Registry(_)
        )
    }
}

/// A convenient result alias for handlers and flows.
pub type ServerResult<T> = Result<T, ServerError>;
