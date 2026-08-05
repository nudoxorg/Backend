//! Embedding-model brands: the sealed trait, the two canonical models of the
//! dual-plane design (09-vector §20.2), and the reranker license deny-list.
//!
//! The model is lifted into the *type* — an `Embedding<JinaCodeV2>` can never
//! feed a `VectorStore<VoyageCode3>`; the mismatch is a compile error (I11).

use std::fmt;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// The stable, wire-visible model identifier (HF-style `org/name`), stamped on
/// every stored vector and folded into [`crate::key::embed_key`] (I2: a model
/// change is a re-embed, never a silent reuse).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelId(SmolStr);

impl ModelId {
    /// Wrap an id string. Prefer `M::id()` from a brand — free-form ids exist
    /// only at deserialization boundaries.
    pub fn new(id: impl Into<SmolStr>) -> Self {
        Self(id.into())
    }

    /// The raw id token.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The distance metric a model's vectors are compared under. Only cosine is
/// admitted (both canonical models are cosine-trained); the enum exists so the
/// metric is part of shard schema and embed keys, not an implicit convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Metric {
    /// Cosine similarity (vectors L2-normalized at the store boundary).
    Cosine,
}

impl Metric {
    /// Stable token folded into digests ([`crate::key::embed_key`]).
    pub const fn as_str(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
        }
    }
}

/// The canonical ONNX weights filename both planes must load (09-vector §20.3:
/// the *same* int8 quantized artifact locally and remotely, so local and
/// remote vectors are bit-comparable — I12).
pub const CANONICAL_WEIGHTS_FILE: &str = "model_quantized.onnx";

/// A pinned local-weights artifact hint for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightsArtifact {
    /// Filename inside the model package (always the canonical int8 ONNX).
    pub file: &'static str,

    /// SHA-256 of the artifact. `None` until operationally pinned; once pinned
    /// it participates in [`crate::key::tool_digest`] (I12).
    pub sha256: Option<[u8; 32]>,
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::JinaCodeV2 {}
    impl Sealed for super::VoyageCode3 {}
}

/// A compile-time embedding-model brand: dimensionality, metric, stable id,
/// and (for locally-runnable models) the weights artifact. Sealed — only the
/// catalog below exists, so a brand always names a real, licensed model (I15
/// is enforced structurally for brands; [`license::assert_licensed`] guards
/// the stringly config boundary).
pub trait EmbeddingModel: sealed::Sealed + Send + Sync + 'static {
    /// The model's output dimensionality.
    const DIMENSIONS: usize;

    /// The distance metric the model was trained for.
    const METRIC: Metric;

    /// The stable model id (`org/name`).
    fn id() -> ModelId;

    /// Local weights hint; `None` for API-only models.
    fn weights_hint() -> Option<WeightsArtifact>;
}

/// `jinaai/jina-embeddings-v2-base-code` — the self-hostable parity model
/// (Apache-2.0; runs identically on both planes as canonical int8 ONNX).
pub enum JinaCodeV2 {}

impl EmbeddingModel for JinaCodeV2 {
    const DIMENSIONS: usize = 768;
    const METRIC: Metric = Metric::Cosine;

    fn id() -> ModelId {
        ModelId::new("jinaai/jina-embeddings-v2-base-code")
    }

    fn weights_hint() -> Option<WeightsArtifact> {
        Some(WeightsArtifact {
            file: CANONICAL_WEIGHTS_FILE,
            sha256: None,
        })
    }
}

/// `voyage/voyage-code-3` — the premium API-only model (remote plane only;
/// no local weights exist).
pub enum VoyageCode3 {}

impl EmbeddingModel for VoyageCode3 {
    const DIMENSIONS: usize = 1024;
    const METRIC: Metric = Metric::Cosine;

    fn id() -> ModelId {
        ModelId::new("voyage/voyage-code-3")
    }

    fn weights_hint() -> Option<WeightsArtifact> {
        None
    }
}

/// The model license deny-list (09b §18.3b, I15): CC-BY-NC models must never
/// be loaded in any serving plane.
pub mod license {
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
    pub struct LicenseError {
        /// The offending model id, verbatim.
        pub model_id: String,
    }

    /// Reject deny-listed model ids (I15). The match is *exact* (whole-id
    /// equality), so `"jinaai/jina-colbert-v2-onnx"` or a substring like
    /// `"colbert"` never trips it — no stringly `contains` heuristics.
    pub fn assert_licensed(model_id: &str) -> Result<(), LicenseError> {
        if FORBIDDEN_MODEL_IDS.contains(&model_id) {
            return Err(LicenseError {
                model_id: model_id.to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_catalog_is_frozen() {
        assert_eq!(JinaCodeV2::DIMENSIONS, 768);
        assert_eq!(VoyageCode3::DIMENSIONS, 1024);
        assert_eq!(
            JinaCodeV2::id().as_str(),
            "jinaai/jina-embeddings-v2-base-code"
        );
        assert_eq!(VoyageCode3::id().as_str(), "voyage/voyage-code-3");
        assert_eq!(JinaCodeV2::METRIC, Metric::Cosine);
    }

    #[test]
    fn jina_hints_canonical_weights_voyage_does_not() {
        let hint = JinaCodeV2::weights_hint().expect("jina is self-hostable");
        assert_eq!(hint.file, "model_quantized.onnx");
        assert_eq!(hint.sha256, None, "sha unpinned until ops freeze");
        assert!(VoyageCode3::weights_hint().is_none(), "voyage is API-only");
    }

    #[test]
    fn deny_list_blocks_exact_ids_only() {
        // I15: both forbidden ids rejected.
        for id in license::FORBIDDEN_MODEL_IDS {
            assert!(license::assert_licensed(id).is_err());
        }
        // Substring-safety: near-misses and substrings pass.
        assert!(license::assert_licensed("jinaai/jina-colbert-v2-onnx").is_ok());
        assert!(license::assert_licensed("jina-reranker-v2-base-multilingual").is_ok());
        assert!(license::assert_licensed("colbert").is_ok());
        assert!(license::assert_licensed(JinaCodeV2::id().as_str()).is_ok());
    }
}
