//! The server's error taxonomy and its mapping onto HTTP responses.
//!
//! Internal subsystem errors (registry, runtime) are carried as their **concrete**
//! typed sources via `#[from]` — never boxed/erased — so the whole chain survives
//! to structured logs and retry classification can delegate straight to the
//! subsystem's own [`Retryable`]. Only the outward [`ServerError::status`]
//! projection decides what the client sees.

use heart::{ConnectError, NameError, PackageId, Retryable};



/// The single error type every handler and coordination flow returns.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// A backing store failed to connect during assembly/health.
    #[error(transparent)]
    Connect(#[from] ConnectError),

    /// The registry tier failed.
    #[error(transparent)]
    Registry(#[from] crate::registry::RegistryError),

    /// The runtime serving tier failed.
    #[error(transparent)]
    Runtime(#[from] registry::runtime::RuntimeError),

    /// The compiler daemon call failed (transport or remote rejection).
    #[error(transparent)]
    Compile(#[from] crate::compiler_client::CompilerClientError),

    /// The request was malformed or violated an invariant (→ 4xx).
    #[error(transparent)]
    BadRequest(#[from] BadRequestReason),

    /// The caller is not permitted to perform the action (→ 403).
    #[error(transparent)]
    Forbidden(#[from] ForbiddenReason),

    /// The requested resource does not exist (→ 404).
    #[error("not found")]
    NotFound,

    /// Configuration failed to resolve.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    /// An internal invariant broke (→ 500).
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// Why a search query (literal or abstract) was rejected. Lives here (in the
/// server's error taxonomy) so it can be carried losslessly inside
/// `BadRequestReason::MalformedQuery` with full `#[from]` chaining, while still
/// being re-exported from `search::query` for the query-parsing API.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
	/// The query string was completely empty (no characters at all).
	#[error("empty query")]
	Empty,

	/// The query contained only whitespace; nothing left after trim.
	#[error("query empty after trimming whitespace")]
	EmptyAfterTrim { raw: String },

	/// Query text exceeded the hard safety limit for literal paste queries.
	#[error("query length {len} exceeds maximum of {max}")]
	TooLong {
		len: usize,
		max: usize,
		/// First ~64 chars of the offending query for diagnostics.
		snippet: String,
	},

	/// Query text contained a control character (NUL, DEL, etc.).
	#[error("query contains control character")]
	ControlCharacter {
		position: usize,
		/// Surrounding snippet around the bad char.
		snippet: String,
	},

	/// An operator character was used in a context the literal surface does not
	/// support (future use by a full query parser surface).
	#[error("invalid operator '{op}' at position {position}")]
	InvalidOperator {
		query: String,
		position: usize,
		op: char,
	},

	/// Parentheses (or other grouping) were unbalanced.
	#[error("unbalanced parentheses in query")]
	UnbalancedParens { query: String, position: usize },

	/// A field name in a structured query clause is not known to this index.
	#[error("unknown field '{field}'")]
	UnknownField {
		query: String,
		field: String,
		position: Option<usize>,
	},

	/// A literal value supplied for a field had the wrong type for the field's
	/// schema (e.g. number where string expected).
	#[error("type mismatch for field '{field}': expected {expected}")]
	TypeMismatch {
		query: String,
		field: String,
		expected: &'static str,
		actual: String,
		position: Option<usize>,
	},

	/// Other query malformation not covered by the explicit cases above.
	/// Prefer adding a variant rather than widening this.
	#[error("malformed query: {detail}")]
	Malformed { detail: String, query: String },
}

/// Rich, typed reasons a request was rejected as bad (client error, 4xx).
/// Never uses dynamic format strings in the variant data; all information is
/// carried as typed fields with #[source] chains preserved for programmatic
/// inspection and structured logging.
#[derive(Debug, thiserror::Error)]
pub enum BadRequestReason {
    /// A required field was absent from the request DTO.
    #[error("missing field: {field}")]
    MissingField { field: &'static str },

    /// The request body was not valid JSON (or failed to deserialize into the
    /// expected shape).
    #[error("invalid json body")]
    InvalidJson(#[source] serde_json::Error),

    /// A literal or abstract search query was syntactically or semantically
    /// invalid. Carries the full `QueryError` (which itself carries the original
    /// snippet + position where possible).
    #[error(transparent)]
    MalformedQuery(#[from] QueryError),

    /// The supplied `origin` name does not correspond to any registered custom
    /// registry.
    #[error("unknown custom registry {name:?}")]
    UnknownCustomRegistry { name: String },

    /// A package name failed validation for its ecosystem.
    #[error(transparent)]
    InvalidPackageName(#[from] NameError),

    /// A package version string failed to parse for its ecosystem.
    #[error(transparent)]
    InvalidPackageVersion(#[from] heart::identity::VersionError),

    /// No package selector in the filter resolved to a valid name for any of
    /// the requested (or default) ecosystems.
    #[error("no requested package name is valid for the requested ecosystems")]
    NoValidPackageSelectors,

    /// A UUID (symbol id or session id) in a path or expand body was malformed.
    #[error("invalid symbol id {raw:?}")]
    InvalidSymbolId {
        raw: String,
        #[source]
        source: uuid::Error,
    },

    /// A pagination cursor (base64url + postcard keyset) could not be decoded.
    #[error("invalid cursor token")]
    InvalidCursor {
        token: String,
        #[source]
        source: heart::cursor::CursorError,
    },

    /// The downloaded archive for an indexing job exceeded a safety limit.
    /// (During a request-driven path this is a client error; during background
    /// it is still classified as Malformed.)
    #[error("archive too large: {actual} bytes (limit {limit})")]
    ArchiveTooLarge { actual: u64, limit: u64 },

    /// The archive body after download exceeded the configured extraction ceiling.
    #[error("archive exceeded the download ceiling")]
    ArchiveExceedsLimit,

    /// A save/verify/rebuild operation named a snapshot that does not match the
    /// one currently held by the blob store for the package.
    #[error("snapshot mismatch for package {package}: blob store holds a different generation")]
    SnapshotMismatch { package: PackageId },

    /// A save/verify operation was attempted against a package that has never
    /// reached `Stored` state (no snapshot recorded).
    #[error("package {package} has no recorded snapshot")]
    NoRecordedSnapshot { package: PackageId },
}

/// Reasons a request was denied by the access policy (403).
#[derive(Debug, thiserror::Error)]
pub enum ForbiddenReason {
    #[error("action not permitted: {action}")]
    ActionDenied { action: &'static str },
}

/// Typed internal (server-side) failures. All data is structured; no bare
/// format strings in the error payload. These always project to 5xx.
#[derive(Debug, thiserror::Error)]
pub enum InternalError {
    /// The HTTP listener could not bind to the configured address.
    #[error("could not bind to {address}")]
    BindFailed {
        address: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },

    /// The axum server task itself failed.
    #[error("http server failed")]
    ServeFailed {
        #[source]
        source: std::io::Error, // axum::serve error is hyper-ish but surfaced as io in practice
    },

    /// An indexing job exceeded its hard deadline.
    #[error("indexing job exceeded deadline")]
    IndexingDeadlineExceeded,

    /// The search planner emitted a semantic plan for a literal query (invariant).
    #[error("planner produced a semantic plan for a literal query")]
    PlannerInvariantSemanticForLiteral,

    /// A constructed archive URL (for known origins) was unparseable.
    #[error("malformed archive url for {raw}")]
    MalformedArchiveUrl { raw: String },

    /// PyPI metadata JSON for a release was structurally wrong.
    #[error("malformed pypi metadata for {name} {version}")]
    MalformedPypiMetadata { name: String, version: String },

    /// The sdist URL extracted from PyPI metadata could not be parsed.
    #[error("malformed sdist url")]
    MalformedSdistUrl,

    /// Serializing the compiler's surface IR to the blob section failed.
    #[error("could not serialize surface IR")]
    IrSerializationFailed {
        #[source]
        source: serde_json::Error,
    },

    /// Materializing the extracted package onto a temporary tree for the
    /// compiler failed.
    #[error("could not materialize package sources for compilation")]
    MaterializeForCompile {
        #[source]
        source: std::io::Error,
    },

    /// Catch-all for other truly internal breakages where a more specific
    /// variant has not yet been introduced. Prefer adding a new variant.
    #[error("internal error: {message}")]
    Other { message: String },
}

impl ServerError {
    /// The HTTP status this error projects to. The single place the internal →
    /// external mapping is decided.
    ///
    /// Most BadRequestReasons are 400. Some (e.g. future validation) could be
    /// 422; oversized archives could be 413. Internal variants and Config are
    /// always 5xx. Subsystem errors decide their own (mostly 503 for unavailable).
    pub fn status(&self) -> http::StatusCode {
        use http::StatusCode;
        match self {
            ServerError::BadRequest(reason) => match reason {
                BadRequestReason::InvalidJson(_) => StatusCode::BAD_REQUEST,
                BadRequestReason::ArchiveTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE, // 413 when it surfaces on a request path
                BadRequestReason::ArchiveExceedsLimit => StatusCode::PAYLOAD_TOO_LARGE,
                // Everything else client-malformed is 400 (could be 422 for
                // some semantic validation cases in the future).
                _ => StatusCode::BAD_REQUEST,
            },
            ServerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServerError::NotFound => StatusCode::NOT_FOUND,
            ServerError::Connect(_) | ServerError::Runtime(_) | ServerError::Registry(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            // Compile failures are server-side pipeline faults (→ 5xx); transient
            // ones still surface as unavailable until retry succeeds.
            ServerError::Compile(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::Config(_) | ServerError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl axum::response::IntoResponse for ServerError {
    /// Project onto the wire: the [`ServerError::status`] code plus a small JSON
    /// body. The full typed chain is logged here (the last point it exists);
    /// clients only ever see the projection.
    ///
    /// Chain rendering uses `to_string` on sources (fine for logs). The original
    /// typed `ServerError` (and therefore all `BadRequestReason`, `InternalError`,
    /// `QueryError`, `NameError` etc. as `#[source]` fields) remain in the value
    /// for any programmatic consumer that inspects before `.into_response()`.
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        let chain = {
            let mut rendered = self.to_string();
            let mut source = std::error::Error::source(&self);
            while let Some(cause) = source {
                rendered.push_str(": ");
                rendered.push_str(&cause.to_string());
                source = cause.source();
            }
            rendered
        };
        if status.is_server_error() {
            tracing::error!(%status, error = %chain, "request failed");
        } else {
            tracing::warn!(%status, error = %chain, "request rejected");
        }
        let body = axum::Json(serde_json::json!({
            "status": status.as_u16(),
            "error": self.to_string(),
        }));
        (status, body).into_response()
    }
}

impl Retryable for ServerError {
    fn is_retryable(&self) -> bool {
        match self {
            ServerError::Connect(_) | ServerError::Runtime(_) | ServerError::Registry(_) => true,
            ServerError::Compile(error) => error.is_retryable(),
            _ => false,
        }
    }
}

/// A convenient result alias for handlers and flows.
pub type ServerResult<T> = Result<T, ServerError>;
