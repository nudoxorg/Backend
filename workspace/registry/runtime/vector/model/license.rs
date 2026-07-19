//! License deny-list for embedding and reranking models.
//!
//! Certain models carry commercial or derivative-work licenses that are
//! incompatible with the index's operating terms. This module is the single
//! enforcement point: call [`assert_licensed`] at model-bind time (startup /
//! `Semantic::connect`) and the process halts rather than silently serving
//! vectors under a forbidden model.

use thiserror::Error;

/// Model ids whose licenses are incompatible with the index service.
///
/// JinaAI's reranker and ColBERT models are licensed CC-BY-NC-4.0 (non-
/// commercial). Adding them here prevents accidental runtime activation.
pub const FORBIDDEN_MODEL_IDS: &[&str] = &[
    "jinaai/jina-reranker-v2-base-multilingual",
    "jinaai/jina-colbert-v2",
];

/// A model id that must not be used at runtime.
#[derive(Debug, Error)]
#[error("model {model_id:?} is on the license deny-list and cannot be used")]
pub struct LicenseError {
    pub model_id: String,
}

/// Assert that `model_id` is not on the deny-list.
///
/// Call at the model-bind site (constructor / `connect()`) so a
/// misconfigured model fails loudly at startup, not silently at query time.
pub fn assert_licensed(model_id: &str) -> Result<(), LicenseError> {
    if FORBIDDEN_MODEL_IDS.contains(&model_id) {
        return Err(LicenseError { model_id: model_id.to_owned() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_models_pass() {
        assert!(assert_licensed("jinaai/jina-embeddings-v2-base-code").is_ok());
        assert!(assert_licensed("voyage/voyage-code-3").is_ok());
        assert!(assert_licensed("openai/text-embedding-3-small").is_ok());
    }

    #[test]
    fn forbidden_reranker_rejected() {
        let err = assert_licensed("jinaai/jina-reranker-v2-base-multilingual").unwrap_err();
        assert!(err.to_string().contains("jinaai/jina-reranker-v2-base-multilingual"));
    }

    #[test]
    fn forbidden_colbert_rejected() {
        let err = assert_licensed("jinaai/jina-colbert-v2").unwrap_err();
        assert!(err.to_string().contains("jinaai/jina-colbert-v2"));
    }
}
