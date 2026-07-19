//! Local embedding runtime for the vector-search plane.
//!
//! This crate is the *compute* half of the vector architecture: [`vector-core`]
//! defines the frozen contracts (`Embedder`, `Embedding<M>`, `embed_key`,
//! `VectorStore`); this crate provides the machinery that honors them:
//!
//! | Module | Role |
//! |---|---|
//! | [`weights`] | Pinned-artifact policy: locate + sha256-verify `model_quantized.onnx` (09c I11/I16). No network code — a missing artifact degrades to "semantic search disabled". |
//! | [`gate`] | Concurrency cap (one ORT session) + idle-unload/lazy-reload of the loaded embedder (09c §1.6 — embedder RSS is budgeted separately from store RSS, and must be 0 when idle). |
//! | [`scheduler`] | Priority queue (Interactive > OpenFile > Background) with ~75 ms batch coalescing, cancellation, and bounded backpressure (09c §4.3 threading model). |
//! | [`stage`] | `EmbedStage` — the core write path (09b §4.1/§16): SymbolDelta-only, layered cutoffs (L1 stage traces, L2 vector CAS), exactly one infer + one upsert for a one-symbol doc edit (§16.4). |
//! | [`mock`] | Deterministic offline `MockEmbedder` for tests. |
//! | [`runtime`] *(feature `onnx`)* | `FastembedOrt` — fastembed 5.17.3 over ort 2.0.0-rc.12, CPU execution provider only (durable-canonical, 09c I11), batch hard-capped at 32 (I16). |
//! | [`tokens`] *(feature `onnx`)* | `HfTokenCounter` over the model's `tokenizer.json` (HF `tokenizers`). |
//!
//! Non-negotiable invariants carried here (09c §0.3):
//!
//! - **I8/I11 — CPU-canonical.** Durable vectors are produced on the CPU EP with
//!   the single sha-pinned int8 ONNX artifact. This crate registers *no*
//!   accelerated execution providers.
//! - **I16 — pinned parameters.** fastembed's default batch of 256 is overridden
//!   to 32 everywhere; an unpinned default is a bug.
//! - **09b §16.1 — delta-only.** `EmbedStage` never walks a corpus; unchanged
//!   symbols never enter the embedder.

pub mod gate;
pub mod mock;
pub mod scheduler;
pub mod stage;
pub mod weights;

#[cfg(feature = "onnx")]
pub mod runtime;
#[cfg(feature = "onnx")]
pub mod tokens;

use vector_core::model::EmbeddingModel;
use vector_core::{EmbedError, Embedding};

/// The frozen batch hard cap for local embedding (09c I16, 09b §3.1b).
///
/// fastembed's `embed(texts, batch_size)` defaults to 256 when passed `None`;
/// long code windows at that batch size OOM. Every call site in this crate
/// passes `Some(MAX_BATCH)` / caps coalesced batches at this value.
pub const MAX_BATCH: usize = 32;

/// The frozen recipe token budget (09b §3.1b): header+sig ≤ 512, doc ≤ 256,
/// body takes the remainder.
pub const MAX_SEQ_LEN: usize = 1024;

/// The ORT package identity folded into `tool_digest` (09c I12).
/// Single source of truth; runtime.rs uses this rather than its own copy.
pub const ORT_PACKAGE_ID: &str = "ort@2.0.0-rc.12";

/// Construct a backend/runtime [`EmbedError`] from a message.
///
/// Centralized so the (sibling-crate) error surface is a one-line change if
/// `vector-core` names the variant differently.
pub(crate) fn backend_error(msg: impl std::fmt::Display) -> EmbedError {
	EmbedError::Backend(msg.to_string())
}

/// The raw float view of an embedding (for CAS blobs).
///
/// Centralized for the same reason as [`backend_error`]: `Embedding<M>` is a
/// sibling-crate type; this is the single point of coupling to its accessor.
pub(crate) fn embedding_floats<M: EmbeddingModel>(embedding: &Embedding<M>) -> &[f32] {
	embedding.as_ref()
}
