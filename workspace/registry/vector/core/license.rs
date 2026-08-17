//! Model-license deny-list: models forbidden from any serving plane.

/// Model ids that are CC-BY-NC licensed and forbidden from any serving
/// path (I15). The Jina v2 rerankers/ColBERT are non-commercial; the
/// replacement rerankers are mxbai / Voyage cross-encoders.
pub const FORBIDDEN_MODEL_IDS: &[&str] = &[
    "jinaai/jina-reranker-v2-base-multilingual",
    "jinaai/jina-colbert-v2",
];

/// A forbidden model was named at a config/load boundary (I15).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("model `{model_id}` is license-forbidden (CC-BY-NC; I15 deny-list)")]
pub struct Error {
    /// The offending model id, verbatim.
    pub model_id: String,
}

/// Reject deny-listed model ids (I15). The match is *exact* (whole-id
/// equality), so `"jinaai/jina-colbert-v2-onnx"` or a substring like
/// `"colbert"` never trips it — no stringly `contains` heuristics.
pub fn assert_licensed(model_id: &str) -> Result<(), Error> {
    if FORBIDDEN_MODEL_IDS.contains(&model_id) {
        return Err(Error {
            model_id: model_id.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::core::model::{EmbeddingModel, JinaCodeV2};

    #[test]
    fn deny_list_blocks_exact_ids_only() {
        // I15: both forbidden ids rejected.
        for id in FORBIDDEN_MODEL_IDS {
            assert!(assert_licensed(id).is_err());
        }
        // Substring-safety: near-misses and substrings pass.
        assert!(assert_licensed("jinaai/jina-colbert-v2-onnx").is_ok());
        assert!(assert_licensed("jina-reranker-v2-base-multilingual").is_ok());
        assert!(assert_licensed("colbert").is_ok());
        assert!(assert_licensed(JinaCodeV2::id().as_str()).is_ok());
    }
}
