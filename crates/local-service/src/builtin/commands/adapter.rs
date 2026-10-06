use super::super::product_state::CatalogLookupIndex;
use super::super::registry::{
    CatalogDependencyRevision, CatalogProjection, RegistryAddError, StagedProject,
};
use super::super::{
    BuiltinIntent, BuiltinModel, BuiltinModelError, BuiltinSemanticChange, BuiltinSemanticRelation,
    Command, CommandReply, ForgeGateway, ProductDaemon, RegistryGateway, WireCertificate,
    WireClaim, WorkspaceModel, capture_basis_for_snapshot, ingest, projection,
    publish_builtin_view,
};
use super::browse_lane::{BrowseLane, Terminal as BrowseTerminal};
use super::diff::execute_semantic_diff;
use super::graph::{execute_certified_graph_query, execute_search};
use super::index::{
    DeferredIndex, DeferredProfileFailure, DeferredProfileTicket, IndexScanFailure,
    IndexScanResult, IndexScanWork, PreparedIndex, PreparedProductSelection, capture_index_scan,
    commit_pending_capture_failure, deferred_compile_was_cancelled, finish_deferred_index,
    finish_deferred_profile, finish_index_scan, index_project_intent_at,
    index_project_intent_with_cluster_and_intent, package_source_membership_page,
    remove_project_intent, run_deferred_compile, run_index_scan, semantic_version_record,
    semantic_versions,
};
use super::index_operation::{
    Acceptance as IndexOperationAcceptance, IndexOperationJournal, JournalEntry,
    JournalError as IndexOperationJournalError, StoredOperation, StoredOperationState,
};
use super::semantic_query::{
    execute_references, execute_semantic_graph, execute_structural_call_graph,
};
use super::semantic_shapes::execute_semantic_shapes;
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticSourceCapture,
};
use backend_library::CompileExecutionIntent;
use backend_library::interface::PackageUrl;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_RETAINED_INDEX_TERMINALS: usize = 64;
const MAX_RETAINED_INDEX_PROGRESS_EVENTS: usize = 256;
const MAX_RETAINED_INDEX_PROGRESS_TICKETS: usize = MAX_RETAINED_INDEX_TERMINALS + 1;
const MAX_WAITING_COMMANDS: usize = 64;

fn new_index_owner_epoch() -> [u8; 16] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-service.index-owner-epoch.v1\0");
    hasher.update(&std::process::id().to_be_bytes());
    hasher.update(&nanos.to_be_bytes());
    let digest = hasher.finalize();
    let mut epoch = [0_u8; 16];
    epoch.copy_from_slice(&digest.as_bytes()[..16]);
    epoch
}

type AdmittedReply = (CommandReply, Option<WireCertificate>);
type RegistryAcquisitionReceiver = std::sync::mpsc::Receiver<RegistryAcquisitionMessage>;

enum RegistryAcquisitionMessage {
    Staging,
    Complete {
        gateway: Option<RegistryGateway>,
        result: Result<StagedProject, RegistryAddError>,
    },
}

fn bounded_index_detail(value: impl std::fmt::Display) -> backend_library::ProductText {
    let mut value = value.to_string().replace('\0', " ");
    if value.len() > backend_library::MAX_PRODUCT_TEXT_BYTES {
        let mut boundary = backend_library::MAX_PRODUCT_TEXT_BYTES;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    backend_library::ProductText::new(value)
        .unwrap_or_else(|_| backend_library::ProductText::from_static("index job failed"))
}

fn refused_index_outcome(
    detail: impl std::fmt::Display,
    compiler_failure: Option<backend_library::PackageCompilerFailure>,
) -> backend_library::IndexJobOutcome {
    let detail = bounded_index_detail(detail);
    match compiler_failure {
        Some(failure) => {
            backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, failure }
        }
        None => backend_library::IndexJobOutcome::Refused(detail),
    }
}

fn capture_terminalization_failed(
    outcome: backend_library::IndexJobOutcome,
    error: impl std::fmt::Display,
) -> backend_library::IndexJobOutcome {
    let primary = match &outcome {
        backend_library::IndexJobOutcome::Published => "semantic publication completed".to_owned(),
        backend_library::IndexJobOutcome::Refused(detail)
        | backend_library::IndexJobOutcome::Failed(detail) => detail.as_str().to_owned(),
        backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, .. } => {
            detail.as_str().to_owned()
        }
        backend_library::IndexJobOutcome::Cancelled => "index job was cancelled".to_owned(),
    };
    let detail = bounded_index_detail(format!(
        "{primary}; additionally, recording or reconciling the terminal source-capture outcome failed: {error}"
    ));
    match outcome {
        backend_library::IndexJobOutcome::RefusedWithCompilerFailure { failure, .. } => {
            backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, failure }
        }
        _ => backend_library::IndexJobOutcome::Failed(detail),
    }
}

fn deferred_profile_refused_outcome(
    refusal: DeferredProfileFailure,
) -> backend_library::IndexJobOutcome {
    match refusal.compiler_failure {
        Some(failure) => {
            // The semantic summary is the bounded, source-bound explanation.
            // Do not copy the broader compiler error chain into persisted or
            // user-visible text: it can contain arbitrary tool output.
            let detail = format!(
                "local compiler rejected {} with {}: {}",
                failure.relative_path(),
                failure.kind_tag(),
                failure.detail(),
            );
            refused_index_outcome(detail, Some(failure))
        }
        None => refused_index_outcome(refusal.detail, None),
    }
}

fn legacy_add_compiler_failure(
    outcome: &backend_library::IndexJobOutcome,
) -> Option<backend_library::CommandFailure> {
    let backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, failure } = outcome
    else {
        return None;
    };
    Some(backend_library::CommandFailure::CompilerRefused {
        detail: detail.as_str().to_owned(),
        failure: failure.clone(),
    })
}

fn index_operation_failure(
    outcome: Option<&backend_library::IndexJobOutcome>,
) -> (
    backend_library::IndexOperationFailureReason,
    backend_library::ProductText,
    Option<backend_library::PackageCompilerFailure>,
) {
    match outcome {
        Some(backend_library::IndexJobOutcome::Cancelled) => (
            backend_library::IndexOperationFailureReason::Cancelled,
            backend_library::ProductText::from_static(
                "index operation was cancelled before commit",
            ),
            None,
        ),
        Some(backend_library::IndexJobOutcome::Refused(detail)) => (
            backend_library::IndexOperationFailureReason::Refused,
            detail.clone(),
            None,
        ),
        Some(backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, failure }) => (
            backend_library::IndexOperationFailureReason::Refused,
            detail.clone(),
            Some(failure.clone()),
        ),
        Some(backend_library::IndexJobOutcome::Failed(detail)) => (
            backend_library::IndexOperationFailureReason::WorkerFailed,
            detail.clone(),
            None,
        ),
        Some(backend_library::IndexJobOutcome::Published) => (
            backend_library::IndexOperationFailureReason::WorkerFailed,
            backend_library::ProductText::from_static(
                "semantic publication was reported while a captured profile remains Pending",
            ),
            None,
        ),
        None => (
            backend_library::IndexOperationFailureReason::WorkerFailed,
            backend_library::ProductText::from_static(
                "owner restarted or stopped before the exact commit was selected",
            ),
            None,
        ),
    }
}

fn pending_capture_unresolved(
    outcome: Option<&backend_library::IndexJobOutcome>,
) -> (
    backend_library::IndexOperationUnresolvedReason,
    backend_library::ProductText,
) {
    match outcome {
        Some(outcome) => {
            let (_, primary, _) = index_operation_failure(Some(outcome));
            (
                backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                bounded_index_detail(format!(
                    "the live index job reached a terminal outcome ({}) but its durable source-capture receipt remains Pending",
                    primary.as_str()
                )),
            )
        }
        None => (
            backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture,
            backend_library::ProductText::from_static(
                "the exact source root is durable, but the owner restarted before recording a semantic worker result",
            ),
        ),
    }
}

fn index_operation_published_observation(
    entry: &StoredOperation,
    operation_key: backend_library::IndexOperationKey,
    receipt: backend_library::IndexOperationPublicationReceipt,
) -> backend_library::IndexOperationObservation {
    backend_library::IndexOperationObservation::Known(
        backend_library::IndexOperationStatus::new(
            operation_key,
            entry.package.clone(),
            entry.execution_intent,
            backend_library::IndexOperationState::Published(receipt),
        )
        .with_source_capture(entry.source_capture.clone()),
    )
}

fn index_attempt_retirement_reason(
    outcome: &backend_library::IndexJobOutcome,
) -> Option<backend_extension_turso::CandidateAttemptRetirementReason> {
    use backend_extension_turso::CandidateAttemptRetirementReason as R;
    match outcome {
        backend_library::IndexJobOutcome::Published => None,
        backend_library::IndexJobOutcome::Refused(_)
        | backend_library::IndexJobOutcome::RefusedWithCompilerFailure { .. } => Some(R::Refused),
        backend_library::IndexJobOutcome::Cancelled => Some(R::Cancelled),
        backend_library::IndexJobOutcome::Failed(_) => Some(R::Failed),
    }
}

const ADD_TARGET_REQUIRED: &str =
    "add target must be an admitted local directory or version-pinned package URL";

#[derive(Debug)]
enum AddTarget {
    LocalDirectory,
    PackageUrl,
}

/// Classifies an add label before any project record is built.
///
/// A directory is indexed in place. A version-pinned package URL goes through
/// registry acquisition. A file, a symlink to a file, or a missing path is
/// refused here so it cannot become an empty project record.
fn classify_add_target(label: &str) -> Result<AddTarget, BuiltinModelError> {
    if Path::new(label).is_dir() {
        Ok(AddTarget::LocalDirectory)
    } else if label.starts_with("pkg:") || label.starts_with("PKG:") {
        Ok(AddTarget::PackageUrl)
    } else {
        Err(BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))
    }
}

pub(in crate::builtin) struct CommandAdapter {
    sql_projection: backend_extension_turso::TursoProjection,
    registry: Option<RegistryGateway>,
    forge: ForgeGateway,
    discovery: Option<crate::discovery::DiscoveryGateway>,
    product_state: super::super::ProductState,
    index_operations: IndexOperationJournal,
    compiler: LocalCompilerClient,
    search_snapshots: super::super::query::SearchSnapshotOwner,
    remote_semantic: super::super::query::RemoteSemantic,
    pending_semantic_search: Option<backend_library::SemanticSearchStatus>,
    published: Option<super::super::view_publish::PublishedRoots>,
    manifests: super::super::local_manifest::LocalManifestResidence,
    project_roots: super::super::project_root_residence::ProjectRootResidence,
    image_rows: super::super::view_build::ImageRowResidence,
    generations: super::super::generation_residence::SemanticGenerationResidence,
    semantic_authority: super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<Arc<super::super::cluster_dispatch::OwnerCompilerClusterRuntime>>,
    pending_stored_acks: Option<Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
    dependencies: Option<ResidentDependencies>,
    graph_limits: backend_library::PackageGraphIndexLimits,
    browse: super::super::browse::BrowseCache,
    browse_lane: BrowseLane,
    /// The index job whose compile runs off the owner loop, if one does.
    indexing: Option<IndexJob>,
    /// Recently completed owner-issued index tickets, for late await/cancel requests.
    index_terminals: std::collections::VecDeque<backend_library::IndexJobTerminal>,
    /// Bounded typed progress stream retained across the active and recent index tickets.
    index_progress: std::collections::VecDeque<backend_library::IndexJobProgressEvent>,
    /// Latest emitted sequence for each retained ticket, including aged-out events.
    index_progress_latest: std::collections::VecDeque<(backend_library::IndexJobTicket, u64)>,
    /// Next owner-local index job identity.
    next_index_ticket: u64,
    /// Process epoch included in every ticket so stale client tickets cannot match after restart.
    index_owner_epoch: [u8; 16],
    /// Commands that change state, waiting for that job: one writer at a
    /// time, in arrival order. Reads never wait here.
    waiting: std::collections::VecDeque<(u64, Vec<u8>)>,
    /// Tickets in `waiting` whose clients stopped waiting. Their commands
    /// remain admitted and will still run, but their eventual response is
    /// discarded without registering another waiter.
    abandoned_replies: BTreeSet<u64>,
}

/// An `Add` of a local folder whose compile runs off the owner loop.
struct IndexJob {
    owner_ticket: backend_library::IndexJobTicket,
    /// Caller-owned durable operation key; legacy starts leave this absent.
    operation_key: Option<backend_library::IndexOperationKey>,
    /// Source captures committed before semantic candidate work began.
    captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    /// Legacy Add request waiting for its committed Added reply.
    legacy_add: Option<(u64, u64)>,
    /// Owner-issued IndexAwait request listeners waiting for this terminal.
    awaiters: Vec<(u64, u64)>,
    /// Cancellation token registered to this exact compiler request only.
    cancelled: Arc<AtomicBool>,
    /// Last sequence emitted for this exact job.
    progress_sequence: u64,
    /// Last stage emitted, used to avoid duplicate stage events.
    progress_stage: Option<backend_library::IndexJobStage>,
    request_id: u64,
    /// Physical package scope used by every committed source capture. The
    /// caller's spelling remains separate for reply correlation.
    captured_package: backend_engine::PackageKey,
    captured_label: String,
    requested_package: backend_engine::PackageKey,
    execution_intent: CompileExecutionIntent,
    /// Retains a verified registry tree through the full job lifetime.
    _staged_project: Option<StagedProject>,
    work: IndexJobWork,
}

enum IndexJobWork {
    Acquiring(RegistryAcquisitionReceiver),
    Scanning(std::sync::mpsc::Receiver<Result<IndexScanResult, IndexScanFailure>>),
    Compiling {
        job: DeferredIndex,
        profile: DeferredProfileTicket,
        compiled: std::sync::mpsc::Receiver<
            Result<
                backend_engine::application::StagedSemanticPackage,
                backend_engine::application::PackageSemanticRuntimeError,
            >,
        >,
    },
    Transition,
}

/// What the owner loop answers while an index job compiles: reads, from the
/// last publication. A command not named here changes state (or might) and
/// waits for the job.
fn answers_while_indexing(command: &Command) -> bool {
    use backend_library::SurfaceCommand as S;
    match command {
        Command::Packages
        | Command::PackagePage(_)
        | Command::Document(_)
        | Command::Source(_)
        | Command::Show { .. }
        | Command::Outline(_)
        | Command::OutlinePage { .. }
        | Command::Name(_)
        | Command::Resolve { .. }
        | Command::Search(_)
        | Command::Graph(_)
        | Command::Related(_)
        | Command::GraphPage { .. }
        | Command::GraphQuery(_)
        | Command::SemanticShapes(_)
        | Command::Health
        | Command::Revision => true,
        Command::Surface(surface) => matches!(
            surface,
            S::Read { .. }
                | S::References { .. }
                | S::Diff { .. }
                | S::Explore { .. }
                | S::Package { .. }
                | S::Dependents { .. }
                | S::Dependencies { .. }
                | S::PackageGraphPage { .. }
                | S::Owner { .. }
                | S::IndexSearch { .. }
                | S::PackageVersions { .. }
                | S::SemanticVersions { .. }
                | S::SemanticShapes { .. }
                | S::PackageSourceMembership { .. }
                | S::PackageProfile { .. }
                | S::Subscriptions
                | S::Releases { .. }
                | S::Projects
                | S::Tree
                | S::ProjectTree { .. }
                | S::CargoPackageSourceFile { .. }
                | S::CargoPackageSourceInventory { .. }
                | S::CargoPackageReadme { .. }
                | S::IndexOperationStatus { .. }
                | S::CargoPackageReadmeLink { .. }
                | S::IndexAwait { .. }
                | S::IndexProgress { .. }
                | S::IndexCancel { .. }
        ),
        Command::Add { .. } | Command::Remove { .. } => false,
    }
}

/// The reply to an `Add` of `requested_package`, as the add path gives it.
fn added_reply(requested_package: backend_engine::PackageKey) -> AdmittedReply {
    let intent_id = backend_engine::intent_id("request_package", requested_package.as_bytes());
    let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
        id: backend_engine::encode_id(intent_id.as_bytes()),
        token: "request_package".to_owned(),
        payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
    });
    (CommandReply::Added(intent_id), Some(certificate))
}

/// What executing one command came to.
pub(in crate::builtin) enum Executed {
    /// The reply body.
    Reply(Vec<u8>),
    /// Answered later under its ticket ([`CommandAdapter::poll_deferred`]).
    Deferred,
}

struct ResidentDependencies {
    catalog: ResidentCatalog,
    graph: ResidentPackageGraph,
}

/// Row overlays may change without invalidating dependency facts. Keep the
/// checked graph and its lookup index resident under the narrower input key.
struct ResidentPackageGraph {
    local_witness: [u8; 32],
    registry_revision: Option<CatalogDependencyRevision>,
    indexed: backend_library::IndexedCheckedPackageGraph,
    synced: Option<GraphProjectionStamp>,
}

/// One successfully projected root/fact pair; a partial pair is unrepresentable.
#[derive(Clone, Copy, Eq, PartialEq)]
struct GraphProjectionStamp {
    root: backend_library::ViewStateRoot,
    facts_witness: [u8; 32],
}

impl ResidentDependencies {
    /// Admit changed graph inputs once. Selecting an overlay with the same
    /// graph inputs must not invoke its fact builder or rebuild its index.
    fn select(
        resident: &mut Option<Self>,
        local_witness: [u8; 32],
        catalog: ResidentCatalog,
        build_facts: impl FnOnce(
            &ResidentCatalog,
        ) -> Result<
            backend_library::IndexedCheckedPackageGraph,
            BuiltinModelError,
        >,
    ) -> Result<(), BuiltinModelError> {
        let registry_revision = catalog.dependency_revision();
        if let Some(cached) = resident.as_mut()
            && cached.graph.local_witness == local_witness
            && cached.graph.registry_revision == registry_revision
        {
            cached.catalog = catalog;
            return Ok(());
        }
        let indexed = build_facts(&catalog)?;
        let synced = resident.as_ref().and_then(|cached| cached.graph.synced);
        *resident = Some(Self {
            catalog,
            graph: ResidentPackageGraph {
                local_witness,
                registry_revision,
                indexed,
                synced,
            },
        });
        Ok(())
    }
}

/// Registry rows, facts and their source revision remain one immutable owner
/// snapshot. An unconfigured registry has no fabricated source revision.
#[derive(Clone)]
enum ResidentCatalog {
    Registry(Arc<CatalogProjection>),
    Unconfigured(Arc<CatalogLookupIndex>),
}

impl ResidentCatalog {
    fn dependency_revision(&self) -> Option<CatalogDependencyRevision> {
        match self {
            Self::Registry(snapshot) => Some(snapshot.dependency_revision),
            Self::Unconfigured(_) => None,
        }
    }

    fn records(&self) -> &[backend_engine::RegistryPackageRecord] {
        match self {
            Self::Registry(snapshot) => &snapshot.records,
            Self::Unconfigured(_) => &[],
        }
    }

    fn index(&self) -> &CatalogLookupIndex {
        match self {
            Self::Registry(snapshot) => &snapshot.index,
            Self::Unconfigured(index) => index,
        }
    }

    fn rows_snapshot(&self) -> [u8; 32] {
        match self {
            Self::Registry(snapshot) => snapshot.selected_rows_snapshot,
            Self::Unconfigured(_) => CatalogLookupIndex::snapshot_for_catalog(&[]),
        }
    }

    fn dependency_facts(&self) -> &[backend_engine::PackageDependencySourceFacts] {
        match self {
            Self::Registry(snapshot) => snapshot.dependency_facts(),
            Self::Unconfigured(_) => &[],
        }
    }
}

impl CommandAdapter {
    pub(in crate::builtin) fn new(
        sql_projection: backend_extension_turso::TursoProjection,
        registry: Option<RegistryGateway>,
        forge: ForgeGateway,
        discovery: Option<crate::discovery::DiscoveryGateway>,
        product_state: super::super::ProductState,
        graph_limits: backend_library::PackageGraphIndexLimits,
        compiler: LocalCompilerClient,
        search_snapshots: super::super::query::SearchSnapshotOwner,
        remote_semantic: super::super::query::RemoteSemantic,
        published: Option<super::super::view_publish::PublishedRoots>,
        image_rows: super::super::view_build::ImageRowResidence,
        generations: super::super::generation_residence::SemanticGenerationResidence,
        semantic_authority: super::super::semantic_authority::SemanticAuthority,
        owner_cluster: Option<Arc<super::super::cluster_dispatch::OwnerCompilerClusterRuntime>>,
        pending_stored_acks: Option<
            Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
        >,
    ) -> Result<Self, BuiltinModelError> {
        let browse_lane = BrowseLane::start().map_err(BuiltinModelError)?;
        let index_operations = IndexOperationJournal::open(
            product_state
                .workspace_path()
                .map_err(BuiltinModelError)?
                .join("index-operations-v1.turso"),
        )
        .map_err(|error| {
            BuiltinModelError(format!("open durable index-operation journal: {error}"))
        })?;
        Ok(Self {
            sql_projection,
            registry,
            forge,
            discovery,
            product_state,
            index_operations,
            compiler,
            search_snapshots,
            remote_semantic,
            pending_semantic_search: None,
            published,
            manifests: super::super::local_manifest::LocalManifestResidence::default(),
            project_roots: super::super::project_root_residence::ProjectRootResidence::default(),
            image_rows,
            generations,
            semantic_authority,
            owner_cluster,
            pending_stored_acks,
            dependencies: None,
            graph_limits,
            browse: super::super::browse::BrowseCache::default(),
            browse_lane,
            indexing: None,
            index_terminals: std::collections::VecDeque::new(),
            index_progress: std::collections::VecDeque::new(),
            index_progress_latest: std::collections::VecDeque::new(),
            next_index_ticket: 1,
            index_owner_epoch: new_index_owner_epoch(),
            waiting: std::collections::VecDeque::new(),
            abandoned_replies: BTreeSet::new(),
        })
    }

    /// [`Self::execute`], except that indexing a local folder hands its
    /// compile off the owner loop and answers under `ticket` once it is
    /// published, and that a command that changes state waits while such a
    /// compile runs. Reads are answered at once, from the last publication.
    pub(in crate::builtin) fn execute_or_defer(
        &mut self,
        daemon: &mut ProductDaemon,
        body: &[u8],
        transport_ticket: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let owner = daemon.engine().daemon().library().cursor();
        let request = backend_engine::decode_command_dto_for_owner(body, owner)
            .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
        if let Command::Surface(backend_library::SurfaceCommand::IndexAwait { ticket }) =
            &request.command
        {
            return self.await_index_job(
                daemon,
                ticket.clone(),
                request.request_id,
                transport_ticket,
            );
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexProgress {
            ticket,
            after_sequence,
        }) = &request.command
        {
            return self.read_index_progress(
                daemon,
                ticket.clone(),
                *after_sequence,
                request.request_id,
            );
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexCancel { ticket }) =
            &request.command
        {
            return self.cancel_index_job(daemon, ticket.clone(), request.request_id);
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexOperationStatus {
            operation_key,
        }) = request.command
        {
            return self.read_index_operation_status(daemon, operation_key, request.request_id);
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexOperationStart {
            operation_key,
            package,
            execution_intent,
        }) = &request.command
            && self
                .indexing
                .as_ref()
                .is_some_and(|indexing| indexing.operation_key == Some(*operation_key))
        {
            let exact = match self.index_operations.entry(*operation_key) {
                Ok(Some(JournalEntry::Retained(entry))) => {
                    entry.package == *package && entry.execution_intent == *execution_intent
                }
                Ok(_) => false,
                Err(error) => {
                    return Err(BuiltinModelError(format!(
                        "read durable index-operation identity: {error}"
                    )));
                }
            };
            if exact {
                let observation = self
                    .resolve_index_operation(daemon, *operation_key, None)
                    .map_err(|error| {
                        BuiltinModelError(format!("read durable index-operation status: {error}"))
                    })?;
                return Self::encode_index_operation(daemon, request.request_id, observation, true);
            }
            return Self::encode_index_operation_start_failure(
                daemon,
                request.request_id,
                IndexOperationJournalError::KeyConflict,
            );
        }
        if self.indexing.is_some() && !answers_while_indexing(&request.command) {
            if self.waiting.len() >= MAX_WAITING_COMMANDS {
                return Err(BuiltinModelError(
                    "owner mutation queue is full; retry after current work completes".to_owned(),
                ));
            }
            self.waiting.push_back((transport_ticket, body.to_vec()));
            return Ok(Executed::Deferred);
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexStart {
            package,
            execution_intent,
        }) = request.command
        {
            return self.start_owner_index_job(
                daemon,
                package,
                execution_intent,
                request.request_id,
            );
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexOperationStart {
            operation_key,
            package,
            execution_intent,
        }) = request.command
        {
            return self.start_index_operation(
                daemon,
                operation_key,
                package,
                execution_intent,
                request.request_id,
            );
        }
        if let Command::Add {
            package,
            execution_intent,
        } = request.command
        {
            let certificate = request.certificate().cloned();
            let label = certified_package_label(certificate.as_ref(), package)?;
            let owner_ticket = self.issue_index_ticket(
                backend_library::PackageReference::parse(label)
                    .map_err(|error| BuiltinModelError(error.to_string()))?,
            )?;
            if let Some(started) = self.start_index_job(
                daemon,
                package,
                execution_intent,
                certificate.as_ref(),
                request.request_id,
                owner_ticket,
                Some(transport_ticket),
                None,
            )? {
                return Self::encode(daemon, request.request_id, started, None)
                    .map(Executed::Reply);
            }
            return Ok(Executed::Deferred);
        }
        if let Command::Surface(surface) = &request.command
            && let backend_library::SurfaceCommand::Package {
                package: backend_library::PackageReference::Purl(package),
            } = surface
        {
            let acquired = self
                .registry
                .as_mut()
                .map(RegistryGateway::catalog_projection)
                .transpose()
                .map_err(BuiltinModelError)?
                .is_some_and(|catalog| {
                    catalog
                        .records
                        .iter()
                        .any(|record| record.coordinate.as_str() == package.as_str())
                });
            let forge = self.forge.search_records().map_err(BuiltinModelError)?;
            let forged = !super::super::forge_gateway::find_package_versions(&forge, package)
                .map_err(|error| {
                    BuiltinModelError(format!("select forge metadata authority: {error:?}"))
                })?
                .is_empty();
            if !acquired
                && !forged
                && let Some(discovery) = self.discovery.as_ref()
                && let crate::discovery::package_metadata::PackageMetadataPreparation::Fetch(
                    metadata,
                ) =
                    discovery.prepare_package(package, self.index_owner_epoch, request.request_id)
            {
                return match self.browse_lane.submit_metadata(
                    transport_ticket,
                    request.request_id,
                    owner,
                    surface.clone(),
                    metadata,
                ) {
                    Ok(()) => Ok(Executed::Deferred),
                    Err(reason) => Self::encode(
                        daemon,
                        request.request_id,
                        (
                            CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                                reason.to_owned(),
                            )),
                            None,
                        ),
                        None,
                    )
                    .map(Executed::Reply),
                };
            }
        }
        if let Command::Surface(surface) = &request.command
            && BrowseLane::accepts(surface)
        {
            return match self.browse_lane.submit(
                transport_ticket,
                request.request_id,
                owner,
                surface.clone(),
                self.registry.as_ref(),
            ) {
                Ok(()) => Ok(Executed::Deferred),
                Err(reason) => Self::encode(
                    daemon,
                    request.request_id,
                    (
                        CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                            reason.to_owned(),
                        )),
                        None,
                    ),
                    None,
                )
                .map(Executed::Reply),
            };
        }
        self.execute(daemon, body).map(Executed::Reply)
    }

    fn start_index_operation(
        &mut self,
        daemon: &mut ProductDaemon,
        operation_key: backend_library::IndexOperationKey,
        package: backend_library::PackageReference,
        execution_intent: CompileExecutionIntent,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        match self
            .index_operations
            .accept(operation_key, package.clone(), execution_intent)
        {
            Ok(IndexOperationAcceptance::Existing) => {
                let observation = self
                    .resolve_index_operation(daemon, operation_key, None)
                    .map_err(|error| {
                        BuiltinModelError(format!("read durable index-operation status: {error}"))
                    })?;
                return Self::encode_index_operation(daemon, request_id, observation, true);
            }
            Ok(IndexOperationAcceptance::New) => {}
            Err(error) => {
                return Self::encode_index_operation_start_failure(daemon, request_id, error);
            }
        }

        let acceptance_head = daemon.engine().daemon().owner().head();
        if let Err(error) = self.index_operations.bind_source_capture_base(
            operation_key,
            *acceptance_head.root().as_bytes(),
            acceptance_head.sequence(),
        ) {
            return self.fail_accepted_index_operation(
                daemon,
                operation_key,
                package,
                execution_intent,
                request_id,
                bounded_index_detail(format!("record accepted workspace base: {error}")),
            );
        }

        let owner_ticket = match self.issue_index_ticket(package.clone()) {
            Ok(ticket) => ticket,
            Err(error) => {
                return self.fail_accepted_index_operation(
                    daemon,
                    operation_key,
                    package,
                    execution_intent,
                    request_id,
                    bounded_index_detail(error),
                );
            }
        };
        let package_key = backend_engine::package_key(package.as_str());
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: backend_engine::encode_id(package_key.as_bytes()),
            value: package.as_str().to_owned(),
        });
        match self.start_index_job(
            daemon,
            package_key,
            execution_intent,
            Some(&certificate),
            request_id,
            owner_ticket,
            None,
            Some(operation_key),
        ) {
            Ok(None) => {
                let observation = self
                    .resolve_index_operation(daemon, operation_key, None)
                    .map_err(|error| {
                        BuiltinModelError(format!("read durable index-operation status: {error}"))
                    })?;
                Self::encode_index_operation(daemon, request_id, observation, true)
            }
            Ok(Some(_)) => {
                let observation = self
                    .resolve_index_operation(daemon, operation_key, None)
                    .map_err(|error| {
                        BuiltinModelError(format!("read durable index-operation status: {error}"))
                    })?;
                Self::encode_index_operation(daemon, request_id, observation, true)
            }
            Err(error) => self.fail_accepted_index_operation(
                daemon,
                operation_key,
                package,
                execution_intent,
                request_id,
                bounded_index_detail(error),
            ),
        }
    }

    fn fail_accepted_index_operation(
        &mut self,
        daemon: &mut ProductDaemon,
        operation_key: backend_library::IndexOperationKey,
        package: backend_library::PackageReference,
        execution_intent: CompileExecutionIntent,
        request_id: u64,
        detail: backend_library::ProductText,
    ) -> Result<Executed, BuiltinModelError> {
        if let Ok(Some(JournalEntry::Retained(mut entry))) =
            self.index_operations.entry(operation_key)
            && matches!(entry.state, StoredOperationState::Accepted)
            && let Some(base) = entry.source_capture_base
            && daemon.engine().daemon().owner().head().sequence() > base.workspace_sequence
        {
            let recovered = self
                .resolve_index_operation(daemon, operation_key, None)
                .map_err(|error| {
                    BuiltinModelError(format!("recover source-captured operation: {error}"))
                })?;
            if let backend_library::IndexOperationObservation::Known(status) = &recovered
                && (status.source_capture.is_some()
                    || matches!(
                        &status.state,
                        backend_library::IndexOperationState::Unresolved { .. }
                    ))
            {
                return Self::encode_index_operation(daemon, request_id, recovered, true);
            }
            // The root has advanced but the operation marker was not recovered.
            // Keep an explicit unresolved result instead of falsely claiming
            // that no structural commit occurred.
            entry.source_capture = None;
            return Self::encode_index_operation(
                daemon,
                request_id,
                Self::unresolved_index_operation(
                    operation_key,
                    &entry,
                    backend_library::IndexOperationUnresolvedReason::WorkspaceEvidenceMismatch,
                    "the workspace advanced before the source-capture receipt could be recovered",
                ),
                true,
            );
        }
        let observation = if self
            .index_operations
            .failed(
                operation_key,
                backend_library::IndexOperationFailureReason::WorkerFailed,
                detail,
            )
            .is_ok()
        {
            self.resolve_index_operation(daemon, operation_key, None)
                .map_err(|error| {
                    BuiltinModelError(format!("read durable index-operation status: {error}"))
                })?
        } else {
            Self::unresolved_index_operation(
                operation_key,
                &StoredOperation {
                    operation_key,
                    request_digest: backend_library::index_operation_request_digest(
                        &package,
                        execution_intent,
                    ),
                    package,
                    execution_intent,
                    source_capture: None,
                    source_capture_base: None,
                    state: StoredOperationState::Accepted,
                },
                backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                "the owner could not durably record why accepted work did not start",
            )
        };
        Self::encode_index_operation(daemon, request_id, observation, true)
    }

    fn encode_index_operation_start_failure(
        daemon: &ProductDaemon,
        request_id: u64,
        error: IndexOperationJournalError,
    ) -> Result<Executed, BuiltinModelError> {
        let detail = backend_library::ProductText::new(error.to_string()).unwrap_or_else(|_| {
            backend_library::ProductText::from_static("index operation refused")
        });
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                    detail.as_str().to_owned(),
                )),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    fn read_index_operation_status(
        &mut self,
        daemon: &mut ProductDaemon,
        operation_key: backend_library::IndexOperationKey,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let observation = self
            .resolve_index_operation(daemon, operation_key, None)
            .map_err(|error| {
                BuiltinModelError(format!("read durable index-operation status: {error}"))
            })?;
        Self::encode_index_operation(daemon, request_id, observation, false)
    }

    fn encode_index_operation(
        daemon: &ProductDaemon,
        request_id: u64,
        observation: backend_library::IndexOperationObservation,
        started: bool,
    ) -> Result<Executed, BuiltinModelError> {
        let reply = if started {
            backend_library::SurfaceReply::IndexOperationStarted(observation)
        } else {
            backend_library::SurfaceReply::IndexOperationStatus(observation)
        };
        Self::encode(
            daemon,
            request_id,
            (CommandReply::Surface(reply), None),
            None,
        )
        .map(Executed::Reply)
    }

    fn resolve_index_operation(
        &mut self,
        daemon: &mut ProductDaemon,
        operation_key: backend_library::IndexOperationKey,
        terminal_outcome: Option<&backend_library::IndexJobOutcome>,
    ) -> Result<backend_library::IndexOperationObservation, IndexOperationJournalError> {
        let Some(entry) = self.index_operations.entry(operation_key)? else {
            return Ok(backend_library::IndexOperationObservation::Unknown { operation_key });
        };
        let mut entry = match entry {
            JournalEntry::Retained(entry) => entry,
            JournalEntry::OutsideReceiptWindow { request_digest } => {
                return Ok(
                    backend_library::IndexOperationObservation::OutsideReceiptWindow {
                        operation_key,
                        request_digest,
                    },
                );
            }
        };
        let active = self
            .indexing
            .as_ref()
            .filter(|indexing| indexing.operation_key == Some(operation_key))
            .map(|indexing| {
                (
                    indexing.owner_ticket.clone(),
                    indexing
                        .progress_stage
                        .unwrap_or(backend_library::IndexJobStage::Scanning),
                )
            });
        if matches!(entry.state, StoredOperationState::Accepted) && active.is_none() {
            if entry.source_capture.is_none() {
                if let Some(base) = entry.source_capture_base {
                    let head = daemon.engine().daemon().owner().head();
                    if head.sequence() == base.workspace_sequence.saturating_add(1) {
                        if let Some(receipt) = super::index::source_capture_receipt_for_root(
                            daemon,
                            &entry.package,
                            operation_key,
                            Some(head.request_identity()),
                        )
                        .map_err(|error| IndexOperationJournalError::Corrupt(error.to_string()))?
                        {
                            if self
                                .index_operations
                                .source_captured(operation_key, receipt.clone())
                                .is_err()
                            {
                                entry.source_capture = Some(receipt);
                                return Ok(Self::unresolved_index_operation(
                                    operation_key,
                                    &entry,
                                    backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                                    "the exact source-capture root is selected but its separate durable receipt could not be persisted",
                                ));
                            }
                            if let Some(JournalEntry::Retained(recovered)) =
                                self.index_operations.entry(operation_key)?
                            {
                                entry = recovered;
                            }
                        }
                    } else if head.sequence() > base.workspace_sequence.saturating_add(1) {
                        return Ok(Self::unresolved_index_operation(
                            operation_key,
                            &entry,
                            backend_library::IndexOperationUnresolvedReason::WorkspaceEvidenceMismatch,
                            "the owner restarted after the recorded base advanced, so the exact source-capture root cannot be identified",
                        ));
                    }
                }
            }
            if let Some(original) = entry.source_capture.clone() {
                let _ = self.refresh_index_operation_source_capture(
                    daemon,
                    operation_key,
                    &entry.package,
                    &original,
                );
                if let Some(JournalEntry::Retained(refreshed)) =
                    self.index_operations.entry(operation_key)?
                {
                    entry = refreshed;
                }
                let profiles = entry
                    .source_capture
                    .as_ref()
                    .map(backend_library::IndexOperationSourceCaptureReceipt::profiles)
                    .unwrap_or_default();
                let all_terminal = !profiles.is_empty()
                    && profiles.iter().all(|profile| {
                        !matches!(
                            profile.state,
                            backend_library::IndexOperationSemanticProfileState::Pending { .. }
                        )
                    });
                let any_published = profiles.iter().any(|profile| {
                    matches!(
                        profile.state,
                        backend_library::IndexOperationSemanticProfileState::Published { .. }
                    )
                });
                if all_terminal && !any_published {
                    let (reason, detail, compiler_failure) =
                        index_operation_failure(terminal_outcome);
                    if self
                        .index_operations
                        .failed_with_compiler_failure(
                            operation_key,
                            reason,
                            detail,
                            compiler_failure,
                        )
                        .is_ok()
                    {
                        return Ok(self
                            .index_operations
                            .observation(operation_key, None)?
                            .expect("failed operation remains in the journal"));
                    }
                    return Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                        "semantic profiles reached terminal source-capture outcomes but the operation failure could not be persisted",
                    ));
                }
                if any_published {
                    return Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        backend_library::IndexOperationUnresolvedReason::WorkspaceEvidenceMismatch,
                        "semantic publication is present after restart without its prepared operation receipt",
                    ));
                }
                if profiles.iter().any(|profile| {
                    matches!(
                        profile.state,
                        backend_library::IndexOperationSemanticProfileState::Pending { .. }
                    )
                }) {
                    let (reason, detail) = pending_capture_unresolved(terminal_outcome);
                    return Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        reason,
                        detail.as_str(),
                    ));
                }
                return Ok(self
                    .index_operations
                    .observation(operation_key, None)?
                    .expect("accepted source-captured operation remains in the journal"));
            }
        }
        match entry.state.clone() {
            StoredOperationState::Accepted if active.is_some() => Ok(self
                .index_operations
                .observation(operation_key, active)?
                .expect("accepted operation remains in the journal")),
            StoredOperationState::Accepted => {
                let (reason, detail, compiler_failure) = index_operation_failure(terminal_outcome);
                if self
                    .index_operations
                    .failed_with_compiler_failure(operation_key, reason, detail, compiler_failure)
                    .is_ok()
                {
                    Ok(self
                        .index_operations
                        .observation(operation_key, None)?
                        .expect("failed operation remains in the journal"))
                } else {
                    Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                        "the owner restarted or lost the job before it could persist a terminal failure",
                    ))
                }
            }
            StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            } => {
                let (selected_exactly, base_still_selected) = {
                    let head = daemon.engine().daemon().owner().head();
                    let selected_exactly = match request_identity {
                        Some(expected) => {
                            head.request_identity() == expected
                                && head.sequence() > base_workspace_sequence
                        }
                        None => {
                            head.root().as_bytes() == &base_workspace_root
                                && head.sequence() == base_workspace_sequence
                        }
                    };
                    let base_still_selected = head.root().as_bytes() == &base_workspace_root
                        && head.sequence() == base_workspace_sequence;
                    (selected_exactly, base_still_selected)
                };
                if selected_exactly {
                    if let Some(source_capture) = entry.source_capture.clone() {
                        let _ = self.refresh_index_operation_source_capture(
                            daemon,
                            operation_key,
                            &entry.package,
                            &source_capture,
                        );
                        if let Some(JournalEntry::Retained(refreshed)) =
                            self.index_operations.entry(operation_key)?
                        {
                            entry = refreshed;
                        }
                    }
                    let receipt = self
                        .current_index_operation_receipt(
                            daemon,
                            request_identity,
                            base_workspace_root,
                            base_workspace_sequence,
                        )
                        .or_else(|| {
                            terminal_outcome.and_then(|_| {
                                self.publish_view(daemon, None).ok()?;
                                self.current_index_operation_receipt(
                                    daemon,
                                    request_identity,
                                    base_workspace_root,
                                    base_workspace_sequence,
                                )
                            })
                        });
                    if let Some(receipt) = receipt {
                        // If the receipt file cannot be replaced after a fully
                        // checked workspace/view proof, keep Prepared on disk;
                        // a future status read or restart can reconstruct it.
                        let _ = self
                            .index_operations
                            .published(operation_key, receipt.clone());
                        return Ok(index_operation_published_observation(
                            &entry,
                            operation_key,
                            receipt,
                        ));
                    }
                    Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        backend_library::IndexOperationUnresolvedReason::ViewEvidenceMismatch,
                        "the exact workspace commit is selected but its product view receipt is not available",
                    ))
                } else if base_still_selected {
                    let (reason, detail, compiler_failure) =
                        index_operation_failure(terminal_outcome);
                    if self
                        .index_operations
                        .failed_with_compiler_failure(
                            operation_key,
                            reason,
                            detail,
                            compiler_failure,
                        )
                        .is_ok()
                    {
                        Ok(self
                            .index_operations
                            .observation(operation_key, None)?
                            .expect("failed operation remains in the journal"))
                    } else {
                        Ok(Self::unresolved_index_operation(
                            operation_key,
                            &entry,
                            backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                            "the owner could not durably record that the prepared commit was not selected",
                        ))
                    }
                } else {
                    Ok(Self::unresolved_index_operation(
                        operation_key,
                        &entry,
                        backend_library::IndexOperationUnresolvedReason::WorkspaceEvidenceMismatch,
                        "the selected workspace is neither the recorded base nor the exact prepared request",
                    ))
                }
            }
            StoredOperationState::Published { .. }
            | StoredOperationState::Failed { .. }
            | StoredOperationState::Unresolved { .. } => Ok(self
                .index_operations
                .observation(operation_key, active)?
                .expect("terminal operation remains in the journal")),
        }
    }

    fn refresh_index_operation_source_capture(
        &mut self,
        daemon: &ProductDaemon,
        operation_key: backend_library::IndexOperationKey,
        package: &backend_library::PackageReference,
        original: &backend_library::IndexOperationSourceCaptureReceipt,
    ) -> Result<(), IndexOperationJournalError> {
        let Some(latest) =
            super::index::source_capture_receipt_for_root(daemon, package, operation_key, None)
                .map_err(|error| IndexOperationJournalError::Corrupt(error.to_string()))?
        else {
            return Ok(());
        };
        if original.profiles().len() != latest.profiles().len()
            || !original
                .profiles()
                .iter()
                .zip(latest.profiles())
                .all(|(left, right)| {
                    left.profile == right.profile
                        && left.source_version == right.source_version
                        && left.input_digest == right.input_digest
                        && left.observation_sequence == right.observation_sequence
                        && left.source_count == right.source_count
                })
        {
            return Err(IndexOperationJournalError::InvalidTransition);
        }
        let refreshed = backend_library::IndexOperationSourceCaptureReceipt::from_checked_parts(
            operation_key,
            *original.commit_identity(),
            *original.workspace_root(),
            original.workspace_sequence(),
            latest.profiles().to_vec().into_boxed_slice(),
        )
        .map_err(|_| IndexOperationJournalError::InvalidTransition)?;
        self.index_operations
            .source_capture_updated(operation_key, refreshed)
    }

    fn current_index_operation_receipt(
        &self,
        daemon: &ProductDaemon,
        request_identity: Option<[u8; 32]>,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
    ) -> Option<backend_library::IndexOperationPublicationReceipt> {
        let owner = daemon.engine().daemon().owner();
        let head = owner.head();
        if match request_identity {
            Some(expected) => {
                head.request_identity() != expected || head.sequence() <= base_workspace_sequence
            }
            None => {
                head.root().as_bytes() != &base_workspace_root
                    || head.sequence() != base_workspace_sequence
            }
        } {
            return None;
        }
        let snapshot = owner.snapshot();
        let source = super::super::view_publish::source_root(&snapshot).ok()?;
        let semantic = super::super::view_publish::semantic_root(&snapshot).ok()?;
        let published = self.published.as_ref()?;
        if published.source != source || published.semantic != semantic {
            return None;
        }
        let library = daemon.engine().daemon().library();
        backend_library::IndexOperationPublicationReceipt::from_published_view(
            request_identity,
            *head.commit().id().as_bytes(),
            *head.root().as_bytes(),
            head.sequence(),
            library.view(),
            library.cursor(),
        )
        .ok()
    }

    fn unresolved_index_operation(
        operation_key: backend_library::IndexOperationKey,
        entry: &StoredOperation,
        reason: backend_library::IndexOperationUnresolvedReason,
        detail: &str,
    ) -> backend_library::IndexOperationObservation {
        backend_library::IndexOperationObservation::Known(
            backend_library::IndexOperationStatus::new(
                operation_key,
                entry.package.clone(),
                entry.execution_intent,
                backend_library::IndexOperationState::Unresolved {
                    reason,
                    detail: backend_library::ProductText::new(detail.to_owned()).unwrap_or_else(
                        |_| {
                            backend_library::ProductText::from_static(
                                "operation outcome unresolved",
                            )
                        },
                    ),
                },
            )
            .with_source_capture(entry.source_capture.clone()),
        )
    }

    pub(in crate::builtin) fn close(&mut self) {
        self.browse_lane.close();
    }

    fn start_owner_index_job(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_library::PackageReference,
        execution_intent: CompileExecutionIntent,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let owner_ticket = self.issue_index_ticket(package.clone())?;
        let package_key = backend_engine::package_key(package.as_str());
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: backend_engine::encode_id(package_key.as_bytes()),
            value: package.as_str().to_owned(),
        });
        let result = self
            .start_index_job(
                daemon,
                package_key,
                execution_intent,
                Some(&certificate),
                request_id,
                owner_ticket.clone(),
                None,
                None,
            )
            .map(|reply| reply.map(|_| ()));
        let result = match result {
            Ok(Some(())) => {
                let terminal = backend_library::IndexJobTerminal {
                    ticket: owner_ticket,
                    outcome: backend_library::IndexJobOutcome::Published,
                };
                self.retain_index_terminal(terminal.clone());
                backend_library::IndexStartResult::Terminal(terminal)
            }
            Ok(None) => {
                return Self::encode(
                    daemon,
                    request_id,
                    (
                        CommandReply::Surface(backend_library::SurfaceReply::IndexStarted(
                            backend_library::IndexStartResult::Started {
                                ticket: owner_ticket,
                                stage: self
                                    .indexing
                                    .as_ref()
                                    .and_then(|indexing| indexing.progress_stage)
                                    .unwrap_or(backend_library::IndexJobStage::Scanning),
                            },
                        )),
                        None,
                    ),
                    None,
                )
                .map(Executed::Reply);
            }
            Err(error) => {
                let terminal = backend_library::IndexJobTerminal {
                    ticket: owner_ticket,
                    outcome: backend_library::IndexJobOutcome::Refused(bounded_index_detail(error)),
                };
                self.retain_index_terminal(terminal.clone());
                backend_library::IndexStartResult::Terminal(terminal)
            }
        };
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Surface(backend_library::SurfaceReply::IndexStarted(result)),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    fn issue_index_ticket(
        &mut self,
        package: backend_library::PackageReference,
    ) -> Result<backend_library::IndexJobTicket, BuiltinModelError> {
        let id = NonZeroU64::new(self.next_index_ticket).ok_or_else(|| {
            BuiltinModelError("owner index ticket sequence is exhausted".to_owned())
        })?;
        self.next_index_ticket = self.next_index_ticket.checked_add(1).unwrap_or(0);
        Ok(backend_library::IndexJobTicket::new(
            id,
            self.index_owner_epoch,
            package,
        ))
    }

    fn retain_index_terminal(&mut self, terminal: backend_library::IndexJobTerminal) {
        self.index_terminals.push_back(terminal);
        while self.index_terminals.len() > MAX_RETAINED_INDEX_TERMINALS {
            if let Some(expired) = self.index_terminals.pop_front() {
                self.index_progress_latest
                    .retain(|(ticket, _)| *ticket != expired.ticket);
            }
        }
    }

    fn emit_index_progress(
        &mut self,
        indexing: &mut IndexJob,
        kind: backend_library::IndexJobProgressKind,
    ) {
        let Some(sequence) = indexing.progress_sequence.checked_add(1) else {
            return;
        };
        indexing.progress_sequence = sequence;
        self.index_progress
            .push_back(backend_library::IndexJobProgressEvent {
                ticket: indexing.owner_ticket.clone(),
                sequence,
                kind,
            });
        while self.index_progress.len() > MAX_RETAINED_INDEX_PROGRESS_EVENTS {
            let _ = self.index_progress.pop_front();
        }
        if let Some((_, latest)) = self
            .index_progress_latest
            .iter_mut()
            .find(|(ticket, _)| *ticket == indexing.owner_ticket)
        {
            *latest = sequence;
        } else {
            self.index_progress_latest
                .push_back((indexing.owner_ticket.clone(), sequence));
            while self.index_progress_latest.len() > MAX_RETAINED_INDEX_PROGRESS_TICKETS {
                let _ = self.index_progress_latest.pop_front();
            }
        }
    }

    fn set_index_progress_stage(
        &mut self,
        indexing: &mut IndexJob,
        stage: backend_library::IndexJobStage,
    ) {
        if indexing.progress_stage == Some(stage) {
            return;
        }
        indexing.progress_stage = Some(stage);
        self.emit_index_progress(
            indexing,
            backend_library::IndexJobProgressKind::StageChanged { stage },
        );
    }

    fn read_index_progress(
        &self,
        daemon: &ProductDaemon,
        ticket: backend_library::IndexJobTicket,
        after_sequence: u64,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let active = self
            .indexing
            .as_ref()
            .is_some_and(|indexing| indexing.owner_ticket == ticket);
        let terminal = self
            .index_terminals
            .iter()
            .find(|terminal| terminal.ticket == ticket)
            .cloned();
        let observation = if let Some(terminal) = terminal {
            backend_library::IndexJobObservation::Terminal(terminal)
        } else if !active {
            backend_library::IndexJobObservation::Unknown {
                ticket,
                current_owner_epoch: self.index_owner_epoch,
            }
        } else {
            let latest_sequence = self
                .indexing
                .as_ref()
                .filter(|indexing| indexing.owner_ticket == ticket)
                .map(|indexing| indexing.progress_sequence)
                .unwrap_or_default();
            let retained = self
                .index_progress
                .iter()
                .filter(|event| event.ticket == ticket && event.sequence > after_sequence)
                .cloned()
                .collect::<Vec<_>>();
            let oldest_retained = self
                .index_progress
                .iter()
                .find(|event| event.ticket == ticket)
                .map(|event| event.sequence);
            let truncated = after_sequence < latest_sequence
                && oldest_retained.is_none_or(|oldest| after_sequence.saturating_add(1) < oldest);
            let has_more = retained.len() > backend_library::MAX_INDEX_PROGRESS_EVENTS;
            let events = retained
                .into_iter()
                .take(backend_library::MAX_INDEX_PROGRESS_EVENTS)
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
            let stage = self
                .indexing
                .as_ref()
                .filter(|indexing| indexing.owner_ticket == ticket)
                .and_then(|indexing| indexing.progress_stage)
                .unwrap_or(backend_library::IndexJobStage::Scanning);
            backend_library::IndexJobObservation::Pending(backend_library::IndexProgressPage {
                ticket,
                stage,
                events,
                next_sequence,
                truncated,
                has_more,
            })
        };
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Surface(backend_library::SurfaceReply::IndexProgress(observation)),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    fn await_index_job(
        &mut self,
        daemon: &ProductDaemon,
        ticket: backend_library::IndexJobTicket,
        request_id: u64,
        transport_ticket: u64,
    ) -> Result<Executed, BuiltinModelError> {
        if let Some(indexing) = &mut self.indexing
            && indexing.owner_ticket == ticket
        {
            indexing.awaiters.push((transport_ticket, request_id));
            return Ok(Executed::Deferred);
        }
        if let Some(terminal) = self
            .index_terminals
            .iter()
            .find(|terminal| terminal.ticket == ticket)
            .cloned()
        {
            return Self::encode(
                daemon,
                request_id,
                (
                    CommandReply::Surface(backend_library::SurfaceReply::IndexTerminal(terminal)),
                    None,
                ),
                None,
            )
            .map(Executed::Reply);
        }
        Err(BuiltinModelError(
            "index job ticket is unknown, expired, or belongs to another package".to_owned(),
        ))
    }

    fn cancel_index_job(
        &mut self,
        daemon: &ProductDaemon,
        ticket: backend_library::IndexJobTicket,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let status = if let Some(indexing) = &self.indexing
            && indexing.owner_ticket == ticket
        {
            indexing.cancelled.store(true, Ordering::Release);
            backend_library::IndexCancelStatus::Requested
        } else if let Some(terminal) = self
            .index_terminals
            .iter()
            .find(|terminal| terminal.ticket == ticket)
            .cloned()
        {
            backend_library::IndexCancelStatus::Terminal(terminal)
        } else {
            backend_library::IndexCancelStatus::Unknown
        };
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Surface(backend_library::SurfaceReply::IndexCancellation(
                    backend_library::IndexCancelReceipt { ticket, status },
                )),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    fn complete_index_job(
        &mut self,
        daemon: &mut ProductDaemon,
        indexing: IndexJob,
        outcome: backend_library::IndexJobOutcome,
        legacy_reply: Option<Result<Vec<u8>, BuiltinModelError>>,
    ) -> Vec<(u64, Result<Vec<u8>, BuiltinModelError>)> {
        if let Some(operation_key) = indexing.operation_key {
            if let Ok(Some(JournalEntry::Retained(entry))) =
                self.index_operations.entry(operation_key)
                && let Some(source_capture) = entry.source_capture.as_ref()
            {
                let _ = self.refresh_index_operation_source_capture(
                    daemon,
                    operation_key,
                    &entry.package,
                    source_capture,
                );
            }
            let _ = self.resolve_index_operation(daemon, operation_key, Some(&outcome));
        }
        let mut ready = Vec::new();
        let terminal = backend_library::IndexJobTerminal {
            ticket: indexing.owner_ticket,
            outcome,
        };
        self.retain_index_terminal(terminal.clone());
        if let Some((transport_ticket, request_id)) = indexing.legacy_add {
            let reply = if let Some(failure) = legacy_add_compiler_failure(&terminal.outcome) {
                Self::encode(
                    daemon,
                    request_id,
                    (CommandReply::Failed(failure), None),
                    None,
                )
            } else {
                legacy_reply.unwrap_or_else(|| {
                    Err(BuiltinModelError(format!(
                        "index job did not publish: {:?}",
                        terminal.outcome
                    )))
                })
            };
            ready.push((transport_ticket, reply));
        }
        for (transport_ticket, request_id) in indexing.awaiters {
            let reply = Self::encode(
                daemon,
                request_id,
                (
                    CommandReply::Surface(backend_library::SurfaceReply::IndexTerminal(
                        terminal.clone(),
                    )),
                    None,
                ),
                None,
            );
            ready.push((transport_ticket, reply));
        }
        ready
    }

    fn spawn_next_index_profile(
        &mut self,
        indexing: &mut IndexJob,
        mut job: DeferredIndex,
    ) -> Result<(), BuiltinModelError> {
        let (profile, sources) = job.take_next_work().ok_or_else(|| {
            BuiltinModelError("deferred index has no remaining compiler profile".to_owned())
        })?;
        self.set_index_progress_stage(indexing, backend_library::IndexJobStage::Compiling);
        self.emit_index_progress(
            indexing,
            backend_library::IndexJobProgressKind::ProfileStarted {
                profile: backend_engine::SemanticLanguageProfile::new(profile.profile()),
                ordinal: profile.ordinal(),
                total: profile.total(),
            },
        );
        let compiler = self.compiler.clone();
        let cancelled = Arc::clone(&indexing.cancelled);
        let (sender, compiled) = std::sync::mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("locald-index-compile".to_owned())
            .spawn(move || {
                let _ = sender.send(run_deferred_compile(&compiler, sources, cancelled));
            });
        if let Err(error) = spawn {
            let mut attempts = vec![profile.candidate_attempt().clone()];
            attempts.extend(job.pending_attempts());
            let mut cleanup_failure = None;
            for attempt in attempts {
                if let Err(retire_error) = self.semantic_authority.retire_candidate_attempt(
                    &attempt,
                    backend_extension_turso::CandidateAttemptRetirementReason::Failed,
                ) {
                    cleanup_failure.get_or_insert(retire_error);
                }
            }
            return Err(match cleanup_failure {
                Some(cleanup_failure) => BuiltinModelError(format!(
                    "start the index compile failed ({error}); retire its candidate attempts failed ({cleanup_failure})"
                )),
                None => BuiltinModelError(format!("start the index compile: {error}")),
            });
        }
        indexing.work = IndexJobWork::Compiling {
            job,
            profile,
            compiled,
        };
        Ok(())
    }

    fn finish_prepared_index_selection(
        &mut self,
        daemon: &mut ProductDaemon,
        indexing: &mut IndexJob,
        prepared: PreparedProductSelection,
        legacy_reply: &mut Option<Result<Vec<u8>, BuiltinModelError>>,
    ) -> backend_library::IndexJobOutcome {
        self.set_index_progress_stage(indexing, backend_library::IndexJobStage::Publishing);
        match self.finish_add(
            daemon,
            prepared,
            indexing.request_id,
            indexing.requested_package,
            indexing.operation_key,
            Arc::clone(&indexing.cancelled),
        ) {
            Ok(reply) => {
                if indexing.legacy_add.is_some() {
                    *legacy_reply = Some(Self::encode(daemon, indexing.request_id, reply, None));
                }
                backend_library::IndexJobOutcome::Published
            }
            Err(_) if indexing.cancelled.load(Ordering::Acquire) => {
                backend_library::IndexJobOutcome::Cancelled
            }
            Err(error) => backend_library::IndexJobOutcome::Failed(bounded_index_detail(error)),
        }
    }

    fn start_staged_package_scan(
        &mut self,
        daemon: &mut ProductDaemon,
        indexing: &mut IndexJob,
        staged: StagedProject,
    ) -> Result<(), BuiltinModelError> {
        let label = indexing.owner_ticket.package().as_str();
        let coordinate = backend_engine::registry::PackageCoordinate::parse(label)
            .map_err(|_| BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))?;
        if coordinate.as_str() != label {
            return Err(BuiltinModelError(
                "package URL is not in canonical form".to_owned(),
            ));
        }
        let work = capture_index_scan(
            daemon,
            indexing.requested_package,
            label,
            staged.path(),
            Some(&coordinate),
            indexing.request_id,
            indexing.execution_intent,
            self.owner_cluster.is_some(),
            Arc::clone(&indexing.cancelled),
        )?;
        indexing._staged_project = Some(staged);
        self.spawn_index_scan(indexing, work)
    }

    /// Advances scan, compile, and publication stages without blocking the
    /// owner loop on filesystem traversal or compiler work. Reads continue to
    /// observe the last committed product root while either worker runs.
    /// Returns every reply that became ready, by transport ticket.
    pub(in crate::builtin) fn poll_deferred(
        &mut self,
        daemon: &mut ProductDaemon,
    ) -> Vec<(u64, Result<Vec<u8>, BuiltinModelError>)> {
        // Derived history is sidecar work: apply any completed status and
        // reschedule selected markers before advancing the active index job.
        // A history-store refusal must not stall product reads or an admitted
        // index publication; selected-marker reconciliation retries it later.
        let _ = self.semantic_authority.drain_native_history_completions();
        let mut ready = Vec::new();
        // These indexed probes intentionally hit the journal on every owner
        // poll: multiprocess WAL lets another process admit or prepare work,
        // so a process-local hint could otherwise hide committed rows. An
        // unavailable probe is treated as pending/prepared, never as empty.
        if self.indexing.is_none()
            && self.index_operations.has_pending().unwrap_or(true)
            && let Ok(Some(operation_key)) = self.index_operations.first_pending_key()
        {
            let _ = self.resolve_index_operation(daemon, operation_key, None);
        }
        if let Some(mut indexing) = self.indexing.take() {
            let mut terminal;
            let mut legacy_reply = None;
            let mut terminal_compiler_profile = None;
            let work = std::mem::replace(&mut indexing.work, IndexJobWork::Transition);
            // Candidate-attempt cleanup inventory is built only when a
            // compiler stage actually reaches a terminal path. Healthy polls
            // therefore do not allocate or clone the queued profile list.
            let mut terminal_attempts = None;
            match work {
                IndexJobWork::Acquiring(acquired) => match acquired.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        indexing.work = IndexJobWork::Acquiring(acquired);
                        self.indexing = Some(indexing);
                        return self.with_browse_completions(daemon, ready);
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        terminal = Some(backend_library::IndexJobOutcome::Failed(
                            backend_library::ProductText::from_static(
                                "registry acquisition worker ended without a terminal receipt",
                            ),
                        ));
                    }
                    Ok(RegistryAcquisitionMessage::Staging) => {
                        self.set_index_progress_stage(
                            &mut indexing,
                            backend_library::IndexJobStage::Staging,
                        );
                        indexing.work = IndexJobWork::Acquiring(acquired);
                        self.indexing = Some(indexing);
                        return self.with_browse_completions(daemon, ready);
                    }
                    Ok(RegistryAcquisitionMessage::Complete { gateway, result }) => {
                        if let Some(gateway) = gateway {
                            self.registry = Some(gateway);
                        }
                        if indexing.cancelled.load(Ordering::Acquire) {
                            terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                        } else {
                            match result {
                                Err(RegistryAddError::Cancelled) => {
                                    terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                                }
                                Err(refusal) => {
                                    terminal = Some(backend_library::IndexJobOutcome::Refused(
                                        bounded_index_detail(refusal),
                                    ));
                                }
                                Ok(staged) => {
                                    match self.start_staged_package_scan(
                                        daemon,
                                        &mut indexing,
                                        staged,
                                    ) {
                                        Ok(()) => {
                                            self.indexing = Some(indexing);
                                            return self.with_browse_completions(daemon, ready);
                                        }
                                        Err(refusal) => {
                                            terminal =
                                                Some(backend_library::IndexJobOutcome::Refused(
                                                    bounded_index_detail(refusal),
                                                ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                IndexJobWork::Scanning(scanned) => match scanned.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        indexing.work = IndexJobWork::Scanning(scanned);
                        self.indexing = Some(indexing);
                        return self.with_browse_completions(daemon, ready);
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        terminal = Some(backend_library::IndexJobOutcome::Failed(
                            backend_library::ProductText::from_static(
                                "index scan worker ended without a terminal receipt",
                            ),
                        ));
                    }
                    Ok(Err(IndexScanFailure::Cancelled)) => {
                        terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                    }
                    Ok(Err(IndexScanFailure::Refused(refusal))) => {
                        terminal = Some(backend_library::IndexJobOutcome::Refused(
                            bounded_index_detail(refusal),
                        ));
                    }
                    Ok(Ok(_)) if indexing.cancelled.load(Ordering::Acquire) => {
                        terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                    }
                    Ok(Ok(scan_result)) => {
                        match finish_index_scan(
                            daemon,
                            scan_result,
                            &self.compiler,
                            &mut self.semantic_authority,
                            self.owner_cluster.as_deref(),
                            self.pending_stored_acks.as_ref(),
                            true,
                            indexing.operation_key,
                            &mut indexing.captures,
                            Some(&mut self.index_operations),
                        ) {
                            Ok(PreparedIndex::Ready(prepared)) => {
                                terminal = Some(self.finish_prepared_index_selection(
                                    daemon,
                                    &mut indexing,
                                    prepared,
                                    &mut legacy_reply,
                                ));
                            }
                            Ok(PreparedIndex::Compile(job)) if job.has_pending_profiles() => {
                                indexing.captures = job.captures.clone();
                                // The source and Pending capture already committed.
                                // Publish their authenticated current view before
                                // compiler work can leave the owner serving reads.
                                match self.publish_view(daemon, None).and_then(|()| {
                                    self.spawn_next_index_profile(&mut indexing, job)
                                }) {
                                    Ok(()) => {
                                        self.indexing = Some(indexing);
                                        return self.with_browse_completions(daemon, ready);
                                    }
                                    Err(error) => {
                                        terminal = Some(backend_library::IndexJobOutcome::Failed(
                                            bounded_index_detail(error),
                                        ));
                                    }
                                }
                            }
                            Ok(PreparedIndex::Compile(job)) => {
                                match finish_deferred_index(daemon, job) {
                                    Ok(prepared) => {
                                        terminal = Some(self.finish_prepared_index_selection(
                                            daemon,
                                            &mut indexing,
                                            prepared,
                                            &mut legacy_reply,
                                        ));
                                    }
                                    Err(refusal) => {
                                        terminal = Some(backend_library::IndexJobOutcome::Refused(
                                            bounded_index_detail(refusal),
                                        ));
                                    }
                                }
                            }
                            Err(refusal) => {
                                terminal = Some(backend_library::IndexJobOutcome::Refused(
                                    bounded_index_detail(refusal),
                                ));
                            }
                        }
                    }
                },
                IndexJobWork::Compiling {
                    mut job,
                    profile,
                    compiled,
                } => {
                    indexing.captures = job.captures.clone();
                    match compiled.try_recv() {
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            indexing.work = IndexJobWork::Compiling {
                                job,
                                profile,
                                compiled,
                            };
                            self.indexing = Some(indexing);
                            return self.with_browse_completions(daemon, ready);
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            let mut attempts = vec![profile.candidate_attempt().clone()];
                            attempts.extend(job.pending_attempts());
                            terminal_attempts = Some(attempts);
                            terminal = Some(backend_library::IndexJobOutcome::Failed(
                                backend_library::ProductText::from_static(
                                    "compiler worker ended without a terminal receipt",
                                ),
                            ));
                        }
                        Ok(_result) if indexing.cancelled.load(Ordering::Acquire) => {
                            let mut attempts = vec![profile.candidate_attempt().clone()];
                            attempts.extend(job.pending_attempts());
                            terminal_attempts = Some(attempts);
                            terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                        }
                        Ok(result) if deferred_compile_was_cancelled(&result) => {
                            let mut attempts = vec![profile.candidate_attempt().clone()];
                            attempts.extend(job.pending_attempts());
                            terminal_attempts = Some(attempts);
                            terminal = Some(backend_library::IndexJobOutcome::Cancelled);
                        }
                        Ok(result) => {
                            // The profile is consumed by admission below. Retain
                            // only its compact attempt capability; clone the
                            // queued list only if admission reports a terminal.
                            let current_attempt = profile.candidate_attempt().clone();
                            let current_profile = profile.profile();
                            let progress = (
                                backend_engine::SemanticLanguageProfile::new(current_profile),
                                profile.ordinal(),
                                profile.total(),
                            );
                            match finish_deferred_profile(
                                daemon,
                                &mut self.semantic_authority,
                                &mut job,
                                profile,
                                result,
                            ) {
                                Ok(()) => {
                                    self.emit_index_progress(
                                        &mut indexing,
                                        backend_library::IndexJobProgressKind::ProfileAdmitted {
                                            profile: progress.0,
                                            ordinal: progress.1,
                                            total: progress.2,
                                        },
                                    );
                                    if job.has_pending_profiles() {
                                        match self.spawn_next_index_profile(&mut indexing, job) {
                                            Ok(()) => {
                                                self.indexing = Some(indexing);
                                                return self.with_browse_completions(daemon, ready);
                                            }
                                            Err(error) => {
                                                terminal =
                                                    Some(backend_library::IndexJobOutcome::Failed(
                                                        bounded_index_detail(error),
                                                    ));
                                            }
                                        }
                                    } else {
                                        match finish_deferred_index(daemon, job) {
                                            Ok(prepared) => {
                                                terminal =
                                                    Some(self.finish_prepared_index_selection(
                                                        daemon,
                                                        &mut indexing,
                                                        prepared,
                                                        &mut legacy_reply,
                                                    ));
                                            }
                                            Err(refusal) => {
                                                terminal = Some(
                                                    backend_library::IndexJobOutcome::Refused(
                                                        bounded_index_detail(refusal),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(refusal) => {
                                    let mut attempts = vec![current_attempt];
                                    attempts.extend(job.pending_attempts());
                                    terminal_attempts = Some(attempts);
                                    if refusal.compiler_failure.is_some() {
                                        terminal_compiler_profile = Some(current_profile);
                                    }
                                    terminal = Some(deferred_profile_refused_outcome(refusal));
                                }
                            }
                        }
                    }
                }
                IndexJobWork::Transition => {
                    terminal = Some(backend_library::IndexJobOutcome::Failed(
                        backend_library::ProductText::from_static(
                            "index job entered an invalid transition state",
                        ),
                    ));
                }
            }
            if let Some(mut outcome) = terminal {
                if !matches!(outcome, backend_library::IndexJobOutcome::Published)
                    && !indexing.captures.is_empty()
                {
                    let compiler_failure = match &outcome {
                        backend_library::IndexJobOutcome::RefusedWithCompilerFailure {
                            failure,
                            ..
                        } => terminal_compiler_profile.map(|profile| (profile, failure.clone())),
                        _ => None,
                    };
                    if matches!(
                        &outcome,
                        backend_library::IndexJobOutcome::RefusedWithCompilerFailure { .. }
                    ) && compiler_failure.is_none()
                    {
                        outcome = backend_library::IndexJobOutcome::Failed(
                            backend_library::ProductText::from_static(
                                "typed compiler refusal lost its captured profile binding",
                            ),
                        );
                    }
                    let reason = if matches!(outcome, backend_library::IndexJobOutcome::Cancelled) {
                        backend_engine::builtin::SemanticUnavailableReason::Cancelled
                    } else {
                        backend_engine::builtin::SemanticUnavailableReason::Rejected
                    };
                    if let Err(error) = commit_pending_capture_failure(
                        daemon,
                        indexing.captured_package,
                        &indexing.captured_label,
                        indexing.request_id,
                        &indexing.captures,
                        reason,
                        compiler_failure,
                    )
                    .and_then(|()| {
                        // Refusal commits a current source/capture frontier even
                        // when it cannot replace a coherent semantic generation.
                        // Reconcile through the same authenticated publication
                        // seam as success before exposing that terminal outcome.
                        self.publish_view(daemon, None).map_err(|error| {
                            BuiltinModelError(format!(
                                "reconcile committed source-capture view: {error}"
                            ))
                        })
                    }) {
                        outcome = capture_terminalization_failed(outcome, error);
                    }
                }
                let outcome = if let Some(reason) = index_attempt_retirement_reason(&outcome) {
                    let mut retirement_failure = None;
                    for attempt in terminal_attempts.take().unwrap_or_default() {
                        if let Err(error) = self
                            .semantic_authority
                            .retire_candidate_attempt(&attempt, reason)
                        {
                            retirement_failure.get_or_insert(error);
                        }
                    }
                    if let Some(error) = retirement_failure {
                        backend_library::IndexJobOutcome::Failed(bounded_index_detail(error))
                    } else {
                        outcome
                    }
                } else {
                    outcome
                };
                ready.extend(self.complete_index_job(daemon, indexing, outcome, legacy_reply));
            }
        }
        while self.indexing.is_none()
            && !self.index_operations.has_prepared().unwrap_or(true)
            && let Some((ticket, body)) = self.waiting.pop_front()
        {
            let abandoned = self.abandoned_replies.remove(&ticket);
            match self.execute_or_defer(daemon, &body, ticket) {
                Ok(Executed::Reply(reply)) if !abandoned => ready.push((ticket, Ok(reply))),
                Ok(Executed::Deferred) if abandoned => self.abandon_active_reply(ticket),
                Ok(Executed::Reply(_)) | Ok(Executed::Deferred) => {}
                Err(error) if !abandoned => ready.push((ticket, Err(error))),
                Err(_) => {}
            }
        }
        self.with_browse_completions(daemon, ready)
    }

    /// Releases a transport response registration only. Commands already in
    /// the writer queue remain queued and accepted index work keeps running.
    pub(in crate::builtin) fn abandon_reply(&mut self, ticket: u64) {
        if self.waiting.iter().any(|(queued, _)| *queued == ticket) {
            self.abandoned_replies.insert(ticket);
            return;
        }
        self.abandon_active_reply(ticket);
    }

    fn abandon_active_reply(&mut self, ticket: u64) {
        if let Some(indexing) = &mut self.indexing {
            if indexing
                .legacy_add
                .is_some_and(|(transport_ticket, _)| transport_ticket == ticket)
            {
                indexing.legacy_add = None;
            }
            indexing
                .awaiters
                .retain(|(transport_ticket, _)| *transport_ticket != ticket);
        }
        self.browse_lane.abandon_reply(ticket);
    }

    /// Checks the current owner after index publication and queued writers,
    /// immediately before serializing a finished Cargo browse reply.
    fn with_browse_completions(
        &mut self,
        daemon: &mut ProductDaemon,
        mut ready: Vec<(u64, Result<Vec<u8>, BuiltinModelError>)>,
    ) -> Vec<(u64, Result<Vec<u8>, BuiltinModelError>)> {
        let current_owner = daemon.engine().daemon().library().cursor();
        for (ticket, request_id, admitted_owner, advisory, terminal) in self.browse_lane.drain() {
            if let BrowseTerminal::Metadata(fetched, permit) = terminal {
                let package = fetched.request.package.clone();
                let observation = if fetched.request.owner_epoch != self.index_owner_epoch
                    || fetched.request.request_id != request_id
                {
                    Some(
                        backend_library::RegistryPackageDiscoveryObservation::Unavailable {
                            source: None,
                            reason: backend_library::ProductText::from_static(
                                "registry metadata request authority changed; retry",
                            ),
                        },
                    )
                } else {
                    self.discovery.as_mut().and_then(|discovery| {
                        discovery.admit_package_completion(fetched, self.index_owner_epoch)
                    })
                };
                let reply = self
                    .surface(
                        daemon,
                        backend_library::SurfaceCommand::Package {
                            package: backend_library::PackageReference::Purl(package.clone()),
                        },
                        request_id,
                    )
                    .map(|(reply, _)| reply);
                let reply = match (reply, observation) {
                    (Ok(CommandReply::Surface(backend_library::SurfaceReply::PackageDiscovery {
                        observation: backend_library::RegistryPackageDiscoveryObservation::Unavailable { .. }, ..
                    })), Some(observation @ backend_library::RegistryPackageDiscoveryObservation::Missing { .. })) => {
                        CommandReply::Surface(backend_library::SurfaceReply::PackageDiscovery { package, observation })
                    }
                    (Ok(CommandReply::Failed(_)) | Err(_), Some(observation)) => {
                        CommandReply::Surface(backend_library::SurfaceReply::PackageDiscovery {
                            package,
                            observation,
                        })
                    }
                    (Ok(reply), _) => reply,
                    (Err(error), _) => CommandReply::Failed(
                        backend_library::CommandFailure::InvalidQuery(error.to_string()),
                    ),
                };
                ready.push((
                    ticket,
                    Self::encode(daemon, request_id, (reply, None), None),
                ));
                drop(permit);
                continue;
            }
            let (reply, permit) = if advisory.is_metadata() {
                let reason = match terminal {
                    BrowseTerminal::Cancelled => "registry metadata request was cancelled",
                    BrowseTerminal::Deadline => "registry metadata request exceeded its deadline",
                    BrowseTerminal::Failed => {
                        "registry metadata worker could not complete the observation"
                    }
                    BrowseTerminal::Reply(_, _) | BrowseTerminal::Metadata(_, _) => {
                        unreachable!("metadata payloads use their source witness")
                    }
                };
                (
                    CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                        reason.to_owned(),
                    )),
                    None,
                )
            } else if admitted_owner != current_owner
                || !advisory.still_selected(self.registry.as_ref())
            {
                (
                    CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                        "owner view changed during Cargo browse observation; retry".to_owned(),
                    )),
                    None,
                )
            } else {
                match terminal {
                    BrowseTerminal::Reply(reply, permit) => (reply, Some(permit)),
                    BrowseTerminal::Cancelled => (
                        CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                            "Cargo browse request was cancelled".to_owned(),
                        )),
                        None,
                    ),
                    BrowseTerminal::Deadline => (
                        CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                            "Cargo browse request exceeded its deadline".to_owned(),
                        )),
                        None,
                    ),
                    BrowseTerminal::Failed => (
                        CommandReply::Failed(backend_library::CommandFailure::InvalidQuery(
                            "Cargo browse worker could not complete the observation".to_owned(),
                        )),
                        None,
                    ),
                    BrowseTerminal::Metadata(_, _) => {
                        unreachable!("metadata terminals use their narrower source witness")
                    }
                }
            };
            let encoded = Self::encode(daemon, request_id, (reply, None), None);
            drop(permit); // release only after serialization consumed the reply
            ready.push((ticket, encoded));
        }
        ready
    }

    /// Starts indexing a local folder. Owner relation capture is bounded and
    /// synchronous; filesystem discovery and workspace inventory run in a
    /// worker. `None` means the ticket remains active.
    fn start_index_job(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        execution_intent: CompileExecutionIntent,
        certificate: Option<&WireCertificate>,
        request_id: u64,
        owner_ticket: backend_library::IndexJobTicket,
        legacy_add: Option<u64>,
        operation_key: Option<backend_library::IndexOperationKey>,
    ) -> Result<Option<AdmittedReply>, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut indexing = IndexJob {
            owner_ticket,
            operation_key,
            captures: BTreeMap::new(),
            legacy_add: legacy_add.map(|ticket| (ticket, request_id)),
            awaiters: Vec::new(),
            cancelled: Arc::clone(&cancelled),
            progress_sequence: 0,
            progress_stage: None,
            request_id,
            captured_package: package,
            captured_label: label.clone(),
            requested_package,
            execution_intent,
            _staged_project: None,
            work: IndexJobWork::Transition,
        };
        match classify_add_target(&label)? {
            AddTarget::LocalDirectory => {
                let work = capture_index_scan(
                    daemon,
                    package,
                    &label,
                    Path::new(&label),
                    None,
                    request_id,
                    execution_intent,
                    self.owner_cluster.is_some(),
                    Arc::clone(&cancelled),
                )?;
                self.spawn_index_scan(&mut indexing, work)?;
            }
            AddTarget::PackageUrl => {
                let coordinate = backend_engine::registry::PackageCoordinate::parse(&label)
                    .map_err(|_| BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))?;
                if coordinate.as_str() != label {
                    return Err(BuiltinModelError(
                        "package URL is not in canonical form".to_owned(),
                    ));
                }
                self.set_index_progress_stage(
                    &mut indexing,
                    backend_library::IndexJobStage::Acquiring,
                );
                self.spawn_registry_acquisition(&mut indexing, coordinate)?;
            }
        }
        self.indexing = Some(indexing);
        Ok(None)
    }

    fn spawn_index_scan(
        &mut self,
        indexing: &mut IndexJob,
        work: IndexScanWork,
    ) -> Result<(), BuiltinModelError> {
        let (sender, scanned) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("locald-index-scan".to_owned())
            .spawn(move || {
                let _ = sender.send(run_index_scan(work));
            })
            .map_err(|error| BuiltinModelError(format!("start the index scan: {error}")))?;
        self.set_index_progress_stage(indexing, backend_library::IndexJobStage::Scanning);
        indexing.work = IndexJobWork::Scanning(scanned);
        Ok(())
    }

    fn spawn_registry_acquisition(
        &mut self,
        indexing: &mut IndexJob,
        coordinate: backend_engine::registry::PackageCoordinate,
    ) -> Result<(), BuiltinModelError> {
        let gateway = self.registry.take().ok_or_else(|| {
            BuiltinModelError(
                "no configured registry authority can acquire this package".to_owned(),
            )
        })?;
        let gateway_slot = Arc::new(Mutex::new(Some(gateway)));
        let worker_slot = Arc::clone(&gateway_slot);
        let cancelled = Arc::clone(&indexing.cancelled);
        let (sender, acquired) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("locald-index-acquire".to_owned())
            .spawn(move || {
                let Some(mut gateway) = worker_slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let archive = gateway.acquire_cancellable(&coordinate, &cancelled)?;
                    if cancelled.load(Ordering::Acquire) {
                        return Err(RegistryAddError::Cancelled);
                    }
                    let _ = sender.send(RegistryAcquisitionMessage::Staging);
                    gateway.stage_archive(&coordinate, &archive)
                }));
                let message = match result {
                    Ok(result) => RegistryAcquisitionMessage::Complete {
                        gateway: Some(gateway),
                        result,
                    },
                    Err(_) => RegistryAcquisitionMessage::Complete {
                        gateway: None,
                        result: Err(RegistryAddError::WorkerPanicked),
                    },
                };
                let _ = sender.send(message);
            });
        if let Err(error) = spawned {
            self.registry = gateway_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            return Err(BuiltinModelError(format!(
                "start the registry acquisition: {error}"
            )));
        }
        indexing.work = IndexJobWork::Acquiring(acquired);
        Ok(())
    }

    /// Commits an index job's semantic intent and publishes the view: the
    /// end of `add` for a local folder.
    fn finish_add(
        &mut self,
        daemon: &mut ProductDaemon,
        prepared: PreparedProductSelection,
        request_id: u64,
        requested_package: backend_engine::PackageKey,
        operation_key: Option<backend_library::IndexOperationKey>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let PreparedProductSelection {
            intent: prepared_intent,
            selected,
            revision_fence,
        } = prepared;
        let intent = match (prepared_intent, operation_key) {
            (Some(intent), Some(operation_key)) => Some(intent.with_operation_key(operation_key)?),
            (intent, _) => intent,
        };
        let prepared_intent = intent
            .as_ref()
            .map(|intent| {
                prepare_builtin_intent_at(
                    daemon,
                    intent,
                    revision_fence.as_ref().map(|fence| fence.canonical_root()),
                    Arc::clone(&cancellation),
                )
            })
            .transpose()?;
        let request_identity = prepared_intent
            .as_ref()
            .map(PreparedBuiltinIntent::request_identity);
        let (base_workspace_root, base_workspace_sequence) = prepared_intent
            .as_ref()
            .map(|intent| {
                (
                    intent.base_workspace_root(),
                    intent.base_workspace_sequence(),
                )
            })
            .unwrap_or_else(|| {
                let head = daemon.engine().daemon().owner().head();
                (*head.root().as_bytes(), head.sequence())
            });
        let removals = prepared_intent
            .as_ref()
            .map(|intent| selected_semantic_removals(&intent.intent))
            .unwrap_or_default();
        let index_operations = &mut self.index_operations;
        let semantic_authority = &mut self.semantic_authority;
        let committed = semantic_authority.commit_product_selection_changes(
            selected,
            removals,
            || {
                    if let Some(revision_fence) = revision_fence.as_ref()
                        && !super::super::ingest::compiler_revision_is_current(revision_fence)
                            .map_err(BuiltinModelError)?
                    {
                        return Err(BuiltinModelError(
                                "compiler source or configuration revision changed before product selection; retry indexing"
                                    .to_owned(),
                            ));
                    }
                    if let Some(operation_key) = operation_key {
                        index_operations
                            .prepare(
                                operation_key,
                                request_identity,
                                base_workspace_root,
                                base_workspace_sequence,
                            )
                            .map_err(|error| {
                                BuiltinModelError(format!(
                                    "prepare durable index-operation receipt: {error}"
                                ))
                            })?;
                    }
                    prepared_intent
                        .map(|prepared| {
                            let intent = prepared.intent.clone();
                            commit_prepared_builtin_intent(daemon, request_id, prepared).map_err(
                                |error| {
                                    BuiltinModelError(format!(
                                        "commit product source intent: {error}"
                                    ))
                                },
                            )?;
                            Ok(intent)
                        })
                        .transpose()
                },
        )?;
        self.publish_view(daemon, committed.as_ref())?;
        if let Some(operation_key) = operation_key {
            if let Ok(Some(JournalEntry::Retained(entry))) =
                self.index_operations.entry(operation_key)
                && let Some(source_capture) = entry.source_capture.as_ref()
            {
                self.refresh_index_operation_source_capture(
                    daemon,
                    operation_key,
                    &entry.package,
                    source_capture,
                )
                .map_err(|error| {
                    BuiltinModelError(format!(
                        "record semantic outcome for source capture: {error}"
                    ))
                })?;
            }
            let receipt = self
                .current_index_operation_receipt(
                    daemon,
                    request_identity,
                    base_workspace_root,
                    base_workspace_sequence,
                )
                .ok_or_else(|| {
                    BuiltinModelError(
                        "published workspace and product view do not establish one exact index-operation receipt"
                            .to_owned(),
                    )
                })?;
            // The workspace commit and view are already authoritative. If
            // this terminal replacement fails, leave the prepared record in
            // place so status can reconstruct the same checked receipt.
            let _ = self.index_operations.published(operation_key, receipt);
        }
        Ok(added_reply(requested_package))
    }

    pub(in crate::builtin) fn execute(
        &mut self,
        daemon: &mut ProductDaemon,
        body: &[u8],
    ) -> Result<Vec<u8>, BuiltinModelError> {
        let owner = daemon.engine().daemon().library().cursor();
        let request = backend_engine::decode_command_dto_for_owner(body, owner)
            .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
        let request_id = request.request_id;
        let certificate = request.certificate().cloned();
        let is_search = matches!(&request.command, Command::Search(_));
        self.pending_semantic_search = None;
        let admitted = self.dispatch(daemon, request.command, certificate, request_id)?;
        let status = if is_search {
            self.pending_semantic_search.take()
        } else {
            None
        };
        Self::encode(daemon, request_id, admitted, status)
    }

    /// One admitted reply as the wire's reply DTO.
    fn encode(
        daemon: &ProductDaemon,
        request_id: u64,
        (reply, certificate): AdmittedReply,
        search_status: Option<backend_library::SemanticSearchStatus>,
    ) -> Result<Vec<u8>, BuiltinModelError> {
        let mut reply = match reply {
            CommandReply::Health(root) => backend_engine::ReplyDto::health(
                request_id,
                root,
                daemon.engine().daemon().library().cursor(),
            ),
            reply => backend_engine::ReplyDto::new(request_id, reply),
        };
        if let Some(status) = search_status {
            reply = reply.with_semantic_search_status(status);
        }
        if let Some(certificate) = certificate {
            reply = reply.with_certificate(certificate);
        }
        backend_engine::encode_reply_dto(&reply).map_err(BuiltinModelError)
    }

    /// Decodes and serves one local semantic range against a fresh Turso head.
    /// The payload's selected stamp is only a claim; the resolver reopens the
    /// exact current metadata before and after the bounded CAS read.
    pub(in crate::builtin) fn serve_semantic_range(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        use backend_replication::{SemanticRangeChunk, SemanticRangeGet};

        let request = SemanticRangeGet::decode(&payload).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl("semantic range request is malformed")
        })?;
        if request.request_id != request_id {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range correlation mismatch",
            ));
        }
        let package = backend_engine::PackageReference::parse(request.target.package().to_owned())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range package target is invalid",
                )
            })?;
        let coordinate =
            PackageUrl::parse(request.target.coordinate().to_owned()).map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range compiler coordinate is invalid",
                )
            })?;
        if coordinate.as_str() != request.target.coordinate() {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range compiler coordinate is not canonical",
            ));
        }
        let key = ProductSemanticPublicationKey::new(
            package,
            coordinate.clone(),
            request.target.profile(),
        )
        .map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic range product target is inconsistent",
            )
        })?;
        let target = backend_engine::application::CompilerPackageTargetV2::for_package(coordinate);
        let identity = self
            .compiler
            .execution_identity(
                target.target(),
                request.target.profile(),
                backend_semantic::vocabulary::Stage::LowerIr,
            )
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic range target has no admitted compiler runtime",
            ))?;
        if identity.target() != target.target()
            || identity.profile() != request.target.profile()
            || identity.stage() != backend_semantic::vocabulary::Stage::LowerIr
        {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range target differs from its compiler runtime",
            ));
        }

        let mut resolver = super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
            &self.semantic_authority,
            key,
        );
        let service = self.semantic_authority.versioned_plane_service();
        let bytes = service
            .serve_range_claim(
                &mut resolver,
                request.selected_stamp,
                request.image,
                request.range_request,
                request.byte_range,
                identity.environment_identity(),
                identity.target_platform_identity(),
            )
            .map_err(|error| {
                map_semantic_authority_error(error, "semantic range request is unavailable")
            })?;
        let chunk = SemanticRangeChunk::for_request(&request, bytes);
        chunk.validate_against(&request).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic range chunk failed identity admission",
            )
        })?;
        chunk
            .encode()
            .map(|bytes| bytes.into_boxed_slice())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range chunk exceeds bounds",
                )
            })
    }

    /// Dispatches the canonical semantic paging DTOs to their typed
    /// authority handlers while preserving one bounded owner admission seam.
    pub(in crate::builtin) fn serve_semantic_control(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        match payload.get(5).copied() {
            Some(1) => self.serve_semantic_range(request_id, payload),
            Some(4 | 6 | 8) => self.serve_semantic_metadata(request_id, payload),
            _ => Err(crate::protocol::ProtocolError::InvalidControl(
                "unknown semantic paging operation",
            )),
        }
    }

    /// Serves one bounded semantic catalog, manifest, or full-image page
    /// against fresh Turso selection. The DTO's stamp and identity fields stay
    /// claims until the local authority reopens and rechecks the current head.
    pub(in crate::builtin) fn serve_semantic_metadata(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        use backend_replication::{
            SelectedSemanticImageGet, SemanticCatalogGet, SemanticManifestGet,
        };

        let tag = payload
            .get(5)
            .copied()
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata request is malformed",
            ))?;
        match tag {
            4 => {
                let get = SemanticCatalogGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog request is malformed",
                    )
                })?;
                let correlated_id = get.request_id;
                let target = get.target.clone();
                let (key, identity) = self.semantic_metadata_context(&target)?;
                if correlated_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog correlation mismatch",
                    ));
                }
                let mut resolver =
                    super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
                        &self.semantic_authority,
                        key,
                    );
                let service = self.semantic_authority.versioned_plane_service();
                let chunk = service
                    .serve_catalog_get(
                        &mut resolver,
                        &get,
                        identity.environment_identity(),
                        identity.target_platform_identity(),
                    )
                    .map_err(|error| {
                        map_semantic_authority_error(
                            error,
                            "semantic catalog request is unavailable",
                        )
                    })?;
                let encoded = chunk.encode().map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog page exceeds bounds",
                    )
                })?;
                return Ok(encoded.into_boxed_slice());
            }
            6 => {
                let get = SemanticManifestGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest request is malformed",
                    )
                })?;
                let correlated_id = get.request_id;
                let target = get.target.clone();
                let (key, identity) = self.semantic_metadata_context(&target)?;
                if correlated_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest correlation mismatch",
                    ));
                }
                let mut resolver =
                    super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
                        &self.semantic_authority,
                        key,
                    );
                let service = self.semantic_authority.versioned_plane_service();
                let chunk = service
                    .serve_manifest_get(
                        &mut resolver,
                        &get,
                        identity.environment_identity(),
                        identity.target_platform_identity(),
                    )
                    .map_err(|error| {
                        map_semantic_authority_error(
                            error,
                            "semantic manifest request is unavailable",
                        )
                    })?;
                let encoded = chunk.encode().map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest page exceeds bounds",
                    )
                })?;
                return Ok(encoded.into_boxed_slice());
            }
            8 => {
                let get = SelectedSemanticImageGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "selected semantic image request is malformed",
                    )
                })?;
                if get.request_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "selected semantic image correlation mismatch",
                    ));
                }
                let (key, identity) = self.semantic_metadata_context(&get.target)?;
                let chunk = super::super::selected_full_image::serve_selected_image_get(
                    &mut self.semantic_authority,
                    &key,
                    &get,
                    identity.environment_identity(),
                    identity.target_platform_identity(),
                )
                .map_err(|error| {
                    let message = match error {
                        super::super::selected_full_image::SelectedFullImageError::StaleSelection => {
                            "selected semantic image is stale"
                        }
                        super::super::selected_full_image::SelectedFullImageError::PlatformOrEnvironmentMismatch => {
                            "selected semantic image targets another environment or platform"
                        }
                        _ => "selected semantic image request is unavailable",
                    };
                    crate::protocol::ProtocolError::InvalidControl(message)
                })?;
                return chunk
                    .encode()
                    .map(|bytes| bytes.into_boxed_slice())
                    .map_err(|_| {
                        crate::protocol::ProtocolError::InvalidControl(
                            "selected semantic image page exceeds bounds",
                        )
                    });
            }
            _ => {
                return Err(crate::protocol::ProtocolError::InvalidControl(
                    "unknown semantic metadata operation",
                ));
            }
        }
    }

    fn semantic_metadata_context(
        &self,
        target: &backend_replication::SemanticTargetKey,
    ) -> Result<
        (
            ProductSemanticPublicationKey,
            backend_engine::application::LocalCompilerExecutionIdentity,
        ),
        crate::protocol::ProtocolError,
    > {
        let package = backend_engine::PackageReference::parse(target.package().to_owned())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic metadata package target is invalid",
                )
            })?;
        let coordinate = PackageUrl::parse(target.coordinate().to_owned()).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata compiler coordinate is invalid",
            )
        })?;
        if coordinate.as_str() != target.coordinate() {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata compiler coordinate is not canonical",
            ));
        }
        let key = ProductSemanticPublicationKey::new(package, coordinate.clone(), target.profile())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic metadata product target is inconsistent",
                )
            })?;
        let compiler_target =
            backend_engine::application::CompilerPackageTargetV2::for_package(coordinate);
        let identity = self
            .compiler
            .execution_identity(
                compiler_target.target(),
                target.profile(),
                backend_semantic::vocabulary::Stage::LowerIr,
            )
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata target has no admitted compiler runtime",
            ))?;
        if identity.target() != compiler_target.target()
            || identity.profile() != target.profile()
            || identity.stage() != backend_semantic::vocabulary::Stage::LowerIr
        {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata target differs from its compiler runtime",
            ));
        }
        Ok((key, identity))
    }

    fn dispatch(
        &mut self,
        daemon: &mut ProductDaemon,
        command: Command,
        certificate: Option<WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        match command {
            Command::Add {
                package,
                execution_intent,
            } => self.add(
                daemon,
                package,
                execution_intent,
                certificate.as_ref(),
                request_id,
            ),
            Command::Remove { package } => {
                self.remove(daemon, package, certificate.as_ref(), request_id)
            }
            Command::Search(query) => self.search(daemon, &query, certificate),
            Command::Graph(query) => self.graph(daemon, query, certificate, false),
            Command::GraphPage { symbol, page } => {
                self.graph_page(daemon, symbol, page, certificate)
            }
            Command::Related(query) => self.graph(daemon, query, certificate, true),
            Command::GraphQuery(request) => execute_certified_graph_query(
                daemon,
                &self.compiler,
                &mut self.search_snapshots,
                &mut self.generations,
                &mut self.image_rows,
                &request,
                certificate,
            ),
            Command::SemanticShapes(request) => {
                let published_roots = self
                    .published
                    .as_ref()
                    .map(|roots| (roots.source, roots.semantic));
                let reply = execute_semantic_shapes(
                    daemon,
                    &self.compiler,
                    &mut self.generations,
                    &mut self.image_rows,
                    &self.semantic_authority,
                    published_roots,
                    &request,
                )?;
                let command = Command::SemanticShapes(request);
                Self::certify(daemon, &command, reply, certificate)
            }
            Command::Surface(surface) => self.surface(daemon, surface, request_id),
            command => self.standard(daemon, &command, certificate),
        }
    }

    fn add(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        execution_intent: CompileExecutionIntent,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let prepared = match classify_add_target(&label)? {
            AddTarget::LocalDirectory => match index_project_intent_with_cluster_and_intent(
                daemon,
                package,
                &label,
                request_id,
                execution_intent,
                &self.compiler,
                &mut self.semantic_authority,
                self.owner_cluster.as_deref(),
                self.pending_stored_acks.as_ref(),
            ) {
                Ok(prepared) => prepared.unwrap_or(PreparedProductSelection {
                    intent: None,
                    selected: Vec::new(),
                    revision_fence: None,
                }),
                Err(refusal) => return Err(refusal),
            },
            AddTarget::PackageUrl => self
                .registry_intent(daemon, package, &label, request_id)?
                .unwrap_or(PreparedProductSelection {
                    intent: None,
                    selected: Vec::new(),
                    revision_fence: None,
                }),
        };
        let PreparedProductSelection {
            intent,
            selected,
            revision_fence,
        } = prepared;
        let removals = intent
            .as_ref()
            .map(selected_semantic_removals)
            .unwrap_or_default();
        let committed =
            self.semantic_authority
                .commit_product_selection_changes(selected, removals, || {
                    if let Some(revision_fence) = revision_fence.as_ref()
                        && !super::super::ingest::compiler_revision_is_current(revision_fence)
                            .map_err(BuiltinModelError)?
                    {
                        return Err(BuiltinModelError(
                            "compiler source or configuration revision changed before product selection; retry indexing"
                                .to_owned(),
                        ));
                    }
                    intent
                        .map(|intent| {
                            commit_builtin_intent(daemon, request_id, &intent).map_err(
                                |error| {
                                    BuiltinModelError(format!(
                                        "commit product source intent: {error}"
                                    ))
                                },
                            )?;
                            Ok(intent)
                        })
                        .transpose()
                })?;
        self.publish_view(daemon, committed.as_ref())?;
        Ok(added_reply(requested_package))
    }

    fn registry_intent(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        label: &str,
        request_id: u64,
    ) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
        let coordinate = backend_engine::registry::PackageCoordinate::parse(label)
            .map_err(|_| BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))?;
        if coordinate.as_str() != label {
            return Err(BuiltinModelError(
                "package URL is not in canonical form".to_owned(),
            ));
        }
        let Some(gateway) = self.registry.as_mut() else {
            // The coordinate itself is still an admitted, exact package
            // identity. Retain it with an explicit compiler-authority
            // unavailable terminal so every downstream surface observes the
            // semantic plane and no source-shaped placeholder is presented as
            // compiler truth. A configured registry continues through the
            // acquisition and compiler-authority path below.
            return BuiltinIntent::add(package, label.to_owned()).map(|intent| {
                Some(PreparedProductSelection {
                    intent: Some(intent),
                    selected: Vec::new(),
                    revision_fence: None,
                })
            });
        };
        let archive = gateway
            .acquire(&coordinate)
            .map_err(|error| BuiltinModelError(format!("registry add: {error}")))?;
        let staged = gateway
            .stage_archive(&coordinate, &archive)
            .map_err(|error| BuiltinModelError(format!("stage registry archive: {error}")))?;
        index_project_intent_at(
            daemon,
            package,
            label,
            staged.path(),
            Some(&coordinate),
            request_id,
            &self.compiler,
            &mut self.semantic_authority,
        )
    }

    /// Resolves a document by the canonical coordinate carried in the
    /// caller's admitted key claim. Semantic rows use a compiler-owned symbol
    /// identity rather than `symbol_key(label)`, while the public document
    /// command intentionally accepts the coordinate an agent copied from a
    /// search page. The claim supplies that preimage, so resolve the row by
    /// exact label after checking the request root instead of returning a
    /// false not-found for a published semantic declaration.
    fn canonical_claim_document(
        &self,
        daemon: &ProductDaemon,
        query: &backend_library::DocumentQuery,
        certificate: Option<&WireCertificate>,
        recover_excerpt: bool,
    ) -> Option<backend_library::Document> {
        let library = daemon.engine().daemon().library();
        let root = library.revision_root();
        let source_basis = backend_library::Basis {
            root,
            ..library.view().basis()
        };
        if !query.basis().matches(root)
            || query
                .source_basis()
                .is_some_and(|source| source != source_basis)
        {
            return None;
        }
        let label = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && query.symbol().matches(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        });
        let row = if query.symbol().is_selected() {
            let symbol = query.resolve_symbol(library.view())?;
            library
                .view()
                .row_ref(backend_engine::RowId::Symbol(symbol))?
        } else {
            symbol_row_by_label(library.view(), label?)?
        };
        let label = row.label.as_str();
        let backend_engine::RowId::Symbol(symbol) = row.id else {
            return None;
        };
        let excerpt =
            if recover_excerpt && row.excerpt == backend_library::SourceExcerpt::NotHydrated {
                self.recover_indexed_excerpt(daemon, row, label)
                    .unwrap_or_else(|| row.excerpt.clone())
            } else {
                row.excerpt.clone()
            };
        let mut document = backend_library::Document::new(symbol, root, row.document.clone())
            .with_source_basis(source_basis)
            .with_location(row.source.clone())
            .with_excerpt(excerpt)
            .with_facts(row.facts.clone());
        if !query.symbol().is_selected() {
            document = document
                .with_selection(backend_library::DocumentSelection::from_row(
                    row,
                    source_basis,
                )?)
                .ok()?;
        }
        document.signature.clone_from(&row.signature);
        Some(document)
    }

    /// Rehydrates a shed declaration excerpt from the exact package source
    /// file admitted by the current source relation. The indexed source
    /// identity must still match before the baseline parser can produce text.
    fn recover_indexed_excerpt(
        &self,
        daemon: &ProductDaemon,
        row: &backend_library::Row,
        label: &str,
    ) -> Option<backend_compile::SourceExcerpt> {
        let location = row.source.captured()?;
        let package = row.package?;
        let workspace = self.product_state.workspace_path().ok()?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let sources = super::super::read_package_sources(&snapshot, package).ok()?;
        let project = sources.projects.get(&package.to_bytes())?;
        let (claimed_project, _) = label.split_once("::")?;
        if project.package != package
            || claimed_project != project.label
            || backend_engine::PackageKey::from_value(&project.label) != package
        {
            return None;
        }
        let source_root = admitted_project_source_root(package, &project.label, workspace)?;
        let fields = sources.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.project == package.to_bytes() && fields.path == location.path())
                .then_some(fields)
        })?;
        let source_identity = fields.source_identity?;
        super::super::ingest::recover_indexed_excerpt(
            &source_root,
            fields.path,
            source_identity,
            fields.language,
            label,
            location.start_line(),
            row.kind,
        )
    }

    fn remove(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let committed = if let Some(intent) =
            remove_project_intent(daemon, package, &label, &mut self.semantic_authority)?
        {
            let removals = self
                .semantic_authority
                .selected_product_keys_for_package(package)?;
            self.semantic_authority.commit_product_selection_changes(
                Vec::new(),
                removals,
                || {
                    commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                        BuiltinModelError(format!("commit product source intent: {error}"))
                    })
                },
            )?;
            Some(intent)
        } else {
            None
        };
        self.publish_view(daemon, committed.as_ref())?;
        let intent_id = backend_engine::intent_id("remove_package", requested_package.as_bytes());
        let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: backend_engine::encode_id(intent_id.as_bytes()),
            token: "remove_package".to_owned(),
            payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
        });
        Ok((CommandReply::Removed(intent_id), Some(certificate)))
    }

    fn publish_view(
        &mut self,
        daemon: &mut ProductDaemon,
        edit: Option<&BuiltinIntent>,
    ) -> Result<(), BuiltinModelError> {
        // Reconcile even after a no-op source intent so a retry heals a crash
        // between the durable source commit and its derived view publication.
        // A missing witness, or an edit that is not the transition adjacent to
        // it, still hydrates the selected relations.
        let deployment = super::super::SemanticDeployment::from_remote(&self.remote_semantic);
        let filesystem_workspace = self
            .product_state
            .workspace_path()
            .map_err(BuiltinModelError)?;
        let outcome = publish_builtin_view(
            daemon,
            &self.compiler,
            deployment,
            filesystem_workspace,
            self.published.as_ref(),
            edit,
            &mut self.image_rows,
            &mut self.generations,
        )
        .map_err(|error| BuiltinModelError(format!("publish product source view: {error}")))?;
        self.published = Some(outcome.roots);
        project_view_deltas(&mut self.sql_projection, daemon, &outcome.deltas)?;
        self.semantic_authority.mark_projections_current()
    }

    fn search(
        &mut self,
        daemon: &ProductDaemon,
        query: &backend_engine::Query,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = Command::Search(query.clone());
        let (reply, status) = execute_search(
            daemon,
            &self.compiler,
            &mut self.search_snapshots,
            &mut self.remote_semantic,
            &mut self.generations,
            &mut self.image_rows,
            query,
        )
        .map_or_else(
            |error| (CommandReply::Error(error.to_string()), None),
            |(reply, status)| (reply, Some(status)),
        );
        self.pending_semantic_search = status;
        Self::certify(daemon, &command, reply, certificate)
    }

    fn graph(
        &mut self,
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<WireCertificate>,
        include_incoming: bool,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = if include_incoming {
            Command::Related(query)
        } else {
            Command::Graph(query)
        };
        let query = Self::claimed_graph_source(daemon, query, certificate.as_ref());
        let reply = self
            .graph_snapshot(daemon, query, include_incoming)?
            .map_or_else(
                || {
                    daemon
                        .engine()
                        .daemon()
                        .library()
                        .execute(command.clone())
                        .unwrap_or_else(|error| CommandReply::Failed(error.into()))
                },
                CommandReply::Graph,
            );
        Self::certify(daemon, &command, reply, certificate)
    }

    fn graph_snapshot(
        &mut self,
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        include_incoming: bool,
    ) -> Result<Option<backend_engine::ViewSnapshot>, BuiltinModelError> {
        match execute_semantic_graph(
            daemon,
            &self.compiler,
            &mut self.generations,
            &mut self.image_rows,
            query,
            include_incoming,
        )? {
            Some(snapshot) => Ok(Some(snapshot)),
            None => execute_structural_call_graph(daemon, query, include_incoming),
        }
    }

    fn graph_page(
        &mut self,
        daemon: &ProductDaemon,
        symbol: backend_engine::SymbolAddress,
        page: backend_engine::PageRequest,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = Command::GraphPage { symbol, page };
        let library = daemon.engine().daemon().library();
        let selected =
            Self::claimed_graph_symbol(daemon, symbol, page.basis(), certificate.as_ref());
        let reply = if let Some(resolved) = selected.resolve(library.view()) {
            let query =
                backend_engine::GraphNeighborhoodQuery::new(resolved, library.revision_root());
            // A stale page must be refused before executing against a newer graph.
            if !page.basis().matches(library.revision_root()) {
                library.graph_page_for_address(resolved, symbol, page)
            } else if let Some(snapshot) = self.graph_snapshot(daemon, query, false)? {
                let ids = snapshot
                    .root
                    .rows()
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>();
                library.graph_page_from_ids(resolved, symbol, page, &ids)
            } else {
                library.graph_page_for_address(resolved, symbol, page)
            }
            .map(CommandReply::ProjectionPage)
            .unwrap_or_else(|error| CommandReply::Error(error.to_string()))
        } else {
            library
                .execute(command.clone())
                .unwrap_or_else(|error| CommandReply::Error(error.to_string()))
        };
        Self::certify(daemon, &command, reply, certificate)
    }

    /// Resolves a graph source named by the canonical coordinate a caller
    /// copied from a result page.
    ///
    /// A client addresses a declaration by `symbol_key(coordinate)`, which is
    /// the row key of a structural declaration but not of a semantic one: a
    /// compiler-backed row is keyed by its compiler-owned identity. Like
    /// `canonical_claim_document`, this reads the coordinate from the
    /// caller's admitted key claim and, when no view row carries the
    /// requested key, selects the one row whose label is exactly that
    /// coordinate. Without it every `graph` and `related` request for a
    /// semantic declaration failed with "semantic graph source is absent".
    fn claimed_graph_source(
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<&WireCertificate>,
    ) -> backend_engine::GraphNeighborhoodQuery {
        let symbol = Self::claimed_graph_symbol(daemon, query.symbol(), query.basis(), certificate);
        symbol
            .resolve(daemon.engine().daemon().library().view())
            .map_or(query, |symbol| query.with_resolved_symbol(symbol))
    }

    /// Shares the exact admitted coordinate lookup between complete and paged graphs.
    fn claimed_graph_symbol(
        daemon: &ProductDaemon,
        address: backend_engine::SymbolAddress,
        basis: backend_library::ViewRevision,
        certificate: Option<&WireCertificate>,
    ) -> backend_engine::SymbolAddress {
        let library = daemon.engine().daemon().library();
        let view = library.view();
        let requested = address.resolve(view);
        if requested.is_some_and(|symbol| view.row(backend_engine::RowId::Symbol(symbol)).is_some())
            || !basis.matches(library.revision_root())
        {
            return address;
        }
        let Some(label) = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && requested == Some(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        }) else {
            return address;
        };
        symbol_row_by_label(view, label)
            .and_then(|row| match row.id {
                backend_engine::RowId::Symbol(symbol) => Some(symbol),
                _ => None,
            })
            .map_or(address, backend_engine::SymbolAddress::selected)
    }

    fn surface(
        &mut self,
        daemon: &mut ProductDaemon,
        surface: backend_engine::SurfaceCommand,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match surface {
            backend_engine::SurfaceCommand::ForgeAdd { coordinate } => {
                self.forge.acquire(coordinate.as_str()).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(backend_engine::SurfaceReply::ForgePackageAdded(
                            record,
                        ))
                    },
                )
            }
            backend_engine::SurfaceCommand::ForgeReference { coordinate } => {
                self.forge.reference(coordinate.as_str()).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(backend_engine::SurfaceReply::ForgePackageReferenced(
                            record,
                        ))
                    },
                )
            }
            backend_engine::SurfaceCommand::ProjectTree { root } => {
                let authority = self.registry.as_ref().map(RegistryGateway::advisory);
                self.browse
                    .project_tree(Path::new(root.as_str()), authority)
                    .map_or_else(
                        |error| {
                            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                                error,
                            ))
                        },
                        |tree| {
                            CommandReply::Surface(backend_engine::SurfaceReply::ProjectTree(
                                Box::new(tree),
                            ))
                        },
                    )
            }
            backend_engine::SurfaceCommand::CargoPackageSourceFile { request, path } => {
                CommandReply::Surface(backend_engine::SurfaceReply::CargoPackageSourceFile(
                    self.browse.source_file(request, path),
                ))
            }
            backend_engine::SurfaceCommand::CargoPackageSourceInventory { request } => {
                CommandReply::Surface(backend_engine::SurfaceReply::CargoPackageSourceInventory(
                    self.browse.source_inventory(request),
                ))
            }
            backend_engine::SurfaceCommand::CargoPackageReadme { request } => {
                CommandReply::Surface(backend_engine::SurfaceReply::CargoPackageReadme(
                    self.browse.package_readme(request),
                ))
            }
            backend_engine::SurfaceCommand::CargoPackageReadmeLink { request } => {
                CommandReply::Surface(backend_engine::SurfaceReply::CargoPackageReadmeLink(
                    self.browse.package_readme_link(request),
                ))
            }
            backend_engine::SurfaceCommand::AdvisoryRefresh => match self.registry.as_mut() {
                Some(gateway) => gateway.refresh_advisories().map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(error))
                    },
                    |states| {
                        CommandReply::Surface(backend_engine::SurfaceReply::AdvisoryRefreshed(
                            states.into_boxed_slice(),
                        ))
                    },
                ),
                None => CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                    "no advisory authority is open in this owner".to_owned(),
                )),
            },
            backend_engine::SurfaceCommand::References { target } => execute_references(
                daemon,
                &self.compiler,
                &mut self.generations,
                &mut self.image_rows,
                &target,
            )
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                CommandReply::Surface,
            ),
            backend_engine::SurfaceCommand::Diff { from, to } => execute_semantic_diff(
                daemon,
                &self.compiler,
                &mut self.generations,
                &mut self.image_rows,
                &from,
                &to,
            )
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                |rows| CommandReply::Surface(backend_engine::SurfaceReply::Diff(rows)),
            ),
            backend_engine::SurfaceCommand::SemanticVersions { package } => {
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root());
                semantic_versions(daemon, &package, workspace, &mut self.semantic_authority)
            }
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                |records| {
                    CommandReply::Surface(backend_engine::SurfaceReply::SemanticVersions(records))
                },
            ),
            backend_engine::SurfaceCommand::SemanticShapes { .. } => CommandReply::Failed(
                backend_engine::CommandFailure::InvalidQuery(
                    "semantic shapes require the certificate-bearing direct command selected by Session".to_owned())),
            backend_engine::SurfaceCommand::PackageSourceMembership { request } => {
                package_source_membership_page(daemon, &request).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |page| {
                        CommandReply::Surface(
                            backend_engine::SurfaceReply::PackageSourceMembershipPage(page),
                        )
                    },
                )
            }
            backend_engine::SurfaceCommand::SelectSemanticVersion {
                package,
                coordinate,
                profile,
                generation,
            } => self
                .select_semantic_version(
                    daemon, package, coordinate, profile, generation, request_id,
                )
                .map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(
                            backend_engine::SurfaceReply::SemanticVersionSelected(record),
                        )
                    },
                ),
            surface => {
                // Capture the SQL base before asking any source owner for its
                // current catalog or local manifest snapshot. A content digest
                // computed from cached facts cannot substitute for this CAS.
                let graph_base =
                    super::super::sql_projection::GraphBase::capture(&self.sql_projection, daemon)
                        .map_err(|error| {
                            BuiltinModelError(format!("capture package graph revision: {error}"))
                        })?;
                if let Some(discovery) = self.discovery.as_mut() {
                    discovery.apply_pending();
                }
                let roots = self
                    .project_roots
                    .roots(&daemon.engine().daemon().owner().snapshot())?;
                self.manifests
                    .refresh(roots.iter().map(std::path::PathBuf::as_path))
                    .map_err(BuiltinModelError)?;
                let local_witness = self.manifests.witness();
                let observed_catalog = self
                    .registry
                    .as_mut()
                    .map(RegistryGateway::catalog_projection)
                    .transpose()
                    .map_err(|error| BuiltinModelError(error.to_string()))?;
                let view = graph_base.view();
                let root = *view.root().as_bytes();
                let catalog = match observed_catalog.as_ref() {
                    Some(snapshot) => ResidentCatalog::Registry(Arc::clone(snapshot)),
                    None => self
                        .dependencies
                        .as_ref()
                        .map(|cached| cached.catalog.clone())
                        .filter(|catalog| catalog.dependency_revision().is_none())
                        .unwrap_or_else(|| {
                            ResidentCatalog::Unconfigured(Arc::new(
                                CatalogLookupIndex::from_catalog(&[]),
                            ))
                        }),
                };
                // Known-package metadata is independent of the asynchronous
                // namespace walk. Acquired records keep their existing owner.
                let package_observation = match &surface {
                    backend_engine::SurfaceCommand::Package {
                        package: backend_engine::PackageReference::Purl(coordinate),
                    } if !catalog.records().iter().any(|record| record.coordinate.as_str() == coordinate.as_str()) => {
                        self.discovery.as_ref().and_then(|gateway| gateway.cached_package_observation(coordinate))
                    }
                    _ => None,
                };
                let manifests = &self.manifests;
                let graph_limits = self.graph_limits;
                ResidentDependencies::select(
                    &mut self.dependencies,
                    local_witness,
                    catalog,
                    |catalog| {
                        backend_library::IndexedCheckedPackageGraph::from_borrowed_facts(
                            catalog.dependency_facts().iter().chain(manifests.facts()),
                            graph_limits,
                        )
                        .map_err(|error| {
                            BuiltinModelError(format!("check package dependency facts: {error}"))
                        })
                    },
                )?;
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root().to_path_buf());
                let cached = self.dependencies.as_mut().ok_or_else(|| {
                    BuiltinModelError("dependency index disappeared after refresh".to_owned())
                })?;
                let target = GraphProjectionStamp {
                    root: view.root(),
                    facts_witness: cached.graph.indexed.witness(),
                };
                if cached.graph.synced != Some(target) {
                    if let (Some(registry), Some(snapshot)) =
                        (self.registry.as_mut(), observed_catalog.as_ref())
                        && !registry
                            .validate_resident_projection(snapshot)
                            .map_err(|error| BuiltinModelError(error.to_string()))?
                    {
                        return Err(BuiltinModelError(
                            "registry source changed before package graph publication".to_owned(),
                        ));
                    }
                    graph_base
                        .synchronize(
                            &mut self.sql_projection,
                            cached.graph.indexed.checked_facts(),
                        )
                        .map_err(|error| {
                            BuiltinModelError(format!("align package graph projection: {error}"))
                        })?;
                    cached.graph.synced = Some(target);
                }
                let discovery_store = self.discovery.as_ref().map(|gateway| gateway.store());
                let forge_records = if matches!(
                    &surface,
                    backend_engine::SurfaceCommand::IndexSearch { .. }
                        | backend_engine::SurfaceCommand::Package { .. }
                ) {
                    self.forge.search_records().map_err(BuiltinModelError)?
                } else {
                    Vec::new()
                };
                let reply = match &surface {
                    backend_engine::SurfaceCommand::PackageGraphPage { request } => request
                        .clone()
                        .bind_catalog_snapshot(cached.catalog.rows_snapshot())
                        .map_err(|error| error.to_string())
                        .and_then(|request| {
                            let page = futures_executor::block_on(
                                self.sql_projection.read_package_graph_page(&request),
                            )
                            .map_err(|error| error.to_string())?;
                            page.admit_against_checked_facts(&request, root, &cached.graph.indexed)
                                .map_err(|error| error.to_string())?;
                            Ok(backend_engine::SurfaceReply::PackageGraphPage(page))
                        }),
                    _ => self
                        .product_state
                        .execute_with_discovery_and_forge_snapshot(
                            surface.clone(),
                            view,
                            cached.catalog.records(),
                            cached.catalog.index(),
                            &cached.graph.indexed,
                            workspace.as_deref(),
                            discovery_store,
                            cached.catalog.rows_snapshot(),
                            &forge_records,
                        ),
                };
                let reply = reply.or_else(|error| match (&surface, package_observation) {
                    (backend_engine::SurfaceCommand::Package {
                        package: backend_engine::PackageReference::Purl(package),
                    }, Some(observation)) => Ok(backend_engine::SurfaceReply::PackageDiscovery {
                        package: package.clone(), observation,
                    }),
                    _ => Err(error),
                });
                reply.map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(error))
                    },
                    CommandReply::Surface,
                )
            }
        };
        Ok((reply, None))
    }

    fn select_semantic_version(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageReference,
        coordinate: PackageUrl,
        profile: backend_engine::SemanticLanguageProfile,
        generation: backend_engine::SemanticGenerationId,
        request_id: u64,
    ) -> Result<backend_engine::SemanticVersionRecord, BuiltinModelError> {
        let profile = profile
            .profile()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let coordinate_text = coordinate.as_str().to_owned();
        let package_key = backend_engine::package_key(package.as_str());
        let selected_key = ProductSemanticPublicationKey::new(package.clone(), coordinate, profile)
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
        if let Ok(history_key) = selected_key.for_generation_bytes(generation.to_bytes()) {
            let relation = daemon
                .engine()
                .daemon()
                .owner()
                .snapshot()
                .relation::<BuiltinSemanticRelation>()
                .map_err(|error| {
                    BuiltinModelError(format!("open semantic version history: {error}"))
                })?;
            let history_record = relation.lookup(&history_key).map_err(|error| {
                BuiltinModelError(format!("read semantic version history: {error}"))
            })?;
            if let Some(record) = history_record {
                history_key.admit_record(&record).map_err(|error| {
                    BuiltinModelError(format!("admit selected semantic generation: {error}"))
                })?;
                let ProductSemanticPublicationRecord::Published { coverage, claim } =
                    record.clone()
                else {
                    return Err(BuiltinModelError(
                        "semantic generation history contains an unavailable terminal".to_owned(),
                    ));
                };
                let before = relation.lookup(&selected_key).map_err(|error| {
                    BuiltinModelError(format!("read selected semantic generation: {error}"))
                })?;
                let committed = if before != Some(record.clone()) {
                    self.semantic_authority
                        .select_existing(&selected_key, claim)?;
                    let intent = BuiltinIntent::index_with_semantics(
                        package_key,
                        package.as_str(),
                        Vec::new(),
                        vec![BuiltinSemanticChange {
                            key: selected_key.clone(),
                            after: Some(record.clone()),
                        }],
                    )?;
                    self.semantic_authority
                        .commit_product_selection_transaction(
                            vec![(selected_key.clone(), claim)],
                            || {
                                commit_builtin_intent(daemon, request_id, &intent).map_err(
                                    |error| {
                                        BuiltinModelError(format!(
                                            "commit semantic generation selection: {error}"
                                        ))
                                    },
                                )
                            },
                        )?;
                    Some(intent)
                } else {
                    None
                };
                self.publish_view(daemon, committed.as_ref())?;
                let freshness_key =
                    super::super::semantic_authority::SelectedSemanticPublicationKey::new(
                        &selected_key,
                    )
                    .map_err(|error| BuiltinModelError(error.to_owned()))?;
                let freshness_snapshot = daemon.engine().daemon().owner().snapshot();
                let mut version = semantic_version_record(
                    &selected_key,
                    coverage,
                    claim,
                    true,
                    self.semantic_authority
                        .freshness(&freshness_snapshot, freshness_key, claim)?,
                );
                version.history_status = self
                    .semantic_authority
                    .native_history_status(&selected_key, claim)?;
                return Ok(version);
            }
        }
        let workspace = self
            .registry
            .as_ref()
            .map(|gateway| gateway.workspace_root());
        let view = daemon.engine().daemon().library().view();
        let indexed =
            super::super::product_state::indexed_semantic_versions(view, &package, workspace)
                .map_err(BuiltinModelError)?;
        let selected_language = profile.language();
        indexed
            .iter()
            .find(|row| {
                row.coordinate.as_str() == coordinate_text
                    && row.generation == generation
                    && row
                        .profile
                        .profile()
                        .ok()
                        .is_some_and(|row_profile| row_profile.language() == selected_language)
            })
            .cloned()
            .ok_or_else(|| BuiltinModelError("semantic generation is not retained".to_owned()))
    }

    fn standard(
        &self,
        daemon: &ProductDaemon,
        command: &Command,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match command {
            Command::Document(query) | Command::Source(query) => self
                .canonical_claim_document(
                    daemon,
                    query,
                    certificate.as_ref(),
                    matches!(command, Command::Source(_)),
                )
                .map_or_else(
                    || daemon.engine().daemon().library().execute(command.clone()),
                    |document| Ok(CommandReply::Document(document)),
                )
                .unwrap_or_else(|error| CommandReply::Error(error.to_string())),
            _ => daemon
                .engine()
                .daemon()
                .library()
                .execute(command.clone())
                .unwrap_or_else(|error| CommandReply::Error(error.to_string())),
        };
        let reply = semantic_readiness(reply, daemon, &self.remote_semantic, &self.compiler)?;
        Self::certify(daemon, command, reply, certificate)
    }

    fn certify(
        daemon: &ProductDaemon,
        command: &Command,
        reply: CommandReply,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let certificate = projection::reply_certificate(
            command,
            &reply,
            daemon.engine().daemon().library().view(),
            daemon.engine().daemon().library().cursor(),
            certificate,
        )?;
        Ok((reply, certificate))
    }
}

fn semantic_readiness(
    reply: CommandReply,
    daemon: &ProductDaemon,
    remote: &super::super::query::RemoteSemantic,
    compiler: &LocalCompilerClient,
) -> Result<CommandReply, BuiltinModelError> {
    let CommandReply::Readiness(report) = reply else {
        return Ok(reply);
    };
    let local = ingest::semantic_capabilities(report.capabilities(), compiler)
        .map_err(|error| BuiltinModelError(format!("local semantic readiness: {error}")))?;
    let capabilities = remote
        .inventory(&local)
        .map_err(|error| BuiltinModelError(format!("project semantic readiness: {error}")))?;
    // A deployment that configured no embedding provider has a terminally
    // unavailable semantic lane, and a health reply must say so instead of
    // dropping the fact on the floor. The published view root already folds the
    // same classification in, so this reconciliation is a fixed point for every
    // reply the owner certifies: `readiness_certificate` rejects a report whose
    // coverage differs from that root, and the lane is described exactly once.
    let coverage = super::super::reconcile_semantic_lane(
        report.coverage(),
        super::super::SemanticDeployment::from_remote(remote),
    );
    // Progress is read out of the committed source relation rather than out of
    // a counter the scan kept, so it describes the revision this very report
    // names. `readiness_certificate` compares revision, basis, coverage, and
    // row count; the counts are derived from the same owner snapshot, so they
    // cannot disagree with the root the certificate commits to.
    let progress = super::super::ingest_progress(&daemon.engine().daemon().owner().snapshot())?;
    Ok(CommandReply::Readiness(
        backend_engine::HealthReport::from_admitted_parts(
            report.revision(),
            report.basis(),
            coverage.into_boxed_slice(),
            report.row_count(),
            capabilities,
        )
        .with_progress(progress),
    ))
}

fn project_view_deltas(
    projection: &mut backend_extension_turso::TursoProjection,
    daemon: &ProductDaemon,
    deltas: &[backend_engine::CommittedViewDelta],
) -> Result<(), BuiltinModelError> {
    if deltas.is_empty() {
        return Ok(());
    }
    if let Err(incremental_error) = futures_executor::block_on(projection.apply_all(deltas)) {
        super::super::sql_projection::synchronize_current(projection, daemon)
        .map_err(|rebuild_error| {
            BuiltinModelError(format!(
                "Turso projection delta failed ({incremental_error}); rebuild failed: {rebuild_error}"
            ))
        })?;
    }
    Ok(())
}

fn canonical_local_package(
    package: backend_engine::PackageKey,
    label: String,
) -> Result<(backend_engine::PackageKey, String), BuiltinModelError> {
    let path = Path::new(&label);
    if !path.is_dir() {
        return Ok((package, label));
    }
    let canonical = path.canonicalize().map_err(|error| {
        BuiltinModelError(format!("canonicalize local package {label}: {error}"))
    })?;
    let label = canonical.to_string_lossy().into_owned();
    Ok((backend_engine::package_key(&label), label))
}

fn certified_package_label(
    certificate: Option<&WireCertificate>,
    package: backend_engine::PackageKey,
) -> Result<String, BuiltinModelError> {
    let id = backend_engine::encode_id(package.as_bytes());
    let certificate = certificate
        .ok_or_else(|| BuiltinModelError("package command certificate is missing".to_owned()))?;
    let mut value = None;
    for claim in &certificate.claims {
        if let WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: claimed,
            value: text,
        } = claim
            && claimed == &id
        {
            if value.is_some() {
                return Err(BuiltinModelError(
                    "package command certificate has duplicate claims".to_owned(),
                ));
            }
            value = Some(text.clone());
        }
    }
    value.ok_or_else(|| {
        BuiltinModelError("package command certificate has no canonical package text".to_owned())
    })
}

fn selected_semantic_removals(intent: &BuiltinIntent) -> Vec<ProductSemanticPublicationKey> {
    intent
        .semantic_changes()
        .iter()
        .filter(|change| {
            change.key.is_selected()
                && matches!(
                    change.after.as_ref(),
                    None | Some(ProductSemanticPublicationRecord::Unavailable(_))
                )
        })
        .map(|change| change.key.clone())
        .collect()
}

pub(in crate::builtin) fn commit_builtin_intent(
    daemon: &mut ProductDaemon,
    request_id: u64,
    intent: &BuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let prepared = prepare_builtin_intent(daemon, intent)?;
    commit_prepared_builtin_intent(daemon, request_id, prepared)
}

/// A canonical intent together with the selected base against which any
/// capture basis was prepared. Its private fields prevent callers from
/// changing the intent after deriving the request identity.
pub(in crate::builtin) struct PreparedBuiltinIntent {
    intent: BuiltinIntent,
    request_identity: [u8; 32],
    base_workspace_root: [u8; 32],
    base_workspace_sequence: u64,
    base_closure_id: [u8; 32],
}

impl PreparedBuiltinIntent {
    pub(in crate::builtin) const fn request_identity(&self) -> [u8; 32] {
        self.request_identity
    }

    pub(in crate::builtin) const fn base_workspace_root(&self) -> [u8; 32] {
        self.base_workspace_root
    }

    pub(in crate::builtin) const fn base_workspace_sequence(&self) -> u64 {
        self.base_workspace_sequence
    }
}

/// Commits the exact prepared intent and refuses if the workspace selection
/// changed after its request identity and capture basis were bound.
pub(in crate::builtin) fn commit_prepared_builtin_intent(
    daemon: &mut ProductDaemon,
    request_id: u64,
    prepared: PreparedBuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let owner = daemon.engine().daemon().owner();
    let snapshot = owner.snapshot();
    if *snapshot.root().as_bytes() != prepared.base_workspace_root
        || snapshot.sequence() != prepared.base_workspace_sequence
        || *snapshot.closure().binding().closure().as_bytes() != prepared.base_closure_id
    {
        return Err(BuiltinModelError(
            "prepared builtin intent base changed before commit".to_owned(),
        ));
    }
    let expected = owner.head().expectation();
    let request = prepared.request_identity;
    let intent = prepared.intent;
    let receiver = daemon
        .client()
        .request(
            request_id,
            crate::Request::Commit {
                request,
                expected,
                intent,
            },
        )
        .map_err(|error| BuiltinModelError(format!("queue builtin intent: {error:?}")))?;
    if !daemon.serve_one() {
        return Err(BuiltinModelError(
            "builtin intent owner did not make progress".to_owned(),
        ));
    }
    let expected_sequence = prepared
        .base_workspace_sequence
        .checked_add(1)
        .ok_or_else(|| BuiltinModelError("workspace sequence exhausted".to_owned()))?;
    let expected_request_identity = prepared.request_identity;
    match crate::service::wait_for_daemon_reply(daemon, &receiver)
        .map_err(|error| BuiltinModelError(error.to_string()))?
    {
        backend_engine::DaemonReply::Commit(Ok(head)) => {
            if head.request_identity() != expected_request_identity
                || head.sequence() != expected_sequence
            {
                return Err(BuiltinModelError(format!(
                    "owner commit receipt did not match the prepared intent (request match: {}, sequence: {} expected {})",
                    head.request_identity() == expected_request_identity,
                    head.sequence(),
                    expected_sequence
                )));
            }
            Ok(())
        }
        backend_engine::DaemonReply::Commit(Err(error)) => {
            Err(BuiltinModelError(error.to_string()))
        }
        _ => Err(BuiltinModelError(
            "builtin intent was sent to the wrong owner lane".to_owned(),
        )),
    }
}

/// Adds the authenticated selected-base binding required by the current
/// capture intent format. Callers that need the canonical request identity
/// before committing must hash this prepared value, then commit that exact
/// value so the persisted capture rows and operation receipt share one ID.
pub(in crate::builtin) fn prepare_builtin_intent(
    daemon: &ProductDaemon,
    intent: &BuiltinIntent,
) -> Result<PreparedBuiltinIntent, BuiltinModelError> {
    prepare_builtin_intent_at(daemon, intent, None, Arc::new(AtomicBool::new(false)))
}

fn prepare_builtin_intent_at(
    daemon: &ProductDaemon,
    intent: &BuiltinIntent,
    source_root: Option<&Path>,
    cancellation: Arc<AtomicBool>,
) -> Result<PreparedBuiltinIntent, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let intent = if intent.has_capture_changes() && intent.capture_basis().is_none() {
        intent
            .clone()
            .with_capture_basis(capture_basis_for_snapshot(&snapshot)?)?
    } else {
        intent.clone()
    };
    let request_identity = BuiltinModel.request_id(&intent);
    let intent =
        super::super::staged_transport::stage(intent, &snapshot, source_root, cancellation)?;
    Ok(PreparedBuiltinIntent {
        intent,
        request_identity,
        base_workspace_root: *snapshot.root().as_bytes(),
        base_workspace_sequence: snapshot.sequence(),
        base_closure_id: *snapshot.closure().binding().closure().as_bytes(),
    })
}

fn symbol_row_by_label<'a>(
    view: &'a backend_engine::ViewRoot,
    label: &str,
) -> Option<&'a backend_engine::Row> {
    view.unique_symbol_by_label(label)
}

/// Resolves the source root named by the package's admitted project record.
/// Local projects are stored under their canonical directory label; staged
/// packages retain their canonical pinned package URL.
fn admitted_project_source_root(
    package: backend_engine::PackageKey,
    project_label: &str,
    workspace: &Path,
) -> Option<std::path::PathBuf> {
    if backend_engine::PackageKey::from_value(project_label) != package {
        return None;
    }
    let root =
        super::super::local_manifest::indexed_package_source_root(project_label, workspace).ok()?;
    let canonical = root.canonicalize().ok()?;
    if !project_label.starts_with("pkg:") && Path::new(project_label) != canonical {
        return None;
    }
    Some(canonical)
}

fn map_semantic_authority_error(
    error: super::super::versioned_planes::VersionedPlaneServiceError,
    unavailable: &'static str,
) -> crate::protocol::ProtocolError {
    match error {
        super::super::versioned_planes::VersionedPlaneServiceError::StaleSelection => {
            crate::protocol::ProtocolError::SemanticStaleSelection
        }
        _ => crate::protocol::ProtocolError::InvalidControl(unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADD_TARGET_REQUIRED, AddTarget, CommandAdapter, Executed, GraphProjectionStamp, IndexJob,
        IndexJobWork, MAX_WAITING_COMMANDS, ProductDaemon, ResidentCatalog, ResidentDependencies,
        admitted_project_source_root, capture_terminalization_failed, classify_add_target,
        index_operation_failure, legacy_add_compiler_failure, pending_capture_unresolved,
    };
    use crate::builtin::{
        BuiltinIntent, BuiltinModel, BuiltinProfile, BuiltinSemanticRelation,
        BuiltinWorkspaceRelation, ECHO_AUTHORITY_SECRET, SemanticDeployment, builtin_dispatcher,
        forge_gateway::ForgeGateway, genesis, profile_descriptor, publish_builtin_view,
    };
    use crate::process::{ForgeAuthentication, ForgeConfig};
    use backend_engine::application::{
        LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
        LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
        LocalRuntimeToolchain,
    };
    use backend_library::CompileExecutionIntent;
    use backend_semantic::vocabulary::NativeTool;
    use std::collections::BTreeMap;
    use std::fs;
    use std::num::NonZeroUsize;

    fn typed_compiler_refusal() -> backend_library::IndexJobOutcome {
        let attempt = backend_library::interface::CompilerAttempt {
            source: backend_library::interface::SourceAuthority {
                identity: backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                    b"source bytes",
                ),
                byte_len: 12,
            },
            recipe: backend_version::ContentId::<backend_version::CompileRecipeDomain>::from_canonical_bytes(
                b"recipe bytes",
            ),
        };
        let fragment_failure = backend_library::interface::CompilerFragmentFailure::build(
            backend_semantic::ir::BuildError::InvalidOccurrenceSpan {
                owner: backend_semantic::ir::EntityId::new(7),
                start: 18,
                end: 24,
            },
        );
        let failure = backend_library::PackageCompilerFailure::from_fragment_failure(
            "src/recovery.ts",
            attempt,
            &fragment_failure,
        )
        .expect("typed package compiler failure");
        backend_library::IndexJobOutcome::RefusedWithCompilerFailure {
            detail: backend_library::ProductText::new("compiler rejected src/recovery.ts")
                .expect("bounded refusal detail"),
            failure,
        }
    }

    #[test]
    fn source_capture_terminalization_failure_keeps_primary_typed_refusal() {
        let primary = typed_compiler_refusal();
        let backend_library::IndexJobOutcome::RefusedWithCompilerFailure { failure, .. } = &primary
        else {
            unreachable!("fixture is typed");
        };
        let expected_failure = failure.clone();
        let combined = capture_terminalization_failed(primary, "store write refused");
        let backend_library::IndexJobOutcome::RefusedWithCompilerFailure { detail, failure } =
            combined
        else {
            panic!("cleanup failure must not erase the typed compiler refusal");
        };
        assert!(
            detail
                .as_str()
                .contains("compiler rejected src/recovery.ts")
        );
        assert!(detail.as_str().contains("store write refused"));
        assert_eq!(failure, expected_failure);
    }

    #[test]
    fn live_terminal_pending_capture_is_not_reported_as_restart() {
        let outcome = typed_compiler_refusal();
        let (reason, detail) = pending_capture_unresolved(Some(&outcome));
        assert_eq!(
            reason,
            backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed
        );
        assert!(
            detail
                .as_str()
                .contains("live index job reached a terminal outcome")
        );
        assert!(
            detail
                .as_str()
                .contains("compiler rejected src/recovery.ts")
        );
        assert!(!detail.as_str().contains("restarted"));

        let (restart_reason, restart_detail) = pending_capture_unresolved(None);
        assert_eq!(
            restart_reason,
            backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture
        );
        assert!(restart_detail.as_str().contains("owner restarted"));
    }

    #[test]
    fn legacy_add_failure_keeps_exact_compiler_summary() {
        let attempt = backend_library::interface::CompilerAttempt {
            source: backend_library::interface::SourceAuthority {
                identity: backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                    b"source bytes",
                ),
                byte_len: 12,
            },
            recipe: backend_version::ContentId::<backend_version::CompileRecipeDomain>::from_canonical_bytes(
                b"recipe bytes",
            ),
        };
        let fragment_failure = backend_library::interface::CompilerFragmentFailure::build(
            backend_semantic::ir::BuildError::InvalidOccurrenceSpan {
                owner: backend_semantic::ir::EntityId::new(7),
                start: 18,
                end: 24,
            },
        );
        let failure = backend_library::PackageCompilerFailure::from_fragment_failure(
            "src/recovery.ts",
            attempt,
            &fragment_failure,
        )
        .expect("typed package compiler failure");
        let outcome = backend_library::IndexJobOutcome::RefusedWithCompilerFailure {
            detail: backend_library::ProductText::new("local compiler rejected src/recovery.ts")
                .expect("bounded refusal detail"),
            failure: failure.clone(),
        };

        let backend_library::CommandFailure::CompilerRefused {
            detail,
            failure: projected,
        } = legacy_add_compiler_failure(&outcome).expect("legacy Add retains typed refusal")
        else {
            panic!("typed outcome must remain a typed command failure");
        };
        assert_eq!(detail, "local compiler rejected src/recovery.ts");
        assert_eq!(projected, failure);
        let (reason, detail, retained) = index_operation_failure(Some(&outcome));
        assert_eq!(
            reason,
            backend_library::IndexOperationFailureReason::Refused
        );
        assert_eq!(detail.as_str(), "local compiler rejected src/recovery.ts");
        assert_eq!(retained, Some(failure));
        let (reason, _, retained) = index_operation_failure(None);
        assert_eq!(
            reason,
            backend_library::IndexOperationFailureReason::WorkerFailed
        );
        assert_eq!(
            retained, None,
            "absence of an attempt cannot manufacture compiler facts"
        );
        assert_eq!(projected.relative_path(), "src/recovery.ts");
        assert_eq!(projected.kind_tag(), "build_invalid_occurrence_span");
        assert!(projected.detail().contains("occurrence span"));
    }
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use super::{Command, WireCertificate, WireClaim};

    fn checked_resident_graph(reason: &'static str) -> backend_library::IndexedCheckedPackageGraph {
        backend_library::IndexedCheckedPackageGraph::new(
            vec![(
                backend_library::PackageGraphSourceKey::new(
                    backend_library::PackageReference::parse("pkg:cargo/resident-fixture@1.0.0")
                        .expect("source coordinate"),
                    backend_library::PackageGraphSourceAuthority::Registry(
                        backend_library::RegistryAuthorityId::from_configured_source([7; 32]),
                    ),
                ),
                backend_library::DependencyFacts::Unknown(
                    backend_library::ProductText::from_static(reason),
                ),
            )],
            crate::process::default_package_graph_limits(),
        )
        .expect("checked source facts")
    }

    fn unconfigured_catalog() -> ResidentCatalog {
        ResidentCatalog::Unconfigured(Arc::new(super::CatalogLookupIndex::from_catalog(&[])))
    }

    #[test]
    fn unchanged_graph_inputs_skip_fact_builder_and_retain_selected_snapshot() {
        let mut resident = None;
        ResidentDependencies::select(&mut resident, [1; 32], unconfigured_catalog(), |_| {
            Ok(checked_resident_graph("original dependency evidence"))
        })
        .expect("initial graph");
        let cached = resident.as_mut().expect("resident graph");
        let original = cached.graph.indexed.checked_facts().clone();
        let stamp = GraphProjectionStamp {
            root: backend_library::view_state_root(&[]),
            facts_witness: original.witness(),
        };
        cached.graph.synced = Some(stamp);
        let overlay = unconfigured_catalog();
        let ResidentCatalog::Unconfigured(expected_overlay) = &overlay else {
            panic!("unconfigured overlay");
        };
        let expected_overlay = Arc::clone(expected_overlay);

        ResidentDependencies::select(&mut resident, [1; 32], overlay, |_| {
            panic!("unchanged graph inputs must never rebuild facts");
        })
        .expect("select overlay without graph work");
        let cached = resident.as_ref().expect("retained graph");
        assert!(std::ptr::eq(cached.graph.indexed.facts(), original.facts()));
        assert!(cached.graph.synced == Some(stamp));
        let ResidentCatalog::Unconfigured(actual_overlay) = &cached.catalog else {
            panic!("unconfigured overlay");
        };
        assert!(Arc::ptr_eq(actual_overlay, &expected_overlay));
        assert!(matches!(
            cached.graph.indexed.dependencies(&original.facts()[0].0.coordinate),
            backend_library::PackageDependencyLookup::Exact {
                facts: backend_library::DependencyFacts::Unknown(reason),
                ..
            } if reason.as_str() == "original dependency evidence"
        ));
    }

    #[test]
    fn changed_graph_inputs_rebuild_once_and_failed_rebuild_preserves_resident() {
        let mut resident = None;
        ResidentDependencies::select(&mut resident, [1; 32], unconfigured_catalog(), |_| {
            Ok(checked_resident_graph("original dependency evidence"))
        })
        .expect("initial graph");
        let original = resident
            .as_ref()
            .expect("resident graph")
            .graph
            .indexed
            .checked_facts()
            .clone();
        let mut builds = 0;
        ResidentDependencies::select(&mut resident, [2; 32], unconfigured_catalog(), |_| {
            builds += 1;
            Ok(checked_resident_graph("updated dependency evidence"))
        })
        .expect("changed graph");
        assert_eq!(builds, 1);
        let cached = resident.as_mut().expect("changed graph residence");
        let changed = cached.graph.indexed.checked_facts().clone();
        assert_ne!(changed.witness(), original.witness());
        assert!(!std::ptr::eq(changed.facts(), original.facts()));
        let stamp = GraphProjectionStamp {
            root: backend_library::view_state_root(&[]),
            facts_witness: changed.witness(),
        };
        cached.graph.synced = Some(stamp);
        let ResidentCatalog::Unconfigured(catalog_before_failure) = &cached.catalog else {
            panic!("unconfigured catalog");
        };
        let catalog_before_failure = Arc::clone(catalog_before_failure);

        let failed =
            ResidentDependencies::select(&mut resident, [3; 32], unconfigured_catalog(), |_| {
                Err(crate::builtin::BuiltinModelError(
                    "rebuild refused".to_owned(),
                ))
            });
        assert!(failed.is_err());
        let cached = resident
            .as_ref()
            .expect("prior successful residence retained");
        assert_eq!(cached.graph.local_witness, [2; 32]);
        assert!(std::ptr::eq(cached.graph.indexed.facts(), changed.facts()));
        assert!(cached.graph.synced == Some(stamp));
        let ResidentCatalog::Unconfigured(catalog_after_failure) = &cached.catalog else {
            panic!("unconfigured catalog");
        };
        assert!(Arc::ptr_eq(catalog_after_failure, &catalog_before_failure));
    }

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nudox-add-guard-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp tree");
            Self(path)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct AdapterFixture {
        root: TempTree,
        daemon: Option<ProductDaemon>,
        adapter: Option<CommandAdapter>,
        package: backend_engine::PackageKey,
        label: String,
    }

    impl AdapterFixture {
        fn new() -> Self {
            let root = TempTree::new();
            let workspace = root.0.join("workspace");
            fs::create_dir_all(&workspace).expect("workspace directory");
            // Commands admit a local directory under its physical identity.
            // Seed the same identity even when the OS temp path is a symlink.
            let workspace = workspace
                .canonicalize()
                .expect("physical workspace directory");
            let project = workspace.join("project");
            fs::create_dir_all(&project).expect("project directory");
            let label = project.to_str().expect("UTF-8 fixture path").to_owned();
            let package = backend_engine::package_key(&label);
            assert_eq!(
                project.canonicalize().expect("physical seed project"),
                project
            );

            let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
            let dispatcher =
                builtin_dispatcher(Some(ECHO_AUTHORITY_SECRET), Arc::clone(&profile), 60_000)
                    .expect("test dispatcher");
            let registry = super::super::super::product_relation_registry()
                .expect("product relation registry");
            let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
                &workspace,
                BuiltinModel,
                genesis().expect("product genesis"),
                dispatcher,
                backend_engine::DaemonConfig::default(),
                registry,
            )
            .expect("open product daemon");
            let intent = BuiltinIntent::add(package, label.clone()).expect("seed project intent");
            super::commit_builtin_intent(&mut daemon, 1, &intent).expect("commit seed project");
            // Product publication applies checked view deltas. Start with the
            // same source-bound baseline that production startup installs;
            // Library::new() is intentionally unbound and cannot authorize a
            // reset transition to the product view.
            let snapshot = daemon.engine().daemon().owner().snapshot();
            let (baseline, cursor) = crate::builtin::initial_view_for_workspace(&snapshot)
                .expect("checked current product baseline");
            let admission = crate::builtin::BuiltinViewAdmission {
                workspace_root: snapshot.root(),
                source_root: baseline.basis().root,
            };
            daemon
                .engine_mut()
                .daemon_mut()
                .publish_view(baseline, cursor, &admission, None)
                .expect("publish checked product baseline");

            let compiler_root = workspace.join("compiler-fixture");
            let paths = LocalCompilerRuntimePaths::new(
                compiler_root.join("artifacts"),
                compiler_root.join("journal"),
                compiler_root.join("native-work"),
            )
            .expect("absolute compiler fixture paths");
            let compiler_configuration = LocalCompilerRuntimeConfiguration::new(
                paths,
                vec![LocalRuntimeToolchain::unavailable(NativeTool::Python)].into_boxed_slice(),
                Box::new([]),
                LocalRuntimePackageAuthority::default(),
                LocalCompilerTimeout::new(Duration::from_secs(2)).expect("bounded timeout"),
                backend_store::journal::PublicationLimits::new(
                    NonZeroUsize::MIN,
                    NonZeroUsize::MIN,
                )
                .expect("bounded publication limits"),
                LocalCompilerScratch::with_fragment_capacity(
                    NonZeroUsize::new(1024 * 1024).expect("nonzero scratch capacity"),
                )
                .expect("bounded compiler scratch"),
            )
            .expect("canonical compiler runtime configuration");
            let compiler = LocalCompilerClient::start(compiler_configuration)
                .expect("start unavailable-toolchain compiler owner");

            let mut authority =
                super::super::super::semantic_authority::SemanticAuthority::open(&workspace)
                    .expect("open semantic authority");
            let mut image_rows = super::super::super::view_build::ImageRowResidence::default();
            let mut generations = super::super::super::SemanticGenerationResidence::default();
            authority.install_image_loader(&mut generations);
            let remote = super::super::super::query::RemoteSemantic::Unconfigured;
            let publication = publish_builtin_view(
                &mut daemon,
                &compiler,
                SemanticDeployment::from_remote(&remote),
                &workspace,
                None,
                Some(&intent),
                &mut image_rows,
                &mut generations,
            )
            .expect("publish seeded product view");
            let projection_path = workspace.join(backend_extension_turso::FILE_NAME);
            let sql_projection =
                super::super::super::sql_projection::open_current(&projection_path, &daemon)
                    .expect("open projection");
            authority
                .mark_projections_current()
                .expect("mark semantic projections current");
            let product_state =
                super::super::super::ProductState::open(workspace.join("product-state.json"))
                    .expect("open product state");
            let forge = ForgeGateway::open(
                workspace.join("forge"),
                ForgeConfig {
                    policy: backend_engine::ForgeAcquisitionPolicy::Offline,
                    limits: backend_engine::ForgeAcquisitionLimits::default(),
                    authentication: ForgeAuthentication::default(),
                },
            )
            .expect("open offline forge gateway");
            let adapter = CommandAdapter::new(
                sql_projection,
                None,
                forge,
                None,
                product_state,
                crate::process::default_package_graph_limits(),
                compiler,
                super::super::super::query::SearchSnapshotOwner::default(),
                remote,
                Some(publication.roots),
                image_rows,
                generations,
                authority,
                None,
                None,
            )
            .expect("build command adapter");

            Self {
                root,
                daemon: Some(daemon),
                adapter: Some(adapter),
                package,
                label,
            }
        }

        fn parts(&mut self) -> (&mut CommandAdapter, &mut ProductDaemon) {
            let adapter = self.adapter.as_mut().expect("adapter is present");
            let daemon = self.daemon.as_mut().expect("daemon is present");
            (adapter, daemon)
        }

        fn add_target(&self) -> (backend_engine::PackageKey, String) {
            let path = self.root.0.join("workspace").join("add-target");
            fs::create_dir_all(&path).expect("empty Add target directory");
            let path = path.canonicalize().expect("physical Add target directory");
            let label = path.to_str().expect("UTF-8 fixture path").to_owned();
            (backend_engine::package_key(&label), label)
        }
    }

    impl Drop for AdapterFixture {
        fn drop(&mut self) {
            if let Some(mut adapter) = self.adapter.take() {
                adapter.close();
                drop(adapter);
            }
            if let Some(mut daemon) = self.daemon.take() {
                daemon.close();
            }
            let _ = fs::remove_dir_all(&self.root.0);
        }
    }

    fn install_transition_job(adapter: &mut CommandAdapter) -> Arc<AtomicBool> {
        let package = backend_library::PackageReference::parse("pkg:cargo/fixture@1.0.0")
            .expect("fixture package reference");
        let owner_ticket = adapter.issue_index_ticket(package).expect("owner ticket");
        let cancelled = Arc::new(AtomicBool::new(false));
        adapter.indexing = Some(IndexJob {
            owner_ticket,
            operation_key: None,
            captures: BTreeMap::new(),
            legacy_add: None,
            awaiters: Vec::new(),
            cancelled: Arc::clone(&cancelled),
            progress_sequence: 0,
            progress_stage: None,
            request_id: 0,
            captured_package: backend_engine::package_key("pkg:cargo/fixture@1.0.0"),
            captured_label: "pkg:cargo/fixture@1.0.0".to_owned(),
            requested_package: backend_engine::package_key("pkg:cargo/fixture@1.0.0"),
            execution_intent: CompileExecutionIntent::Interactive,
            _staged_project: None,
            work: IndexJobWork::Transition,
        });
        cancelled
    }

    fn stalled_package_metadata_owner(abandon: bool) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").expect("real TCP metadata fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let endpoint = backend_engine::registry::RegistryEndpoint::new(
            backend_engine::registry::RegistryEcosystem::Pypi,
            format!("http://{}", listener.local_addr().expect("address")),
        )
        .expect("loopback metadata authority");
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stopped);
        let server = std::thread::spawn(move || {
            let mut release = Some(release_rx);
            let mut handlers = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(8);
            while !server_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("metadata accept: {error}"),
                };
                stream
                    .set_nonblocking(false)
                    .expect("blocking accepted stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read bound");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("write bound");
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0_u8; 1];
                    stream
                        .read_exact(&mut byte)
                        .expect("complete request headers");
                    headers.push(byte[0]);
                    assert!(headers.len() <= 16 * 1024);
                }
                let request = String::from_utf8(headers).expect("HTTP headers");
                if request.starts_with("GET /pypi/requests/json ") {
                    let release = release.take().expect("one exact point request");
                    let entered = entered_tx.clone();
                    handlers.push(std::thread::spawn(move || {
                        entered.send(()).expect("point entered");
                        release.recv_timeout(Duration::from_secs(5)).expect("release stalled HTTP");
                        let body = r#"{"info":{"name":"requests","version":"2.34.2"},"releases":{"2.34.2":[{"yanked":false,"upload_time_iso_8601":"2026-10-01T12:00:00Z"}]}}"#;
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        stream.write_all(response.as_bytes()).expect("complete point response");
                    }));
                } else {
                    stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("namespace failure is bounded");
                }
            }
            for handler in handlers {
                handler.join().expect("point handler");
            }
        });
        let mut fixture = AdapterFixture::new();
        let (new_package, new_label) = fixture.add_target();
        let discovery_root = fixture.root.0.join("registry-metadata");
        let (adapter, daemon) = fixture.parts();
        adapter.discovery = Some(
            crate::discovery::DiscoveryGateway::open(
                discovery_root,
                crate::process::RegistryDiscoveryConfig {
                    sources: vec![endpoint],
                    offline: false,
                    max_pages: 1,
                },
            )
            .expect("independent real discovery owner"),
        );
        let coordinate =
            backend_engine::registry::PackageCoordinate::parse("pkg:pypi/requests@2.34.2")
                .expect("pinned package");
        let body = |id, command| {
            serde_json::to_vec(&backend_engine::CommandDto::new(id, command)).expect("command DTO")
        };
        let started = Instant::now();
        assert!(matches!(
            adapter.execute_or_defer(
                daemon,
                &body(
                    801,
                    Command::Surface(backend_library::SurfaceCommand::Package {
                        package: backend_library::PackageReference::Purl(coordinate.clone()),
                    })
                ),
                1801
            ),
            Ok(Executed::Deferred)
        ));
        let admission_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(admission_ms < 1000.0);
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("HTTP connection remains stalled");
        let cancelled = install_transition_job(adapter);
        let index_ticket = adapter
            .indexing
            .as_ref()
            .expect("index owner")
            .owner_ticket
            .clone();
        let mut measurements = BTreeMap::new();
        for (id, name, command) in [
            (802, "health", Command::Health),
            (
                803,
                "index_search",
                Command::Surface(backend_library::SurfaceCommand::IndexSearch {
                    query: backend_library::ProductText::from_static("requests"),
                    limit: 2,
                    cursor: None,
                }),
            ),
            (
                804,
                "index_cancel",
                Command::Surface(backend_library::SurfaceCommand::IndexCancel {
                    ticket: index_ticket,
                }),
            ),
        ] {
            let started = Instant::now();
            assert!(
                matches!(
                    adapter.execute_or_defer(daemon, &body(id, command), id + 1000),
                    Ok(Executed::Reply(_))
                ),
                "{name} must reply while HTTP is stalled"
            );
            let millis = started.elapsed().as_secs_f64() * 1000.0;
            assert!(millis < 1000.0, "{name} blocked for {millis} ms");
            measurements.insert(name, millis);
        }
        assert!(cancelled.load(Ordering::Acquire));
        let before = owner_cursor(daemon);
        let intent =
            BuiltinIntent::add(new_package, new_label).expect("unrelated source publication");
        super::commit_builtin_intent(daemon, 805, &intent).expect("commit unrelated source");
        adapter
            .publish_view(daemon, Some(&intent))
            .expect("publish unrelated source");
        assert_ne!(owner_cursor(daemon), before);
        if abandon {
            adapter.abandon_reply(1801);
        }
        release_tx.send(()).expect("release network response");
        if abandon {
            adapter.browse_lane.close();
            assert!(
                adapter
                    .poll_deferred(daemon)
                    .iter()
                    .all(|(ticket, _)| *ticket != 1801)
            );
        } else {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let replies = adapter.poll_deferred(daemon);
                if let Some((_, reply)) = replies.into_iter().find(|(ticket, _)| *ticket == 1801) {
                    let reply =
                        reply.expect("metadata reply after unrelated workspace publication");
                    assert!(!String::from_utf8_lossy(&reply).contains("owner view changed"));
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "metadata terminal did not arrive"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let published = adapter
            .discovery
            .as_ref()
            .expect("gateway")
            .store()
            .facts()
            .any(|(_, fact)| fact.coordinate == coordinate);
        assert_eq!(
            published, !abandon,
            "only a current un-abandoned metadata task can publish"
        );
        eprintln!(
            "package_metadata_stalled_http_measurements {}",
            serde_json::json!({"abandoned": abandon, "admission_ms": admission_ms, "owner_reads_ms": measurements, "published": published})
        );
        drop(fixture);
        stopped.store(true, Ordering::Release);
        server.join().expect("bounded HTTP server");
    }

    #[test]
    fn package_metadata_stalled_http_keeps_owner_reads_responsive_and_survives_workspace_publish() {
        stalled_package_metadata_owner(false);
    }

    #[test]
    fn package_metadata_abandoned_stalled_http_cannot_publish() {
        stalled_package_metadata_owner(true);
    }

    fn remove_body(request_id: u64, package: backend_engine::PackageKey, label: &str) -> Vec<u8> {
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: backend_engine::encode_id(package.as_bytes()),
            value: label.to_owned(),
        });
        serde_json::to_vec(
            &backend_engine::CommandDto::new(request_id, Command::Remove { package })
                .with_certificate(certificate),
        )
        .expect("encode certified remove command")
    }

    fn add_body(request_id: u64, package: backend_engine::PackageKey, label: &str) -> Vec<u8> {
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: backend_engine::encode_id(package.as_bytes()),
            value: label.to_owned(),
        });
        serde_json::to_vec(
            &backend_engine::CommandDto::new(
                request_id,
                Command::Add {
                    package,
                    execution_intent: CompileExecutionIntent::Interactive,
                },
            )
            .with_certificate(certificate),
        )
        .expect("encode certified add command")
    }

    fn project_is_admitted(daemon: &ProductDaemon, package: backend_engine::PackageKey) -> bool {
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let relation = snapshot
            .relation::<BuiltinWorkspaceRelation>()
            .expect("workspace relation");
        relation
            .lookup(&package.to_bytes())
            .expect("project relation lookup")
            .is_some()
    }

    fn owner_cursor(daemon: &ProductDaemon) -> backend_library::Cursor {
        daemon.engine().daemon().library().cursor()
    }

    fn label(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Creates a directory symlink. Windows needs Developer Mode or the
    /// symbolic-link privilege; a refusal fails the test loudly rather than
    /// skipping the case.
    fn symlink_dir(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(original, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(original, link)
        }
    }

    /// Creates a file symlink; see [`symlink_dir`] for the Windows requirement.
    fn symlink_file(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(original, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(original, link)
        }
    }

    #[test]
    fn a_directory_is_indexed_in_place() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        fs::create_dir(&project).expect("project dir");
        assert!(matches!(
            classify_add_target(&label(&project)),
            Ok(AddTarget::LocalDirectory)
        ));
    }

    #[test]
    fn a_symlink_to_a_directory_is_indexed_in_place() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        let link = tree.0.join("link");
        fs::create_dir(&project).expect("project dir");
        symlink_dir(&project, &link).expect("directory symlink");
        assert!(matches!(
            classify_add_target(&label(&link)),
            Ok(AddTarget::LocalDirectory)
        ));
    }

    #[test]
    fn a_file_add_is_refused_instead_of_an_empty_project() {
        let tree = TempTree::new();
        let file = tree.0.join("lib.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        let name = label(&file);
        let package = backend_engine::package_key(&name);
        assert!(
            BuiltinIntent::add(package, name.clone()).is_ok(),
            "the empty-project constructor still accepts a file label"
        );
        let error = classify_add_target(&name).expect_err("file add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_symlink_to_a_file_is_refused() {
        let tree = TempTree::new();
        let file = tree.0.join("lib.rs");
        let link = tree.0.join("link.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        symlink_file(&file, &link).expect("file symlink");
        let error = classify_add_target(&label(&link)).expect_err("symlink add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_missing_path_is_refused() {
        let tree = TempTree::new();
        let missing = tree.0.join("missing");
        let error = classify_add_target(&label(&missing)).expect_err("missing add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_version_pinned_package_url_stays_a_registry_add() {
        assert!(matches!(
            classify_add_target("pkg:cargo/serde@1.0.0"),
            Ok(AddTarget::PackageUrl)
        ));
        assert!(matches!(
            classify_add_target("PKG:cargo/serde@1.0.0"),
            Ok(AddTarget::PackageUrl)
        ));
    }

    #[test]
    fn abandoned_queued_remove_runs_once_through_the_command_adapter() {
        let mut fixture = AdapterFixture::new();
        let package = fixture.package;
        let label = fixture.label.clone();
        let (adapter, daemon) = fixture.parts();
        let cancelled = install_transition_job(adapter);
        let before = owner_cursor(daemon);
        assert!(project_is_admitted(daemon, package));

        assert!(matches!(
            adapter.execute_or_defer(daemon, &remove_body(501, package, &label), 9001),
            Ok(Executed::Deferred)
        ));
        adapter.abandon_reply(9001);
        assert!(adapter.abandoned_replies.contains(&9001));
        assert!(!cancelled.load(Ordering::Acquire));

        let replies = adapter.poll_deferred(daemon);
        assert!(replies.iter().all(|(ticket, _)| *ticket != 9001));
        assert!(!adapter.abandoned_replies.contains(&9001));
        assert!(adapter.waiting.is_empty());
        assert!(adapter.indexing.is_none());
        assert!(!project_is_admitted(daemon, package));
        let after = owner_cursor(daemon);
        assert_ne!(after, before, "the admitted remove must commit");

        assert!(adapter.poll_deferred(daemon).is_empty());
        assert_eq!(owner_cursor(daemon), after, "the queued command runs once");
        assert!(!project_is_admitted(daemon, package));
    }

    #[test]
    fn refused_capture_reconciles_resident_view_before_terminal_reply() {
        let mut fixture = AdapterFixture::new();
        let (package, label) = fixture.add_target();
        fs::write(
            std::path::Path::new(&label).join("pyproject.toml"),
            "[project]\nname=\"refused_capture\"\nversion=\"1.0.0\"\n",
        )
        .expect("write Python manifest");
        fs::write(
            std::path::Path::new(&label).join("source.py"),
            "def captured_name() -> str:\n    return \"captured\"\n",
        )
        .expect("write source captured before unavailable compiler refusal");
        let (adapter, daemon) = fixture.parts();
        let before = daemon.engine().daemon().library().view().root();
        assert!(matches!(
            adapter.execute_or_defer(daemon, &add_body(850, package, &label), 9850),
            Ok(Executed::Deferred)
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut replies = Vec::new();
        let mut observed_inflight_capture = false;
        while adapter.indexing.is_some() {
            replies.extend(adapter.poll_deferred(daemon));
            if adapter
                .indexing
                .as_ref()
                .is_some_and(|job| matches!(&job.work, IndexJobWork::Compiling { .. }))
            {
                // The background result is not applied until the next owner
                // poll. Read the committed Pending frontier in that interval.
                let snapshot = daemon.engine().daemon().owner().snapshot();
                let expected = crate::builtin::builtin_view_capability_for_workspace(&snapshot)
                    .expect("current Pending capture capability");
                let view = daemon.engine().daemon().library().view();
                assert_ne!(
                    view.root(),
                    before,
                    "initial capture is visible before compilation"
                );
                assert_eq!(view.capability(), Some(expected));
                let query = backend_engine::Query::new(
                    "captured_name",
                    view.root(),
                    backend_engine::QueryLimit::default(),
                );
                let (reply, _) = adapter
                    .search(daemon, &query, None)
                    .expect("Pending owner query");
                assert!(
                    matches!(reply, backend_engine::CommandReply::Search(_)),
                    "{reply:?}"
                );
                observed_inflight_capture = true;
            }
            assert!(std::time::Instant::now() < deadline, "refused add terminal");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            observed_inflight_capture,
            "query the committed capture before terminal refusal"
        );
        let reply = replies
            .into_iter()
            .find(|(ticket, _)| *ticket == 9850)
            .expect("legacy terminal reply")
            .1
            .expect("encode typed refusal");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("actual DTO reply");
        assert_eq!(reply["reply"]["kind"], "failed", "{reply}");
        assert_eq!(
            reply["reply"]["data"]["kind"], "compiler_refused",
            "{reply}"
        );
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let expected = crate::builtin::builtin_view_capability_for_workspace(&snapshot)
            .expect("current committed workspace capability");
        let view = daemon.engine().daemon().library().view();
        assert_ne!(
            view.root(),
            before,
            "captured structural rows are published"
        );
        assert_eq!(view.capability(), Some(expected.clone()));
        assert!(
            view.row_refs()
                .any(|row| row.label.ends_with("::captured_name"))
        );
        let query = backend_engine::Query::new(
            "captured_name",
            view.root(),
            backend_engine::QueryLimit::default(),
        );
        let (reply, _) = adapter
            .search(daemon, &query, None)
            .expect("owner query reply");
        assert!(
            matches!(reply, backend_engine::CommandReply::Search(_)),
            "{reply:?}"
        );
        assert!(adapter.poll_deferred(daemon).is_empty());
        assert_eq!(
            daemon.engine().daemon().library().view().capability(),
            Some(expected)
        );
    }

    #[test]
    fn abandoning_legacy_add_reply_does_not_cancel_accepted_scan() {
        let mut fixture = AdapterFixture::new();
        let (package, label) = fixture.add_target();
        let (adapter, daemon) = fixture.parts();
        let before = owner_cursor(daemon);
        assert!(!project_is_admitted(daemon, package));

        assert!(matches!(
            adapter.execute_or_defer(daemon, &add_body(502, package, &label), 9002),
            Ok(Executed::Deferred)
        ));
        let indexing = adapter.indexing.as_ref().expect("Add scan accepted");
        assert_eq!(indexing.legacy_add, Some((9002, 502)));
        let cancelled = Arc::clone(&indexing.cancelled);
        adapter.abandon_reply(9002);
        let indexing = adapter
            .indexing
            .as_ref()
            .expect("accepted scan remains active");
        assert!(indexing.legacy_add.is_none(), "only the reply is detached");
        assert!(
            !cancelled.load(Ordering::Acquire),
            "Add work is not cancelled"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut replies = Vec::new();
        while adapter.indexing.is_some() {
            replies.extend(adapter.poll_deferred(daemon));
            assert!(
                std::time::Instant::now() < deadline,
                "accepted Add scan did not reach a terminal state"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(replies.iter().all(|(ticket, _)| *ticket != 9002));
        assert!(!cancelled.load(Ordering::Acquire));
        assert!(project_is_admitted(daemon, package));
        let after = owner_cursor(daemon);
        assert_ne!(after, before, "the accepted Add publishes its project");

        assert!(adapter.poll_deferred(daemon).is_empty());
        assert_eq!(owner_cursor(daemon), after);
        assert!(project_is_admitted(daemon, package));
    }

    #[test]
    fn aliased_local_add_refusal_closes_its_canonical_source_capture() {
        let mut fixture = AdapterFixture::new();
        let (canonical_package, canonical_label) = fixture.add_target();
        fs::write(
            std::path::Path::new(&canonical_label).join("module.py"),
            "class Session:\n    pass\n",
        )
        .expect("real Python source");
        let requested_label = format!("{canonical_label}/.");
        let requested_package = backend_engine::package_key(&requested_label);
        assert_ne!(requested_package, canonical_package);
        let profile = backend_semantic::vocabulary::LanguageProfile::Python(
            backend_semantic::vocabulary::PythonVersion::Python314,
        );
        let key = backend_engine::builtin::ProductSemanticPublicationKey::new(
            backend_engine::PackageReference::parse(canonical_label.clone())
                .expect("physical package reference"),
            crate::builtin::compiler_scope::semantic_coordinate(canonical_package, profile, None)
                .expect("physical semantic coordinate"),
            profile,
        )
        .expect("physical capture key");
        let (adapter, daemon) = fixture.parts();
        // The fixture deliberately has no Python toolchain. Each refusal must
        // close the actual Pending row, allowing another attempt at this alias.
        for request in [701, 702] {
            assert!(matches!(
                adapter.execute_or_defer(
                    daemon,
                    &add_body(request, requested_package, &requested_label),
                    request + 1000,
                ),
                Ok(Executed::Deferred)
            ));
            let indexing = adapter.indexing.as_ref().expect("accepted alias scan");
            assert_eq!(indexing.requested_package, requested_package);
            assert_eq!(indexing.captured_package, canonical_package);
            assert_eq!(indexing.captured_label, canonical_label);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while adapter.indexing.is_some() {
                adapter.poll_deferred(daemon);
                assert!(std::time::Instant::now() < deadline, "alias scan timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            let snapshot = daemon.engine().daemon().owner().snapshot();
            let capture = backend_engine::builtin::semantic_capture_relation(&snapshot)
                .expect("capture relation")
                .expect("committed source capture")
                .lookup(&key)
                .expect("read physical capture")
                .expect("retained physical capture");
            assert!(matches!(
                capture.outcome(),
                backend_engine::builtin::ProductSemanticCaptureOutcome::Unavailable { .. }
            ));
        }
    }

    #[test]
    fn saturated_mutation_queue_refuses_before_admission() {
        let mut fixture = AdapterFixture::new();
        let package = fixture.package;
        let label = fixture.label.clone();
        let (adapter, daemon) = fixture.parts();
        let cancelled = install_transition_job(adapter);
        let before = owner_cursor(daemon);

        for offset in 0..MAX_WAITING_COMMANDS {
            let request_id = 600 + u64::try_from(offset).expect("bounded request index");
            assert!(matches!(
                adapter.execute_or_defer(
                    daemon,
                    &remove_body(request_id, package, &label),
                    10_000 + request_id,
                ),
                Ok(Executed::Deferred)
            ));
        }
        assert_eq!(adapter.waiting.len(), MAX_WAITING_COMMANDS);
        let refused =
            match adapter.execute_or_defer(daemon, &remove_body(700, package, &label), 20_000) {
                Err(error) => error,
                Ok(_) => panic!("the full mutation queue must refuse the next command"),
            };
        assert!(refused.0.contains("mutation queue is full"));
        assert_eq!(adapter.waiting.len(), MAX_WAITING_COMMANDS);
        assert_eq!(owner_cursor(daemon), before, "refusal precedes admission");
        assert!(project_is_admitted(daemon, package));
        assert!(!cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn admitted_local_project_label_resolves_its_canonical_source_root() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        fs::create_dir(&project).expect("project dir");
        let label = label(&project.canonicalize().expect("canonical project"));
        let package = backend_engine::PackageKey::from_value(&label);

        assert_eq!(
            admitted_project_source_root(package, &label, &tree.0),
            Some(project.canonicalize().expect("canonical project"))
        );
        assert!(
            admitted_project_source_root(
                backend_engine::PackageKey::from_value("/outside/project"),
                &label,
                &tree.0,
            )
            .is_none()
        );
    }
}
