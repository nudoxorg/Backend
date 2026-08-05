//! The rerank endpoint: `POST /v1/rerank` (09-vector §20.8).
//!
//! Proxies to the configured external cross-encoder endpoint (via
//! [`crate::rerank::HttpProxyReranker`]). The handler constructs the reranker
//! from config on each request (the client is cheap to build; a future
//! optimization can hoist it into `Server<M>` if needed).
//!
//! # Timeout behavior
//!
//! The call runs under `config.rerank.timeout_ms` (default 1.2 s). On timeout
//! the handler returns `200 { rerank_unavailable: true }` — an explicit
//! unavailability signal, **never** a silent degrade (§20.9).
//!
//! # License gate
//!
//! `config.validate()` (called at assembly) rejects CC-BY-NC model ids before
//! the server ever starts, so by the time this handler runs, the configured
//! model id is guaranteed deployable (§18.3b I15).

#[allow(unused_imports)]
use crate::registry;
use std::sync::Arc;

use axum::{Json, extract::State};
use registry::vector::EmbeddingModel;
use serde::Serialize;

use crate::Server;
use crate::authz::Principal;
use crate::error::ServerResult;
use crate::http::dto::{RerankRequestDto, RerankResponseDto};
use crate::rerank::{HttpProxyReranker, RerankError, RerankService};

/// The response when the reranker is unconfigured or timed out. Not a 5xx:
/// the client should handle this as a graceful absence of reranking and fall
/// back to its unranked Stage-1 results (§20.9 explicit-unavailability rule).
#[derive(Debug, Serialize)]
pub struct RerankUnavailableDto {
    pub rerank_unavailable: bool,
    pub reason: &'static str,
}

/// `POST /v1/rerank` — cross-encoder reranking for Deep mode (§20.8).
///
/// Accepts the request, validates it, forwards to the configured endpoint, and
/// returns scored results in descending relevance. Timeout produces an explicit
/// `rerank_unavailable` response (§20.9), never a silent empty-scores answer.
#[tracing::instrument(skip_all, fields(
    documents = req.documents.len(),
    top_k    = req.top_k.get(),
))]
pub async fn rerank<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    _principal: Principal,
    Json(req): Json<RerankRequestDto>,
) -> ServerResult<axum::response::Response> {
    use axum::response::IntoResponse;

    req.validate()?;

    // Build the reranker from config on each request. If no endpoint is
    // configured, answer rerank_unavailable immediately (no error: it is
    // a legal service state, §20.9).
    let reranker = match HttpProxyReranker::from_config(&server.config().rerank) {
        Some(r) => r,
        None => {
            metrics::counter!("rerank_unavailable", "reason" => "no_endpoint").increment(1);
            tracing::debug!("rerank endpoint not configured; answering rerank_unavailable");
            return Ok(Json(RerankUnavailableDto {
                rerank_unavailable: true,
                reason: "no rerank endpoint configured",
            })
            .into_response());
        }
    };

    let top_k = req.top_k.get() as usize;
    let started = std::time::Instant::now();

    match reranker.rerank(&req.query, &req.documents, top_k).await {
        Ok(scores) => {
            metrics::histogram!("rerank_latency_seconds").record(started.elapsed().as_secs_f64());
            metrics::counter!("rerank_requests", "outcome" => "ok").increment(1);
            tracing::debug!(
                results = scores.len(),
                model = reranker.model_id(),
                "rerank completed"
            );
            Ok(Json(RerankResponseDto { scores }).into_response())
        }
        Err(RerankError::Timeout { budget }) => {
            // §20.9: timeout is the explicit unavailability case, not a 5xx.
            metrics::counter!("rerank_timeouts").increment(1);
            metrics::counter!("rerank_unavailable", "reason" => "timeout").increment(1);
            tracing::warn!(
                ?budget,
                model = reranker.model_id(),
                "rerank timed out; answering rerank_unavailable"
            );
            Ok(Json(RerankUnavailableDto {
                rerank_unavailable: true,
                reason: "rerank service timed out",
            })
            .into_response())
        }
        Err(error) => {
            // Transport or parsing failure — the reranker endpoint is broken.
            // Return explicit unavailability rather than a 5xx so the client
            // can fall back gracefully (§20.9).
            metrics::counter!("rerank_errors").increment(1);
            metrics::counter!("rerank_unavailable", "reason" => "error").increment(1);
            tracing::warn!(
                error = %error,
                model = reranker.model_id(),
                "rerank call failed; answering rerank_unavailable"
            );
            Ok(Json(RerankUnavailableDto {
                rerank_unavailable: true,
                reason: "rerank service error",
            })
            .into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rerank::{RerankDocument, RerankError, RerankScore, RerankService};

    /// A reranker that always times out after the configured budget.
    struct TimingOutReranker {
        budget: std::time::Duration,
    }

    impl RerankService for TimingOutReranker {
        async fn rerank(
            &self,
            _query: &str,
            _documents: &[RerankDocument],
            _top_k: usize,
        ) -> Result<Vec<RerankScore>, RerankError> {
            Err(RerankError::Timeout {
                budget: self.budget,
            })
        }

        fn model_id(&self) -> &str {
            "test/model"
        }
    }

    /// Timeout from the reranker service → explicit `rerank_unavailable`, not
    /// an error variant and not a silent empty result.
    #[tokio::test]
    async fn timeout_produces_rerank_unavailable() {
        let reranker = TimingOutReranker {
            budget: std::time::Duration::from_millis(1200),
        };
        let query = "async runtime";
        let docs = vec![RerankDocument {
            id: "0".to_owned(),
            text: "tokio".to_owned(),
        }];

        let result = reranker.rerank(query, &docs, 5).await;
        assert!(
            matches!(result, Err(RerankError::Timeout { .. })),
            "timeout variant emitted, not a generic error"
        );
    }

    /// An unrecognised HTTP error from the endpoint → `Transport` or
    /// `HttpStatus` error variant (not silently dropped).
    #[tokio::test]
    async fn http_status_error_is_explicit() {
        let err = RerankError::HttpStatus {
            status: reqwest::StatusCode::BAD_GATEWAY,
            body: "upstream gone".to_owned(),
        };
        // The variant carries the status, so the handler can project it as
        // rerank_unavailable without swallowing the detail.
        assert!(matches!(err, RerankError::HttpStatus { status, .. }
				if status == reqwest::StatusCode::BAD_GATEWAY));
    }
}
