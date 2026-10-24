//! Complete canonical-row document embeddings from the already admitted compiler owner.

use super::{
    AdmittedLocalQueryRecipe, LocalQueryRecipeDescriptor, LocalQueryRecipeError, QueryCoordinator,
    QueryError, QueryResult,
};
use backend_compile::{
    EmbeddingExecutableError, EmbeddingMemoryError, EmbeddingMemoryReservation, EmbeddingPurpose,
};
use backend_extension_qdrant as vector;
use backend_library::SemanticSearchReason;
use backend_version::{CoverageWitness, RelationState};
use std::{
    fmt,
    io::{self, Read},
    num::NonZeroUsize,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
#[cfg(test)]
use {super::LocalAnswer, backend_library::SemanticSearchStatus};

const MAX_DESCRIPTOR_BYTES: u64 = 214;
const PAGE_ROWS: usize = 256;

/// Why the local lane refused setup or one complete operation. No string classification.
#[derive(Debug)]
pub enum LocalSemanticError {
    /// No activated compiler runtime/artifact closure exists.
    RuntimeUnavailable(backend_engine::application::EmbeddingRuntimeStatus),
    /// A runtime exists but its explicitly declared query semantics were not installed.
    RecipeNotConfigured,
    /// The owner-private descriptor could not be read safely.
    DescriptorIo(io::Error),
    /// The declared query recipe differs from its activated runtime.
    Recipe(LocalQueryRecipeError),
    /// A selected corpus was empty, incomplete, or disagreed with its complete witness.
    CorpusEnvelope,
    /// The complete allocation cannot be represented by the host or credit counter.
    Representation {
        /// Allocation or counter whose representation refused admission.
        allocation: ProjectionAllocation,
        /// Required bytes before any allocation or inference.
        required: u128,
        /// Largest representable byte extent for that allocation or counter.
        maximum: u128,
    },
    /// Exact canonical row input exceeded the activated text bound or its corpus was invalid.
    Corpus(QueryError),
    /// Shared compiler/query credits refused the entire operation.
    Memory(EmbeddingMemoryError),
    /// Host allocator refused pre-reserved operation descriptors.
    Allocation(std::collections::TryReserveError),
    /// The real supervised embedding invocation failed, including cancellation/deadline.
    Inference(EmbeddingExecutableError),
    /// The complete vector relation or exact score failed typed admission.
    Vector(vector::Error),
    /// Canonical relation construction refused before publication.
    Relation(backend_version::TreeError),
    /// The exact lexical backend refused during controlled worker execution.
    Lexical(backend_extension_tantivy::TantivySourceError),
    /// The bounded owner worker could not start or retired without a complete receipt.
    Worker(LocalSemanticWorkerError),
}

/// Closed failures of the one local semantic execution owner.
#[derive(Debug)]
pub enum LocalSemanticWorkerError {
    /// Thread creation refused before the retained projection owner was handed off.
    Spawn(io::Error),
    /// The worker panicked and returned no complete projection.
    Panicked,
    /// The finished worker returned no complete receipt.
    Stopped,
}
impl fmt::Display for LocalSemanticWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "local semantic worker could not start: {error}"),
            Self::Panicked => f.write_str("local semantic worker panicked"),
            Self::Stopped => f.write_str("local semantic worker stopped without a receipt"),
        }
    }
}
impl std::error::Error for LocalSemanticWorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Panicked | Self::Stopped => None,
        }
    }
}

impl LocalSemanticError {
    /// Bounded actionable setup instruction without private paths or artifact contents.
    #[must_use]
    pub const fn setup_instruction(&self) -> &'static str {
        match self {
            Self::RuntimeUnavailable(_) => {
                "Run nudox embedding-setup with a matched helper package, reopen the local owner, then run nudox add . to reindex under the installed recipe."
            }
            Self::RecipeNotConfigured | Self::Recipe(_) | Self::DescriptorIo(_) => {
                "Run nudox embedding-setup to install matching runtime and query recipe artifacts, reopen the local owner, then run nudox add .; inspect the retained setup refusal if installation fails."
            }
            Self::CorpusEnvelope => {
                "Select a complete coherent view; a partial or sampled projection cannot answer exact search."
            }
            Self::Representation { .. } => {
                "The complete projection exceeds a typed host allocation or credit-counter representation; inspect the required and representable byte extents."
            }
            Self::Corpus(QueryError::SemanticDocumentTooLarge { .. }) => {
                "The exact canonical document exceeds the activated text bound; explicitly provision sufficient per-document input limits or reduce the selection."
            }
            Self::Corpus(_) => {
                "Inspect the typed selected-corpus refusal and select a complete coherent view before retrying."
            }
            Self::Allocation(_) => {
                "Release retained work or reduce the admitted corpus; a local allocation failed before a complete result."
            }
            Self::Memory(_) => {
                "The shared compiler/query embedding credits are exhausted; release retained work or explicitly provision sufficient owner credits."
            }
            Self::Inference(_)
            | Self::Vector(_)
            | Self::Relation(_)
            | Self::Lexical(_)
            | Self::Worker(_) => {
                "Inspect the typed local inference or vector failure before retrying; a partial projection is never available."
            }
        }
    }

    pub(super) const fn reason(&self) -> SemanticSearchReason {
        match self {
            Self::RuntimeUnavailable(
                backend_engine::application::EmbeddingRuntimeStatus::NotConfigured,
            ) => SemanticSearchReason::Unconfigured,
            Self::RuntimeUnavailable(_) => SemanticSearchReason::ModelUnavailable,
            Self::RecipeNotConfigured | Self::Recipe(_) | Self::DescriptorIo(_) => {
                SemanticSearchReason::InvalidConfiguration
            }
            Self::CorpusEnvelope
            | Self::Representation { .. }
            | Self::Corpus(_)
            | Self::Memory(_)
            | Self::Allocation(_)
            | Self::Inference(_)
            | Self::Vector(_)
            | Self::Relation(_)
            | Self::Lexical(_)
            | Self::Worker(_) => SemanticSearchReason::ProviderUnavailable,
        }
    }
}
impl fmt::Display for LocalSemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable(status) => {
                write!(f, "local semantic runtime unavailable: {status:?}")
            }
            Self::RecipeNotConfigured => {
                f.write_str("local semantic query recipe is not configured")
            }
            Self::DescriptorIo(error) => {
                write!(f, "local semantic descriptor read failed: {error}")
            }
            Self::Recipe(error) => error.fmt(f),
            Self::CorpusEnvelope => {
                f.write_str("selected local semantic corpus is empty, incomplete, or inconsistent")
            }
            Self::Representation {
                allocation,
                required,
                maximum,
            } => write!(
                f,
                "local semantic {allocation:?} requires {required} bytes; representation permits {maximum}"
            ),
            Self::Corpus(error) => error.fmt(f),
            Self::Memory(error) => error.fmt(f),
            Self::Allocation(error) => error.fmt(f),
            Self::Inference(error) => error.fmt(f),
            Self::Vector(error) => error.fmt(f),
            Self::Relation(error) => error.fmt(f),
            Self::Lexical(error) => error.fmt(f),
            Self::Worker(error) => error.fmt(f),
        }?;
        write!(f, "; {}", self.setup_instruction())
    }
}
impl std::error::Error for LocalSemanticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorIo(error) => Some(error),
            Self::Recipe(error) => Some(error),
            Self::Corpus(error) => Some(error),
            Self::Memory(error) => Some(error),
            Self::Allocation(error) => Some(error),
            Self::Inference(error) => Some(error),
            Self::Vector(error) => Some(error),
            Self::Relation(error) => Some(error),
            Self::Lexical(error) => Some(error),
            Self::Worker(error) => Some(error),
            Self::RuntimeUnavailable(_)
            | Self::RecipeNotConfigured
            | Self::CorpusEnvelope
            | Self::Representation { .. } => None,
        }
    }
}

impl From<std::collections::TryReserveError> for LocalSemanticError {
    fn from(error: std::collections::TryReserveError) -> Self {
        Self::Allocation(error)
    }
}
impl From<QueryError> for LocalSemanticError {
    fn from(error: QueryError) -> Self {
        Self::Corpus(error)
    }
}
impl From<vector::Error> for LocalSemanticError {
    fn from(error: vector::Error) -> Self {
        Self::Vector(error)
    }
}
impl From<backend_version::TreeError> for LocalSemanticError {
    fn from(error: backend_version::TreeError) -> Self {
        Self::Relation(error)
    }
}
impl From<backend_extension_tantivy::TantivySourceError> for LocalSemanticError {
    fn from(error: backend_extension_tantivy::TantivySourceError) -> Self {
        Self::Lexical(error)
    }
}

/// A configured declaration. Availability requires actual document and query inference.
pub struct LocalSemantic {
    admitted: std::sync::Arc<AdmittedLocalQueryRecipe>,
    active: Option<Projection>,
    failure: Option<LocalSemanticError>,
    #[cfg(test)]
    cpu_control: Option<CpuControl>,
}

struct Projection {
    index: vector::VectorIndex<vector::MemorySource>,
    limits: vector::Limits,
    maximum_row_clone_bytes: usize,
    // Includes canonical payload/slab storage, relation metadata, and ID descriptors.
    _memory: EmbeddingMemoryReservation,
}

impl LocalSemantic {
    pub(super) fn open(
        provision: &backend_engine::application::EmbeddingRuntimeProvision,
        path: &Path,
    ) -> Result<Self, LocalSemanticError> {
        let runtime = provision
            .runtime()
            .ok_or(LocalSemanticError::RuntimeUnavailable(provision.status()))?;
        let addressed = super::local_recipe::recipe_descriptor_path(
            path.parent()
                .ok_or(LocalSemanticError::RecipeNotConfigured)?,
            runtime.execution_identity().recipe(),
        );
        let selected = match std::fs::symlink_metadata(&addressed) {
            Ok(_) => addressed.as_path(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => path,
            Err(error) => return Err(LocalSemanticError::DescriptorIo(error)),
        };
        let descriptor = read_descriptor(selected)?;
        let admitted = descriptor
            .admit(runtime)
            .map_err(LocalSemanticError::Recipe)?;
        Ok(Self {
            admitted: std::sync::Arc::new(admitted),
            active: None,
            failure: None,
            #[cfg(test)]
            cpu_control: None,
        })
    }

    pub(super) fn admitted(&self) -> std::sync::Arc<AdmittedLocalQueryRecipe> {
        std::sync::Arc::clone(&self.admitted)
    }

    pub(super) fn from_admitted(admitted: std::sync::Arc<AdmittedLocalQueryRecipe>) -> Self {
        Self {
            admitted,
            active: None,
            failure: None,
            #[cfg(test)]
            cpu_control: None,
        }
    }

    pub(super) fn refuse(&mut self, error: LocalSemanticError) {
        self.active = None;
        self.failure = Some(error);
    }

    /// Query-only resource/producer failures do not invalidate a complete selected relation.
    /// Cancellation/stale selection and structural failures still retire it.
    pub(super) fn record_query_failure(&mut self, error: LocalSemanticError) {
        let keep = matches!(
            &error,
            LocalSemanticError::Worker(LocalSemanticWorkerError::Spawn(_))
                | LocalSemanticError::Memory(_)
                | LocalSemanticError::Allocation(_)
                | LocalSemanticError::Inference(EmbeddingExecutableError::Memory(_))
                | LocalSemanticError::Inference(EmbeddingExecutableError::TextLimit { .. })
                | LocalSemanticError::Inference(EmbeddingExecutableError::Terminal(_))
                | LocalSemanticError::Inference(
                    EmbeddingExecutableError::InferenceAdmissionTimeout
                )
                | LocalSemanticError::Inference(EmbeddingExecutableError::Process(
                    backend_compile::ProcessError::Deadline
                        | backend_compile::ProcessError::SpawnFailure
                        | backend_compile::ProcessError::Io
                        | backend_compile::ProcessError::SupervisorCapacity
                ))
        );
        if !keep {
            self.active = None;
        }
        self.failure = Some(error);
    }

    /// Called only by the bounded owner worker. The output retains its query allocation lease.
    pub(super) fn execute_query(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        seed: super::local::LexicalSeed,
        text: &str,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<(QueryResult, EmbeddingMemoryReservation), LocalSemanticError> {
        checkpoint(cancelled, deadline)?;
        seed.admit_selection(coordinator)?;
        let (vector, memory, deadline) = self.prepare_and_query(
            coordinator,
            coverage,
            text,
            cancelled,
            seed.limit(),
            deadline,
        )?;
        // The selected lexical stream ran once on the initial page. Restore
        // only its bounded canonical rows under this query's existing lease.
        let (local, _seed_memory) =
            seed.restore(coordinator, &mut || checkpoint(cancelled, deadline))?;
        let active = self
            .active
            .as_ref()
            .ok_or(LocalSemanticError::CorpusEnvelope)?;
        let result = local
            .accelerate_exact_memory(&active.index, &vector, active.limits, || {
                checkpoint(cancelled, deadline)
            })
            .map_err(|(_, error)| error)?;
        self.failure = None;
        Ok((result, memory))
    }

    pub(super) fn recipe(&self) -> vector::EmbeddingRecipe {
        self.admitted.recipe()
    }
    pub(super) fn failure(&self) -> Option<&LocalSemanticError> {
        self.failure.as_ref()
    }
    pub(super) fn has_projection(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn inventory_status(&self) -> backend_engine::CapabilityStatus {
        let execution = self.admitted.execution_identity();
        let recipe = super::remote::embedding_capability_recipe_with_input(
            self.recipe(),
            execution.maximum_text_bytes(),
        );
        let id = backend_engine::CapabilityId::new(recipe.identity().as_bytes());
        let family = backend_engine::CapabilityFamily::Embedding {
            recipe: Some(recipe.identity()),
        };
        if self.failure.is_some() && self.active.is_none() {
            return backend_engine::CapabilityStatus::unavailable(
                id,
                family,
                backend_engine::CapabilityUnavailable::ProbeFailed,
            );
        }
        let abi = match self.admitted.runtime().batch_protocol() {
            backend_compile::EmbeddingBatchProtocol::SingleV1 => 1,
            backend_compile::EmbeddingBatchProtocol::BatchV2
            | backend_compile::EmbeddingBatchProtocol::PersistentBatchV2 => 2,
        };
        backend_engine::CapabilityStatus::observed(
            id,
            family,
            execution.recipe(),
            backend_engine::CapabilityTarget::Native {
                os: super::remote::target_os(),
                architecture: super::remote::target_architecture(),
            },
            abi,
            backend_engine::CapabilityAuthority::Embedding(recipe),
            if self.has_projection() {
                backend_engine::CapabilityLifecycle::Ready
            } else {
                backend_engine::CapabilityLifecycle::Installed
            },
        )
    }

    #[cfg(test)]
    pub(super) fn search(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        local: LocalAnswer,
        text: &str,
        cancelled: &AtomicBool,
    ) -> (QueryResult, SemanticSearchStatus) {
        let result = self
            .admitted
            .runtime()
            .inference_deadline()
            .map_err(LocalSemanticError::Inference)
            .and_then(|deadline| {
                self.prepare_and_query(
                    coordinator,
                    coverage,
                    text,
                    cancelled,
                    local.query.limit(),
                    deadline,
                )
            });
        match result {
            Ok((query, _query_memory, deadline)) => {
                let Some(active) = self.active.as_ref() else {
                    return (
                        local.finish(),
                        SemanticSearchStatus::Unavailable {
                            reason: SemanticSearchReason::NoActiveProjection,
                        },
                    );
                };
                match local.accelerate_exact_memory(&active.index, &query, active.limits, || {
                    checkpoint(cancelled, deadline)
                }) {
                    Ok(result) => {
                        self.failure = None;
                        (result, SemanticSearchStatus::Available)
                    }
                    Err((local, error)) => {
                        let reason = error.reason();
                        self.active = None;
                        self.failure = Some(error);
                        (local.finish(), SemanticSearchStatus::Unavailable { reason })
                    }
                }
            }
            Err(error) => {
                let reason = error.reason();
                self.active = None;
                self.failure = Some(error);
                (local.finish(), SemanticSearchStatus::Unavailable { reason })
            }
        }
    }

    fn prepare_and_query(
        &mut self,
        coordinator: &QueryCoordinator,
        coverage: CoverageWitness,
        text: &str,
        cancelled: &AtomicBool,
        retained_rows: usize,
        deadline: Instant,
    ) -> Result<(vector::QueryVector, EmbeddingMemoryReservation, Instant), LocalSemanticError>
    {
        let runtime = self.admitted.runtime();
        checkpoint(cancelled, deadline)?;
        if !matches!(coverage, CoverageWitness::Complete(_)) {
            return Err(LocalSemanticError::CorpusEnvelope);
        }
        let recipe = self.admitted.recipe();
        let matches = self.active.as_ref().is_some_and(|projection| {
            coordinator.semantic_binding_matches(&projection.index.binding())
                && projection.index.facts().coverage() == coverage
                && projection.index.binding().recipe == recipe.version()
        });
        if !matches {
            // Drop the old complete projection before asking this same account to admit another.
            self.active = None;
            #[cfg(test)]
            let control = &mut self.cpu_control;
            self.active = Some(build_projection_checked(
                coordinator,
                coverage,
                &self.admitted,
                cancelled,
                deadline,
                &mut |phase| {
                    #[cfg(test)]
                    if let Some(control) = control.as_mut() {
                        control.observe(phase, cancelled, deadline)?;
                    }
                    #[cfg(not(test))]
                    let _ = phase;
                    checkpoint(cancelled, deadline)
                },
            )?);
        }
        checkpoint(cancelled, deadline)?;
        let maximum_row_clone_bytes = self
            .active
            .as_ref()
            .ok_or(LocalSemanticError::CorpusEnvelope)?
            .maximum_row_clone_bytes;
        let query_bytes = u64::from(recipe.dimensions.get())
            .checked_mul(8)
            .and_then(|bytes| {
                bytes.checked_add(
                    u64::try_from(maximum_row_clone_bytes)
                        .ok()?
                        .checked_add(512)?
                        .checked_mul(
                            u64::try_from(retained_rows)
                                .ok()?
                                .checked_mul(2)?
                                .checked_add(256)?,
                        )?,
                )
            })
            .and_then(|bytes| {
                // Merge ownership moves payloads, but its three pre-reserved descriptor
                // allocations coexist. Charge actual layout rather than a per-row guess.
                bytes.checked_add(
                    u64::try_from(size_of::<Option<super::local::RankedRow>>())
                        .ok()?
                        .checked_mul(3)?
                        .checked_mul(u64::try_from(retained_rows).ok()?.checked_add(256)?)?,
                )
            })
            .and_then(|bytes| bytes.checked_add(1024))
            .ok_or(LocalSemanticError::CorpusEnvelope)?;
        // QueryVector's owned slice, exact topK and bounded composition scratch are distinct
        // allocations from the charged producer coordinates.
        let query_memory = runtime
            .memory_budget()
            .reserve(query_bytes)
            .map_err(LocalSemanticError::Memory)?;
        let coordinates = runtime
            .infer_batch_with_cancellation_until(
                EmbeddingPurpose::Query,
                &[text],
                cancelled,
                deadline,
            )
            .map_err(LocalSemanticError::Inference)?;
        let coordinates = coordinates
            .first()
            .ok_or(LocalSemanticError::CorpusEnvelope)?;
        let query = vector::QueryVector::new(recipe, coordinates.values().to_vec())
            .map_err(LocalSemanticError::Vector)?;
        checkpoint(cancelled, deadline)?;
        Ok((query, query_memory, deadline))
    }
}

// CPU phases form one complete operation. The production checkpoint observes the
// same selected-owner flag and absolute deadline as real batch inference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectionPhase {
    TextBounds,
    RowCloneExtent,
    DocumentPage,
    Payload,
    OrderEntries,
    Relation,
    CandidateState,
    Facts,
    Base,
    Source,
    Index,
}

// A test-only bounded gate on the actual owned worker, absent from product state/DTOs.
// Tests use it to set the actual selected cancellation Arc inside a CPU phase.
#[cfg(test)]
struct CpuControl {
    phase: ProjectionPhase,
    stop_at: usize,
    visits: usize,
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
impl CpuControl {
    fn observe(
        &mut self,
        phase: ProjectionPhase,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<(), LocalSemanticError> {
        if phase != self.phase {
            return Ok(());
        }
        self.visits += 1;
        if self.visits != self.stop_at {
            return Ok(());
        }
        self.entered
            .try_send(())
            .map_err(|_| LocalSemanticError::Worker(LocalSemanticWorkerError::Stopped))?;
        match self
            .release
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(()) => checkpoint(cancelled, deadline),
            Err(_) => {
                checkpoint(cancelled, deadline)?;
                Err(LocalSemanticError::Worker(
                    LocalSemanticWorkerError::Stopped,
                ))
            }
        }
    }
}

#[cfg(test)]
fn build_projection(
    coordinator: &QueryCoordinator,
    coverage: CoverageWitness,
    admitted: &AdmittedLocalQueryRecipe,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<Projection, LocalSemanticError> {
    build_projection_checked(
        coordinator,
        coverage,
        admitted,
        cancelled,
        deadline,
        &mut |_| checkpoint(cancelled, deadline),
    )
}

/// Concrete representation boundaries, rather than a maximum project size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionAllocation {
    /// A single encoded float32 vector is a contiguous Vec allocation.
    VectorPayload,
    /// The sorted complete input descriptors are one contiguous Vec allocation.
    EntryDescriptors,
    /// The extension's complete payload counter uses usize.
    PayloadCounter,
    /// Shared embedding memory credits use u64.
    CreditCounter,
}

struct ProjectionEnvelope {
    limits: vector::Limits,
    credits: u64,
}

impl ProjectionEnvelope {
    fn new(count: usize, dimensions: u32) -> Result<Self, LocalSemanticError> {
        fn represent(
            allocation: ProjectionAllocation,
            required: u128,
            maximum: u128,
        ) -> Result<u128, LocalSemanticError> {
            if required > maximum {
                return Err(LocalSemanticError::Representation {
                    allocation,
                    required,
                    maximum,
                });
            }
            Ok(required)
        }
        // count is usize and dimensions is u32, so these products fit u128.
        let payload = represent(
            ProjectionAllocation::VectorPayload,
            u128::from(dimensions) * 4 + 4,
            isize::MAX as u128,
        )?;
        represent(
            ProjectionAllocation::EntryDescriptors,
            count as u128 * size_of::<(u64, Vec<u8>)>() as u128,
            isize::MAX as u128,
        )?;
        let total = represent(
            ProjectionAllocation::PayloadCounter,
            count as u128 * payload,
            usize::MAX as u128,
        )?;
        // Prospective construction credits include both owned payloads and canonical
        // encoding, plus the existing conservative descriptor/tree construction allowance.
        // This is accounting, not an assertion about allocator overhead or measured RSS.
        let credits = represent(
            ProjectionAllocation::CreditCounter,
            count as u128 * (payload * 2 + 512) + 1024,
            u128::from(u64::MAX),
        )?;
        Ok(Self {
            limits: vector::Limits {
                max_candidates: count.max(PAGE_ROWS),
                max_tombstones: count.max(PAGE_ROWS),
                // Checked above, before these narrowing conversions.
                max_payload_bytes: usize::try_from(payload)
                    .map_err(|_| LocalSemanticError::CorpusEnvelope)?,
                max_total_payload_bytes: usize::try_from(total)
                    .map_err(|_| LocalSemanticError::CorpusEnvelope)?,
                max_page: PAGE_ROWS,
            },
            credits: u64::try_from(credits).map_err(|_| LocalSemanticError::CorpusEnvelope)?,
        })
    }
}

fn build_projection_checked(
    coordinator: &QueryCoordinator,
    coverage: CoverageWitness,
    admitted: &AdmittedLocalQueryRecipe,
    cancelled: &AtomicBool,
    deadline: Instant,
    check: &mut impl FnMut(ProjectionPhase) -> Result<(), LocalSemanticError>,
) -> Result<Projection, LocalSemanticError> {
    let count = coordinator.semantic_document_count();
    if count == 0 || !matches!(coverage, CoverageWitness::Complete(_)) {
        return Err(LocalSemanticError::CorpusEnvelope);
    }
    check(ProjectionPhase::TextBounds)?;
    let runtime = admitted.runtime();
    coordinator.admit_semantic_text_bounds_controlled(runtime.maximum_text_bytes(), &mut || {
        check(ProjectionPhase::TextBounds)
    })?;
    let maximum_row_clone_bytes = coordinator
        .semantic_row_clone_extent_controlled(&mut || check(ProjectionPhase::RowCloneExtent))?;
    check(ProjectionPhase::DocumentPage)?;
    let recipe = admitted.recipe();
    // Coverage never depends on a project-size constant. The entire selected relation
    // is admitted against representable storage and the configured shared credit account.
    let envelope = ProjectionEnvelope::new(count, recipe.dimensions.get())?;
    let memory = runtime
        .memory_budget()
        .reserve(envelope.credits)
        .map_err(LocalSemanticError::Memory)?;
    let limits = envelope.limits;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(LocalSemanticError::Allocation)?;
    let rows = NonZeroUsize::new(PAGE_ROWS).ok_or(LocalSemanticError::CorpusEnvelope)?;
    let mut pages = coordinator
        .bounded_semantic_document_pages(rows, runtime.maximum_text_bytes())
        .map_err(LocalSemanticError::Corpus)?;
    while let Some((rows, text_bytes)) = pages
        .next_page_extent()
        .map_err(LocalSemanticError::Corpus)?
    {
        check(ProjectionPhase::DocumentPage)?;
        // Exact page text and bounded document/ref metadata are charged before next() allocates.
        let page_bytes = rows
            .checked_mul(128)
            .and_then(|bytes| bytes.checked_add(text_bytes))
            .ok_or(LocalSemanticError::CorpusEnvelope)?;
        let _page_memory = runtime
            .memory_budget()
            .reserve(u64::try_from(page_bytes).map_err(|_| LocalSemanticError::CorpusEnvelope)?)
            .map_err(LocalSemanticError::Memory)?;
        let page = pages.next().ok_or(LocalSemanticError::CorpusEnvelope)?;
        let page = page.map_err(LocalSemanticError::Corpus)?;
        let texts = page
            .iter()
            .map(|document| document.text.as_str())
            .collect::<Vec<_>>();
        let coordinates = runtime
            .infer_batch_with_cancellation_until(
                EmbeddingPurpose::Document,
                &texts,
                cancelled,
                deadline,
            )
            .map_err(LocalSemanticError::Inference)?;
        if coordinates.len() != page.len() {
            return Err(LocalSemanticError::CorpusEnvelope);
        }
        for (document, coordinates) in page.iter().zip(coordinates) {
            check(ProjectionPhase::Payload)?;
            let id = coordinator
                .semantic_candidate(document.row)
                .ok_or(LocalSemanticError::CorpusEnvelope)?;
            let vector =
                vector::DocumentVector::from_owned_values(recipe, id, coordinates.shared_values())
                    .map_err(LocalSemanticError::Vector)?;
            entries.push((id.0, vector.point().to_payload()));
        }
    }
    if entries.len() != count {
        return Err(LocalSemanticError::CorpusEnvelope);
    }
    let entries = super::controlled_sort::sort_checked(entries, |entry| entry.0, &mut || {
        check(ProjectionPhase::OrderEntries)
    })?;
    let relation = RelationState::<vector::CandidateRelation>::from_sorted_entries_controlled(
        entries,
        coverage,
        &mut || check(ProjectionPhase::Relation),
    )?;
    let binding = coordinator.semantic_binding(relation.root(), recipe.version());
    let state =
        vector::CandidateState::from_relation_controlled(binding, relation, limits, &mut || {
            check(ProjectionPhase::CandidateState)
        })?;
    let facts = vector::VectorFacts::from_recipe_controlled(state, recipe, &mut || {
        check(ProjectionPhase::Facts)
    })?;
    let base = vector::AnnBase::from_facts_controlled(
        &facts,
        vector::SearchQuality::Exact,
        limits,
        &mut || check(ProjectionPhase::Base),
    )?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(count)
        .map_err(LocalSemanticError::Allocation)?;
    for id in facts.ids() {
        check(ProjectionPhase::Source)?;
        ids.push(id);
    }
    let source = vector::MemorySource::from_sorted_ids_controlled(
        binding,
        coverage,
        ids,
        limits,
        &mut || check(ProjectionPhase::Source),
    )?;
    let index =
        vector::VectorIndex::new(base, facts, source).map_err(LocalSemanticError::Vector)?;
    check(ProjectionPhase::Index)?;
    Ok(Projection {
        index,
        limits,
        maximum_row_clone_bytes,
        _memory: memory,
    })
}

pub(super) fn checkpoint(
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<(), LocalSemanticError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled),
        ));
    }
    if Instant::now() >= deadline {
        return Err(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Deadline),
        ));
    }
    Ok(())
}

fn read_descriptor(path: &Path) -> Result<LocalQueryRecipeDescriptor, LocalSemanticError> {
    let parent = path
        .parent()
        .ok_or(LocalSemanticError::RecipeNotConfigured)?;
    std::fs::symlink_metadata(parent).map_err(LocalSemanticError::DescriptorIo)?;
    let mut file = match backend_platform::durable::open_private_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::symlink_metadata(parent).map_err(LocalSemanticError::DescriptorIo)?;
            return Err(LocalSemanticError::RecipeNotConfigured);
        }
        Err(error) => return Err(LocalSemanticError::DescriptorIo(error)),
    };
    let metadata = file.metadata().map_err(LocalSemanticError::DescriptorIo)?;
    if !metadata.is_file() || metadata.len() != MAX_DESCRIPTOR_BYTES {
        return Err(LocalSemanticError::Recipe(
            LocalQueryRecipeError::DescriptorFormat,
        ));
    }
    let mut bytes = [0u8; MAX_DESCRIPTOR_BYTES as usize];
    file.read_exact(&mut bytes)
        .map_err(LocalSemanticError::DescriptorIo)?;
    let mut trailing = [0];
    if file
        .read(&mut trailing)
        .map_err(LocalSemanticError::DescriptorIo)?
        != 0
    {
        return Err(LocalSemanticError::Recipe(
            LocalQueryRecipeError::DescriptorFormat,
        ));
    }
    LocalQueryRecipeDescriptor::decode(&bytes).map_err(LocalSemanticError::Recipe)
}

#[cfg(test)]
#[path = "local_semantic_tests.rs"]
pub(crate) mod tests;
