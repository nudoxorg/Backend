//! The rerank service behind `POST /v1/rerank` (09-vector §20.8 "Deep" mode).
//!
//! This pass does **not** self-host the cross-encoder: the default
//! implementation, [`HttpProxyReranker`], forwards to a configured external
//! rerank endpoint — the mirror of the embedder pattern (typed client,
//! OpenAI/TEI-style wire shape, optional bearer token, hard timeout). The
//! embedder itself now rides [`embedrs`]; this proxy is still hand-rolled.
//!
//! # Latency budget (§20.7 / §20.9)
//!
//! Every call runs under `rerank.timeout_ms` (default 1.2 s — the whole deep
//! path's budget). On timeout the caller receives an explicit
//! [`Error::Timeout`] which the HTTP surface projects as
//! `rerank_unavailable` — reranking **never silently degrades** into unranked
//! results presented as ranked.
//!
//! # License gate (09b §18.3b, I15)
//!
//! The configured model id is validated against the vector-core license
//! deny-list at startup ([`crate::server::config::ServerConfiguration::validate`]);
//! a CC-BY-NC reranker id is a hard boot error, so this module can assume the
//! model it names is deployable.
//!
//! [`HttpEmbedder`]: crate::server::search::semantic::embedder::HttpEmbedder

use std::future::Future;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use smol_str::SmolStr;
use url::Url;

use crate::server::config::RerankConfig;

/// One candidate document for reranking: the caller's opaque id plus the text
/// the cross-encoder scores against the query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankDocument {
    /// The caller's identity for this document (echoed back on the score).
    pub id: String,
    /// The text scored against the query.
    pub text: String,
}

/// One reranked result: the document's id and its relevance score, higher is
/// more relevant. Returned sorted descending.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankScore {
    /// The id of the scored document (from [`RerankDocument::id`]).
    pub id: String,
    /// The cross-encoder relevance score.
    pub score: f32,
}

/// Why a rerank call failed. Timeout is its own variant because the HTTP
/// surface must project it as the explicit `rerank_unavailable` signal
/// (§20.9) rather than a generic 5xx.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The call outlived the configured latency budget.
    #[error("rerank timed out after {budget:?}")]
    Timeout { budget: Duration },

    /// The HTTP round-trip failed (connect, TLS, mid-body).
    #[error("rerank transport failed")]
    Transport(#[source] reqwest::Error),

    /// The endpoint answered a non-success status.
    #[error("rerank endpoint returned {status}: {body}")]
    HttpStatus {
        status: reqwest::StatusCode,
        body: String,
    },

    /// The response body did not match the expected wire shape.
    #[error("malformed rerank response")]
    Malformed(#[source] serde_json::Error),

    /// The endpoint scored a document index outside the request's range.
    #[error("rerank result index {index} out of bounds ({count} documents)")]
    IndexOutOfBounds { index: usize, count: usize },
}

/// Backwards-compatible alias: the rerank error (now [`Error`]).
pub use self::Error as RerankError;

/// A service that scores documents against a query with a cross-encoder.
/// Trait-based so the deep search path and the `/v1/rerank` handler stay
/// implementation-agnostic (self-hosting lands behind the same trait later).
pub trait RerankService: Send + Sync {
    /// Score `documents` against `query`, returning at most `top_k` results
    /// sorted by descending relevance.
    fn rerank(
        &self,
        query: &str,
        documents: &[RerankDocument],
        top_k: usize,
    ) -> impl Future<Output = Result<Vec<RerankScore>, Error>> + Send;

    /// The model this service scores with (for telemetry labels).
    fn model_id(&self) -> &str;
}

/// The default [`RerankService`]: a proxy speaking the ubiquitous rerank wire
/// shape (`{"model", "query", "documents": [text, ...], "top_n"}` →
/// `{"results": [{"index", "relevance_score"}, ...]}`) served by TEI, Cohere,
/// and the mxbai reference server alike.
pub struct HttpProxyReranker {
    client: reqwest::Client,
    endpoint: Url,
    api_key: Option<SecretString>,
    model: SmolStr,
    budget: Duration,
}

impl HttpProxyReranker {
    /// Build from the resolved config; `None` when no endpoint is configured
    /// (the surface then answers `rerank_unavailable`).
    pub fn from_config(config: &RerankConfig) -> Option<Self> {
        let endpoint = config.endpoint.clone()?;
        Some(Self {
            client: reqwest::Client::new(),
            endpoint,
            api_key: config.api_key.clone(),
            model: config.model_id.clone(),
            budget: Duration::from_millis(config.timeout_ms),
        })
    }

    /// One wire round-trip, *without* the budget guard (applied by `rerank`).
    async fn request(
        &self,
        query: &str,
        documents: &[RerankDocument],
        top_k: usize,
    ) -> Result<Vec<RerankScore>, Error> {
        let payload = serde_json::json!({
            "model": self.model.as_str(),
            "query": query,
            "documents": documents.iter().map(|doc| doc.text.as_str()).collect::<Vec<_>>(),
            "top_n": top_k,
        });

        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header("content-type", "application/json")
            .json(&payload);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key.expose_secret());
        }

        let response = request.send().await.map_err(Error::Transport)?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            let body: String = message.chars().take(512).collect();
            return Err(Error::HttpStatus { status, body });
        }

        let bytes = response.bytes().await.map_err(Error::Transport)?;
        let decoded: WireResponse = serde_json::from_slice(&bytes).map_err(Error::Malformed)?;

        let mut scores = Vec::with_capacity(decoded.results.len().min(top_k));
        for row in decoded.results.into_iter().take(top_k) {
            let document = documents.get(row.index).ok_or(Error::IndexOutOfBounds {
                index: row.index,
                count: documents.len(),
            })?;
            scores.push(RerankScore {
                id: document.id.clone(),
                score: row.relevance_score,
            });
        }
        // Descending relevance, whatever order the endpoint chose.
        scores.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(scores)
    }
}

impl RerankService for HttpProxyReranker {
    async fn rerank(
        &self,
        query: &str,
        documents: &[RerankDocument],
        top_k: usize,
    ) -> Result<Vec<RerankScore>, Error> {
        if documents.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        tracing::trace!(
            count = documents.len(),
            top_k,
            model = %self.model,
            "rerank batch"
        );
        // The budget guard: the whole round-trip must land inside the deep
        // path's latency budget or answer the explicit timeout (§20.9).
        match tokio::time::timeout(self.budget, self.request(query, documents, top_k)).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout {
                budget: self.budget,
            }),
        }
    }

    fn model_id(&self) -> &str {
        self.model.as_str()
    }
}

/// The subset of the rerank response the server reads.
#[derive(Deserialize)]
struct WireResponse {
    results: Vec<WireResult>,
}

#[derive(Deserialize)]
struct WireResult {
    index: usize,
    relevance_score: f32,
}
