//! Local embedding runtime for the vector-search plane.
//!
//! | Module | Role |
//! |---|---|
//! | [`weights`] | Pinned-artifact policy: locate + sha256-verify `model.onnx` (09c I11/I16). |
//! | [`gate`] | Concurrency cap + idle-unload/lazy-reload of the loaded embedder (09c §1.6). |
//! | [`scheduler`] | Priority queue with ~75 ms batch coalescing, cancellation, and bounded backpressure (09c §4.3). |
//! | [`stage`] | `EmbedStage` — the core write path (09b §4.1/§16). |
//! | [`mock`] | Deterministic offline `MockEmbedder` for tests. |
//! | [`runtime`] *(feature `onnx`)* | `FastembedOrt` — fastembed 5.17.3 over ort 2.0.0-rc.12. |
//! | [`tokens`] *(feature `onnx`)* | `HfTokenCounter` over the model's `tokenizer.json`. |

pub mod gate;
pub mod mock;
pub mod scheduler;
pub mod stage;
pub mod weights;

#[cfg(feature = "onnx")]
pub mod runtime;
#[cfg(feature = "onnx")]
pub mod tokens;

use crate::vector::core::model::EmbeddingModel;
use crate::vector::core::{EmbedError, Embedding};

/// The frozen batch hard cap for local embedding (09c I16, 09b §3.1b).
pub const MAX_BATCH: usize = 32;

/// The frozen recipe token budget (09b §3.1b).
pub const MAX_SEQ_LEN: usize = 1024;

/// The ORT package identity folded into `tool_digest` (09c I12).
pub const ORT_PACKAGE_ID: &str = "ort@2.0.0-rc.12";

/// Construct a backend/runtime [`EmbedError`] from a message.
pub(crate) fn backend_error(msg: impl std::fmt::Display) -> EmbedError {
    EmbedError::Backend(msg.to_string())
}

/// The raw float view of an embedding (for CAS blobs).
pub(crate) fn embedding_floats<M: EmbeddingModel>(embedding: &Embedding<M>) -> &[f32] {
    embedding.as_ref()
}
