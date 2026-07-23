//! The vector plane — folded in from the former standalone `vector` crate.
//!
//! # Modules
//! - [`core`]: always-on pure shared logic (model brands, embeddings, the
//!   `VectorStore` / `Embedder` traits, quantization, routing, admission,
//!   RRF fusion, recipes).
//! - [`local`] *(feature `local`)*: qdrant-edge embedded store plane.
//! - [`embed`] *(feature `embed`/`onnx`)*: fastembed/ort CPU-canonical runtime.
//! - [`remote`] *(feature `remote`, on by default)*: qdrant-client + Voyage/rerank plane.
//!
//! Plus the two *serving* concerns that were never part of the pure plane:
//! - [`gate::SemanticGate`] — the capability token gating the expensive
//!   semantic-search path.
//! - [`cache::EmbeddingCache`] — the content-addressed query/document embedding
//!   cache in front of the (network) embedder.

pub mod core;

#[cfg(feature = "local")]
pub mod local;

#[cfg(feature = "remote")]
pub mod remote;

#[cfg(feature = "embed")]
pub mod embed;

pub mod cache;
pub mod gate;

pub use cache::{EmbeddingCache, EmbeddingKey};
pub use gate::SemanticGate;

// Re-export the pure vector vocabulary from `core`, so consumers keep saying
// `registry::vector::X` for both the serving pieces above and the core essentials.
pub use self::core::*;
