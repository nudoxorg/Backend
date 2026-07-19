//! The remote (index) plane of the dual local/remote vector-search
//! architecture.
//!
//! - [`store`] — [`RemoteStore<M>`]: `VectorStore<M>` over Qdrant.
//! - [`voyage`] — [`VoyageEmbedder`]: `Embedder<VoyageCode3>` via the Voyage
//!   REST API with fully-pinned request parameters (I16: `input_type` is never
//!   left as a default).
//! - [`rerank`] — [`Reranker`] trait + [`HttpReranker`] + [`VoyageReranker`].
//! - [`hedged`] — [`hedged`]: race local and remote futures, merge by RRF when
//!   the remote lands (R6), label every hit by [`SourceTag`], and never
//!   silently narrow results on remote timeout (I14).

pub mod hedged;
pub mod rerank;
pub mod store;
pub mod voyage;

pub use hedged::{HedgedOutcome, hedged};
pub use rerank::{HttpReranker, RerankDoc, RerankError, RerankScore, Reranker, VoyageReranker};
pub use store::{CollectionConfig, RemoteStore};
pub use voyage::VoyageEmbedder;
