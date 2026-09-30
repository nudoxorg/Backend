//! Optional production Qdrant configuration and activation.
//!
//! Configuration is pure and bounded. Network I/O occurs only in
//! [`ConfiguredQdrant::activate`], after the embedding owner supplies exact
//! document coordinates. A missing or invalid optional configuration remains
//! an observable unavailable state and never prevents local lexical queries.

use super::embedding_cache::EmbeddingCacheFile;
use super::projection_state::ProjectionState;
use super::{LocalAnswer, QueryCoordinator, QueryResult, SemanticAcceleration, SemanticDocument};
use backend_compile::{
    EmbeddingArtifact, EmbeddingBatchProtocol, EmbeddingExecutable, EmbeddingExecutableError,
    EmbeddingInputIdentity, EmbeddingInvocation, EmbeddingNormalization, EmbeddingPurpose,
    EmbeddingRuntimeSpecV1, MAX_EMBEDDING_BATCH_ITEMS, ProcessEnvironment, ProcessLimits,
    ToolchainArtifact,
};
use backend_engine::RowId;
use backend_extension_qdrant as qdrant;
use backend_library::{SemanticSearchReason, SemanticSearchStatus};
use backend_version::{CoverageWitness, RelationState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::mem::size_of;
use std::num::{NonZeroU8, NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Qdrant endpoint environment variable.
pub const QDRANT_ENDPOINT_ENV: &str = "BACKEND_QDRANT_ENDPOINT";
/// Qdrant collection environment variable.
pub const QDRANT_COLLECTION_ENV: &str = "BACKEND_QDRANT_COLLECTION";
/// Optional Qdrant API-key environment variable.
pub const QDRANT_API_KEY_ENV: &str = "BACKEND_QDRANT_API_KEY";
/// Immutable embedding-model revision label.
pub const EMBEDDING_MODEL_ENV: &str = "BACKEND_EMBEDDING_MODEL";
/// Immutable tokenizer revision label.
pub const EMBEDDING_TOKENIZER_ENV: &str = "BACKEND_EMBEDDING_TOKENIZER";
/// Absolute executable implementing the bounded embedding protocol.
pub const EMBEDDING_PROGRAM_ENV: &str = "BACKEND_EMBEDDING_PROGRAM";
/// Immutable model artifact bytes consumed by the embedding executable.
pub const EMBEDDING_MODEL_FILE_ENV: &str = "BACKEND_EMBEDDING_MODEL_FILE";
/// Immutable tokenizer artifact bytes consumed by the embedding executable.
pub const EMBEDDING_TOKENIZER_FILE_ENV: &str = "BACKEND_EMBEDDING_TOKENIZER_FILE";
/// Exact embedding dimension.
pub const EMBEDDING_DIMENSIONS_ENV: &str = "BACKEND_EMBEDDING_DIMENSIONS";
/// Query treatment/prefix revision label.
pub const EMBEDDING_QUERY_TREATMENT_ENV: &str = "BACKEND_EMBEDDING_QUERY_TREATMENT";
/// Document treatment/prefix revision label.
pub const EMBEDDING_DOCUMENT_TREATMENT_ENV: &str = "BACKEND_EMBEDDING_DOCUMENT_TREATMENT";
/// Embedding worker protocol: `json-v1` (compatibility default) or shared supervised `bem2`.
pub const EMBEDDING_PROTOCOL_ENV: &str = "BACKEND_EMBEDDING_PROTOCOL";
/// Device requested from BEM2 workers: `metal` or portable `cpu`.
pub const EMBEDDING_DEVICE_ENV: &str = "BACKEND_EMBEDDING_DEVICE";

const MAX_CONFIG_VALUE_BYTES: usize = 4096;
const MAX_EMBEDDING_DIMENSIONS: u32 = 16_384;
const MAX_PROGRAM_BYTES: usize = 64 * 1024 * 1024;
const MAX_MODEL_BYTES: usize = 256 * 1024 * 1024;
const MAX_TOKENIZER_BYTES: usize = 64 * 1024 * 1024;
const MAX_EMBEDDING_TEXT_BYTES: usize = 64 * 1024;
const MAX_EMBEDDING_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_REMOTE_SEMANTIC_DOCUMENTS: usize = 65_536;
const MAX_VECTOR_FACT_BYTES: usize = 512 * 1024 * 1024;
const EMBEDDING_PROTOCOL_ABI: u16 = 1;
const EMBEDDING_BATCH_PROTOCOL_ABI: u16 = 2;
const EMBEDDING_DEADLINE: Duration = Duration::from_secs(5);
// The environment-configured Qdrant producer is separate from the compiler's persisted
// EmbeddingRuntimeLimits. Apply a fixed local-worker policy: bounded workspace/output, a short
// wall/CPU deadline, and supported OS process/address-space limits. Outside 64-bit Linux the
// process API cannot enforce a memory limit; on non-Unix platforms it cannot enforce CPU/process
// limits, so the configured local executable remains trusted code within the portable I/O, wall,
// and workspace bounds.
const EMBEDDING_WORKSPACE_GROWTH_BYTES: usize = 256 * 1024 * 1024;
#[cfg(unix)]
const EMBEDDING_PROCESS_COUNT_LIMIT: usize = 256;
#[cfg(unix)]
const EMBEDDING_CPU_TIME_LIMIT: Duration = EMBEDDING_DEADLINE;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
const EMBEDDING_ADDRESS_SPACE_LIMIT_BYTES: usize = 8 * 1024 * 1024 * 1024;
const PRODUCER_RETRY_DELAY: Duration = Duration::from_secs(5);

fn embedding_capability_recipe(
    recipe: qdrant::EmbeddingRecipe,
) -> backend_engine::EmbeddingCapabilityRecipe {
    backend_engine::EmbeddingCapabilityRecipe::new(
        *recipe.model.as_bytes(),
        *recipe.tokenizer.as_bytes(),
        backend_engine::EmbeddingSource::SemanticEvidence,
        Some(MAX_EMBEDDING_TEXT_BYTES as u32),
        None,
        0,
        recipe.dimensions.get(),
        match recipe.metric {
            qdrant::Metric::CosineDistance => backend_engine::EmbeddingMetric::Cosine,
            qdrant::Metric::NegativeDot => backend_engine::EmbeddingMetric::Dot,
            qdrant::Metric::EuclideanSquared => backend_engine::EmbeddingMetric::Euclidean,
        },
        match recipe.pooling {
            qdrant::EmbeddingPooling::ClassificationToken => backend_engine::EmbeddingPooling::Cls,
            qdrant::EmbeddingPooling::Mean => backend_engine::EmbeddingPooling::Mean,
            qdrant::EmbeddingPooling::LastToken => backend_engine::EmbeddingPooling::LastToken,
        },
        match recipe.normalization {
            qdrant::EmbeddingNormalization::None => backend_engine::EmbeddingNormalization::None,
            qdrant::EmbeddingNormalization::UnitL2 => {
                backend_engine::EmbeddingNormalization::UnitL2
            }
        },
        match recipe.encoding {
            qdrant::EmbeddingEncoding::Float32 => backend_engine::EmbeddingEncoding::Float32,
            qdrant::EmbeddingEncoding::SignedInt8 => backend_engine::EmbeddingEncoding::Signed8,
        },
        *recipe.query_treatment.as_bytes(),
        *recipe.document_treatment.as_bytes(),
    )
}

/// Non-blocking state of the optional remote semantic provider.
pub enum RemoteSemantic {
    /// No endpoint was configured.
    Unconfigured,
    /// Configuration was present and admitted; no network claim is implied.
    Configured(Box<ConfiguredQdrant>),
    /// Optional configuration was malformed and local search remains usable.
    Unavailable(Box<RemoteConfigError>),
}

impl RemoteSemantic {
    /// Reads and validates the optional production configuration without
    /// opening a socket or delaying local service startup.
    #[must_use]
    pub fn from_environment() -> Self {
        match ConfiguredQdrant::from_environment() {
            Ok(Some(configured)) => Self::Configured(Box::new(configured)),
            Ok(None) => Self::Unconfigured,
            Err(error) => Self::Unavailable(Box::new(error)),
        }
    }

    /// Whether this deployment named no embedding endpoint at all.
    ///
    /// An absent endpoint is terminal for the lifetime of the process: the
    /// environment is read once at startup and no retry can supply one. Lane
    /// coverage therefore reports it as an unavailable lane rather than as a
    /// fraction that would tell a surface to keep waiting. A malformed
    /// configuration is deliberately excluded here; the capability inventory
    /// reports that separately as a dependency fault.
    #[must_use]
    pub const fn is_unconfigured(&self) -> bool {
        matches!(self, Self::Unconfigured)
    }

    /// Configured provider, when every required input was admitted.
    #[must_use]
    pub fn configured(&self) -> Option<&ConfiguredQdrant> {
        match self {
            Self::Configured(configured) => Some(configured.as_ref()),
            Self::Unconfigured | Self::Unavailable(_) => None,
        }
    }

    /// Exact embedding recipe identity, when configured.
    #[must_use]
    pub fn recipe(&self) -> Option<qdrant::Recipe> {
        self.configured()
            .map(|configured| configured.recipe.version())
    }

    /// Reconciles the optional semantic projection with one owner-selected
    /// complete view.  Provider or model failures are retained by the
    /// configured owner and leave lexical search available to callers.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration or provider failure while preserving the
    /// last admitted active projection.
    pub fn reconcile(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
    ) -> Result<(), RemoteConfigError> {
        match self {
            Self::Configured(configured) => {
                if !configured.producer_health.can_attempt(Instant::now()) {
                    return Ok(());
                }
                let result = configured.reconcile(coordinator, coverage);
                if result.is_ok() {
                    configured.producer_health = ProducerHealth::Ready;
                } else {
                    configured.producer_health = ProducerHealth::retry_after(
                        backend_engine::CapabilityUnavailable::ProbeFailed,
                    );
                }
                result
            }
            Self::Unconfigured | Self::Unavailable(_) => Ok(()),
        }
    }

    /// Searches the semantic projection after the complete local lexical
    /// answer has been built.  Any stale-root, provider, or model failure
    /// returns the unchanged lexical answer with an unavailable semantic lane.
    #[must_use]
    pub fn search(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        local: LocalAnswer,
        text: &str,
    ) -> QueryResult {
        match self {
            Self::Configured(configured) => configured.search(coordinator, coverage, local, text),
            Self::Unconfigured | Self::Unavailable(_) => local.finish(),
        }
    }

    /// Searches while retaining a per-query semantic-lane status beside the
    /// complete lexical result.
    #[must_use]
    pub fn search_with_status(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        local: LocalAnswer,
        text: &str,
        reconciliation_failed: bool,
    ) -> (QueryResult, SemanticSearchStatus) {
        match self {
            Self::Unconfigured => (
                local.finish(),
                SemanticSearchStatus::Unavailable {
                    reason: SemanticSearchReason::Unconfigured,
                },
            ),
            Self::Unavailable(_) => (
                local.finish(),
                SemanticSearchStatus::Unavailable {
                    reason: SemanticSearchReason::InvalidConfiguration,
                },
            ),
            Self::Configured(configured) => {
                let active_matches = configured.active.as_ref().is_some_and(|active| {
                    !configured.projection_uncertain
                        && active.matches(coordinator)
                        && active.coverage() == coverage
                });
                let fallback = if configured.producer.is_none() {
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::ModelUnavailable,
                    }
                } else if configured.active.is_some() && !active_matches {
                    SemanticSearchStatus::Stale {
                        reason: SemanticSearchReason::StaleProjection,
                    }
                } else if reconciliation_failed
                    || configured.producer_health.failure().is_some()
                    || active_matches
                {
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::ProviderUnavailable,
                    }
                } else {
                    SemanticSearchStatus::Unavailable {
                        reason: SemanticSearchReason::NoActiveProjection,
                    }
                };
                let result = configured.search(coordinator, coverage, local, text);
                let status = semantic_status_from_result(&result).unwrap_or(fallback);
                (result, status)
            }
        }
    }

    /// Replaces the baseline embedding slot with this process's honest
    /// configured state while preserving every independently owned status.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteConfigError::InvalidInventory`] if replacement would
    /// violate the inventory's bounded, sorted, unique representation.
    pub fn inventory(
        &self,
        baseline: &backend_engine::CapabilityInventory,
    ) -> Result<backend_engine::CapabilityInventory, RemoteConfigError> {
        let mut rows = baseline
            .as_slice()
            .iter()
            .copied()
            .filter(|status| {
                !matches!(
                    status.family(),
                    backend_engine::CapabilityFamily::Embedding { .. }
                )
            })
            .collect::<Vec<_>>();
        match self {
            Self::Configured(configured) => {
                let capability_recipe = embedding_capability_recipe(configured.recipe);
                let recipe = capability_recipe.identity();
                let id = backend_engine::CapabilityId::new(recipe.as_bytes());
                let family = backend_engine::CapabilityFamily::Embedding {
                    recipe: Some(recipe),
                };
                let status = match (
                    configured.producer.as_ref(),
                    configured.producer_health.failure(),
                    configured.active.as_ref(),
                ) {
                    (_, Some(reason), _) => {
                        backend_engine::CapabilityStatus::unavailable(id, family, reason)
                    }
                    (Some(producer), None, Some(_)) => producer.inventory_status(
                        id,
                        family,
                        configured.recipe,
                        backend_engine::CapabilityLifecycle::Ready,
                    ),
                    (Some(producer), None, None) => producer.inventory_status(
                        id,
                        family,
                        configured.recipe,
                        backend_engine::CapabilityLifecycle::Installed,
                    ),
                    (None, None, _) => backend_engine::CapabilityStatus::unavailable(
                        id,
                        family,
                        backend_engine::CapabilityUnavailable::MissingDependency,
                    ),
                };
                rows.push(status);
            }
            Self::Unconfigured => rows.push(backend_engine::CapabilityStatus::unavailable(
                backend_engine::CapabilityId::new(*blake3::hash(b"embedding/default").as_bytes()),
                backend_engine::CapabilityFamily::Embedding { recipe: None },
                backend_engine::CapabilityUnavailable::NoManifest,
            )),
            Self::Unavailable(_) => rows.push(backend_engine::CapabilityStatus::unavailable(
                backend_engine::CapabilityId::new(
                    *blake3::hash(b"embedding/invalid-config").as_bytes(),
                ),
                backend_engine::CapabilityFamily::Embedding { recipe: None },
                backend_engine::CapabilityUnavailable::MissingDependency,
            )),
        }
        rows.sort_unstable_by_key(backend_engine::CapabilityStatus::id);
        backend_engine::CapabilityInventory::try_new(rows)
            .map_err(|_| RemoteConfigError::InvalidInventory)
    }
}

fn semantic_status_from_result(result: &QueryResult) -> Option<SemanticSearchStatus> {
    let semantic = result
        .lanes
        .iter()
        .rev()
        .find(|lane| lane.lane == super::Lane::Semantic)?;
    match (semantic.coverage, semantic.freshness) {
        (super::CoverageBasis::CandidateSubset { .. }, super::Freshness::Current) => {
            Some(SemanticSearchStatus::Available)
        }
        (super::CoverageBasis::Unavailable, super::Freshness::Stale) => {
            Some(SemanticSearchStatus::Stale {
                reason: SemanticSearchReason::StaleProjection,
            })
        }
        _ => None,
    }
}

impl fmt::Debug for RemoteSemantic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unconfigured => formatter.write_str("RemoteSemantic::Unconfigured"),
            Self::Configured(configured) => formatter
                .debug_tuple("RemoteSemantic::Configured")
                .field(configured)
                .finish(),
            Self::Unavailable(error) => formatter
                .debug_tuple("RemoteSemantic::Unavailable")
                .field(error)
                .finish(),
        }
    }
}

/// Admitted Qdrant transport and complete embedding recipe before activation.
pub struct ConfiguredQdrant {
    client: qdrant::QdrantHttpClient,
    recipe: qdrant::EmbeddingRecipe,
    projection_scope: [u8; 32],
    producer: Option<EmbeddingProducer>,
    producer_health: ProducerHealth,
    document_embeddings: BTreeMap<DocumentEmbeddingId, Arc<[f32]>>,
    embedding_cache: Option<EmbeddingCacheFile>,
    projection_state: Option<ProjectionState>,
    projection_uncertain: bool,
    active_embedding_identities: BTreeMap<String, DocumentEmbeddingId>,
    active: Option<ActiveQdrant>,
    retired: Option<RetiredQdrant>,
}

/// Resource envelope derived from the configured coordinate representation.
///
/// Keeping this separate from Qdrant's provider-page defaults prevents a
/// transport paging bound from accidentally becoming a corpus-size or vector-
/// dimension bound.
#[derive(Clone, Copy)]
struct ProjectionEnvelope(qdrant::Limits);

impl ProjectionEnvelope {
    fn for_recipe(recipe: qdrant::EmbeddingRecipe) -> Result<Self, RemoteConfigError> {
        let dimensions = usize::try_from(recipe.dimensions.get())
            .map_err(|_| RemoteConfigError::ProjectionLimit)?;
        let payload_bytes = dimensions
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(size_of::<u32>()))
            .ok_or(RemoteConfigError::ProjectionLimit)?;
        let memory_bounded_documents = MAX_VECTOR_FACT_BYTES / payload_bytes;
        let max_candidates = MAX_REMOTE_SEMANTIC_DOCUMENTS.min(memory_bounded_documents);
        let limits = qdrant::Limits {
            max_candidates,
            max_tombstones: max_candidates,
            max_payload_bytes: payload_bytes,
            max_total_payload_bytes: MAX_VECTOR_FACT_BYTES,
            max_page: qdrant::Limits::default().max_page.min(max_candidates),
        };
        limits
            .validate()
            .map(Self)
            .map_err(|_| RemoteConfigError::ProjectionLimit)
    }

    fn admit(self, documents: usize) -> Result<AdmittedProjectionEnvelope, RemoteConfigError> {
        if documents > self.0.max_candidates {
            return Err(RemoteConfigError::ProjectionLimit);
        }
        Ok(AdmittedProjectionEnvelope(self.0))
    }
}

/// Proof that a complete vector projection fits its dimension-aware envelope.
#[derive(Clone, Copy)]
struct AdmittedProjectionEnvelope(qdrant::Limits);

impl ConfiguredQdrant {
    fn from_environment() -> Result<Option<Self>, RemoteConfigError> {
        let Some(endpoint) = optional(QDRANT_ENDPOINT_ENV)? else {
            return Ok(None);
        };
        let collection = required(QDRANT_COLLECTION_ENV)?;
        let projection_scope = qdrant_projection_scope(&endpoint, &collection);
        let model_revision = required(EMBEDDING_MODEL_ENV)?;
        let tokenizer_revision = required(EMBEDDING_TOKENIZER_ENV)?;
        let dimensions = NonZeroU32::new(parse::<u32>(EMBEDDING_DIMENSIONS_ENV)?)
            .ok_or(RemoteConfigError::InvalidValue(EMBEDDING_DIMENSIONS_ENV))?;
        if dimensions.get() > MAX_EMBEDDING_DIMENSIONS {
            return Err(RemoteConfigError::InvalidValue(EMBEDDING_DIMENSIONS_ENV));
        }
        let query_treatment = required(EMBEDDING_QUERY_TREATMENT_ENV)?;
        let document_treatment = required(EMBEDDING_DOCUMENT_TREATMENT_ENV)?;
        let api_key = optional(QDRANT_API_KEY_ENV)?
            .map(qdrant::ApiKey::new)
            .transpose()
            .map_err(RemoteConfigError::Provider)?;
        let attempts =
            NonZeroU8::new(2).ok_or(RemoteConfigError::InvalidValue(QDRANT_ENDPOINT_ENV))?;
        let mut producer = EmbeddingProducer::from_environment(dimensions)?;
        EmbeddingProducer::verify_revision(
            EMBEDDING_MODEL_ENV,
            &model_revision,
            producer.model_identity,
        )?;
        EmbeddingProducer::verify_revision(
            EMBEDDING_TOKENIZER_ENV,
            &tokenizer_revision,
            producer.tokenizer_identity,
        )?;
        let recipe = qdrant::EmbeddingRecipe {
            model: qdrant::ModelVersion::from_value(producer.model_bytes()),
            tokenizer: qdrant::TokenizerVersion::from_value(producer.tokenizer_bytes()),
            dimensions,
            metric: qdrant::Metric::CosineDistance,
            pooling: qdrant::EmbeddingPooling::Mean,
            normalization: qdrant::EmbeddingNormalization::UnitL2,
            encoding: qdrant::EmbeddingEncoding::Float32,
            query_treatment: qdrant::TreatmentVersion::from_value(query_treatment.as_bytes()),
            document_treatment: qdrant::TreatmentVersion::from_value(document_treatment.as_bytes()),
        };
        producer.activate_batch_runtime(recipe)?;
        let transport = qdrant::QdrantHttpConfig {
            endpoint,
            collection,
            api_key,
            connect_deadline: Duration::from_secs(2),
            read_deadline: Duration::from_secs(5),
            attempts,
            max_response_bytes: 4 * 1024 * 1024,
            max_request_bytes: 8 * 1024 * 1024,
            max_batch_points: qdrant::Limits::default().max_page,
        };
        let client = qdrant::QdrantHttpClient::new(transport, recipe)
            .map_err(RemoteConfigError::Provider)?;
        let embedding_cache =
            EmbeddingCacheFile::open(*recipe.version().as_bytes(), dimensions.get());
        Ok(Some(Self {
            client,
            recipe,
            projection_scope,
            producer: Some(producer),
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        }))
    }

    /// Exact configured embedding recipe.
    #[must_use]
    pub const fn recipe(&self) -> qdrant::EmbeddingRecipe {
        self.recipe
    }

    /// Creates or validates the collection, writes exact document vectors,
    /// and verifies complete cardinality before returning an active source.
    ///
    /// # Errors
    ///
    /// Returns a typed [`RemoteConfigError`] when owner coverage, row
    /// identity, vector admission, transport, or projection verification fails.
    pub fn activate(
        &self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        documents: Vec<QdrantDocument>,
    ) -> Result<ActiveQdrant, RemoteConfigError> {
        if !matches!(coverage, CoverageWitness::Complete(_)) {
            return Err(RemoteConfigError::IncompleteCoverage);
        }
        let envelope = ProjectionEnvelope::for_recipe(self.recipe)?.admit(documents.len())?;
        let writes = documents
            .iter()
            .map(|document| document.write)
            .collect::<Vec<_>>();
        let rows = documents
            .iter()
            .map(|document| document.row)
            .collect::<Vec<_>>();
        let row_keys = rows.iter().map(|row| row.stable_key()).collect::<Vec<_>>();
        let vectors = self.admit_documents(coordinator, documents)?;
        let (binding, facts) =
            vector_facts(coordinator, coverage, self.recipe, &vectors, envelope)?;
        let mut residences = Vec::with_capacity(rows.len());
        let mut seen = BTreeSet::new();
        for row in rows {
            let residence = qdrant::PointResidence::for_row(
                coordinator.corpus.workspace,
                binding.recipe,
                &row.stable_key(),
            )
            .map_err(RemoteConfigError::Provider)?;
            if !seen.insert(residence) {
                return Err(RemoteConfigError::StaleRow);
            }
            residences.push(residence);
        }
        let resident = residences
            .iter()
            .zip(vectors.iter())
            .zip(writes)
            .map(|((residence, document), write)| qdrant::ResidentDocument {
                residence: *residence,
                write,
                document,
            })
            .collect::<Vec<_>>();
        let base = qdrant::AnnBase::from_facts(&facts, qdrant::SearchQuality::Exact, envelope.0)
            .map_err(RemoteConfigError::Extension)?;
        self.client
            .ensure_collection()
            .map_err(RemoteConfigError::Provider)?;
        self.client
            .upsert_resident(binding, &resident)
            .map_err(RemoteConfigError::Provider)?;
        let source = self
            .client
            .clone()
            .verify_projection(binding, coverage, vectors.len())
            .map_err(RemoteConfigError::Provider)?;
        let index =
            qdrant::VectorIndex::new(base, facts, source).map_err(RemoteConfigError::Extension)?;
        Ok(ActiveQdrant {
            recipe: self.recipe,
            index,
            residences: residences.into_boxed_slice(),
            row_keys: row_keys.into_boxed_slice(),
        })
    }

    fn reconcile(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
    ) -> Result<(), RemoteConfigError> {
        let workspace = coordinator.workspace_root();
        let recipe = self.recipe.version();
        if self
            .projection_state
            .as_ref()
            .is_none_or(|state| state.workspace() != workspace || state.recipe() != recipe)
        {
            self.projection_state = ProjectionState::open(workspace, recipe, self.projection_scope);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !self.projection_uncertain && active.matches(coordinator))
        {
            return self.retire_pending();
        }
        // A failed retirement is retried before another generation can be
        // admitted. This keeps cleanup debt bounded to one fully identified
        // projection even while the remote service is unhealthy.
        self.retire_pending()?;
        ProjectionEnvelope::for_recipe(self.recipe)?
            .admit(coordinator.semantic_document_count())?;
        let (documents, target_inputs) =
            self.embed_documents_with_input_identities(coordinator.semantic_documents())?;
        let target_rows = documents
            .iter()
            .map(|document| document.row.stable_key())
            .collect::<Vec<_>>();
        if let Some(state) = self.projection_state.as_mut() {
            state
                .stage(&target_rows)
                .map_err(RemoteConfigError::ProjectionStateIo)?;
        }
        // A failed upsert can leave a mixed remote projection. Keep the last
        // active source unavailable until a later complete verification.
        self.projection_uncertain = true;
        let replacement = self.activate(coordinator, coverage, documents)?;
        if let Some(previous) = self.active.replace(replacement) {
            debug_assert!(self.retired.is_none());
            self.retired = Some(previous.into_retired());
        }
        self.active_embedding_identities = target_inputs;
        self.projection_uncertain = false;
        self.retire_pending()
    }

    fn retire_pending(&mut self) -> Result<(), RemoteConfigError> {
        let live = self
            .active
            .as_ref()
            .map(|active| active.residences.iter().copied().collect::<BTreeSet<_>>());
        if let Some(retired) = self.retired.as_ref() {
            let stale = retired
                .residences
                .iter()
                .copied()
                .filter(|residence| live.as_ref().is_none_or(|live| !live.contains(residence)))
                .collect::<Vec<_>>();
            if !stale.is_empty() {
                self.client
                    .delete_residences(retired.workspace, retired.recipe, &stale)
                    .map_err(RemoteConfigError::Provider)?;
            }
            self.retired = None;
        }

        let active_state = self.active.as_ref().map(|active| {
            let binding = active.index.binding();
            (
                binding.workspace,
                binding.recipe,
                active.residences.iter().copied().collect::<BTreeSet<_>>(),
                active.row_keys.to_vec(),
            )
        });
        if let (Some(state), Some((workspace, recipe, live, row_keys))) =
            (self.projection_state.as_mut(), active_state)
            && state.workspace() == workspace
            && state.recipe() == recipe
        {
            let stale = state
                .residences()
                .map_err(RemoteConfigError::Provider)?
                .into_iter()
                .filter(|residence| !live.contains(residence))
                .collect::<Vec<_>>();
            if !stale.is_empty() {
                self.client
                    .delete_residences(workspace, recipe, &stale)
                    .map_err(RemoteConfigError::Provider)?;
            }
            state
                .commit(&row_keys)
                .map_err(RemoteConfigError::ProjectionStateIo)?;
        }
        Ok(())
    }

    fn embed_documents(
        &mut self,
        documents: &[SemanticDocument],
    ) -> Result<Vec<QdrantDocument>, RemoteConfigError> {
        self.embed_documents_with_input_identities(documents)
            .map(|(documents, _)| documents)
    }

    fn embed_documents_with_input_identities(
        &mut self,
        documents: &[SemanticDocument],
    ) -> Result<(Vec<QdrantDocument>, BTreeMap<String, DocumentEmbeddingId>), RemoteConfigError>
    {
        let producer = self
            .producer
            .as_ref()
            .ok_or(RemoteConfigError::ProducerUnavailable)?;
        let planned = documents
            .iter()
            .map(|document| {
                let identity = producer.input_identity(
                    self.recipe.version(),
                    EmbeddingTreatment::Document,
                    &document.text,
                );
                (document, DocumentEmbeddingId(identity.as_bytes()))
            })
            .collect::<Vec<_>>();
        let projection_uncertain = self.projection_uncertain;
        let active_inputs = &self.active_embedding_identities;
        let target_inputs = planned
            .iter()
            .map(|(document, identity)| (document.row.stable_key(), *identity))
            .collect::<BTreeMap<_, _>>();
        let live = planned
            .iter()
            .map(|(_, identity)| *identity)
            .collect::<BTreeSet<_>>();
        // The previous view may be large. Drop its dead entries before staging
        // new misses so retained cache vectors plus pending vectors remain
        // within the admitted live projection's byte envelope.
        self.document_embeddings
            .retain(|identity, _| live.contains(identity));
        if let Some(cache) = self.embedding_cache.as_mut() {
            for (_, identity) in &planned {
                if !self.document_embeddings.contains_key(identity)
                    && let Some(coordinates) = cache.load(identity.0)
                {
                    self.document_embeddings.insert(*identity, coordinates);
                }
            }
        }
        let mut misses = Vec::new();
        let mut requested = BTreeSet::new();
        for (document, identity) in &planned {
            if !self.document_embeddings.contains_key(identity) && requested.insert(*identity) {
                misses.push((*identity, document.text.as_str()));
            }
        }
        let mut pending = BTreeMap::new();
        if !misses.is_empty() {
            let texts = misses.iter().map(|(_, text)| *text).collect::<Vec<_>>();
            let coordinates = producer.embed_many(EmbeddingTreatment::Document, &texts)?;
            if coordinates.len() != misses.len() {
                return Err(RemoteConfigError::ProducerProtocol);
            }
            pending.extend(
                misses
                    .into_iter()
                    .zip(coordinates)
                    .map(|((identity, _), coordinates)| (identity, coordinates)),
            );
        }
        let mut embedded = Vec::with_capacity(planned.len());
        for (document, identity) in planned {
            let coordinates = self
                .document_embeddings
                .get(&identity)
                .or_else(|| pending.get(&identity))
                .cloned()
                .ok_or(RemoteConfigError::ProducerProtocol)?;
            let write = coordinate_write_for_input(
                active_inputs,
                document.row,
                identity,
                projection_uncertain,
            );
            embedded.push(QdrantDocument {
                row: document.row,
                coordinates,
                write,
            });
        }
        if let Some(cache) = self.embedding_cache.as_mut() {
            let entries = pending
                .iter()
                .map(|(identity, coordinates)| (identity.0, coordinates.as_ref()))
                .collect::<Vec<_>>();
            let _ = cache.store_batch(&entries);
        }
        self.document_embeddings.extend(pending);
        Ok((embedded, target_inputs))
    }

    fn search(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        local: LocalAnswer,
        text: &str,
    ) -> QueryResult {
        let Some(producer) = self.producer.as_ref() else {
            return local.finish();
        };
        if !self.producer_health.can_attempt(Instant::now()) {
            return local.finish();
        }
        let Some(active) = self.active.as_ref().filter(|active| {
            !self.projection_uncertain
                && active.matches(coordinator)
                && active.coverage() == coverage
        }) else {
            return local.finish();
        };
        let Ok(coordinates) = producer.embed(EmbeddingTreatment::Query, text) else {
            self.producer_health =
                ProducerHealth::retry_after(backend_engine::CapabilityUnavailable::ProbeFailed);
            return local.finish();
        };
        active.accelerate(local, coordinates)
    }

    fn admit_documents(
        &self,
        coordinator: &QueryCoordinator,
        documents: Vec<QdrantDocument>,
    ) -> Result<Vec<qdrant::DocumentVector>, RemoteConfigError> {
        documents
            .into_iter()
            .map(|document| {
                let id = coordinator
                    .semantic_candidate(document.row)
                    .ok_or(RemoteConfigError::StaleRow)?;
                qdrant::DocumentVector::from_shared(self.recipe, id, document.coordinates)
                    .map_err(RemoteConfigError::Extension)
            })
            .collect()
    }
}

fn vector_facts(
    coordinator: &QueryCoordinator,
    coverage: CoverageWitness,
    recipe: qdrant::EmbeddingRecipe,
    vectors: &[qdrant::DocumentVector],
    envelope: AdmittedProjectionEnvelope,
) -> Result<(qdrant::Binding, qdrant::VectorFacts), RemoteConfigError> {
    let entries = vectors
        .iter()
        .map(|vector| (vector.point().id().0, vector.point().to_payload()))
        .collect::<Vec<_>>();
    let relation =
        RelationState::<qdrant::CandidateRelation>::from_entries(entries.clone(), coverage)
            .map_err(|_| RemoteConfigError::InvalidRelation)?;
    let binding = coordinator.semantic_binding(relation.root(), recipe.version());
    let candidates = entries
        .into_iter()
        .map(|(id, payload)| (qdrant::CandidateId(id), payload))
        .collect();
    let state = qdrant::CandidateState::new(binding, coverage, candidates, envelope.0)
        .map_err(RemoteConfigError::Extension)?;
    let facts =
        qdrant::VectorFacts::from_recipe(state, recipe).map_err(RemoteConfigError::Extension)?;
    Ok((binding, facts))
}

fn qdrant_projection_scope(endpoint: &str, collection: &str) -> [u8; 32] {
    let mut hasher =
        blake3::Hasher::new_derive_key("backend.local-service.qdrant-projection-scope.v1");
    for value in [endpoint.as_bytes(), collection.as_bytes()] {
        let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
        hasher.update(&length.to_be_bytes());
        hasher.update(value);
    }
    *hasher.finalize().as_bytes()
}

impl fmt::Debug for ConfiguredQdrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredQdrant")
            .field("recipe", &self.recipe)
            .field("producer", &self.producer)
            .field("active", &self.active.is_some())
            .finish_non_exhaustive()
    }
}

type ProducerFailure = backend_engine::CapabilityUnavailable;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DocumentEmbeddingId([u8; 32]);

impl DocumentEmbeddingId {
    fn new(
        recipe: qdrant::Recipe,
        producer_manifest: [u8; 32],
        text: &str,
    ) -> Result<Self, RemoteConfigError> {
        let mut configuration =
            blake3::Hasher::new_derive_key("backend.local-service.embedding-configuration.v1");
        configuration.update(recipe.as_bytes());
        configuration.update(&producer_manifest);
        let identity = EmbeddingInputIdentity::for_configuration(
            *configuration.finalize().as_bytes(),
            EmbeddingInvocation {
                purpose: EmbeddingPurpose::Document,
                text,
            },
        );
        Ok(Self(identity.as_bytes()))
    }
}

fn coordinate_write_for_input(
    active_inputs: &BTreeMap<String, DocumentEmbeddingId>,
    row: RowId,
    identity: DocumentEmbeddingId,
    projection_uncertain: bool,
) -> qdrant::CoordinateWrite {
    if !projection_uncertain && active_inputs.get(&row.stable_key()) == Some(&identity) {
        qdrant::CoordinateWrite::Hold
    } else {
        qdrant::CoordinateWrite::Replace
    }
}

#[derive(Clone, Copy, Debug)]
enum ProducerHealth {
    Ready,
    RetryAfter {
        at: Instant,
        reason: ProducerFailure,
    },
}

impl ProducerHealth {
    fn retry_after(reason: ProducerFailure) -> Self {
        Self::RetryAfter {
            at: Instant::now() + PRODUCER_RETRY_DELAY,
            reason,
        }
    }

    fn can_attempt(self, now: Instant) -> bool {
        match self {
            Self::Ready => true,
            Self::RetryAfter { at, .. } => now >= at,
        }
    }

    const fn failure(self) -> Option<ProducerFailure> {
        match self {
            Self::Ready => None,
            Self::RetryAfter { reason, .. } => Some(reason),
        }
    }
}

#[derive(Debug)]
struct EmbeddingProducer {
    program: PathBuf,
    model: PathBuf,
    tokenizer: PathBuf,
    program_identity: [u8; 32],
    model_identity: [u8; 32],
    tokenizer_identity: [u8; 32],
    dimensions: NonZeroU32,
    manifest: [u8; 32],
    protocol: EmbeddingProducerProtocol,
    batch_runtime: Option<EmbeddingExecutable>,
    _batch_workspace: Option<EmbeddingRuntimeWorkspace>,
}

struct EmbeddingRuntimeWorkspace {
    path: PathBuf,
}

impl fmt::Debug for EmbeddingRuntimeWorkspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EmbeddingRuntimeWorkspace(<private>)")
    }
}

static EMBEDDING_WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl EmbeddingRuntimeWorkspace {
    fn create() -> Result<Self, RemoteConfigError> {
        for _ in 0..32 {
            let sequence = EMBEDDING_WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| RemoteConfigError::ProducerProtocol)?
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-qdrant-embedding-{}-{stamp}-{sequence}",
                std::process::id(),
            ));
            #[cfg(unix)]
            let result = {
                use std::os::unix::fs::DirBuilderExt as _;

                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700).create(&path)
            };
            #[cfg(not(unix))]
            let result = fs::create_dir(&path);
            match result {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(RemoteConfigError::ProducerIo(error)),
            }
        }
        Err(RemoteConfigError::ProducerProtocol)
    }
}

impl Drop for EmbeddingRuntimeWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmbeddingProducerProtocol {
    JsonV1,
    Bem2,
}

impl EmbeddingProducerProtocol {
    const fn abi(self) -> u16 {
        match self {
            Self::JsonV1 => EMBEDDING_PROTOCOL_ABI,
            Self::Bem2 => EMBEDDING_BATCH_PROTOCOL_ABI,
        }
    }
}

impl EmbeddingProducer {
    fn from_environment(dimensions: NonZeroU32) -> Result<Self, RemoteConfigError> {
        let program = absolute_file(EMBEDDING_PROGRAM_ENV, MAX_PROGRAM_BYTES)?;
        let model = absolute_file(EMBEDDING_MODEL_FILE_ENV, MAX_MODEL_BYTES)?;
        let tokenizer = absolute_file(EMBEDDING_TOKENIZER_FILE_ENV, MAX_TOKENIZER_BYTES)?;
        let model_bytes = read_bounded(&model, MAX_MODEL_BYTES, EMBEDDING_MODEL_FILE_ENV)?;
        let tokenizer_bytes = read_bounded(
            &tokenizer,
            MAX_TOKENIZER_BYTES,
            EMBEDDING_TOKENIZER_FILE_ENV,
        )?;
        let program_bytes = read_bounded(&program, MAX_PROGRAM_BYTES, EMBEDDING_PROGRAM_ENV)?;
        let protocol = match optional(EMBEDDING_PROTOCOL_ENV)?.as_deref() {
            None | Some("json-v1") => EmbeddingProducerProtocol::JsonV1,
            Some("bem2") => EmbeddingProducerProtocol::Bem2,
            Some(_) => return Err(RemoteConfigError::InvalidValue(EMBEDDING_PROTOCOL_ENV)),
        };
        let protocol_abi = protocol.abi();
        let program_identity = *blake3::hash(&program_bytes).as_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.embedding.producer.v1\0");
        hasher.update(&program_bytes);
        hasher.update(&model_bytes);
        hasher.update(&tokenizer_bytes);
        hasher.update(&protocol_abi.to_be_bytes());
        let manifest = *hasher.finalize().as_bytes();
        let model_identity = *blake3::hash(&model_bytes).as_bytes();
        let tokenizer_identity = *blake3::hash(&tokenizer_bytes).as_bytes();
        Ok(Self {
            program,
            model,
            tokenizer,
            program_identity,
            model_identity,
            tokenizer_identity,
            dimensions,
            manifest,
            protocol,
            batch_runtime: None,
            _batch_workspace: None,
        })
    }

    fn activate_batch_runtime(
        &mut self,
        recipe: qdrant::EmbeddingRecipe,
    ) -> Result<(), RemoteConfigError> {
        if self.protocol != EmbeddingProducerProtocol::Bem2 {
            return Ok(());
        }
        self.verify_artifacts()?;
        let program = ToolchainArtifact::from_path(&self.program, Vec::new())
            .map_err(|_| RemoteConfigError::ProducerProtocol)?;
        let model_bytes = read_bounded(&self.model, MAX_MODEL_BYTES, EMBEDDING_MODEL_FILE_ENV)?;
        let tokenizer_bytes = read_bounded(
            &self.tokenizer,
            MAX_TOKENIZER_BYTES,
            EMBEDDING_TOKENIZER_FILE_ENV,
        )?;
        let model = EmbeddingArtifact::new(Arc::from(model_bytes));
        let tokenizer = EmbeddingArtifact::new(Arc::from(tokenizer_bytes));
        if model.identity().as_bytes() != self.model_identity
            || tokenizer.identity().as_bytes() != self.tokenizer_identity
        {
            return Err(RemoteConfigError::ArtifactChanged);
        }
        let dimensions = u16::try_from(self.dimensions.get())
            .ok()
            .and_then(NonZeroU16::new)
            .ok_or(RemoteConfigError::InvalidValue(EMBEDDING_DIMENSIONS_ENV))?;
        let mut options = blake3::Hasher::new_derive_key("backend.qdrant.embedding-options.v1");
        options.update(recipe.version().as_bytes());
        options.update(&self.manifest);
        let maximum_text = u32::try_from(MAX_EMBEDDING_TEXT_BYTES)
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .ok_or(RemoteConfigError::InvalidValue(EMBEDDING_DIMENSIONS_ENV))?;
        let normalization = match recipe.normalization {
            qdrant::EmbeddingNormalization::None => EmbeddingNormalization::None,
            qdrant::EmbeddingNormalization::UnitL2 => EmbeddingNormalization::L2,
        };
        let spec = EmbeddingRuntimeSpecV1::new(
            model.identity().as_bytes(),
            self.model_identity,
            tokenizer.identity().as_bytes(),
            program.identity().to_bytes(),
            dimensions,
            normalization,
            maximum_text,
            *options.finalize().as_bytes(),
        );
        let maximum_batch_output = MAX_EMBEDDING_BATCH_ITEMS
            .checked_mul(
                usize::from(dimensions.get())
                    .checked_mul(size_of::<f32>())
                    .and_then(|bytes| bytes.checked_add(32))
                    .ok_or(RemoteConfigError::ProjectionLimit)?,
            )
            .and_then(|bytes| bytes.checked_add(10))
            .ok_or(RemoteConfigError::ProjectionLimit)?;
        let maximum_batch_input = MAX_EMBEDDING_BATCH_ITEMS
            .checked_mul(MAX_EMBEDDING_TEXT_BYTES + 36)
            .and_then(|bytes| bytes.checked_add(108))
            .ok_or(RemoteConfigError::ProjectionLimit)?;
        let resource_bound = 20 * 1024 * 1024;
        if maximum_batch_output > resource_bound || maximum_batch_input > resource_bound {
            return Err(RemoteConfigError::ProjectionLimit);
        }
        let limits = bounded_embedding_process_limits(resource_bound)?;
        let device = selected_embedding_device()?;
        let environment = ProcessEnvironment::new(vec![
            ("BACKEND_EMBEDDING_DEVICE".into(), device.into()),
            ("LC_ALL".into(), "C".into()),
        ])
        .map_err(|_| RemoteConfigError::ProducerProtocol)?;
        let workspace = EmbeddingRuntimeWorkspace::create()?;
        let runtime = EmbeddingExecutable::activate_with_spec(
            spec,
            self.program.clone(),
            Vec::new(),
            workspace.path.clone(),
            environment,
            limits,
            program,
            model,
            tokenizer,
        )
        .map_err(RemoteConfigError::ProducerRuntime)?;
        if runtime.batch_protocol() != EmbeddingBatchProtocol::BatchV2 {
            return Err(RemoteConfigError::ProducerProtocol);
        }
        self.verify_artifacts()?;
        self.batch_runtime = Some(runtime);
        self._batch_workspace = Some(workspace);
        Ok(())
    }

    fn model_bytes(&self) -> &[u8; 32] {
        &self.model_identity
    }

    fn input_identity(
        &self,
        recipe: qdrant::Recipe,
        treatment: EmbeddingTreatment,
        text: &str,
    ) -> EmbeddingInputIdentity {
        let purpose = match treatment {
            EmbeddingTreatment::Query => EmbeddingPurpose::Query,
            EmbeddingTreatment::Document => EmbeddingPurpose::Document,
        };
        let invocation = EmbeddingInvocation { purpose, text };
        if let Some(runtime) = self.batch_runtime.as_ref() {
            return EmbeddingInputIdentity::new(runtime.execution_identity(), invocation);
        }
        let mut configuration =
            blake3::Hasher::new_derive_key("backend.local-service.embedding-configuration.v1");
        configuration.update(recipe.as_bytes());
        configuration.update(&self.manifest);
        EmbeddingInputIdentity::for_configuration(*configuration.finalize().as_bytes(), invocation)
    }

    fn tokenizer_bytes(&self) -> &[u8; 32] {
        &self.tokenizer_identity
    }

    fn verify_revision(
        name: &'static str,
        declared: &str,
        identity: [u8; 32],
    ) -> Result<(), RemoteConfigError> {
        let expected = format!("blake3:{}", hexadecimal(identity));
        if declared != expected {
            return Err(RemoteConfigError::ArtifactRevision { name });
        }
        Ok(())
    }

    fn verify_artifacts(&self) -> Result<(), RemoteConfigError> {
        verify_artifact(&self.program, MAX_PROGRAM_BYTES, self.program_identity)?;
        verify_artifact(&self.model, MAX_MODEL_BYTES, self.model_identity)?;
        verify_artifact(
            &self.tokenizer,
            MAX_TOKENIZER_BYTES,
            self.tokenizer_identity,
        )
    }

    fn inventory_status(
        &self,
        id: backend_engine::CapabilityId,
        family: backend_engine::CapabilityFamily,
        recipe: qdrant::EmbeddingRecipe,
        lifecycle: backend_engine::CapabilityLifecycle,
    ) -> backend_engine::CapabilityStatus {
        let capability_recipe = embedding_capability_recipe(recipe);
        backend_engine::CapabilityStatus::observed(
            id,
            family,
            self.manifest,
            backend_engine::CapabilityTarget::Native {
                os: target_os(),
                architecture: target_architecture(),
            },
            self.protocol.abi(),
            backend_engine::CapabilityAuthority::Embedding(capability_recipe),
            lifecycle,
        )
    }

    fn embed(
        &self,
        treatment: EmbeddingTreatment,
        text: &str,
    ) -> Result<Vec<f32>, RemoteConfigError> {
        self.embed_many(treatment, &[text])?
            .into_iter()
            .next()
            .map(|coordinates| coordinates.as_ref().to_vec())
            .ok_or(RemoteConfigError::ProducerProtocol)
    }

    fn embed_many(
        &self,
        treatment: EmbeddingTreatment,
        texts: &[&str],
    ) -> Result<Vec<Arc<[f32]>>, RemoteConfigError> {
        if texts
            .iter()
            .any(|text| text.is_empty() || text.len() > MAX_EMBEDDING_TEXT_BYTES)
        {
            return Err(RemoteConfigError::EmbeddingInput);
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.verify_artifacts()?;
        let coordinates = if let Some(runtime) = self.batch_runtime.as_ref() {
            let purpose = match treatment {
                EmbeddingTreatment::Query => EmbeddingPurpose::Query,
                EmbeddingTreatment::Document => EmbeddingPurpose::Document,
            };
            runtime
                .infer_batch(purpose, texts)
                .map_err(RemoteConfigError::ProducerRuntime)?
                .into_iter()
                .map(|coordinates| coordinates.shared_values())
                .collect::<Vec<_>>()
        } else {
            texts
                .iter()
                .map(|text| self.embed_json(treatment, text).map(Arc::from))
                .collect::<Result<Vec<_>, _>>()?
        };
        self.verify_artifacts()?;
        Ok(coordinates)
    }

    fn embed_json(
        &self,
        treatment: EmbeddingTreatment,
        text: &str,
    ) -> Result<Vec<f32>, RemoteConfigError> {
        if text.is_empty() || text.len() > MAX_EMBEDDING_TEXT_BYTES {
            return Err(RemoteConfigError::EmbeddingInput);
        }
        let mut child = Command::new(&self.program)
            .env_clear()
            .env("LC_ALL", "C")
            .env(EMBEDDING_MODEL_FILE_ENV, &self.model)
            .env(EMBEDDING_TOKENIZER_FILE_ENV, &self.tokenizer)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(RemoteConfigError::ProducerIo)?;
        let request = serde_json::to_vec(&EmbeddingRequest {
            abi: EMBEDDING_PROTOCOL_ABI,
            treatment,
            text,
        })
        .map_err(RemoteConfigError::ProducerJson)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or(RemoteConfigError::ProducerProtocol)?;
        stdin
            .write_all(&request)
            .map_err(RemoteConfigError::ProducerIo)?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .ok_or(RemoteConfigError::ProducerProtocol)?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_EMBEDDING_RESPONSE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + EMBEDDING_DEADLINE;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(RemoteConfigError::ProducerIo)? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RemoteConfigError::ProducerDeadline);
            }
            thread::sleep(Duration::from_millis(2));
        };
        let bytes = reader
            .join()
            .map_err(|_| RemoteConfigError::ProducerProtocol)?
            .map_err(RemoteConfigError::ProducerIo)?;
        if !status.success() || bytes.len() as u64 > MAX_EMBEDDING_RESPONSE_BYTES {
            return Err(RemoteConfigError::ProducerProtocol);
        }
        let response: EmbeddingResponse =
            serde_json::from_slice(&bytes).map_err(RemoteConfigError::ProducerJson)?;
        if response.abi != EMBEDDING_PROTOCOL_ABI
            || response.dimensions != self.dimensions.get()
            || response.values.len() != self.dimensions.get() as usize
            || response.values.iter().any(|value| !value.is_finite())
        {
            return Err(RemoteConfigError::ProducerProtocol);
        }
        Ok(response.values)
    }
}

fn bounded_embedding_process_limits(
    resource_bound: usize,
) -> Result<ProcessLimits, RemoteConfigError> {
    let limits = ProcessLimits::new(
        resource_bound,
        64 * 1024,
        EMBEDDING_DEADLINE,
        resource_bound,
    )
    .and_then(|limits| limits.with_input_bytes_limit(resource_bound))
    .and_then(|limits| limits.with_workspace_limit(EMBEDDING_WORKSPACE_GROWTH_BYTES))
    .map_err(|_| RemoteConfigError::ProducerProtocol)?;
    #[cfg(unix)]
    let limits = limits
        .with_process_count_limit(EMBEDDING_PROCESS_COUNT_LIMIT)
        .and_then(|limits| limits.with_cpu_time_limit(EMBEDDING_CPU_TIME_LIMIT))
        .map_err(|_| RemoteConfigError::ProducerProtocol)?;
    #[cfg(not(unix))]
    let limits = limits;
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    let limits = limits
        .with_memory_bytes_limit(EMBEDDING_ADDRESS_SPACE_LIMIT_BYTES)
        .map_err(|_| RemoteConfigError::ProducerProtocol)?;
    #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
    let limits = limits;
    Ok(limits)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
fn test_bounded_embedding_process_limits() {
    let limits = bounded_embedding_process_limits(20 * 1024 * 1024)
        .expect("construct bounded embedding worker limits");
    assert_eq!(limits.input_bytes(), 20 * 1024 * 1024);
    assert_eq!(limits.output_bytes(), 20 * 1024 * 1024);
    assert_eq!(
        limits.workspace_limit(),
        Some(EMBEDDING_WORKSPACE_GROWTH_BYTES)
    );
    #[cfg(unix)]
    {
        assert_eq!(
            limits.process_count_limit(),
            Some(EMBEDDING_PROCESS_COUNT_LIMIT)
        );
        assert_eq!(limits.cpu_time_limit(), Some(EMBEDDING_CPU_TIME_LIMIT));
    }
    #[cfg(not(unix))]
    {
        assert_eq!(limits.process_count_limit(), None);
        assert_eq!(limits.cpu_time_limit(), None);
    }
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    assert_eq!(
        limits.memory_bytes_limit(),
        Some(EMBEDDING_ADDRESS_SPACE_LIMIT_BYTES)
    );
    #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
    assert_eq!(limits.memory_bytes_limit(), None);
    assert_eq!(limits.unsupported_limit(), None);
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum EmbeddingTreatment {
    Query,
    Document,
}

#[derive(Serialize)]
struct EmbeddingRequest<'text> {
    abi: u16,
    treatment: EmbeddingTreatment,
    text: &'text str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingResponse {
    abi: u16,
    dimensions: u32,
    values: Vec<f32>,
}

fn absolute_file(name: &'static str, max_bytes: usize) -> Result<PathBuf, RemoteConfigError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(RemoteConfigError::InvalidValue(name));
    }
    let metadata = fs::metadata(&path).map_err(|_| RemoteConfigError::InvalidValue(name))?;
    if !metadata.is_file() || metadata.len() > max_bytes as u64 {
        return Err(RemoteConfigError::InvalidValue(name));
    }
    Ok(path)
}

fn read_bounded(
    path: &Path,
    max_bytes: usize,
    name: &'static str,
) -> Result<Vec<u8>, RemoteConfigError> {
    let bytes = fs::read(path).map_err(|_| RemoteConfigError::InvalidValue(name))?;
    if bytes.is_empty() || bytes.len() > max_bytes {
        return Err(RemoteConfigError::InvalidValue(name));
    }
    Ok(bytes)
}

fn verify_artifact(
    path: &Path,
    max_bytes: usize,
    expected: [u8; 32],
) -> Result<(), RemoteConfigError> {
    let bytes = fs::read(path).map_err(RemoteConfigError::ProducerIo)?;
    if bytes.is_empty() || bytes.len() > max_bytes || blake3::hash(&bytes).as_bytes() != &expected {
        return Err(RemoteConfigError::ArtifactChanged);
    }
    Ok(())
}

fn hexadecimal(bytes: [u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

const fn target_os() -> u8 {
    if cfg!(target_os = "macos") {
        1
    } else if cfg!(target_os = "linux") {
        2
    } else if cfg!(target_os = "windows") {
        3
    } else {
        255
    }
}

const fn target_architecture() -> u8 {
    if cfg!(target_arch = "aarch64") {
        1
    } else if cfg!(target_arch = "x86_64") {
        2
    } else {
        255
    }
}

/// One canonical row and its document-side embedding.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantDocument {
    /// Row selected from the coordinator's exact view.
    pub row: RowId,
    /// Finite coordinates produced under the configured document treatment.
    pub coordinates: Arc<[f32]>,
    /// Whether these coordinates may replace a vector already stored for the row.
    pub write: qdrant::CoordinateWrite,
}

/// Verified live Qdrant source paired with exact local vector facts.
pub struct ActiveQdrant {
    recipe: qdrant::EmbeddingRecipe,
    index: qdrant::VectorIndex<qdrant::QdrantHttpSource>,
    residences: Box<[qdrant::PointResidence]>,
    row_keys: Box<[String]>,
}

impl ActiveQdrant {
    fn matches(&self, coordinator: &QueryCoordinator) -> bool {
        coordinator.semantic_binding_matches(&self.index.binding())
    }

    fn coverage(&self) -> CoverageWitness {
        self.index.facts().coverage()
    }

    fn into_retired(self) -> RetiredQdrant {
        let binding = self.index.binding();
        RetiredQdrant {
            workspace: binding.workspace,
            recipe: binding.recipe,
            residences: self.residences,
        }
    }

    /// Accelerates an already complete local answer using a query-side
    /// embedding. Failures remain an unavailable semantic lane.
    #[must_use]
    pub fn accelerate(&self, local: LocalAnswer, coordinates: Vec<f32>) -> QueryResult {
        let Ok(query) = qdrant::QueryVector::new(self.recipe, coordinates) else {
            return local.finish();
        };
        local.accelerate(SemanticAcceleration {
            index: &self.index,
            query: &query,
        })
    }

    /// Exact active embedding recipe.
    #[must_use]
    pub const fn recipe(&self) -> qdrant::EmbeddingRecipe {
        self.recipe
    }
}

struct RetiredQdrant {
    workspace: backend_version::WorkspaceRoot,
    recipe: qdrant::Recipe,
    residences: Box<[qdrant::PointResidence]>,
}

fn optional(name: &'static str) -> Result<Option<String>, RemoteConfigError> {
    let value = match std::env::var(name) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(RemoteConfigError::InvalidValue(name));
        }
    };
    if value.is_empty() || value.len() > MAX_CONFIG_VALUE_BYTES {
        return Err(RemoteConfigError::InvalidValue(name));
    }
    Ok(Some(value))
}

fn required(name: &'static str) -> Result<String, RemoteConfigError> {
    optional(name)?.ok_or(RemoteConfigError::MissingValue(name))
}

fn selected_embedding_device() -> Result<&'static str, RemoteConfigError> {
    match optional(EMBEDDING_DEVICE_ENV)?.as_deref() {
        Some("cpu") => Ok("cpu"),
        Some("metal") if cfg!(target_os = "macos") => Ok("metal"),
        None if cfg!(target_os = "macos") => Ok("metal"),
        None => Ok("cpu"),
        Some(_) => Err(RemoteConfigError::InvalidValue(EMBEDDING_DEVICE_ENV)),
    }
}

fn parse<T: std::str::FromStr>(name: &'static str) -> Result<T, RemoteConfigError> {
    required(name)?
        .parse()
        .map_err(|_| RemoteConfigError::InvalidValue(name))
}

/// Optional semantic configuration or activation failure.
#[derive(Debug)]
pub enum RemoteConfigError {
    /// A required environment value was absent.
    MissingValue(&'static str),
    /// An environment value was empty, oversized, non-Unicode, or malformed.
    InvalidValue(&'static str),
    /// A declared immutable artifact revision does not match its bytes.
    ArtifactRevision {
        /// Environment field containing the rejected revision.
        name: &'static str,
    },
    /// Installed producer artifacts changed around one execution.
    ArtifactChanged,
    /// A document row does not belong to the selected view.
    StaleRow,
    /// Projection activation requires owner-authorized complete coverage.
    IncompleteCoverage,
    /// Durable membership could not be committed around the projection mutation.
    ProjectionStateIo(std::io::Error),
    /// Candidate relation construction failed.
    InvalidRelation,
    /// Capability inventory replacement violated its bounded canonical form.
    InvalidInventory,
    /// The supplied projection exceeds its dimension-aware count or memory bound.
    ProjectionLimit,
    /// A configured producer was not installed.
    ProducerUnavailable,
    /// Embedding input violates the producer's bounded contract.
    EmbeddingInput,
    /// The embedding process exceeded its deadline.
    ProducerDeadline,
    /// The embedding process violated its ABI or output bounds.
    ProducerProtocol,
    /// The shared supervised embedding runtime failed its activation or batch protocol.
    ProducerRuntime(EmbeddingExecutableError),
    /// The embedding process could not be started, written, read, or waited.
    ProducerIo(std::io::Error),
    /// The embedding process exchanged malformed JSON.
    ProducerJson(serde_json::Error),
    /// Typed Qdrant extension admission failed.
    Extension(qdrant::Error),
    /// Bounded HTTP provider configuration or operation failed.
    Provider(qdrant::HttpProviderError),
}

impl fmt::Display for RemoteConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(name) => {
                write!(formatter, "{name} is required when Qdrant is configured")
            }
            Self::InvalidValue(name) => write!(formatter, "{name} is invalid"),
            Self::ArtifactRevision { name } => {
                write!(
                    formatter,
                    "{name} does not match the admitted artifact bytes"
                )
            }
            Self::ArtifactChanged => {
                formatter.write_str("embedding producer artifacts changed during execution")
            }
            Self::StaleRow => {
                formatter.write_str("embedding names a row outside the selected view")
            }
            Self::IncompleteCoverage => {
                formatter.write_str("Qdrant activation requires complete owner coverage")
            }
            Self::ProjectionStateIo(error) => {
                write!(formatter, "Qdrant projection state I/O failed: {error}")
            }
            Self::InvalidRelation => formatter.write_str("Qdrant candidate relation is invalid"),
            Self::InvalidInventory => {
                formatter.write_str("semantic capability inventory is invalid")
            }
            Self::ProjectionLimit => {
                formatter.write_str("Qdrant projection exceeds its count or memory bound")
            }
            Self::ProducerUnavailable => formatter.write_str("embedding producer is unavailable"),
            Self::EmbeddingInput => formatter.write_str("embedding input violates its byte bound"),
            Self::ProducerDeadline => {
                formatter.write_str("embedding producer exceeded its deadline")
            }
            Self::ProducerProtocol => {
                formatter.write_str("embedding producer violated its protocol")
            }
            Self::ProducerRuntime(error) => write!(formatter, "embedding runtime failed: {error}"),
            Self::ProducerIo(error) => write!(formatter, "embedding producer I/O failed: {error}"),
            Self::ProducerJson(error) => {
                write!(formatter, "embedding producer JSON failed: {error}")
            }
            Self::Extension(error) => write!(formatter, "Qdrant admission failed: {error}"),
            Self::Provider(error) => write!(formatter, "Qdrant provider failed: {error}"),
        }
    }
}

impl std::error::Error for RemoteConfigError {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::super::{CoverageBasis, Freshness, LocalQuery};
    use super::*;
    use backend_engine::{CapabilityFamily, CapabilityLifecycle, CapabilityUnavailable, Row};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
    use std::thread;

    const CONFIG: [(&str, &str); 6] = [
        (QDRANT_ENDPOINT_ENV, "https://qdrant.invalid"),
        (QDRANT_COLLECTION_ENV, "backend-test"),
        (EMBEDDING_DIMENSIONS_ENV, "2"),
        (EMBEDDING_QUERY_TREATMENT_ENV, "query-v1"),
        (EMBEDDING_DOCUMENT_TREATMENT_ENV, "document-v1"),
        (EMBEDDING_PROTOCOL_ENV, "json-v1"),
    ];

    #[test]
    fn bem2_producer_process_limits_are_explicit() {
        test_bounded_embedding_process_limits();
    }

    #[test]
    fn qdrant_projection_state_is_scoped_by_endpoint_and_collection() {
        let configured = qdrant_projection_scope("https://qdrant.example", "semantic");
        assert_eq!(
            configured,
            qdrant_projection_scope("https://qdrant.example", "semantic")
        );
        assert_ne!(
            configured,
            qdrant_projection_scope("https://other.example", "semantic")
        );
        assert_ne!(
            qdrant_projection_scope("ab", "c"),
            qdrant_projection_scope("a", "bc")
        );
    }

    #[test]
    fn remote_hold_requires_same_verified_row_input_and_certain_projection() {
        let recipe = test_recipe();
        let row = RowId::Symbol(backend_engine::symbol_key("coordinate::stable"));
        let identity = DocumentEmbeddingId::new(recipe.version(), [8; 32], "unchanged text")
            .expect("input identity");
        let active = BTreeMap::from([(row.stable_key(), identity)]);
        assert_eq!(
            coordinate_write_for_input(&active, row, identity, false),
            qdrant::CoordinateWrite::Hold
        );
        assert_eq!(
            coordinate_write_for_input(
                &active,
                row,
                DocumentEmbeddingId::new(recipe.version(), [8; 32], "changed text")
                    .expect("changed input identity"),
                false,
            ),
            qdrant::CoordinateWrite::Replace
        );
        assert_eq!(
            coordinate_write_for_input(&active, row, identity, true),
            qdrant::CoordinateWrite::Replace
        );
        assert_eq!(
            coordinate_write_for_input(&BTreeMap::new(), row, identity, false),
            qdrant::CoordinateWrite::Replace
        );
    }

    #[test]
    fn projection_membership_recovers_staged_union_then_commits_changed_view() {
        let (coordinator, _, documents, _, recipe) = http_projection_inputs();
        let workspace = coordinator.workspace_root();
        let mut target = documents
            .iter()
            .map(|document| document.row.stable_key())
            .collect::<Vec<_>>();
        target.sort_unstable();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "backend-qdrant-membership-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("fixture directory");
        let scope = qdrant_projection_scope("https://qdrant.example", "semantic");
        let mut state =
            ProjectionState::open_in_directory(&directory, workspace, recipe.version(), scope)
                .expect("open membership state");
        assert!(state.row_keys().is_empty());
        state.stage(&target).expect("stage complete target");
        drop(state);

        let mut recovered =
            ProjectionState::open_in_directory(&directory, workspace, recipe.version(), scope)
                .expect("recover staged membership");
        assert_eq!(recovered.row_keys().len(), target.len());
        assert_eq!(
            recovered.residences().expect("typed residences").len(),
            target.len()
        );

        let changed = target.iter().take(1).cloned().collect::<Vec<_>>();
        recovered
            .stage(&changed)
            .expect("persist old and changed rows before upsert");
        assert_eq!(recovered.row_keys().len(), target.len());
        recovered
            .commit(&changed)
            .expect("commit verified current membership");
        assert_eq!(recovered.row_keys(), changed.as_slice());
        drop(recovered);

        let other_scope =
            ProjectionState::open_in_directory(&directory, workspace, recipe.version(), [0xA5; 32])
                .expect("open distinct collection scope");
        assert!(other_scope.row_keys().is_empty());
        drop(other_scope);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_document_payloads_share_one_exact_producer_result() {
        use std::os::unix::fs::PermissionsExt;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "backend-document-embedding-cache-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("fixture directory");
        let program = directory.join("embed.sh");
        let model = directory.join("model.bin");
        let tokenizer = directory.join("tokenizer.bin");
        let calls = directory.join("calls.txt");
        let script = format!(
            r#"#!/bin/sh
cat >/dev/null
printf 'call\n' >> '{}'
printf '{{"abi":1,"dimensions":2,"values":[1.0,0.0]}}'
"#,
            calls.display()
        );
        fs::write(&program, script).expect("producer script");
        let mut permissions = fs::metadata(&program)
            .expect("producer metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).expect("producer executable");
        fs::write(&model, b"stable model artifact").expect("model artifact");
        fs::write(&tokenizer, b"stable tokenizer artifact").expect("tokenizer artifact");
        let program_bytes = fs::read(&program).expect("program bytes");
        let model_bytes = fs::read(&model).expect("model bytes");
        let tokenizer_bytes = fs::read(&tokenizer).expect("tokenizer bytes");
        let model_identity = *blake3::hash(&model_bytes).as_bytes();
        let tokenizer_identity = *blake3::hash(&tokenizer_bytes).as_bytes();
        let mut manifest = blake3::Hasher::new();
        manifest.update(b"backend.embedding.producer.v1\0");
        manifest.update(&program_bytes);
        manifest.update(&model_bytes);
        manifest.update(&tokenizer_bytes);
        manifest.update(&EMBEDDING_PROTOCOL_ABI.to_be_bytes());
        let manifest = *manifest.finalize().as_bytes();
        let producer = EmbeddingProducer {
            program,
            model,
            tokenizer,
            program_identity: *blake3::hash(&program_bytes).as_bytes(),
            model_identity,
            tokenizer_identity,
            dimensions: NonZeroU32::new(2).expect("dimension"),
            manifest,
            protocol: EmbeddingProducerProtocol::JsonV1,
            batch_runtime: None,
            _batch_workspace: None,
        };
        let recipe = test_recipe();
        let client =
            qdrant::QdrantHttpClient::new(test_transport("http://127.0.0.1:9".into()), recipe)
                .expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: Some(producer),
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let first = DocumentEmbeddingId::new(recipe.version(), manifest, "same semantic text")
            .expect("first identity");
        assert_ne!(
            first,
            DocumentEmbeddingId::new(recipe.version(), [0xA5; 32], "same semantic text")
                .expect("changed producer identity")
        );
        assert_ne!(
            first,
            DocumentEmbeddingId::new(recipe.version(), manifest, "changed semantic text")
                .expect("changed input identity")
        );
        let documents = [
            SemanticDocument {
                row: RowId::Symbol(backend_engine::symbol_key("duplicate::first")),
                text: "same semantic text".into(),
            },
            SemanticDocument {
                row: RowId::Symbol(backend_engine::symbol_key("duplicate::second")),
                text: "same semantic text".into(),
            },
        ];
        let cold = configured
            .embed_documents(&documents)
            .expect("embed the distinct input once");
        assert_eq!(
            fs::read_to_string(&calls).expect("calls").lines().count(),
            1
        );
        assert!(Arc::ptr_eq(&cold[0].coordinates, &cold[1].coordinates));
        assert!(
            cold.iter()
                .all(|document| document.write == qdrant::CoordinateWrite::Replace)
        );

        let warm = configured
            .embed_documents(&documents)
            .expect("reuse the exact cached vector");
        assert_eq!(
            fs::read_to_string(&calls).expect("calls").lines().count(),
            1
        );
        assert!(Arc::ptr_eq(&cold[0].coordinates, &warm[0].coordinates));
        assert!(
            warm.iter()
                .all(|document| document.write == qdrant::CoordinateWrite::Replace)
        );
        drop(configured);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn qdrant_bem2_documents_share_one_supervised_batch() {
        use std::os::unix::fs::PermissionsExt;

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "backend-qdrant-bem2-batch-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("fixture directory");
        let program = directory.join("embed.py");
        let model = directory.join("model.bin");
        let tokenizer = directory.join("tokenizer.bin");
        let calls = directory.join("calls.txt");
        let interpreter = Command::new("python3")
            .arg("-c")
            .arg("import sys; print(sys.executable)")
            .output()
            .expect("python interpreter");
        assert!(interpreter.status.success());
        let interpreter = String::from_utf8(interpreter.stdout)
            .expect("interpreter path")
            .trim()
            .to_owned();
        let script = format!(
            r#"#!{}
import struct, sys
CALLS = {:?}
frame = sys.stdin.buffer.read()
if len(frame) < 108 or frame[:4] != b"BEM2": sys.exit(72)
count = struct.unpack(">I", frame[104:108])[0]
dimension = struct.unpack(">H", frame[6:8])[0]
items = []
offset = 108
for _ in range(count):
    identity = frame[offset:offset + 32]
    length = struct.unpack(">I", frame[offset + 32:offset + 36])[0]
    offset += 36 + length
    if offset > len(frame): sys.exit(73)
    items.append(identity)
if offset != len(frame): sys.exit(74)
with open(CALLS, "a") as output: output.write("call\n")
sys.stdout.buffer.write(b"BEC2" + struct.pack(">HI", dimension, count))
for identity in items:
    values = [1.0] + [0.0] * (dimension - 1)
    sys.stdout.buffer.write(identity + struct.pack("<" + "f" * dimension, *values))
"#,
            interpreter,
            calls.to_string_lossy(),
        );
        fs::write(&program, script).expect("batch producer");
        let mut permissions = fs::metadata(&program)
            .expect("producer metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).expect("producer executable");
        fs::write(&model, b"BEM2 model").expect("model artifact");
        fs::write(&tokenizer, b"BEM2 tokenizer").expect("tokenizer artifact");
        let program_bytes = fs::read(&program).expect("program bytes");
        let model_bytes = fs::read(&model).expect("model bytes");
        let tokenizer_bytes = fs::read(&tokenizer).expect("tokenizer bytes");
        let producer = EmbeddingProducer {
            program,
            model,
            tokenizer,
            program_identity: *blake3::hash(&program_bytes).as_bytes(),
            model_identity: *blake3::hash(&model_bytes).as_bytes(),
            tokenizer_identity: *blake3::hash(&tokenizer_bytes).as_bytes(),
            dimensions: NonZeroU32::new(2).expect("dimension"),
            manifest: {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"backend.embedding.producer.v1\0");
                hasher.update(&program_bytes);
                hasher.update(&model_bytes);
                hasher.update(&tokenizer_bytes);
                hasher.update(&EMBEDDING_BATCH_PROTOCOL_ABI.to_be_bytes());
                *hasher.finalize().as_bytes()
            },
            protocol: EmbeddingProducerProtocol::Bem2,
            batch_runtime: None,
            _batch_workspace: None,
        };
        let recipe = test_recipe();
        let mut producer = producer;
        producer
            .activate_batch_runtime(recipe)
            .expect("activate shared batch runtime");
        assert_eq!(
            producer
                .batch_runtime
                .as_ref()
                .map(EmbeddingExecutable::batch_protocol),
            Some(EmbeddingBatchProtocol::BatchV2)
        );
        let client =
            qdrant::QdrantHttpClient::new(test_transport("http://127.0.0.1:9".into()), recipe)
                .expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: Some(producer),
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let documents = [
            SemanticDocument {
                row: RowId::Symbol(backend_engine::symbol_key("bem2::first")),
                text: "same semantic text".into(),
            },
            SemanticDocument {
                row: RowId::Symbol(backend_engine::symbol_key("bem2::duplicate")),
                text: "same semantic text".into(),
            },
            SemanticDocument {
                row: RowId::Symbol(backend_engine::symbol_key("bem2::second")),
                text: "different semantic text".into(),
            },
        ];
        let cold = configured
            .embed_documents(&documents)
            .expect("embed unique documents in one worker");
        assert_eq!(
            fs::read_to_string(&calls).expect("calls").lines().count(),
            2
        );
        assert!(Arc::ptr_eq(&cold[0].coordinates, &cold[1].coordinates));
        assert!(
            cold.iter()
                .all(|document| document.write == qdrant::CoordinateWrite::Replace)
        );
        let warm = configured
            .embed_documents(&documents)
            .expect("reuse exact document vectors");
        assert_eq!(
            fs::read_to_string(&calls).expect("calls").lines().count(),
            2
        );
        assert!(
            warm.iter()
                .all(|document| document.write == qdrant::CoordinateWrite::Replace)
        );

        drop(configured);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn production_environment_constructs_bounded_http_configuration() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("backend-embedding-fixture-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("fixture directory");
        let program = directory.join("embed.sh");
        let model = directory.join("model.bin");
        let tokenizer = directory.join("tokenizer.json");
        fs::write(
            &program,
            b"#!/bin/sh\nIFS= read -r input || true\ncase \"$input\" in *document*) printf '{\"abi\":1,\"dimensions\":2,\"values\":[0.0,1.0]}' ;; *query*) printf '{\"abi\":1,\"dimensions\":2,\"values\":[1.0,0.0]}' ;; *) exit 64 ;; esac\n",
        )
        .expect("fixture producer");
        let mut permissions = fs::metadata(&program)
            .expect("producer metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).expect("producer executable");
        fs::write(&model, b"model artifact v1").expect("model artifact");
        fs::write(&tokenizer, b"tokenizer artifact v1").expect("tokenizer artifact");
        let executable = std::env::current_exe().expect("current test executable");
        let mut child = Command::new(executable);
        child
            .arg("--ignored")
            .arg("--exact")
            .arg("builtin::query::remote::tests::configured_environment_builds_http_client_child");
        for (name, value) in CONFIG {
            child.env(name, value);
        }
        child
            .env(EMBEDDING_PROGRAM_ENV, &program)
            .env(EMBEDDING_MODEL_FILE_ENV, &model)
            .env(EMBEDDING_TOKENIZER_FILE_ENV, &tokenizer)
            .env(
                EMBEDDING_MODEL_ENV,
                revision_for_fixture(b"model artifact v1"),
            )
            .env(
                EMBEDDING_TOKENIZER_ENV,
                revision_for_fixture(b"tokenizer artifact v1"),
            );
        let status = child.status().expect("configuration child");
        fs::remove_dir_all(directory).expect("remove fixture directory");
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "executed in an isolated environment by its parent test"]
    fn configured_environment_builds_http_client_child() {
        let remote = RemoteSemantic::from_environment();
        let configured = remote.configured().expect("configured provider");
        assert_eq!(
            configured.recipe().dimensions,
            NonZeroU32::new(2).expect("dimension")
        );

        let inventory = remote
            .inventory(&backend_engine::CapabilityInventory::explicitly_unavailable())
            .expect("inventory");
        let embedding = inventory
            .as_slice()
            .iter()
            .find(|status| matches!(status.family(), CapabilityFamily::Embedding { .. }))
            .expect("embedding status");
        assert!(matches!(
            embedding.family(),
            CapabilityFamily::Embedding { recipe: Some(_) }
        ));
        let capability_recipe = embedding_capability_recipe(configured.recipe);
        assert_eq!(
            embedding.id(),
            backend_engine::CapabilityId::new(capability_recipe.identity().as_bytes())
        );
        assert_eq!(embedding.lifecycle(), CapabilityLifecycle::Installed);
        let producer = configured.producer.as_ref().expect("installed producer");
        assert_eq!(
            producer
                .embed(EmbeddingTreatment::Query, "symbol")
                .expect("query embedding"),
            vec![1.0, 0.0]
        );
        assert_eq!(
            producer
                .embed(EmbeddingTreatment::Document, "symbol docs")
                .expect("document embedding"),
            vec![0.0, 1.0]
        );
    }

    fn revision_for_fixture(bytes: &[u8]) -> String {
        format!("blake3:{}", hexadecimal(*blake3::hash(bytes).as_bytes()))
    }

    #[test]
    fn absent_and_invalid_configuration_have_explicit_degraded_readiness() {
        let baseline = backend_engine::CapabilityInventory::explicitly_unavailable();
        for (remote, reason) in [
            (
                RemoteSemantic::Unconfigured,
                CapabilityUnavailable::NoManifest,
            ),
            (
                RemoteSemantic::Unavailable(Box::new(RemoteConfigError::MissingValue(
                    QDRANT_COLLECTION_ENV,
                ))),
                CapabilityUnavailable::MissingDependency,
            ),
        ] {
            let inventory = remote.inventory(&baseline).expect("inventory");
            let embedding = inventory
                .as_slice()
                .iter()
                .find(|status| matches!(status.family(), CapabilityFamily::Embedding { .. }))
                .expect("embedding status");
            assert_eq!(
                embedding.lifecycle(),
                CapabilityLifecycle::Unavailable(reason)
            );
        }
    }

    #[test]
    fn unconfigured_cold_start_returns_lexical_rows_with_unavailable_status() {
        let (coordinator, coverage, _, _, _) = http_projection_inputs();
        let local = coordinator
            .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
            .expect("local search");
        let lexical_ids = local
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>();
        let mut remote = RemoteSemantic::Unconfigured;

        let (result, status) =
            remote.search_with_status(&coordinator, coverage, local, "alpha", false);

        assert!(!result.rows.is_empty());
        assert_eq!(
            result
                .rows
                .iter()
                .map(|ranked| ranked.row.id)
                .collect::<Vec<_>>(),
            lexical_ids
        );
        assert_eq!(
            status,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::Unconfigured,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn qdrant_query_outage_keeps_lexical_rows_and_reports_degraded_status() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "backend-query-outage-fixture-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("fixture directory");
        let program = directory.join("embed.sh");
        let model = directory.join("model.bin");
        let tokenizer = directory.join("tokenizer.json");
        fs::write(
            &program,
            b"#!/bin/sh\nIFS= read -r input || true\nprintf '{\"abi\":1,\"dimensions\":2,\"values\":[1.0,0.0]}'\n",
        )
        .expect("fixture producer");
        let mut permissions = fs::metadata(&program)
            .expect("producer metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).expect("producer executable");
        fs::write(&model, b"query outage model").expect("model artifact");
        fs::write(&tokenizer, b"query outage tokenizer").expect("tokenizer artifact");
        let program_bytes = fs::read(&program).expect("program bytes");
        let model_bytes = fs::read(&model).expect("model bytes");
        let tokenizer_bytes = fs::read(&tokenizer).expect("tokenizer bytes");
        let program_identity = *blake3::hash(&program_bytes).as_bytes();
        let model_identity = *blake3::hash(&model_bytes).as_bytes();
        let tokenizer_identity = *blake3::hash(&tokenizer_bytes).as_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.embedding.producer.v1\0");
        hasher.update(&program_bytes);
        hasher.update(&model_bytes);
        hasher.update(&tokenizer_bytes);
        hasher.update(&EMBEDDING_PROTOCOL_ABI.to_be_bytes());
        let producer = EmbeddingProducer {
            program,
            model,
            tokenizer,
            program_identity,
            model_identity,
            tokenizer_identity,
            dimensions: NonZeroU32::new(2).expect("dimension"),
            manifest: *hasher.finalize().as_bytes(),
            protocol: EmbeddingProducerProtocol::JsonV1,
            batch_runtime: None,
            _batch_workspace: None,
        };

        let (coordinator, coverage, documents, selected_point, recipe) = http_projection_inputs();
        let (endpoint, server) = serve_projection(ProjectionScript {
            requests: 6,
            expected_points: documents.len(),
            selected_point,
            count_offset: 0,
            swap_query_id: false,
            corrupt_coordinate: false,
            fail_query: true,
        });
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: Some(producer),
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        configured.active = Some(
            configured
                .activate(&coordinator, coverage, documents)
                .expect("initial projection activates before outage"),
        );
        let mut remote = RemoteSemantic::Configured(Box::new(configured));
        let local = coordinator
            .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
            .expect("local search");
        let lexical_ids = local
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>();

        let (result, status) =
            remote.search_with_status(&coordinator, coverage, local, "alpha", false);

        assert!(!result.rows.is_empty());
        assert_eq!(
            result
                .rows
                .iter()
                .map(|ranked| ranked.row.id)
                .collect::<Vec<_>>(),
            lexical_ids
        );
        assert_eq!(
            status,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::ProviderUnavailable,
            }
        );
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            ["get", "get", "retrieve", "put", "count", "query"]
        );
        drop(remote);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn verified_http_projection_constructs_the_real_source() {
        let (workspace, view) = super::super::tests::selected_view();
        let coverage = crate::builtin::admitted_coverage().expect("coverage");
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let coordinator = QueryCoordinator::new(workspace, view.clone(), coverage, evidence)
            .expect("coordinator");
        let target_index = coordinator
            .semantic_documents()
            .iter()
            .position(|document| document.text.contains("semantic-only"))
            .expect("semantic document");
        let row = coordinator.semantic_documents()[target_index].row;
        let documents = coordinator
            .semantic_documents()
            .iter()
            .map(|document| QdrantDocument {
                row: document.row,
                coordinates: Arc::from([1.0, 0.0]),
                write: qdrant::CoordinateWrite::Hold,
            })
            .collect::<Vec<_>>();
        let (endpoint, server) = qdrant_fixture(documents.len(), target_index);
        let recipe = test_recipe();
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let active = configured
            .activate(&coordinator, coverage, documents)
            .expect("verified HTTP projection");
        assert_eq!(active.recipe(), recipe);
        let query = qdrant::QueryVector::new(recipe, vec![1.0, 0.0]).expect("query vector");
        let provider_result = active
            .index
            .search_embedding(&query, 4, qdrant::Limits::default())
            .expect("HTTP semantic query");
        assert!(
            provider_result
                .candidates
                .iter()
                .any(|candidate| { coordinator.semantic_candidate(row) == Some(candidate.id) })
        );

        let local = coordinator
            .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
            .expect("local search");
        let accelerated = active.accelerate(local, vec![1.0, 0.0]);
        assert_eq!(accelerated.rows[0].row.id, row);
        assert_eq!(accelerated.lanes[2].freshness, Freshness::Current);
        assert_eq!(
            semantic_status_from_result(&accelerated),
            Some(SemanticSearchStatus::Available)
        );

        let extra = Row::new(
            RowId::Symbol(backend_engine::symbol_key("next::generation")),
            view.basis(),
            "next generation",
        );
        let mut next_rows = view.rows().to_vec();
        next_rows.push(extra);
        let next_view = backend_engine::ViewRoot::new_checked(
            view.recipe(),
            view.basis(),
            view.frontier(),
            next_rows,
            view.coverage().to_vec(),
            view.capability().expect("view capability"),
        )
        .expect("next view");
        let next_evidence = super::super::tests::semantic_evidence(workspace, &next_view);
        let next = QueryCoordinator::new(workspace, next_view, coverage, next_evidence)
            .expect("next coordinator");
        let stale_local = next
            .search_local(LocalQuery::prefix("alpha", 3).expect("query"))
            .expect("next local search");
        let lexical_rows = stale_local
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<Vec<_>>();
        let stale = active.accelerate(stale_local, vec![1.0, 0.0]);
        assert_eq!(
            stale
                .rows
                .iter()
                .map(|ranked| ranked.row.id)
                .collect::<Vec<_>>(),
            lexical_rows
        );
        assert_eq!(stale.lanes[2].freshness, Freshness::Stale);
        assert_eq!(stale.lanes[2].coverage, CoverageBasis::Unavailable);
        assert_eq!(
            semantic_status_from_result(&stale),
            Some(SemanticSearchStatus::Stale {
                reason: SemanticSearchReason::StaleProjection,
            })
        );
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            ["get", "get", "retrieve", "put", "count", "query", "query"]
        );
    }

    #[test]
    fn http_projection_rejects_a_short_point_count() {
        let (coordinator, coverage, documents, _, recipe) = http_projection_inputs();
        let (endpoint, server) = serve_projection(ProjectionScript {
            requests: 5,
            expected_points: documents.len(),
            selected_point: 0,
            count_offset: -1,
            swap_query_id: false,
            corrupt_coordinate: false,
            fail_query: false,
        });
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let expected_points = u64::try_from(documents.len()).expect("point count");
        let error = match configured.activate(&coordinator, coverage, documents) {
            Ok(_active) => panic!("short count must not activate"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            RemoteConfigError::Provider(qdrant::HttpProviderError::IncompleteProjection {
                expected,
                observed,
            }) if expected == expected_points && observed == expected_points - 1
        ));
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            ["get", "get", "retrieve", "put", "count"]
        );
    }

    #[test]
    fn http_query_rejects_a_payload_that_names_a_different_point() {
        let (coordinator, coverage, documents, target_index, recipe) = http_projection_inputs();
        let (endpoint, server) = serve_projection(ProjectionScript {
            requests: 6,
            expected_points: documents.len(),
            selected_point: target_index,
            count_offset: 0,
            swap_query_id: true,
            corrupt_coordinate: false,
            fail_query: false,
        });
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let active = configured
            .activate(&coordinator, coverage, documents)
            .expect("count still matches");
        let query = qdrant::QueryVector::new(recipe, vec![1.0, 0.0]).expect("query vector");
        let error = active
            .index
            .search_embedding(&query, 4, qdrant::Limits::default())
            .expect_err("swapped id");
        assert!(matches!(
            error,
            qdrant::AdapterError::Provider(qdrant::HttpProviderError::BindingMismatch)
        ));
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            ["get", "get", "retrieve", "put", "count", "query"]
        );
    }

    #[test]
    fn second_http_activation_skips_the_put_when_coordinates_match() {
        let (coordinator, coverage, documents, _, recipe) = http_projection_inputs();
        let (endpoint, server) = serve_projection(ProjectionScript {
            requests: 9,
            expected_points: documents.len(),
            selected_point: 0,
            count_offset: 0,
            swap_query_id: false,
            corrupt_coordinate: false,
            fail_query: false,
        });
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        configured
            .activate(&coordinator, coverage, documents.clone())
            .expect("first projection");
        configured
            .activate(&coordinator, coverage, documents)
            .expect("unchanged projection");
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            [
                "get", "get", "retrieve", "put", "count", "get", "get", "retrieve", "count"
            ]
        );
    }

    #[test]
    fn second_http_activation_rejects_a_rewritten_coordinate() {
        let (coordinator, coverage, documents, _, recipe) = http_projection_inputs();
        let (endpoint, server) = serve_projection(ProjectionScript {
            requests: 8,
            expected_points: documents.len(),
            selected_point: 0,
            count_offset: 0,
            swap_query_id: false,
            corrupt_coordinate: true,
            fail_query: false,
        });
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        configured
            .activate(&coordinator, coverage, documents.clone())
            .expect("first projection");
        let error = match configured.activate(&coordinator, coverage, documents) {
            Ok(_active) => panic!("rewritten coordinate must not activate"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            RemoteConfigError::Provider(qdrant::HttpProviderError::ImmutableVector)
        ));
        let lines = server.join().expect("fixture server");
        assert_eq!(
            request_kinds(&lines),
            [
                "get", "get", "retrieve", "put", "count", "get", "get", "retrieve"
            ]
        );
    }

    #[test]
    fn adding_a_row_rebinds_resident_payloads_and_writes_one_vector() {
        let (coordinator, coverage, documents, _, recipe) = http_projection_inputs();
        let (workspace, view) = super::super::tests::selected_view();
        let kept = documents.len();
        let (endpoint, server) = serve_resident(11);
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        configured
            .activate(&coordinator, coverage, documents.clone())
            .expect("first projection");
        let extra_id = RowId::Symbol(backend_engine::symbol_key("next::generation"));
        let mut next_rows = view.rows().to_vec();
        next_rows.push(Row::new(extra_id, view.basis(), "next generation"));
        let next_view = backend_engine::ViewRoot::new_checked(
            view.recipe(),
            view.basis(),
            view.frontier(),
            next_rows,
            view.coverage().to_vec(),
            view.capability().expect("view capability"),
        )
        .expect("next view");
        let next_evidence = super::super::tests::semantic_evidence(workspace, &next_view);
        let next = QueryCoordinator::new(workspace, next_view, coverage, next_evidence)
            .expect("next coordinator");
        let mut next_documents = documents;
        next_documents.push(QdrantDocument {
            row: extra_id,
            coordinates: Arc::from([0.0, 1.0]),
            write: qdrant::CoordinateWrite::Hold,
        });
        configured
            .activate(&next, coverage, next_documents)
            .expect("rebound projection");
        let events = server.join().expect("fixture server");
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                "get", "get", "retrieve", "put", "count", "get", "get", "retrieve", "put",
                "payload", "count"
            ]
        );
        assert_eq!(events[3].points, kept);
        assert!(events[3].carries_vector);
        assert!(events[3].bytes > 0);
        assert_eq!(events[8].points, 1);
        assert!(events[8].carries_vector);
        assert!(events[8].bytes > 0);
        assert!(events[8].bytes < events[3].bytes);
        assert_eq!(events[9].points, kept);
        assert!(!events[9].carries_vector);
        assert!(events[9].bytes > 0);
    }

    #[test]
    fn dropping_a_row_deletes_only_that_residence() {
        let (coordinator, coverage, documents, _, recipe) = http_projection_inputs();
        assert!(
            documents.len() > 1,
            "the fixture view has more than one document"
        );
        let (workspace, view) = super::super::tests::selected_view();
        let (endpoint, server) = serve_resident(11);
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: None,
        };
        let first = configured
            .activate(&coordinator, coverage, documents.clone())
            .expect("first projection");
        let removed = documents.last().expect("document").row;
        let kept_documents = documents
            .iter()
            .filter(|document| document.row != removed)
            .cloned()
            .collect::<Vec<_>>();
        let kept_rows = view
            .rows()
            .iter()
            .filter(|row| row.id != removed)
            .cloned()
            .collect::<Vec<_>>();
        let next_view = backend_engine::ViewRoot::new_checked(
            view.recipe(),
            view.basis(),
            view.frontier(),
            kept_rows,
            view.coverage().to_vec(),
            view.capability().expect("view capability"),
        )
        .expect("smaller view");
        let next_evidence = super::super::tests::semantic_evidence(workspace, &next_view);
        let next = QueryCoordinator::new(workspace, next_view, coverage, next_evidence)
            .expect("next coordinator");
        let second = configured
            .activate(&next, coverage, kept_documents)
            .expect("rebound survivors");
        configured.retired = Some(first.into_retired());
        configured.active = Some(second);
        configured.retire_pending().expect("delete the dropped row");
        let events = server.join().expect("fixture server");
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                "get", "get", "retrieve", "put", "count", "get", "get", "retrieve", "payload",
                "count", "delete"
            ]
        );
        assert_eq!(events[8].points, documents.len() - 1);
        assert!(!events[8].carries_vector);
        assert!(events[8].bytes > 0);
        assert_eq!(events[10].points, 1);
        assert!(events[10].bytes > 0);
    }

    fn http_projection_inputs() -> (
        QueryCoordinator,
        CoverageWitness,
        Vec<QdrantDocument>,
        usize,
        qdrant::EmbeddingRecipe,
    ) {
        let (workspace, view) = super::super::tests::selected_view();
        let coverage = crate::builtin::admitted_coverage().expect("coverage");
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let coordinator =
            QueryCoordinator::new(workspace, view, coverage, evidence).expect("coordinator");
        let target_index = coordinator
            .semantic_documents()
            .iter()
            .position(|document| document.text.contains("semantic-only"))
            .expect("semantic document");
        let documents = coordinator
            .semantic_documents()
            .iter()
            .map(|document| QdrantDocument {
                row: document.row,
                coordinates: Arc::from([1.0, 0.0]),
                write: qdrant::CoordinateWrite::Hold,
            })
            .collect::<Vec<_>>();
        (
            coordinator,
            coverage,
            documents,
            target_index,
            test_recipe(),
        )
    }

    #[test]
    fn retired_projection_is_deleted_by_binding_scoped_physical_identity() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            let request = read_request(&mut stream);
            let body = r#"{"result":{"status":"completed"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("fixture response");
            String::from_utf8(request).expect("request text")
        });
        let (workspace, view) = super::super::tests::selected_view();
        let coverage = crate::builtin::admitted_coverage().expect("coverage");
        let evidence = super::super::tests::semantic_evidence(workspace, &view);
        let coordinator =
            QueryCoordinator::new(workspace, view, coverage, evidence).expect("coordinator");
        let row = coordinator
            .semantic_documents()
            .first()
            .expect("semantic document")
            .row;
        let candidate = coordinator.semantic_candidate(row).expect("candidate");
        let recipe = test_recipe();
        let residence =
            qdrant::PointResidence::for_row(workspace, recipe.version(), &row.stable_key())
                .expect("residence");
        let client =
            qdrant::QdrantHttpClient::new(test_transport(format!("http://{address}")), recipe)
                .expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            projection_scope: [0; 32],
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            embedding_cache: None,
            projection_state: None,
            projection_uncertain: false,
            active_embedding_identities: BTreeMap::new(),
            active: None,
            retired: Some(RetiredQdrant {
                workspace,
                recipe: recipe.version(),
                residences: vec![residence].into_boxed_slice(),
            }),
        };

        configured.retire_pending().expect("retire projection");
        assert!(configured.retired.is_none());
        let request = server.join().expect("fixture server");
        assert!(request.starts_with("POST /collections/backend-test/points/delete"));
        assert!(request.contains(r#""points":["#));
        assert!(!request.contains(&format!(r#""points":[{}]"#, candidate.0)));
    }

    fn test_recipe() -> qdrant::EmbeddingRecipe {
        qdrant::EmbeddingRecipe {
            model: qdrant::ModelVersion::from_value(&[11; 32]),
            tokenizer: qdrant::TokenizerVersion::from_value(&[12; 32]),
            dimensions: NonZeroU32::new(2).expect("dimension"),
            metric: qdrant::Metric::CosineDistance,
            pooling: qdrant::EmbeddingPooling::Mean,
            normalization: qdrant::EmbeddingNormalization::UnitL2,
            encoding: qdrant::EmbeddingEncoding::Float32,
            query_treatment: qdrant::TreatmentVersion::from_value(b"query-v1"),
            document_treatment: qdrant::TreatmentVersion::from_value(b"document-v1"),
        }
    }

    #[test]
    fn projection_envelope_separates_corpus_and_provider_page_bounds() {
        let mut recipe = test_recipe();
        recipe.dimensions = NonZeroU32::new(1_536).expect("dimension");
        let envelope = ProjectionEnvelope::for_recipe(recipe).expect("projection envelope");

        assert_eq!(envelope.0.max_candidates, MAX_REMOTE_SEMANTIC_DOCUMENTS);
        assert_eq!(envelope.0.max_page, qdrant::Limits::default().max_page);
        assert_eq!(envelope.0.max_payload_bytes, 4 + 1_536 * 4);
        envelope
            .admit(MAX_REMOTE_SEMANTIC_DOCUMENTS)
            .expect("typical vectors fit the complete corpus bound");
    }

    #[test]
    fn projection_envelope_rejects_before_exceeding_its_memory_bound() {
        let mut recipe = test_recipe();
        recipe.dimensions = NonZeroU32::new(MAX_EMBEDDING_DIMENSIONS).expect("dimension");
        let envelope = ProjectionEnvelope::for_recipe(recipe).expect("projection envelope");
        let encoded_vector_bytes = 4 + MAX_EMBEDDING_DIMENSIONS as usize * 4;

        assert_eq!(
            envelope.0.max_candidates,
            MAX_VECTOR_FACT_BYTES / encoded_vector_bytes
        );
        assert!(envelope.admit(envelope.0.max_candidates).is_ok());
        assert!(matches!(
            envelope.admit(envelope.0.max_candidates + 1),
            Err(RemoteConfigError::ProjectionLimit)
        ));
    }

    fn test_transport(endpoint: String) -> qdrant::QdrantHttpConfig {
        qdrant::QdrantHttpConfig {
            endpoint,
            collection: "backend-test".to_owned(),
            api_key: None,
            connect_deadline: Duration::from_secs(1),
            read_deadline: Duration::from_secs(1),
            attempts: NonZeroU8::new(1).expect("attempts"),
            max_response_bytes: 4096,
            max_request_bytes: 4096,
            max_batch_points: qdrant::Limits::default().max_page,
        }
    }

    struct ResidentEvent {
        kind: &'static str,
        points: usize,
        bytes: usize,
        carries_vector: bool,
    }

    fn serve_resident(requests: usize) -> (String, thread::JoinHandle<Vec<ResidentEvent>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let metadata =
            r#"{"result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}}}}"#;
        let server = thread::spawn(move || {
            let mut stored = std::collections::HashMap::<String, serde_json::Value>::new();
            let mut events = Vec::with_capacity(requests);
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().expect("fixture connection");
                let request = read_request(&mut stream);
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("HTTP header");
                let header = std::str::from_utf8(&request[..header_end]).expect("HTTP header");
                let first = header.lines().next().expect("request line");
                let body = &request[header_end + 4..];
                let carries_vector = body.windows(8).any(|window| window == b"\"vector\"");
                let (kind, points, response) = if first.starts_with("GET ") {
                    ("get", 0, metadata.to_owned())
                } else if first.starts_with("PUT ") && first.contains("/points") {
                    let value: serde_json::Value =
                        serde_json::from_slice(body).expect("upsert JSON");
                    let points = value["points"].as_array().cloned().unwrap_or_default();
                    let count = points.len();
                    for point in points {
                        let id = point["id"].as_str().expect("point id").to_owned();
                        stored.insert(id, point);
                    }
                    (
                        "put",
                        count,
                        r#"{"result":{"status":"completed"}}"#.to_owned(),
                    )
                } else if first.contains("/points/batch") {
                    let value: serde_json::Value =
                        serde_json::from_slice(body).expect("payload JSON");
                    let mut count = 0_usize;
                    for operation in value["operations"].as_array().into_iter().flatten() {
                        let payload = operation["set_payload"]["payload"].clone();
                        for id in operation["set_payload"]["points"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            let id = id.as_str().expect("point id");
                            let point = stored.get_mut(id).expect("resident point");
                            point["payload"] = payload.clone();
                            count += 1;
                        }
                    }
                    (
                        "payload",
                        count,
                        r#"{"result":{"status":"completed"}}"#.to_owned(),
                    )
                } else if first.contains("/points/count") {
                    let value: serde_json::Value =
                        serde_json::from_slice(body).expect("count JSON");
                    let matched = stored
                        .values()
                        .filter(|point| payload_matches_filter(point, &value["filter"]))
                        .count();
                    (
                        "count",
                        matched,
                        serde_json::json!({"result": {"count": matched}}).to_string(),
                    )
                } else if first.contains("/points/delete") {
                    let value: serde_json::Value =
                        serde_json::from_slice(body).expect("delete JSON");
                    let ids = value["points"].as_array().cloned().unwrap_or_default();
                    for id in &ids {
                        stored.remove(id.as_str().expect("point id"));
                    }
                    (
                        "delete",
                        ids.len(),
                        r#"{"result":{"status":"completed"}}"#.to_owned(),
                    )
                } else if first.contains("/points") {
                    let value: serde_json::Value =
                        serde_json::from_slice(body).expect("retrieve JSON");
                    let points = value["ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| id.as_str())
                        .filter_map(|id| {
                            let mut point = stored.get(id)?.clone();
                            if let Some(object) = point.as_object_mut() {
                                object.remove("vector");
                            }
                            Some(point)
                        })
                        .collect::<Vec<_>>();
                    (
                        "retrieve",
                        points.len(),
                        serde_json::json!({"result": points}).to_string(),
                    )
                } else {
                    panic!("unexpected Qdrant request: {first}");
                };
                events.push(ResidentEvent {
                    kind,
                    points,
                    bytes: body.len(),
                    carries_vector,
                });
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .expect("fixture response");
            }
            events
        });
        (format!("http://{address}"), server)
    }

    fn payload_matches_filter(point: &serde_json::Value, filter: &serde_json::Value) -> bool {
        filter["must"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|condition| {
                let key = condition["key"].as_str().unwrap_or("");
                let expected = condition["match"]["value"].as_str().unwrap_or("");
                point["payload"][key].as_str() == Some(expected)
            })
    }

    fn qdrant_fixture(
        expected_points: usize,
        selected_point: usize,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        serve_projection(ProjectionScript {
            requests: 7,
            expected_points,
            selected_point,
            count_offset: 0,
            swap_query_id: false,
            corrupt_coordinate: false,
            fail_query: false,
        })
    }

    struct ProjectionScript {
        requests: usize,
        expected_points: usize,
        selected_point: usize,
        count_offset: i64,
        swap_query_id: bool,
        corrupt_coordinate: bool,
        fail_query: bool,
    }

    fn serve_projection(script: ProjectionScript) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let metadata =
            r#"{"result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}}}}"#;
        let server = thread::spawn(move || {
            let mut lines = Vec::with_capacity(script.requests);
            let mut captured = Vec::new();
            for _ in 0..script.requests {
                let (mut stream, _) = listener.accept().expect("fixture connection");
                let request = read_request(&mut stream);
                let header = std::str::from_utf8(&request).expect("HTTP header");
                let first = header.lines().next().expect("request line").to_owned();
                lines.push(first.clone());
                let body_start = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|offset| offset + 4)
                    .unwrap_or(request.len());
                let body = if first.starts_with("GET ") {
                    metadata.to_owned()
                } else if first.starts_with("PUT ") {
                    let value: serde_json::Value = serde_json::from_slice(&request[body_start..])
                        .expect("upsert request JSON");
                    captured = value["points"]
                        .as_array()
                        .map(|points| {
                            points
                                .iter()
                                .map(|point| {
                                    serde_json::json!({
                                        "id": point["id"].clone(),
                                        "payload": point["payload"].clone()
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    r#"{"result":{"status":"completed"}}"#.to_owned()
                } else if first.contains("/points/count") {
                    let count = i64::try_from(script.expected_points)
                        .expect("point count")
                        .saturating_add(script.count_offset);
                    serde_json::json!({"result": {"count": count}}).to_string()
                } else if first.contains("/points/batch") {
                    r#"{"result":{"status":"completed"}}"#.to_owned()
                } else if first.contains("/points/query") {
                    let mut point = captured
                        .get(script.selected_point)
                        .cloned()
                        .expect("query follows the upsert");
                    if script.swap_query_id {
                        point["id"] = serde_json::json!("not-the-payload-id");
                    }
                    serde_json::json!({"result": {"points": [point]}}).to_string()
                } else if first.contains("/points") {
                    let mut points = captured.iter().cloned().collect::<Vec<_>>();
                    if script.corrupt_coordinate {
                        for point in &mut points {
                            point["payload"]["coordinate_key"] =
                                serde_json::json!("rewritten-coordinate");
                        }
                    }
                    serde_json::json!({"result": points}).to_string()
                } else {
                    panic!("unexpected Qdrant request: {first}");
                };
                let status = if script.fail_query && first.contains("/points/query") {
                    "503 Service Unavailable"
                } else {
                    "200 OK"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("fixture response");
            }
            lines
        });
        (format!("http://{address}"), server)
    }

    fn request_kinds(lines: &[String]) -> Vec<&'static str> {
        lines
            .iter()
            .map(|line| {
                if line.starts_with("GET ") {
                    "get"
                } else if line.starts_with("PUT ") {
                    "put"
                } else if line.contains("/points/count") {
                    "count"
                } else if line.contains("/points/batch") {
                    "payload"
                } else if line.contains("/points/query") {
                    "query"
                } else if line.contains("/points/delete") {
                    "delete"
                } else if line.contains("/points") {
                    "retrieve"
                } else {
                    "other"
                }
            })
            .collect()
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("request header");
            request.push(byte[0]);
        }
        let header = std::str::from_utf8(&request).expect("HTTP header");
        let content_length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .and_then(|length| length.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; content_length];
        stream.read_exact(&mut body).expect("request body");
        request.extend_from_slice(&body);
        request
    }
}
