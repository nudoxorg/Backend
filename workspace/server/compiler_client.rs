//! HTTP client for the compiler daemon.

use thiserror::Error;
use url::Url;

/// Why a compiler-daemon request failed.
#[derive(Debug, Error)]
pub enum CompilerClientError {
    /// A transport or connection error (retryable).
    #[error("compiler transport error: {0}")]
    Transport(#[from] reqwest::Error),

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

    /// POST a compile request to `{base}/compile` and return the response.
    pub async fn compile(
        &self,
        req: registry::protocol::CompileRequest,
    ) -> Result<registry::protocol::CompileResponse, CompilerClientError> {
        let url = self
            .base
            .join("/compile")
            .expect("base URL is valid; /compile path is valid");
        let response = self.http.post(url).json(&req).send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(CompilerClientError::NonSuccess { status, body });
        }
        let resp: registry::protocol::CompileResponse = response.json().await?;
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
