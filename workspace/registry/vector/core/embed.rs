//! The embedder interface and its runtime self-description.
//!
//! This is the P0 trait fix flagged in 09-vector §20: the role axis
//! ([`EmbedRole`]) is threaded through `embed`, because asymmetric models
//! embed queries and documents differently (09b §3.1b) — collapsing them
//! silently degrades retrieval.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use super::{
    embedding::{EmbedError, Embedding},
    model::{EmbeddingModel, ModelId},
};

/// Which side of the retrieval asymmetry a text is embedded as (09b §3.1b).
/// The same string under `Query` vs `Document` yields *different* vectors on
/// asymmetric models — never interchange them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbedRole {
    /// A user query (search-time).
    Query,
    /// A corpus document (index-time).
    Document,
}

/// What a text is embedded *as* — routes to the named vector family. Distinct
/// from [`EmbedRole`]: purpose picks the vector name, role picks the model
/// prompt side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbeddingPurpose {
    /// The code/API surface of a symbol.
    Code,
    /// The documentation/prose around it.
    Documentation,
}

/// The acceleration backend an ORT session ended up on (09c §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccelKind {
    Cpu,
    CoreMl,
    Cuda,
    DirectMl,
    /// A provider not in the known set (recorded, never trusted as canonical).
    Other,
}

/// An embedder's self-description, surfaced for diagnostics and for the tool
/// digest (I12: model + runtime identity is part of vector provenance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedRuntimeInfo {
    /// The model this runtime serves (matches the brand's `id()`).
    pub model_id: ModelId,

    /// The execution provider actually selected.
    pub accel: AccelKind,

    /// Whether this runtime's output is a function of its input alone: only
    /// durable-canonical outputs may be persisted and shared across planes
    /// (I12); non-canonical accel is query-side only.
    ///
    /// Two independent things can break it, and both must be checked:
    ///
    /// * a non-CPU execution provider, which may reassociate float operations;
    /// * a **dynamically quantized** artifact, whose activation scale is refit
    ///   from each batch's own value range — so the same text embedded next to
    ///   different neighbours yields a different vector. See
    ///   [`crate::vector::core::model::Quantization::is_batch_invariant`].
    ///
    /// The second was live in this repo until 2026-08-08: this field was set to
    /// a literal `true` while the brand pointed at `model_quantized.onnx`, and
    /// the deviation was ~0.015 per component — 400× the float32 noise floor.
    /// Nothing failed, because nothing compared two batchings of the same text.
    /// Implementors should therefore **derive** this from the loaded artifact
    /// rather than assert it.
    pub durable_canonical: bool,

    /// Max texts per `embed_batch` call.
    pub max_batch: usize,

    /// Max input tokens per text before the runtime truncates.
    pub max_seq_len: usize,

    /// SHA-256 of the loaded weights, when pinned (I12).
    pub weights_sha256: Option<[u8; 32]>,

    /// The exact ORT package identity (name+version+build), part of the tool
    /// digest (I12).
    pub ort_package_id: SmolStr,
}

/// Anything that turns text into a branded embedding. Fallible and batched;
/// the brand ties output to exactly one store family (I11).
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// The model brand this embedder produces vectors for.
    type Model: EmbeddingModel;

    /// Embed one text under `role` (09b §3.1b: role is never implicit).
    async fn embed(
        &self,
        text: &str,
        role: EmbedRole,
    ) -> Result<Embedding<Self::Model>, EmbedError>;

    /// Embed a batch in one round-trip, positionally aligned with `texts`;
    /// failure is all-or-nothing.
    async fn embed_batch(
        &self,
        texts: &[&str],
        role: EmbedRole,
    ) -> Result<Vec<Embedding<Self::Model>>, EmbedError>;

    /// The runtime's self-description (accel, canonicality, limits).
    fn runtime(&self) -> EmbedRuntimeInfo;
}
