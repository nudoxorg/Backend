//! The compiled locald local-first composition.
//!
//! The profile owns a real `WorkspaceOwner`: it opens the configured
//! directory, acquires the durable lease, validates the checked genesis
//! closure, and uses the normal journal recovery path. It also installs a
//! configured scheduler, output/semantic validators, signed authority, and an
//! optional bounded worker transport. Without a worker endpoint, remote work
//! follows the same owner admission path and activates the checked local
//! fallback.

use crate::process::{AUTHORITY_SECRET_ENV, ProcessConfig, ProcessError};
use crate::protocol::EngineStatus;
use crate::reconcile::validate_canonical_output;
use crate::service::{NoCompletionAdmission, ReplicationAdmission, daemon_replicate};
use crate::worker_transport::AsyncWorkerTransport;
use backend_engine::{
    AuthorityClaim, AuthorityEpoch, AuthorityScopeClaim, AuthorityVersionSchema,
    Blake3AuthorityVerifier, Budget, ClosureManifest, ClosureRootOffer, Command, CommandReply,
    Commit, CommitProvenance, CompleteSemanticCoverage, CompositeAdmissionValidator, CostSnapshot,
    CoverageWitness, DaemonConfig, DependencyManifest, DispatchAttemptKey, DispatchError,
    DispatchPlan, DispatchRecoveryAction, Dispatcher, ExpectedInput, Fragment, Frame, Frontier,
    LocalCapability, LocalState, ObjectClosure, ObjectKey, ObjectVersion, OutputVersion,
    PendingRemoteKey, PlacementClass, PreparedTransition, ProducerObservationClaims,
    ProducerObservationVerifier, ProductSourceRecord, RebuildScope, RefreshChoice, RefreshCost,
    RelationAdmissionRegistry, RelationState, RelationTransition, RemoteAuthorityPolicy,
    RemoteCapability, RemoteDispatchContract, RemoteState, ResourceEnvelope, ResourceVector,
    RevocationVersion, Row, ScheduleRequest, Scheduler, Schema, SemanticCoverageAdmissionError,
    SemanticCoverageBinding, TransactionId, TransactionSchema, TransportLimits, TransportMessage,
    TypedObject, UntrustedProducerObservation, UntrustedSemanticCoverageClaim,
    UntrustedWorkspaceManifest, VersionedWorkIdentity, ViewDelta, ViewRoot, WireAuthorityPolicy,
    WireCertificate, WireClaim, WireIdentity, WireRecipeRequest, WorkspaceClosure, WorkspaceDelta,
    WorkspaceHead, WorkspaceManifest, WorkspaceModel, WorkspaceRoot, WorkspaceSnapshot,
    WorkspaceViewProducerAdmission, admit_complete_scope, admit_producer_observation,
};
use std::collections::BTreeMap;
use std::fmt;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const AUTHORITY_VALUE: &[u8] = backend_engine::PRODUCT_AUTHORITY_BYTES;
const VIEW_SOURCE_VALUE: &[u8] = b"product-source-relation-v2";
const MAX_REBUILD_PACKAGES: usize = 1_000_000;
pub(super) const MAX_REBUILD_BYTES: usize = 64 * 1024 * 1024;
// One compact event is fsynced before publication. Keep short edit suffixes
// incremental, while cold loads and generated files use one paged snapshot
// instead of turning a client request into an unbounded fsync loop.
/// Deterministic key retained only for the explicitly named compatibility
/// profile. Product verification material is loaded from the host.
const ECHO_AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];

#[path = "builtin/ingest.rs"]
mod ingest;
#[path = "builtin/profile.rs"]
mod profile;
use profile::{
    BuiltinAuthorityVerifier, BuiltinProfile, BuiltinSemanticChange, BuiltinSemanticRelation,
    BuiltinSourceChange, BuiltinValidator, BuiltinWorkspaceRelation, ProfileDescriptor, ProfileIds,
    builtin_dispatcher, execution_manifest, execution_resources, product_dependency_manifest,
    profile_descriptor,
};
pub use profile::{BuiltinIntent, BuiltinModel, BuiltinModelError};

#[path = "builtin/replication.rs"]
mod replication;
use replication::BuiltinReplication;
#[path = "builtin/worker.rs"]
mod worker;
use worker::connect_worker;
#[path = "builtin/view_build.rs"]
mod view_build;
#[path = "builtin/view_journal.rs"]
mod view_journal;
use view_build::rows_for_indexed_sources;
use view_journal::ViewJournal;
#[path = "builtin/commands.rs"]
mod commands;
#[path = "builtin/registry.rs"]
mod registry;
use registry::RegistryGateway;
#[path = "builtin/product_state.rs"]
mod product_state;
use product_state::ProductState;
#[path = "builtin/coverage.rs"]
mod coverage;
use coverage::{SemanticDeployment, reconcile_semantic_lane, view_coverage};

#[path = "query/mod.rs"]
pub mod query;

/// Process-private bootstrap verifier for the compiled local source owner.
/// This type never crosses the app crate boundary; remote workers must use an
/// authenticated `AdmittedAuthority` capability instead.
struct CompiledSourceVerifier {
    authority: ObjectVersion<AuthorityVersionSchema>,
}

impl CompiledSourceVerifier {
    fn observation(&self) -> UntrustedProducerObservation {
        let scope = AuthorityScopeClaim::from_object_version(self.authority).scope_root();
        let mut context = Vec::new();
        context.extend_from_slice(b"backend.locald.compiled-source.context.v1\0");
        context.extend_from_slice(&self.authority.to_bytes());
        let context = *blake3::hash(&context).as_bytes();
        let mut identity = Vec::new();
        identity.extend_from_slice(b"backend.locald.compiled-source.identity.v1\0");
        identity.extend_from_slice(&self.authority.to_bytes());
        identity.extend_from_slice(scope.as_bytes());
        let identity = *blake3::hash(&identity).as_bytes();
        let mut evidence = Vec::new();
        evidence.extend_from_slice(b"backend.locald.compiled-source.evidence.v1\0");
        evidence.extend_from_slice(&self.authority.to_bytes());
        evidence.extend_from_slice(scope.as_bytes());
        evidence.extend_from_slice(&context);
        UntrustedProducerObservation::new(identity, scope, context, evidence)
    }
}

impl ProducerObservationVerifier for CompiledSourceVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation == &self.observation())
            .then(|| {
                ProducerObservationClaims::new(
                    observation.producer_identity(),
                    observation.scope_root(),
                    observation.context(),
                    *blake3::hash(observation.evidence()).as_bytes(),
                )
            })
            .ok_or("compiled source observation mismatch")
    }
}

pub(crate) fn admitted_coverage() -> Result<CoverageWitness, BuiltinModelError> {
    let authority = ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = CompiledSourceVerifier { authority };
    let observation = admit_producer_observation(verifier.observation(), &verifier)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    admit_complete_scope(declaration, observation)
        .map(CoverageWitness::Complete)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

/// Compiler-owner activation that has also been rebound to its persisted
/// product key. The private wrapper prevents graph, diff, and view projection
/// from receiving an image whose manifest is valid under another package or
/// language scope.
pub(super) struct ActivatedProductSemantics {
    publication: compiler_application::ActivatedSemanticPackage,
}

impl ActivatedProductSemantics {
    pub(super) fn images(&self) -> &[interface_core::SemanticImageSnapshot] {
        &self.publication.images
    }
}

pub(super) fn activate_semantic_publication(
    compiler: &compiler_application::LocalCompilerClient,
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    claim: backend_engine::builtin::SemanticPublicationClaim,
) -> Result<ActivatedProductSemantics, BuiltinModelError> {
    let publication = compiler
        .activate_semantic_generation(key.profile(), claim.manifest(), claim.binding())
        .map_err(|error| BuiltinModelError(format!("activate semantic publication: {error}")))?;
    for image in &publication.images {
        let view = compiler_ir::SemanticImageView::reopen(image.as_ref()).map_err(|error| {
            BuiltinModelError(format!("reopen activated semantic publication: {error}"))
        })?;
        key.admit_image(&view).map_err(|error| {
            BuiltinModelError(format!("bind semantic publication to product key: {error}"))
        })?;
    }
    Ok(ActivatedProductSemantics { publication })
}

fn workspace_relation(
    intent: Option<&BuiltinIntent>,
) -> Result<RelationState<BuiltinWorkspaceRelation>, BuiltinModelError> {
    let mut entries = Vec::new();
    if let Some(intent) = intent {
        entries.push((intent.package.to_bytes(), intent.record()?));
    }
    RelationState::from_entries(entries, admitted_coverage()?)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

fn semantic_relation() -> Result<RelationState<BuiltinSemanticRelation>, BuiltinModelError> {
    RelationState::from_entries(Vec::new(), admitted_coverage()?)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

fn workspace_manifest(
    relation: &RelationState<BuiltinWorkspaceRelation>,
    semantic: &RelationState<BuiltinSemanticRelation>,
) -> Result<WorkspaceManifest, BuiltinModelError> {
    WorkspaceManifest::from_versions(
        1,
        vec![
            backend_engine::RelationBinding::from_state(relation),
            backend_engine::RelationBinding::from_state(semantic),
        ],
        Vec::new(),
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
        admitted_coverage()?,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))
}

fn workspace_manifest_from_root(
    root: &backend_engine::PersistedTreeRoot<BuiltinWorkspaceRelation>,
    semantic: &backend_engine::PersistedTreeRoot<BuiltinSemanticRelation>,
) -> Result<WorkspaceManifest, BuiltinModelError> {
    WorkspaceManifest::from_versions(
        1,
        vec![
            backend_engine::RelationBinding::from_persisted_root(root, admitted_coverage()?),
            backend_engine::RelationBinding::from_persisted_root(semantic, admitted_coverage()?),
        ],
        Vec::new(),
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
        admitted_coverage()?,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))
}

fn genesis_closure(
    manifest: &WorkspaceManifest,
    relation: &RelationState<BuiltinWorkspaceRelation>,
    semantic: &RelationState<BuiltinSemanticRelation>,
    transaction: TransactionId,
    commit: &Commit,
    transition: &WorkspaceDelta,
) -> Result<WorkspaceClosure, BuiltinModelError> {
    let authority_key = ObjectKey::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    let relation_object = TypedObject::from_relation_state(relation)
        .map_err(|error| BuiltinModelError(format!("materialize relation closure: {error:?}")))?;
    let mut objects = vec![
        relation_object,
        TypedObject::from_relation_state(semantic).map_err(|error| {
            BuiltinModelError(format!("materialize semantic relation closure: {error:?}"))
        })?,
        TypedObject::from_value(&authority_key, AUTHORITY_VALUE),
    ];
    let transaction_bytes = transaction.as_bytes();
    let transaction_key = ObjectKey::<TransactionSchema>::from_value(&transaction_bytes[..]);
    objects.push(TypedObject::from_value(
        &transaction_key,
        &transaction_bytes[..],
    ));
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let objects =
        ClosureManifest::new(objects).map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    if commit.transaction() != ObjectClosure::from_version(transaction.version()) {
        return Err(BuiltinModelError(
            "request-bound commit transaction does not match the transition".to_owned(),
        ));
    }
    // These conversions bind closure admission to the exact checked delta and
    // request provenance that the owner will publish.
    let checked_transition = transition.checked();
    let checked_commit = commit.clone().into_checked();
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("register semantic relation: {error:?}")))?;
    WorkspaceClosure::from_checked_transition_with_registry(
        manifest,
        &checked_transition,
        Some(&checked_commit),
        objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))
}

pub(super) struct LazyClosureUpdate<'a> {
    pub(super) base_source: TypedObject,
    pub(super) base_semantic: TypedObject,
    pub(super) changed_sources: &'a [backend_engine::CanonicalNode<BuiltinWorkspaceRelation>],
    pub(super) source_root: backend_engine::StateRoot<BuiltinWorkspaceRelation>,
    pub(super) changed_semantics: &'a [backend_engine::CanonicalNode<BuiltinSemanticRelation>],
    pub(super) semantic_root: backend_engine::StateRoot<BuiltinSemanticRelation>,
    pub(super) transaction: TransactionId,
    pub(super) intent: &'a BuiltinIntent,
}

fn transition_closure_lazy(
    base: &WorkspaceClosure,
    manifest: &WorkspaceManifest,
    update: LazyClosureUpdate<'_>,
) -> Result<WorkspaceClosure, BuiltinModelError> {
    let authority_key = ObjectKey::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    let mut objects = vec![TypedObject::from_value(&authority_key, AUTHORITY_VALUE)];
    objects.push(update.base_source);
    objects.push(update.base_semantic);
    let intent_bytes = update.intent.encode();
    let intent_key = ObjectKey::<profile::BuiltinIntentSchema>::from_value(&intent_bytes);
    objects.push(TypedObject::from_value(&intent_key, &intent_bytes));
    let transaction_bytes = update.transaction.as_bytes();
    let transaction_key = ObjectKey::<TransactionSchema>::from_value(&transaction_bytes[..]);
    objects.push(TypedObject::from_value(
        &transaction_key,
        &transaction_bytes[..],
    ));
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("register semantic relation: {error:?}")))?;
    if update.changed_sources.is_empty() {
        WorkspaceClosure::extend_checked_nodes_with_registry(
            base,
            manifest,
            update.semantic_root,
            update.changed_semantics,
            objects,
            &registry,
        )
    } else {
        for node in update.changed_semantics {
            objects.push(
                TypedObject::from_state_root(node.commitment(), node).map_err(|error| {
                    BuiltinModelError(format!("retain changed semantic node: {error:?}"))
                })?,
            );
        }
        WorkspaceClosure::extend_checked_nodes_with_registry(
            base,
            manifest,
            update.source_root,
            update.changed_sources,
            objects,
            &registry,
        )
    }
    .map_err(|error| BuiltinModelError(format!("extend lazy transition closure: {error:?}")))
}

fn admit_manifest(
    untrusted: UntrustedWorkspaceManifest,
    manifest: &WorkspaceManifest,
) -> Result<WorkspaceManifest, BuiltinModelError> {
    untrusted
        .admit_checked(
            manifest.relations().to_vec(),
            Vec::new(),
            ObjectClosure::from_version(ObjectVersion::<AuthorityVersionSchema>::from_value(
                AUTHORITY_VALUE,
            )),
            admitted_coverage()?,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))
}

pub(crate) fn genesis() -> Result<WorkspaceHead, BuiltinModelError> {
    let relation = workspace_relation(None)?;
    let semantic = semantic_relation()?;
    let manifest = workspace_manifest(&relation, &semantic)?;
    // `WorkspaceHead::genesis` derives this same deterministic transaction and
    // commit internally. Build the closure from the matching checked commit so
    // the head's initial publication contains every provenance object it
    // references (including the genesis transaction).
    let transaction = TransactionId::derive(0, manifest.root(), [0_u8; 32], 0);
    let authority = manifest
        .authority_closure()
        .ok_or_else(|| BuiltinModelError("builtin manifest has no authority closure".to_owned()))?;
    let provenance = CommitProvenance::new(
        authority,
        ObjectClosure::from_version(transaction.version()),
        b"backend.engine.genesis.v1".to_vec(),
    );
    let commit = backend_engine::commit_checked(&manifest, Vec::new(), provenance)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let empty_delta = WorkspaceDelta::new(&manifest, &manifest, Vec::new())
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let closure = genesis_closure(
        &manifest,
        &relation,
        &semantic,
        transaction,
        &commit,
        &empty_delta,
    )?;
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("register semantic relation: {error:?}")))?;
    WorkspaceHead::genesis_with_registry(manifest, closure, &registry)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

/// Source binding used by the compiled view producer. The projection is a
/// versioned product view rooted in the durable workspace and carries an
/// explicit coverage capability for every published snapshot.
#[derive(Clone, Copy, Debug)]
struct BuiltinViewAdmission {
    workspace_root: WorkspaceRoot,
    source_root: backend_engine::ViewStateRoot,
}

impl backend_engine::ViewBindingAdmission for BuiltinViewAdmission {
    fn admit(&self, workspace: &WorkspaceSnapshot, view: &ViewRoot) -> Result<(), String> {
        let producer = WorkspaceViewProducerAdmission::from_snapshot(
            workspace,
            backend_engine::object_version(VIEW_SOURCE_VALUE),
        );
        let expected_observation = producer.admit().map_err(|error| error.clone())?;
        if workspace.root() != self.workspace_root
            || view.basis().root != self.source_root
            || view.basis().object != backend_engine::object_version(VIEW_SOURCE_VALUE)
            || !view.is_coherent()
            || view.capability().is_none()
            || view.capability().is_some_and(|capability| {
                capability.producer_identity() != expected_observation.producer_identity()
                    || capability.context() != expected_observation.context()
                    || capability.evidence_digest() != expected_observation.evidence_digest()
            })
        {
            return Err("builtin view is not bound to its checked source scope".to_owned());
        }
        Ok(())
    }
}

fn builtin_view_capability_for_workspace(
    workspace: &WorkspaceSnapshot,
) -> Result<backend_engine::CoverageCapability, BuiltinModelError> {
    let object = backend_engine::object_version(VIEW_SOURCE_VALUE);
    let declared = AuthorityScopeClaim::from_object_version(object);
    let producer = WorkspaceViewProducerAdmission::from_snapshot(workspace, object);
    let evidence = producer.observation().evidence().to_vec();
    let observation = producer.admit().map_err(BuiltinModelError)?;
    let coverage = admit_complete_scope(declared, observation)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    backend_engine::CoverageCapability::from_authorized_with_evidence(coverage, evidence)
        .map_err(BuiltinModelError)
}

fn initial_view_for_workspace(
    workspace: &WorkspaceSnapshot,
) -> Result<(ViewRoot, backend_engine::Cursor), BuiltinModelError> {
    let source_root = backend_engine::view_state_root(&[]);
    let source_object = backend_engine::object_version(VIEW_SOURCE_VALUE);
    let basis = backend_engine::Basis::new(source_root, source_object);
    let view = ViewRoot::new_checked(
        backend_engine::view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        Vec::new(),
        vec![backend_engine::ViewCoverage::Complete],
        builtin_view_capability_for_workspace(workspace)?,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    let cursor = backend_engine::Cursor::for_view_root(&view);
    Ok((view, cursor))
}

#[cfg(test)]
pub(crate) fn initial_view() -> Result<(ViewRoot, backend_engine::Cursor), BuiltinModelError> {
    let head = genesis()?;
    let snapshot = head.snapshot();
    initial_view_for_workspace(&snapshot)
}

#[cfg(test)]
pub(crate) fn test_builtin_view_capability()
-> Result<backend_engine::CoverageCapability, BuiltinModelError> {
    let head = genesis()?;
    let snapshot = head.snapshot();
    builtin_view_capability_for_workspace(&snapshot)
}

struct IndexedProject {
    package: backend_engine::PackageKey,
    label: String,
    files: Arc<[[u8; 32]]>,
}

struct IndexedSources {
    projects: BTreeMap<[u8; 32], IndexedProject>,
    files: Vec<([u8; 32], ProductSourceRecord)>,
}

fn read_indexed_sources(snapshot: &WorkspaceSnapshot) -> Result<IndexedSources, BuiltinModelError> {
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open indexed source relation: {error}")))?;
    let root = relation
        .root_node()
        .map_err(|error| BuiltinModelError(format!("open indexed source root: {error}")))?;
    let row_count = usize::try_from(root.row_count())
        .map_err(|_| BuiltinModelError("workspace package count overflows usize".to_owned()))?;
    if row_count > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package relation exceeds rebuild row bound".to_owned(),
        ));
    }
    let mut projects = BTreeMap::new();
    let mut files = Vec::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("read indexed source page: {error}")))?;
        for (key, record) in page.entries() {
            match record {
                ProductSourceRecord::Project { label, files, .. } => {
                    let package = backend_engine::PackageKey::from_value(label.as_str());
                    if package.to_bytes() != *key
                        || projects
                            .insert(
                                *key,
                                IndexedProject {
                                    package,
                                    label: label.clone(),
                                    files: Arc::clone(files),
                                },
                            )
                            .is_some()
                    {
                        return Err(BuiltinModelError(
                            "project record does not match its canonical coordinate".to_owned(),
                        ));
                    }
                }
                ProductSourceRecord::File { .. } => files.push((*key, record.clone())),
            }
        }
        let Some(next) = page.next().copied() else {
            break;
        };
        after = Some(next);
    }
    Ok(IndexedSources { projects, files })
}

fn admitted_view_bytes(rows: &[Row]) -> Result<usize, BuiltinModelError> {
    let bytes = rows
        .iter()
        .try_fold(0usize, |total, row| {
            let document = row.document.iter().try_fold(0usize, |bytes, fragment| {
                bytes.checked_add(match fragment {
                    Fragment::Text(value) | Fragment::Code(value) => value.len(),
                    Fragment::Link { label, .. } => label.len(),
                    Fragment::Break => 1,
                })
            })?;
            total
                .checked_add(row.label.len())?
                .checked_add(row.signature.as_deref().map_or(0, str::len))?
                .checked_add(document)
        })
        .ok_or_else(|| BuiltinModelError("workspace view bytes overflow".to_owned()))?;
    if bytes > MAX_REBUILD_BYTES {
        return Err(BuiltinModelError(
            "workspace view exceeds rebuild byte bound".to_owned(),
        ));
    }
    Ok(bytes)
}

fn view_for_workspace(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &compiler_application::LocalCompilerClient,
    deployment: SemanticDeployment,
) -> Result<ViewRoot, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_indexed_sources(&snapshot)?;
    let (initial, _) = initial_view_for_workspace(&snapshot)?;
    let projected = rows_for_indexed_sources(&initial, sources, &snapshot, compiler)?;
    let coverage = view_coverage(&snapshot, &projected.activated, deployment)?;
    let _admitted_bytes = admitted_view_bytes(&projected.rows)?;
    ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        projected.rows,
        coverage,
        builtin_view_capability_for_workspace(&snapshot)?,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))
}

fn publish_builtin_view(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &compiler_application::LocalCompilerClient,
    deployment: SemanticDeployment,
) -> Result<Vec<backend_engine::CommittedViewDelta>, BuiltinModelError> {
    let mut current = daemon.engine().daemon().library().view().clone();
    let target = view_for_workspace(daemon, compiler, deployment)?;
    if current.basis() == target.basis()
        && current.coverage() == target.coverage()
        && current.rows() == target.rows()
    {
        return Ok(Vec::new());
    }
    let changes = changed_rows(&current, &target);
    let deltas = if current.row_count() == 0
        || changes.is_empty()
        || changes.len() > backend_engine::MAX_VIEW_PATCH_ROWS
    {
        vec![ViewDelta::Reset {
            root: Box::new(target),
        }]
    } else {
        vec![ViewDelta::Patch {
            changes: changes.into(),
        }]
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let workspace_root = snapshot.root();
    let capability = builtin_view_capability_for_workspace(&snapshot)?;
    let mut committed_deltas = Vec::with_capacity(deltas.len());
    for delta in deltas {
        let prepared = current
            .prepare(delta, capability.clone())
            .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
        let (view, committed) = current
            .commit(prepared)
            .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
        let cursor = backend_engine::Cursor::for_view_root(&view);
        let admission = BuiltinViewAdmission {
            workspace_root,
            source_root: view.basis().root,
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .publish_view(
                view.clone(),
                cursor,
                &admission,
                Some(backend_engine::CursorEvent::View {
                    delta: Box::new(committed.clone()),
                }),
            )
            .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
        current = view;
        committed_deltas.push(committed);
    }
    Ok(committed_deltas)
}

fn changed_rows(base: &ViewRoot, target: &ViewRoot) -> Vec<backend_engine::RowChange> {
    use std::cmp::Ordering;

    let mut base_rows = base.rows().iter().peekable();
    let mut target_rows = target.rows().iter().peekable();
    let mut changes = Vec::new();
    loop {
        match (base_rows.peek(), target_rows.peek()) {
            (Some(before), Some(after)) => match before.id.cmp(&after.id) {
                Ordering::Less => {
                    changes.push(backend_engine::RowChange::Remove(before.id));
                    let _ = base_rows.next();
                }
                Ordering::Greater => {
                    changes.push(backend_engine::RowChange::Upsert(Box::new(
                        (*after).clone(),
                    )));
                    let _ = target_rows.next();
                }
                Ordering::Equal => {
                    if before != after {
                        changes.push(backend_engine::RowChange::Upsert(Box::new(
                            (*after).clone(),
                        )));
                    }
                    let _ = base_rows.next();
                    let _ = target_rows.next();
                }
            },
            (Some(before), None) => {
                changes.push(backend_engine::RowChange::Remove(before.id));
                let _ = base_rows.next();
            }
            (None, Some(after)) => {
                changes.push(backend_engine::RowChange::Upsert(Box::new(
                    (*after).clone(),
                )));
                let _ = target_rows.next();
            }
            (None, None) => break,
        }
    }
    changes
}

#[expect(
    clippy::too_many_lines,
    reason = "startup must complete all durable admission before binding"
)]
pub(crate) fn compose_owner(
    config: &ProcessConfig,
) -> Result<impl crate::service::OwnerService + 'static, ProcessError> {
    let limits = config.listener.limits.transport;
    let profile = match config.profile.as_str() {
        "builtin" => BuiltinProfile::Product,
        "builtin-echo" => BuiltinProfile::EchoFixture,
        other => {
            return Err(ProcessError::Profile(format!(
                "unknown compiled profile {other}"
            )));
        }
    };
    let product_secret = if profile == BuiltinProfile::EchoFixture {
        None
    } else {
        let path = config.authority_secret.as_deref().ok_or_else(|| {
            ProcessError::Profile(
                "product profile requires --authority-secret-file (or ".to_owned()
                    + AUTHORITY_SECRET_ENV
                    + ")",
            )
        })?;
        Some(
            backend_engine::read_authority_secret(path).map_err(|error| {
                ProcessError::Profile(format!(
                    "load authority credential {}: {error}",
                    path.display()
                ))
            })?,
        )
    };
    let profile = profile_descriptor(profile).map_err(ProcessError::Profile)?;
    // The attempt lease spans one closure exchange, one execution/fallback
    // exchange, and the worker's declared wall budget. All values use the
    // same Unix-millisecond owner clock as dispatch completion.
    let io_ticks = u64::try_from(config.listener.io_timeout.as_millis()).unwrap_or(u64::MAX);
    let attempt_lease_ticks = io_ticks
        .saturating_mul(2)
        .saturating_add(execution_resources(profile.ids).wall_millis)
        .max(1);
    let dispatcher = builtin_dispatcher(product_secret, Arc::clone(&profile), attempt_lease_ticks)
        .map_err(ProcessError::Profile)?;
    let relation_registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| ProcessError::Profile(format!("register builtin relation: {error:?}")))?
        .with_relation::<BuiltinSemanticRelation>()
        .map_err(|error| ProcessError::Profile(format!("register semantic relation: {error:?}")))?;
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
        &config.workspace,
        BuiltinModel,
        genesis().map_err(|error| ProcessError::Profile(error.to_string()))?,
        dispatcher,
        DaemonConfig::default(),
        relation_registry,
    )
    .map_err(|error| ProcessError::Profile(error.to_string()))?;
    let compiler =
        compiler_application::LocalCompilerHost::production_at(config.workspace.join("compiler"))
            .open()
            .map_err(|error| ProcessError::Profile(format!("open compiler owner: {error}")))?;
    // Admit optional semantic configuration without network I/O. Missing or
    // malformed remote settings remain a retained unavailable state and can
    // never delay the local owner or its lexical query path. The classification
    // is read before the first view publication because the published view
    // root, not the reply, is where an unconfigured lane must be recorded.
    let remote_semantic = query::RemoteSemantic::from_environment();
    let semantic_deployment = SemanticDeployment::from_remote(&remote_semantic);
    let view_path = config.workspace.join("view.journal");
    let view_journal = ViewJournal::open(&view_path).map_err(ProcessError::Profile)?;
    let workspace_root = daemon.engine().daemon().owner().head().root();
    let workspace_snapshot = daemon.engine().daemon().owner().snapshot();
    let view_capability = builtin_view_capability_for_workspace(&workspace_snapshot)
        .map_err(|error| ProcessError::Profile(error.to_string()))?;
    let recovered_view = view_journal
        .load_for_workspace(workspace_root, &view_capability)
        .map_err(ProcessError::Profile)?;
    daemon
        .engine_mut()
        .daemon_mut()
        .set_view_persistence(Box::new(view_journal));
    if let Some(recovered) = recovered_view {
        let admission = BuiltinViewAdmission {
            workspace_root,
            source_root: recovered.view.basis().root,
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .restore_view(
                recovered.view,
                recovered.cursor,
                &admission,
                recovered.events,
                recovered.base_sequence,
            )
            .map_err(|error| ProcessError::Profile(error.to_string()))?;
    } else {
        let view = view_for_workspace(&daemon, &compiler, semantic_deployment)
            .map_err(|error| ProcessError::Profile(error.to_string()))?;
        let cursor = backend_engine::Cursor::for_view_root(&view);
        let admission = BuiltinViewAdmission {
            workspace_root,
            source_root: view.basis().root,
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .publish_view(view, cursor, &admission, None)
            .map_err(|error| ProcessError::Profile(error.to_string()))?;
    }
    // The workspace journal is authoritative. A crash can occur after a
    // workspace commit and between several bounded view-row publications;
    // repair that derived suffix before the listener becomes visible.
    let _recovered_view_deltas = publish_builtin_view(&mut daemon, &compiler, semantic_deployment)
        .map_err(|error| ProcessError::Profile(format!("repair product view: {error}")))?;
    let projection_path = config.workspace.join(backend_extension_turso::FILE_NAME);
    let mut sql_projection = futures_executor::block_on(
        backend_extension_turso::TursoProjection::open(&projection_path),
    )
    .map_err(|error| {
        ProcessError::Profile(format!(
            "open Turso projection {}: {error}",
            projection_path.display()
        ))
    })?;
    futures_executor::block_on(
        sql_projection.synchronize(daemon.engine().daemon().library().view()),
    )
    .map_err(|error| ProcessError::Profile(format!("align Turso projection: {error}")))?;
    let search_snapshots = query::SearchSnapshotOwner::default();
    let worker_secret = match profile.kind {
        BuiltinProfile::Product => product_secret.ok_or_else(|| {
            ProcessError::Profile("product authority credential disappeared".to_owned())
        })?,
        BuiltinProfile::EchoFixture => ECHO_AUTHORITY_SECRET,
    };
    let mut replication = BuiltinReplication::new(
        limits,
        Arc::clone(&profile),
        config.worker_endpoint.clone(),
        worker_secret,
        config.listener.io_timeout,
    )
    .map_err(ProcessError::Profile)?;
    let recovered = daemon.engine_mut().daemon_mut().take_recovered_dispatch();
    replication.install_recovered(recovered);
    if config.worker_endpoint.is_some() {
        // Reconnect/authentication/capability negotiation happens on the
        // bounded connector thread. The owner can bind its client socket
        // immediately and remains available for local fallback while the
        // worker is offline.
        replication.start_reconnect();
    }
    let registry = RegistryGateway::open(&config.registry, config.workspace.join("registry"))
        .map_err(|error| ProcessError::Profile(format!("open registry owner: {error}")))?;
    let product_state = ProductState::open(config.workspace.join("product-state.json"))
        .map_err(|error| ProcessError::Profile(format!("open product state: {error}")))?;
    let mut commands = commands::CommandAdapter::new(
        sql_projection,
        registry,
        product_state,
        compiler,
        search_snapshots,
        remote_semantic,
    );
    let command = move |daemon: &mut crate::Locald<
        BuiltinModel,
        BuiltinValidator,
        BuiltinAuthorityVerifier,
    >,
                        body: &[u8]| { commands.execute(daemon, body) };
    Ok(daemon.into_owner_with_admission(command, NoCompletionAdmission, replication))
}

/// Starts the compiled locald profile. It does all startup work before the
/// listener is bound, so an invalid durable directory cannot look ready.
#[must_use]
pub fn run(config: ProcessConfig) -> ExitCode {
    match compose_owner(&config) {
        Ok(owner) => crate::process::run_process(owner, config),
        Err(error) => {
            eprintln!("backend-locald: {error}");
            ExitCode::from(70)
        }
    }
}

#[path = "builtin/projection.rs"]
mod projection;

pub(crate) use projection::{certificate_for_compact_event, certificate_for_snapshot_page};

#[cfg(test)]
#[allow(clippy::expect_used)]
mod authority_tests {
    use super::*;

    #[test]
    fn product_genesis_and_dispatch_share_one_authority_identity() {
        let head = genesis().expect("checked product genesis");
        let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
        assert_eq!(
            head.manifest().authority_closure(),
            Some(ObjectClosure::from_version(profile.ids.authority))
        );
    }
}
