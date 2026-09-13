//! Optional production Qdrant configuration and activation.
//!
//! Configuration is pure and bounded. Network I/O occurs only in
//! [`ConfiguredQdrant::activate`], after the embedding owner supplies exact
//! document coordinates. A missing or invalid optional configuration remains
//! an observable unavailable state and never prevents local lexical queries.

use super::{LocalAnswer, QueryCoordinator, QueryResult, SemanticAcceleration, SemanticDocument};
use backend_engine::RowId;
use backend_extension_qdrant as qdrant;
use backend_version::{CoverageWitness, RelationState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::mem::size_of;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
const EMBEDDING_DEADLINE: Duration = Duration::from_secs(5);
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
    producer: Option<EmbeddingProducer>,
    producer_health: ProducerHealth,
    document_embeddings: BTreeMap<DocumentEmbeddingId, Arc<[f32]>>,
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
        let producer = EmbeddingProducer::from_environment(dimensions)?;
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
        Ok(Some(Self {
            client,
            recipe,
            producer: Some(producer),
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
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
        let vectors = self.admit_documents(coordinator, documents)?;
        let (binding, facts) =
            vector_facts(coordinator, coverage, self.recipe, &vectors, envelope)?;
        let base = qdrant::AnnBase::from_facts(&facts, qdrant::SearchQuality::Exact, envelope.0)
            .map_err(RemoteConfigError::Extension)?;
        self.client
            .ensure_collection()
            .map_err(RemoteConfigError::Provider)?;
        self.client
            .upsert(binding, &vectors)
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
            ids: vectors
                .iter()
                .map(|vector| vector.point().id())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
    }

    fn reconcile(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
    ) -> Result<(), RemoteConfigError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.matches(coordinator))
        {
            return self.retire_pending();
        }
        // A failed retirement is retried before another generation can be
        // admitted. This keeps cleanup debt bounded to one fully identified
        // projection even while the remote service is unhealthy.
        self.retire_pending()?;
        ProjectionEnvelope::for_recipe(self.recipe)?
            .admit(coordinator.semantic_document_count())?;
        let documents = self.embed_documents(coordinator.semantic_documents())?;
        let replacement = self.activate(coordinator, coverage, documents)?;
        if let Some(previous) = self.active.replace(replacement) {
            debug_assert!(self.retired.is_none());
            self.retired = Some(previous.into_retired());
        }
        self.retire_pending()
    }

    fn retire_pending(&mut self) -> Result<(), RemoteConfigError> {
        if let Some(retired) = self.retired.as_ref() {
            self.client
                .delete(retired.binding, &retired.ids)
                .map_err(RemoteConfigError::Provider)?;
            self.retired = None;
        }
        Ok(())
    }

    fn embed_documents(
        &mut self,
        documents: &[SemanticDocument],
    ) -> Result<Vec<QdrantDocument>, RemoteConfigError> {
        let producer = self
            .producer
            .as_ref()
            .ok_or(RemoteConfigError::ProducerUnavailable)?;
        let planned = documents
            .iter()
            .map(|document| {
                DocumentEmbeddingId::new(self.recipe.version(), &document.text)
                    .map(|identity| (document, identity))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let live = planned
            .iter()
            .map(|(_, identity)| *identity)
            .collect::<BTreeSet<_>>();
        let mut pending = BTreeMap::new();
        let mut embedded = Vec::with_capacity(planned.len());
        for (document, identity) in planned {
            let coordinates = if let Some(coordinates) = self.document_embeddings.get(&identity) {
                Arc::clone(coordinates)
            } else {
                let coordinates: Arc<[f32]> = producer
                    .embed(EmbeddingTreatment::Document, &document.text)?
                    .into();
                pending.insert(identity, Arc::clone(&coordinates));
                coordinates
            };
            embedded.push(QdrantDocument {
                row: document.row,
                coordinates,
            });
        }
        self.document_embeddings
            .retain(|identity, _| live.contains(identity));
        self.document_embeddings.extend(pending);
        Ok(embedded)
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
        let Some(active) = self
            .active
            .as_ref()
            .filter(|active| active.matches(coordinator) && active.coverage() == coverage)
        else {
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
    fn new(recipe: qdrant::Recipe, text: &str) -> Result<Self, RemoteConfigError> {
        let length = u64::try_from(text.len()).map_err(|_| RemoteConfigError::EmbeddingInput)?;
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.embedding.document.v1\0");
        identity.update(recipe.as_bytes());
        identity.update(&length.to_be_bytes());
        identity.update(text.as_bytes());
        Ok(Self(*identity.finalize().as_bytes()))
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
        let program_identity = *blake3::hash(&program_bytes).as_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.embedding.producer.v1\0");
        hasher.update(&program_bytes);
        hasher.update(&model_bytes);
        hasher.update(&tokenizer_bytes);
        hasher.update(&EMBEDDING_PROTOCOL_ABI.to_be_bytes());
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
        })
    }

    fn model_bytes(&self) -> &[u8; 32] {
        &self.model_identity
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
            EMBEDDING_PROTOCOL_ABI,
            backend_engine::CapabilityAuthority::Embedding(capability_recipe),
            lifecycle,
        )
    }

    fn embed(
        &self,
        treatment: EmbeddingTreatment,
        text: &str,
    ) -> Result<Vec<f32>, RemoteConfigError> {
        if text.is_empty() || text.len() > MAX_EMBEDDING_TEXT_BYTES {
            return Err(RemoteConfigError::EmbeddingInput);
        }
        self.verify_artifacts()?;
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
        self.verify_artifacts()?;
        Ok(response.values)
    }
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
}

/// Verified live Qdrant source paired with exact local vector facts.
pub struct ActiveQdrant {
    recipe: qdrant::EmbeddingRecipe,
    index: qdrant::VectorIndex<qdrant::QdrantHttpSource>,
    ids: Box<[qdrant::CandidateId]>,
}

impl ActiveQdrant {
    fn matches(&self, coordinator: &QueryCoordinator) -> bool {
        coordinator.semantic_binding_matches(&self.index.binding())
    }

    fn coverage(&self) -> CoverageWitness {
        self.index.facts().coverage()
    }

    fn into_retired(self) -> RetiredQdrant {
        RetiredQdrant {
            binding: self.index.binding(),
            ids: self.ids,
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
    binding: qdrant::Binding,
    ids: Box<[qdrant::CandidateId]>,
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

    const CONFIG: [(&str, &str); 5] = [
        (QDRANT_ENDPOINT_ENV, "https://qdrant.invalid"),
        (QDRANT_COLLECTION_ENV, "backend-test"),
        (EMBEDDING_DIMENSIONS_ENV, "2"),
        (EMBEDDING_QUERY_TREATMENT_ENV, "query-v1"),
        (EMBEDDING_DOCUMENT_TREATMENT_ENV, "document-v1"),
    ];

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
            })
            .collect::<Vec<_>>();
        let (endpoint, server) = qdrant_fixture(documents.len(), target_index);
        let recipe = test_recipe();
        let client =
            qdrant::QdrantHttpClient::new(test_transport(endpoint), recipe).expect("HTTP client");
        let configured = ConfiguredQdrant {
            client,
            recipe,
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
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
        server.join().expect("fixture server");
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
        let vector = qdrant::DocumentVector::new(recipe, candidate, vec![1.0, 0.0])
            .expect("document vector");
        let envelope = ProjectionEnvelope::for_recipe(recipe)
            .and_then(|envelope| envelope.admit(1))
            .expect("projection envelope");
        let (binding, _) = vector_facts(&coordinator, coverage, recipe, &[vector], envelope)
            .expect("vector binding");
        let client =
            qdrant::QdrantHttpClient::new(test_transport(format!("http://{address}")), recipe)
                .expect("HTTP client");
        let mut configured = ConfiguredQdrant {
            client,
            recipe,
            producer: None,
            producer_health: ProducerHealth::Ready,
            document_embeddings: BTreeMap::new(),
            active: None,
            retired: Some(RetiredQdrant {
                binding,
                ids: vec![candidate].into_boxed_slice(),
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

    fn qdrant_fixture(
        expected_points: usize,
        selected_point: usize,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let metadata =
            r#"{"result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}}}}"#;
        let server = thread::spawn(move || {
            let mut projected_point = None;
            for request_index in 0..6 {
                let (mut stream, _) = listener.accept().expect("fixture connection");
                let request = read_request(&mut stream);
                if request_index == 2 {
                    let body_start = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|offset| offset + 4)
                        .expect("upsert body separator");
                    let value: serde_json::Value = serde_json::from_slice(&request[body_start..])
                        .expect("upsert request JSON");
                    projected_point = value["points"].as_array().and_then(|points| {
                        points.get(selected_point).map(|point| {
                            serde_json::json!({
                                "id": point["id"].clone(),
                                "payload": point["payload"].clone()
                            })
                        })
                    });
                }
                let owned;
                let body = match request_index {
                    0 | 1 => metadata,
                    2 => r#"{"result":{"status":"completed"}}"#,
                    3 => {
                        owned =
                            serde_json::json!({"result": {"count": expected_points}}).to_string();
                        &owned
                    }
                    4 | 5 => {
                        owned = serde_json::json!({
                            "result": { "points": [projected_point.clone().expect("upsert point")] }
                        })
                        .to_string();
                        &owned
                    }
                    _ => unreachable!(),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("fixture response");
            }
        });
        (format!("http://{address}"), server)
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
