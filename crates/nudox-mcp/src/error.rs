//! `McpError` — the crate's single error enum (§L7.4).
//!
//! `anyhow` appears nowhere in this crate outside tests. Every fallible
//! operation returns `Result<_, McpError>`, and the MCP tool layer converts
//! that into an `rmcp::ErrorData` with a JSON-RPC error code chosen from the
//! variant — so an agent sees `invalid_params` for a malformed `SymbolKey` and
//! `internal_error` for an engine failure, rather than one opaque code for
//! everything.

use std::net::SocketAddr;

use nudox_engine::wire::EngineError;

/// Errors raised by the hosted MCP server.
///
/// `#[non_exhaustive]` per §L7.5: adding a variant when a new tool or transport
/// concern appears must not be a breaking change for `lindsey`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The request carried no session token, or the wrong one.
    ///
    /// Deliberately carries no detail: a caller that guessed wrong learns only
    /// that it guessed wrong (§L6 local-only-by-construction).
    #[error("missing or invalid session token")]
    Unauthenticated,

    /// A `SymbolKey` argument was not in `ecosystem:name#introhex` form.
    #[error("malformed symbol key {key:?}: {reason}")]
    MalformedKey {
        /// The rejected input, echoed back so an agent can self-correct.
        key: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A tool argument was structurally valid but semantically out of range.
    #[error("invalid argument {argument}: {reason}")]
    InvalidArgument {
        /// The offending argument's name.
        argument: &'static str,
        /// Why it was rejected.
        reason: String,
    },

    /// The engine stream failed or was cancelled mid-flight.
    #[error("engine error: {0}")]
    Engine(#[from] EngineError),

    /// The engine dropped the stream before emitting a terminal event.
    ///
    /// Distinct from [`McpError::Engine`]: this is a broken invariant on our
    /// side of the seam, not a failure the engine reported.
    #[error("engine stream ended without a terminal event")]
    TruncatedStream,

    /// The requested resource URI is not one this server serves.
    #[error("unknown resource uri: {0}")]
    UnknownResource(String),

    /// The listener could not be bound.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The address we tried to bind.
        addr: SocketAddr,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The HTTP server terminated abnormally.
    #[error("mcp http server failed: {0}")]
    Serve(#[source] std::io::Error),
}

impl McpError {
    /// Convert to the JSON-RPC error an MCP client sees.
    ///
    /// The mapping is deliberate rather than uniform: an agent that gets
    /// `invalid_params` knows to fix its arguments and retry, whereas
    /// `internal_error` means retrying the same call is pointless.
    pub fn into_error_data(self) -> rmcp::ErrorData {
        let message = self.to_string();
        match self {
            Self::Unauthenticated => rmcp::ErrorData::invalid_request(message, None),
            Self::MalformedKey { .. } | Self::InvalidArgument { .. } => {
                rmcp::ErrorData::invalid_params(message, None)
            }
            Self::UnknownResource(_) => rmcp::ErrorData::resource_not_found(message, None),
            Self::Engine(_)
            | Self::TruncatedStream
            | Self::Bind { .. }
            | Self::Serve(_) => rmcp::ErrorData::internal_error(message, None),
        }
    }
}

impl From<McpError> for rmcp::ErrorData {
    fn from(e: McpError) -> Self {
        e.into_error_data()
    }
}
