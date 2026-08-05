//! The pure shared plane of the dual local/remote vector-search architecture.
//!
//! Everything here is deterministic, I/O-free logic shared verbatim by the
//! local (edge) plane and the remote (index) plane: model brands, the frozen
//! EmbedText v2 recipe, embed-key derivation, store/embedder interfaces,
//! RRF fusion, the §20.5 routing table, §20.4 hot-set admission, the §17.3
//! quantization ladder, and edgepack shard identity.
//!
//! Authoritative specs: `.research/librarification/09-vector/PLAN.md`,
//! `09b-retrieval-pipeline-plan.md`, `09c-embeddings-runtime-adversarial.md`.
//! Invariants are cited on items as `I1`–`I16` (09b §23).

pub mod admission;
pub mod embed;
pub mod embedding;
pub mod fusion;
pub mod key;
pub mod model;
pub mod quant;
pub mod recipe;
pub mod routing;
pub mod shard;
pub mod store;

pub use admission::{
    AdmissionBudget, AdmissionOutcome, DepCandidate, HitEma, admit, eviction_victim,
    ram_estimate_bytes,
};
pub use embed::{AccelKind, EmbedRole, EmbedRuntimeInfo, Embedder, EmbeddingPurpose};
pub use embedding::{EmbedError, Embedding, l2_normalize};
pub use fusion::{FusedHit, RRF_K, RankedList, rrf_fuse};
pub use key::{ChangedSymbol, SymbolDelta, SymbolPartHashes, embed_key, tool_digest};
pub use model::{
    CANONICAL_WEIGHTS_FILE, EmbeddingModel, JinaCodeV2, Metric, ModelId, VoyageCode3,
    WeightsArtifact,
    license::{FORBIDDEN_MODEL_IDS, LicenseError, assert_licensed},
};
pub use quant::{HnswParams, QP1, QuantProfile, RescorePolicy, project_ladder, search_ef};
pub use recipe::{
    ApproxTokenCounter, EmbedFacets, EmbedFacetsBuf, EmbedText, RECIPE_ID, TokenCounter,
    VectorName, build_embed_text,
};
pub use routing::{
    DEEP_STAGE1_TOP_K, DenseTarget, QualityMode, QueryScope, RerankTier, RouteInputs, RouteLabel,
    RoutePlan, Stage2, plan_route,
};
pub use shard::{EDGE_FORMAT_VERSION, EdgepackKey, ShardSchema};
pub use store::{
    FilterClause, NAMESPACE_NUDOX, Payload, PayloadValue, PointId, SearchFilter, SearchHit,
    SearchRequest, SourceTag, StoreCapabilities, StoreError, VectorPoint, VectorStore,
};
