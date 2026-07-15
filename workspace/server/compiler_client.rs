//! HTTP client for the compiler daemon.
//!
//! Request/response bodies are **postcard** (binary), not JSON. The protocol
//! types are postcard-serializable mirrors shared with the daemon — shipping
//! every source file as a JSON number array was both huge and wrong for the
//! wire contract documented on [`registry::protocol`].

use thiserror::Error;
use url::Url;

/// Content-Type for the postcard compile envelope (both directions).
pub const POSTCARD_CONTENT_TYPE: &str = "application/x-postcard";

/// Why a compiler-daemon request failed.
#[derive(Debug, Error)]
pub enum CompilerClientError {
    /// A transport or connection error (retryable).
    #[error("compiler transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Encoding the request as postcard failed.
    #[error("compiler request encode error: {0}")]
    Encode(#[from] postcard::Error),

    /// The daemon responded with a non-2xx status.
    #[error("compiler daemon returned status {status}: {body}")]
    NonSuccess { status: u16, body: String },

    /// The daemon returned CompileResponse::Err.
    #[error("compiler rejected package: [{kind}] {message}")]
    RemoteError { kind: String, message: String },
}

impl heart::Retryable for CompilerClientError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_))
    }
}

/// A typed HTTP client for the compiler daemon.
#[derive(Clone, Debug)]
pub struct CompilerClient {
    base: Url,
    http: reqwest::Client,
}

impl CompilerClient {
    /// Create a new client targeting `base` (e.g. `http://127.0.0.1:8080`).
    pub fn new(base: Url) -> Self {
        Self { base, http: reqwest::Client::new() }
    }

    /// POST a postcard-encoded compile request to `{base}/compile` and decode
    /// the postcard response.
    pub async fn compile(
        &self,
        req: registry::protocol::CompileRequest,
    ) -> Result<registry::protocol::CompileResponse, CompilerClientError> {
        let url = self
            .base
            .join("/compile")
            .expect("base URL is valid; /compile path is valid");
        let body = postcard::to_allocvec(&req)?;
        let response = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, POSTCARD_CONTENT_TYPE)
            .header(reqwest::header::ACCEPT, POSTCARD_CONTENT_TYPE)
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(CompilerClientError::NonSuccess { status, body });
        }
        let bytes = response.bytes().await?;
        let resp: registry::protocol::CompileResponse =
            postcard::from_bytes(&bytes).map_err(CompilerClientError::Encode)?;
        match &resp {
            registry::protocol::CompileResponse::Err { kind, message } => {
                Err(CompilerClientError::RemoteError {
                    kind: kind.clone(),
                    message: message.clone(),
                })
            }
            registry::protocol::CompileResponse::Ok { .. } => Ok(resp),
        }
    }
}
