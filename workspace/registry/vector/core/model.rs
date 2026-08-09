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

/// How an ONNX artifact encodes its weights numerically.
///
/// This exists because [`Self::DynamicInt8`] silently breaks the property the
/// whole durable-vector plane rests on, and nothing else in the load path can
/// detect it. See [`Self::is_batch_invariant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantization {
    /// Full float precision, as exported. Activations are computed in float.
    Float32,

    /// Half precision. Values are converted at export; no run-time refitting.
    Float16,

    /// int8 with the activation scales baked in at export ("static"/"QDQ").
    StaticInt8,

    /// int8 whose activation scale is computed **per batch, at run time**.
    ///
    /// A vector produced this way depends on the other texts it was batched
    /// with, so the same input yields different output on two runs that batch
    /// differently. Never durable-canonical.
    DynamicInt8,
}

impl Quantization {
    /// Whether a vector depends only on its own input, and not on whatever it
    /// happened to be batched alongside.
    ///
    /// This is the precondition for [`crate::vector::core::EmbedRuntimeInfo::
    /// durable_canonical`]: a vector may only be persisted, shared across
    /// planes, or compared with a vector computed elsewhere if it is a function
    /// of its input alone.
    pub const fn is_batch_invariant(self) -> bool {
        !matches!(self, Self::DynamicInt8)
    }
}

/// The canonical ONNX weights filename both planes must load.
///
/// # Why fp32 and not the int8 artifact (changed 2026-08-08)
///
/// This was `model_quantized.onnx` (161,895,621 bytes), chosen per 09-vector
/// §20.3 so both planes would load *the same* artifact and produce
/// bit-comparable vectors (I12). Measured against the real artifact, that
/// rationale inverts: `model_quantized.onnx` is **dynamically** quantized
/// (its producer string is `onnx.quantize`), so its activation scale is refit
/// from each batch's own value range — and the same text embedded next to
/// different neighbours comes out different.
///
/// Measured worst-component deviation for one text embedded alone vs. in a
/// batch (`vector_onnx_live::live_model_vectors_do_not_depend_on_batch_composition`):
///
/// | neighbour | `model_quantized.onnx` | `model_fp16.onnx` | `model.onnx` |
/// |---|---|---|---|
/// | same length  | 0.01525469 | 0.0 | 0.0 |
/// | much longer  | 0.02113844 | 0.00004499 | 0.00000010 |
/// | much shorter | 0.01431698 | 0.0 | 0.0 |
///
/// A batch of six *identical* texts reproduced the solo vector exactly under
/// all three, which rules out batch size and non-determinism: neighbour
/// *content* is the variable, which is dynamic range refitting and nothing
/// else. So the int8 artifact was not merely a worse choice than fp32 — it
/// could not deliver the one property it was selected for.
///
/// fp16 is 321 MB and deviates only at 4.5e-5 (fine for ranking, and its
/// residual is a sequence-length effect, not range refitting). It is rejected
/// anyway because `durable_canonical` is a claim about reproducibility, not
/// about being close enough.
pub const CANONICAL_WEIGHTS_FILE: &str = "model.onnx";

/// A pinned local-weights artifact hint for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightsArtifact {
    /// Filename inside the model package.
    pub file: &'static str,

    /// SHA-256 of the artifact. `None` until operationally pinned; once pinned
    /// it participates in [`crate::key::tool_digest`] (I12).
    pub sha256: Option<[u8; 32]>,

    /// How this artifact encodes its weights.
    ///
    /// Carried as a field rather than inferred from the filename because the
    /// consequence — whether vectors may be persisted at all — is too large to
    /// rest on a naming convention. A dynamically quantized artifact declared
    /// here makes every runtime loading it non-canonical, structurally.
    pub quantization: Quantization,
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
/// (Apache-2.0; runs identically on both planes as canonical fp32 ONNX).
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
            // Pinned 2026-08-08 against the artifact this was actually verified
            // with: `onnx/model.onnx` (641,517,466 bytes) at revision
            // 516f4baf13dec4ddddda8631e019b5737c8bc250 of
            // `jinaai/jina-embeddings-v2-base-code`. weights.rs I16 says an
            // unpinned artifact in a shipping config is a bug; leaving this
            // `None` is what let an unexamined file become canonical.
            sha256: Some([
                0x63, 0x36, 0x3f, 0xc1, 0x78, 0x42, 0x8b, 0x74, 0x62, 0x0c, 0x6f, 0x37, 0x80, 0xcb,
                0xc7, 0x19, 0x18, 0x83, 0xfa, 0x5c, 0x7f, 0x84, 0xc0, 0x94, 0x5c, 0x45, 0xeb, 0x5c,
                0x42, 0x56, 0x73, 0x3b,
            ]),
            quantization: Quantization::Float32,
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
        assert_eq!(hint.file, "model.onnx");
        assert!(hint.sha256.is_some(), "the canonical artifact is pinned");
        assert!(VoyageCode3::weights_hint().is_none(), "voyage is API-only");
    }

    /// Any artifact a brand names as canonical must be batch-invariant.
    ///
    /// This is the assertion that would have caught the 2026-08-08 defect at
    /// compile-test time instead of after a live run: the brand pointed at a
    /// dynamically quantized artifact while the runtime hardcoded
    /// `durable_canonical: true`. Stated as a property over the whole catalog
    /// rather than about Jina specifically, so a future brand cannot reintroduce
    /// it — a `DynamicInt8` hint is a compile-time-visible contradiction with
    /// persisting the vectors it produces.
    #[test]
    fn no_brand_may_declare_a_batch_dependent_artifact_canonical() {
        for hint in [JinaCodeV2::weights_hint(), VoyageCode3::weights_hint()]
            .into_iter()
            .flatten()
        {
            assert!(
                hint.quantization.is_batch_invariant(),
                "{} is named as a brand's canonical weights artifact but is \
                 {:?}, whose activation scale is refit per batch. Vectors from \
                 it depend on what they were batched with, so they cannot be \
                 persisted or compared across planes (I12). Either ship a \
                 statically-quantized/float artifact, or stop calling the \
                 runtime that loads it durable-canonical.",
                hint.file,
                hint.quantization,
            );
        }
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
