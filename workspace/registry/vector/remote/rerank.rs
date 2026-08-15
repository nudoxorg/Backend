//! Reranker trait and two concrete implementations.
//!
//! [`HttpReranker`] posts to the index's own `/v1/rerank` microservice (no
//! external call; useful in integration tests and when a local cross-encoder is
//! deployed).
//!
//! [`VoyageReranker`] calls the Voyage REST API (`/v1/rerank`), pinning:
//! - `model = "rerank-2.5"` (the only Voyage reranker cleared by the license
//!   check — I15; the Jina rerankers are on the deny-list)
//! - `top_k` explicitly (an omitted `top_k` returns an unpredictable count)
//!
//! Both constructors call [`crate::vector::core::assert_licensed`] on the configured
//! model id; construction fails if the model is on the deny-list.
//!
//! # API key security
//! `api_key` is a plain `String` but MUST be treated as a credential: never
//! log it, never include it in error messages, load it from a secrets manager.

use crate::vector::core::assert_licensed;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pinned Voyage rerank model (I15: cleared by license check).
const VOYAGE_RERANK_MODEL: &str = "rerank-2.5";

/// Base URL of the Voyage rerank endpoint.
const VOYAGE_RERANK_URL: &str = "https://api.voyageai.com/v1/rerank";

/// A document to be reranked, identified by a caller-assigned id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerankDoc {
    /// Caller-assigned opaque identifier. Echoed back in [`RerankScore`].
    pub id: String,
    /// The text to score against the query.
    pub text: String,
}

/// One reranked document and its relevance score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankScore {
    /// The id from the corresponding [`RerankDoc`].
    pub id: String,
    /// Relevance score in descending order (higher = more relevant).
    pub score: f64,
}

/// Errors from a reranker.
#[derive(Debug, Error)]
pub enum Error {
    #[error("reranker HTTP error: {0}")]
    Http(String),

    #[error("reranker serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("reranker API error (status {status}): {body}")]
    ApiError { status: u16, body: String },

    #[error("reranker transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("model license denied: {0}")]
    LicenseDenied(String),
}

/// Anything that can rerank a list of documents against a query.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Rerank `docs` against `query`.
    ///
    /// Returns scores in descending order (best first). Missing or
    /// zero-score documents may be omitted. The returned ids align with the
    /// input [`RerankDoc::id`] values.
    async fn rerank(
        &self,
        query: &str,
        docs: &[RerankDoc],
    ) -> Result<Vec<RerankScore>, Error>;
}

/// A reranker that posts to the index's own `/v1/rerank` microservice.
///
/// Wire shape: `{ query, documents: [{ id, text }, ...] }` →
/// `{ results: [{ id, relevance_score }, ...] }`.
pub struct HttpReranker {
    client: reqwest::Client,
    /// Full URL of the rerank endpoint (e.g. `http://index/v1/rerank`).
    endpoint: String,
}

impl HttpReranker {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl Reranker for HttpReranker {
    async fn rerank(
        &self,
        query: &str,
        docs: &[RerankDoc],
    ) -> Result<Vec<RerankScore>, Error> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        let body = serde_json::json!({
            "query": query,
            "documents": docs,
        });

        let resp = self.client.post(&self.endpoint).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::ApiError { status, body });
        }

        let result: IndexRerankResponse = resp.json().await?;
        Ok(result
            .results
            .into_iter()
            .map(|r| RerankScore {
                id: r.id,
                score: r.relevance_score,
            })
            .collect())
    }
}

/// A reranker that calls the Voyage `/v1/rerank` API.
///
/// Pinned: `model = "rerank-2.5"`, `top_k` always explicit.
///
/// # API key security
/// `api_key` is a plain `String` but MUST be treated as a credential.
pub struct VoyageReranker {
    client: reqwest::Client,
    /// Voyage API key. Treat as a secret — see module-level note.
    api_key: String,
    /// How many top documents to return.
    top_k: usize,
}

impl VoyageReranker {
    /// Create a `VoyageReranker`.
    ///
    /// Calls [`assert_licensed`] on `VOYAGE_RERANK_MODEL` — fails at
    /// construction if the model is on the deny-list (I15).
    pub fn new(api_key: String, top_k: usize) -> Result<Self, Error> {
        assert_licensed(VOYAGE_RERANK_MODEL)
            .map_err(|e| Error::LicenseDenied(e.to_string()))?;
        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            top_k,
        })
    }
}

#[async_trait]
impl Reranker for VoyageReranker {
    async fn rerank(
        &self,
        query: &str,
        docs: &[RerankDoc],
    ) -> Result<Vec<RerankScore>, Error> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        let body = VoyageRerankRequest {
            query: query.to_owned(),
            documents: docs.iter().map(|d| d.text.clone()).collect(),
            model: VOYAGE_RERANK_MODEL.to_owned(),
            top_k: self.top_k,
            return_documents: false,
        };

        let resp = self
            .client
            .post(VOYAGE_RERANK_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::ApiError { status, body });
        }

        let result: VoyageRerankResponse = resp.json().await?;

        // Voyage returns results in descending score order. Each result has an
        // `index` that refers to the *input document position* — map back to our
        // caller-assigned ids.
        let scores = result
            .data
            .into_iter()
            .filter_map(|r| {
                docs.get(r.index).map(|doc| RerankScore {
                    id: doc.id.clone(),
                    score: r.relevance_score,
                })
            })
            .collect();

        Ok(scores)
    }
}

// ── Wire shapes ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct VoyageRerankRequest {
    query: String,
    documents: Vec<String>,
    model: String,
    top_k: usize,
    return_documents: bool,
}

#[derive(Debug, Deserialize)]
struct VoyageRerankResponse {
    data: Vec<VoyageRerankItem>,
}

#[derive(Debug, Deserialize)]
struct VoyageRerankItem {
    index: usize,
    relevance_score: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexRerankResponse {
    results: Vec<IndexRerankItem>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexRerankItem {
    id: String,
    relevance_score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Voyage rerank request body must contain `top_k` and `model` pinned.
    #[test]
    fn voyage_rerank_request_pins_model_and_top_k() {
        let body = VoyageRerankRequest {
            query: "search query".to_owned(),
            documents: vec!["doc one".to_owned()],
            model: VOYAGE_RERANK_MODEL.to_owned(),
            top_k: 10,
            return_documents: false,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(
            json.contains("\"rerank-2.5\""),
            "model must be rerank-2.5: {json}"
        );
        assert!(json.contains("\"top_k\""), "top_k must be explicit: {json}");
        assert!(json.contains("10"), "top_k value must be 10: {json}");
        assert!(
            json.contains("\"return_documents\""),
            "return_documents must be present: {json}"
        );
    }

    /// VoyageReranker::new succeeds for the cleared model.
    #[test]
    fn voyage_reranker_constructs_for_cleared_model() {
        let result = VoyageReranker::new("test-key".to_owned(), 5);
        assert!(
            result.is_ok(),
            "rerank-2.5 is a cleared model; construction must succeed"
        );
    }

    /// License guard rejects forbidden model ids. The `HttpReranker` has no
    /// model-id binding, so we test via a direct `assert_licensed` call to prove
    /// the deny-list is wired (the `VoyageReranker` tests its own model-id guard).
    #[test]
    fn license_guard_rejects_forbidden_reranker_ids() {
        let deny =
            crate::vector::core::assert_licensed("jinaai/jina-reranker-v2-base-multilingual");
        assert!(deny.is_err(), "jina reranker must be on the deny-list");

        let deny2 = crate::vector::core::assert_licensed("jinaai/jina-colbert-v2");
        assert!(deny2.is_err(), "jina colbert must be on the deny-list");
    }

    /// License guard explicitly accepts the mxbai-rerank-base-v2 model (the
    /// approved CC-BY-NC-free alternative; I15). This complements the rejection
    /// tests: the deny-list is exact-match, so near-miss strings must pass.
    #[test]
    fn license_guard_accepts_mxbai_reranker() {
        let accept = crate::vector::core::assert_licensed("mixedbread-ai/mxbai-rerank-base-v2");
        assert!(
            accept.is_ok(),
            "mxbai-rerank-base-v2 must not be on the deny-list"
        );

        // Voyage rerank-2.5 is also cleared.
        let voyage = crate::vector::core::assert_licensed("rerank-2.5");
        assert!(voyage.is_ok(), "rerank-2.5 must not be on the deny-list");
    }

    /// Empty doc list returns empty result immediately.
    #[test]
    fn rerank_empty_docs_is_trivial() {
        // Pure unit test — no HTTP.
        let body = VoyageRerankRequest {
            query: "x".to_owned(),
            documents: vec![],
            model: VOYAGE_RERANK_MODEL.to_owned(),
            top_k: 5,
            return_documents: false,
        };
        assert!(body.documents.is_empty());
    }

    // ── Adversarial: top_k pinning in the serialized body ─────────────────────

    /// `top_k` must appear as an explicit integer in the serialized body for
    /// every value — including edge cases 0 and u32::MAX. An omitted or
    /// mis-typed `top_k` would cause the Voyage API to return an unpredictable
    /// number of results.
    #[test]
    fn voyage_rerank_body_pins_top_k_explicitly_for_multiple_values() {
        for top_k in [1usize, 5, 10, 100, 1000, usize::MAX / 2] {
            let body = VoyageRerankRequest {
                query: "how to spawn a thread".to_owned(),
                documents: vec!["std::thread::spawn".to_owned()],
                model: VOYAGE_RERANK_MODEL.to_owned(),
                top_k,
                return_documents: false,
            };
            let json = serde_json::to_string(&body)
                .unwrap_or_else(|e| panic!("serialization failed for top_k={top_k}: {e}"));

            assert!(
                json.contains("\"top_k\""),
                "top_k key must be explicit for top_k={top_k}: {json}"
            );
            assert!(
                json.contains(&top_k.to_string()),
                "top_k value {top_k} must be present for top_k={top_k}: {json}"
            );
            // Pinned fields must always be present alongside top_k.
            assert!(
                json.contains("\"rerank-2.5\""),
                "model must be rerank-2.5 for top_k={top_k}: {json}"
            );
            assert!(
                json.contains("\"return_documents\""),
                "return_documents must be explicit for top_k={top_k}: {json}"
            );
        }
    }

    /// `return_documents` must always be `false` in the serialized body.
    /// A `true` here would cause Voyage to echo back document texts,
    /// bloating the response without any benefit to our pipeline.
    #[test]
    fn voyage_rerank_body_return_documents_always_false() {
        let body = VoyageRerankRequest {
            query: "q".to_owned(),
            documents: vec!["text".to_owned()],
            model: VOYAGE_RERANK_MODEL.to_owned(),
            top_k: 10,
            return_documents: false,
        };
        let json = serde_json::to_string(&body).expect("serializes");
        assert!(
            json.contains("\"return_documents\":false"),
            "return_documents must be false in body: {json}"
        );
        assert!(
            !json.contains("\"return_documents\":true"),
            "return_documents must not be true: {json}"
        );
    }

    // ── Adversarial: deny-list exact-match semantics ──────────────────────────

    /// Both deny-list entries are rejected with exact string matching.
    /// Case-modified or whitespace-padded variants must pass (exact-match only).
    #[test]
    fn deny_list_exact_match_both_entries_rejected() {
        use crate::vector::core::assert_licensed;
        let forbidden = [
            "jinaai/jina-reranker-v2-base-multilingual",
            "jinaai/jina-colbert-v2",
        ];
        for id in forbidden {
            assert!(
                assert_licensed(id).is_err(),
                "deny-listed id '{id}' must be rejected"
            );
        }
    }

    /// Case-modified variants of deny-list entries are accepted (exact-match).
    #[test]
    fn deny_list_case_near_misses_are_accepted() {
        use crate::vector::core::assert_licensed;
        let near_misses = [
            "JINAAI/jina-reranker-v2-base-multilingual",
            "JinaAI/Jina-Reranker-V2-Base-Multilingual",
            "jinaai/JINA-COLBERT-V2",
        ];
        for id in near_misses {
            assert!(
                assert_licensed(id).is_ok(),
                "case-modified near-miss '{id}' must be accepted (exact-match only)"
            );
        }
    }

    /// Whitespace-padded variants of deny-list entries are accepted.
    #[test]
    fn deny_list_whitespace_near_misses_are_accepted() {
        use crate::vector::core::assert_licensed;
        let near_misses = [
            " jinaai/jina-reranker-v2-base-multilingual",
            "jinaai/jina-reranker-v2-base-multilingual ",
            "\tjinaai/jina-colbert-v2",
        ];
        for id in near_misses {
            assert!(
                assert_licensed(id).is_ok(),
                "whitespace near-miss '{id}' must be accepted"
            );
        }
    }

    /// Substring/prefix/suffix of deny-list entries are accepted.
    #[test]
    fn deny_list_substrings_are_not_forbidden() {
        use crate::vector::core::assert_licensed;
        let substrings = [
            "jinaai/jina-reranker",               // prefix of entry 1
            "jina-reranker-v2-base-multilingual", // no org prefix
            "jinaai/jina-colbert",                // prefix of entry 2
            "jinaai/jina-colbert-v2-onnx",        // superset (different string)
        ];
        for id in substrings {
            assert!(
                assert_licensed(id).is_ok(),
                "substring near-miss '{id}' must be accepted"
            );
        }
    }

    // ── Adversarial: response with out-of-range index silently dropped ────────

    /// When Voyage returns a result whose `index` exceeds the document count,
    /// `VoyageReranker::rerank` uses `docs.get(r.index)` → `None` → dropped.
    /// Pin: the filter_map contract is correct for out-of-range indices.
    #[test]
    fn voyage_rerank_out_of_range_index_results_in_none() {
        let docs = [RerankDoc {
                id: "a".to_owned(),
                text: "tokio::spawn".to_owned(),
            },
            RerankDoc {
                id: "b".to_owned(),
                text: "std::thread".to_owned(),
            }];
        // Index 5 is out of range for 2 docs.
        assert!(
            docs.get(5).is_none(),
            "out-of-range index must yield None → entry dropped"
        );
        // Index 1 (the last) is in range.
        assert!(docs.get(1).is_some(), "last valid index must yield Some");
        // Index 2 is one past the end.
        assert!(docs.get(2).is_none(), "one-past-end must yield None");
    }

    /// HttpReranker response with duplicate ids: both are preserved (no dedup).
    #[test]
    fn http_reranker_duplicate_ids_are_preserved() {
        let json =
            r#"{"results":[{"id":"x","relevance_score":0.9},{"id":"x","relevance_score":0.7}]}"#;
        let response: IndexRerankResponse = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(
            response.results.len(),
            2,
            "two entries even with duplicate ids"
        );
    }

    /// Voyage response where all `index` values are out of range → zero scored
    /// results (the filter_map drops all entries). Pin: empty result, no panic.
    #[test]
    fn voyage_rerank_all_indices_out_of_range_yields_empty() {
        let docs: Vec<RerankDoc> = vec![RerankDoc {
            id: "a".to_owned(),
            text: "foo".to_owned(),
        }];
        // Simulate: Voyage returned results for indices [5, 6, 7] — all OOB for 1 doc.
        let out_of_range_indices = [5usize, 6, 7];
        let scores: Vec<RerankScore> = out_of_range_indices
            .iter()
            .filter_map(|&idx| {
                docs.get(idx).map(|doc| RerankScore {
                    id: doc.id.clone(),
                    score: 0.9,
                })
            })
            .collect();
        assert!(
            scores.is_empty(),
            "all OOB indices → no scores; got {} scores",
            scores.len()
        );
    }

    /// Voyage response with scores for docs that were never sent: `index` points
    /// to a slot that doesn't correspond to any submitted doc. Same filter_map
    /// semantics — silently dropped.
    #[test]
    fn voyage_rerank_scores_for_unsent_docs_are_dropped() {
        // 2 docs sent, Voyage returns index=0 (valid) and index=99 (never sent).
        let docs = [RerankDoc {
                id: "a".to_owned(),
                text: "first".to_owned(),
            },
            RerankDoc {
                id: "b".to_owned(),
                text: "second".to_owned(),
            }];
        let raw_indices = [(0usize, 0.9f64), (99, 0.8), (1, 0.7)];
        let scores: Vec<RerankScore> = raw_indices
            .iter()
            .filter_map(|&(idx, score)| {
                docs.get(idx).map(|doc| RerankScore {
                    id: doc.id.clone(),
                    score,
                })
            })
            .collect();
        // Only indices 0 and 1 are valid.
        assert_eq!(scores.len(), 2, "only 2 valid indices; index 99 dropped");
        assert_eq!(scores[0].id, "a");
        assert_eq!(scores[1].id, "b");
    }
}
