//! Compatibility names for the worker's persisted embedding runtime interface.
//!
//! The config codec, artifact validation, and BEM1/BEC1 activation live in
//! `backend_engine::application` so locald and the worker share one production
//! implementation without a dependency from local-service to the worker app.

pub use backend_engine::application::{
    EmbeddingRuntimeError as WorkerEmbeddingError,
    EmbeddingRuntimeInstall as WorkerEmbeddingInstall,
    EmbeddingRuntimeLimits as WorkerEmbeddingLimits,
    EmbeddingRuntimeProvision as WorkerEmbeddingProvision,
    EmbeddingRuntimeStatus as WorkerEmbeddingStatus,
    EmbeddingRuntimeSummary as WorkerEmbeddingSummary, EmbeddingUnavailableReason,
    embedding_runtime_resident_credit_bytes as configured_resident_credit_bytes,
    inspect_embedding_runtime as inspect_worker_embedding_runtime,
    install_embedding_runtime as install_worker_embedding_runtime,
    remove_embedding_runtime as remove_worker_embedding_runtime,
};
