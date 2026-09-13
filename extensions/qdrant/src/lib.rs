//! Bounded vector-search extension contracts.
//!
//! The built-in bounded HTTP provider and custom [`CandidateSource`] implementations feed an
//! adapter that validates each page against the exact
//! workspace, input root, recipe, read manifest, and coverage selected by the
//! caller. Provider IDs never cross this boundary; only logical candidate
//! identities are admitted.
//!
//! Every admitted page and result carries one [`Binding`], a complete
//! [`backend_version::CoverageWitness`], and an explicit [`SearchQuality`].
//! Approximate pages retain their recipe, model, and recall metadata through
//! reranking; this optional leaf owns no workspace head, snapshot lifecycle,
//! or compare-and-set authority.
#![forbid(unsafe_code)]

mod admission;
mod contracts;
mod delta;
mod http;
mod identity;
mod incremental;
mod provider;

pub use admission::{
    AdapterError, Candidates, Error, Reranked, accept_remote, accept_remote_approximate,
    accept_remote_with_limits, accept_remote_with_quality, incomplete_coverage, rerank,
};
pub use contracts::{
    ApproximationMetadata, CandidatePage, CandidateSource, Cursor, RecallMetadata, SearchQuality,
    SearchRequest, SearchResult,
};
pub use delta::{CandidateChange, CandidateDelta, CandidateState};
pub use http::{
    ApiKey, HttpProviderError, QdrantHttpClient, QdrantHttpConfig, QdrantHttpSource,
    QdrantMutationReceipt,
};
pub use identity::{
    Authority, AuthoritySchema, Binding, CandidateId, CandidateRelation, Frontier, FrontierSchema,
    Limits, ModelSchema, ModelVersion, QuerySchema, QueryVersion, ReadManifest, ReadManifestSchema,
    Recipe, RecipeSchema, Root, SchemaVersion, TokenizerSchema, TokenizerVersion, Tombstones,
    TreatmentSchema, TreatmentVersion,
};
pub use incremental::{
    AnnBase, AnnCursor, AnnPage, AnnSource, DocumentVector, EmbeddingEncoding,
    EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, ExactOverlay, Metric, OverlayLimits,
    QueryVector, RefreshKind, RefreshOutcome, RefreshPlan, ScoredCandidate, VectorFacts,
    VectorIndex, VectorPoint, VectorQuery, VectorQueryBinding, VectorSearchRequest,
    VectorSearchResult,
};
pub use provider::{Adapter, MemorySource};

#[cfg(test)]
mod tests;
