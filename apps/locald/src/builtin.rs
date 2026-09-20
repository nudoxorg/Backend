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
    CoverageWitness, DaemonConfig, DeltaPlanner, DependencyManifest, DispatchAttemptKey,
    DispatchError, DispatchPlan, DispatchRecoveryAction, Dispatcher, ExpectedInput, Frame,
    Frontier, LocalCapability, LocalState, ObjectClosure, ObjectKey, ObjectVersion, OutputVersion,
    PendingRemoteKey, PlacementClass, PreparedTransition, ProductInput, ProductSourceDeltaFacts,
    ProductSourceSnapshot, RebuildScope, RefreshChoice, RefreshCost, RelationAdmissionRegistry,
    RelationState, RelationTransition, RemoteAuthorityPolicy, RemoteCapability,
    RemoteDispatchContract, RemoteState, ResourceEnvelope, ResourceVector, RevocationVersion, Row,
    RowId, ScheduleRequest, Scheduler, Schema, SemanticCoverageAdmissionError,
    SemanticCoverageBinding, TransactionId, TransactionSchema, TransportLimits, TransportMessage,
    TypedObject, UntrustedSemanticCoverageClaim, UntrustedWorkspaceDelta,
    UntrustedWorkspaceManifest, VersionedWorkIdentity, ViewDelta, ViewRoot, WireAuthorityPolicy,
    WireCertificate, WireClaim, WireIdentity, WireRecipeRequest, WorkspaceClosure, WorkspaceDelta,
    WorkspaceHead, WorkspaceManifest, WorkspaceModel, WorkspaceRoot, WorkspaceSnapshot,
    WorkspaceViewProducerAdmission, admit_complete_scope,
};
use std::fmt;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const AUTHORITY_VALUE: &[u8] = backend_engine::PRODUCT_AUTHORITY_BYTES;
const MAX_REBUILD_PACKAGES: usize = 1_000_000;
const MAX_REBUILD_BYTES: usize = 64 * 1024 * 1024;
/// Deterministic key retained only for the explicitly named compatibility
/// profile. Product verification material is loaded from the host.
const ECHO_AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];

#[path = "builtin/profile.rs"]
mod profile;
use profile::{
    BuiltinAuthorityVerifier, BuiltinPackageRecord, BuiltinProfile, BuiltinValidator,
    BuiltinWorkspaceRelation, ProductRelation, ProfileDescriptor, ProfileIds, builtin_dispatcher,
    execution_input_basis, execution_manifest, execution_resources, output_bytes,
    product_dependency_manifest, product_input_bytes, product_input_version,
    product_source_fixture_with_authority, profile_descriptor,
};
pub use profile::{BuiltinIntent, BuiltinModel, BuiltinModelError};

#[path = "builtin/replication.rs"]
mod replication;
use replication::BuiltinReplication;
#[path = "builtin/worker.rs"]
mod worker;
use worker::connect_worker;
#[path = "builtin/view_journal.rs"]
mod view_journal;
use view_journal::ViewJournal;

fn admitted_coverage() -> Result<CoverageWitness, BuiltinModelError> {
    let authority = ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    backend_engine::builtin_coverage(authority).map_err(BuiltinModelError)
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

fn workspace_manifest(
    relation: &RelationState<BuiltinWorkspaceRelation>,
) -> Result<WorkspaceManifest, BuiltinModelError> {
    WorkspaceManifest::from_versions(
        1,
        vec![backend_engine::RelationBinding::from_state(relation)],
        Vec::new(),
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
        admitted_coverage()?,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))
}

fn workspace_manifest_from_root(
    root: &backend_engine::PersistedTreeRoot<BuiltinWorkspaceRelation>,
) -> Result<WorkspaceManifest, BuiltinModelError> {
    WorkspaceManifest::from_versions(
        1,
        vec![backend_engine::RelationBinding::from_persisted_root(
            root,
            admitted_coverage()?,
        )],
        Vec::new(),
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
        admitted_coverage()?,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))
}

fn transition_closure(
    manifest: &WorkspaceManifest,
    relation: &RelationState<BuiltinWorkspaceRelation>,
    base_relation: Option<&RelationState<BuiltinWorkspaceRelation>>,
    transaction: Option<TransactionId>,
    commit: Option<&Commit>,
    transition: Option<&WorkspaceDelta>,
    intent: Option<&BuiltinIntent>,
) -> Result<WorkspaceClosure, BuiltinModelError> {
    let authority_key = ObjectKey::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    let relation_object = TypedObject::from_relation_state(relation)
        .map_err(|error| BuiltinModelError(format!("materialize relation closure: {error:?}")))?;
    let mut objects = vec![
        relation_object,
        TypedObject::from_value(&authority_key, AUTHORITY_VALUE),
    ];
    if let Some(base_relation) = base_relation
        && base_relation.root() != relation.root()
    {
        objects.push(
            TypedObject::from_relation_state(base_relation).map_err(|error| {
                BuiltinModelError(format!("materialize base relation closure: {error:?}"))
            })?,
        );
    }
    if let Some(intent) = intent {
        let bytes = intent
            .encode()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let key = ObjectKey::<profile::BuiltinIntentSchema>::from_value(&bytes);
        objects.push(TypedObject::from_value(&key, &bytes));
    }
    if let Some(transaction) = transaction {
        let bytes = transaction.as_bytes();
        let key = ObjectKey::<TransactionSchema>::from_value(&bytes[..]);
        objects.push(TypedObject::from_value(&key, &bytes[..]));
    }
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let objects =
        ClosureManifest::new(objects).map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    if let Some(transaction) = transaction {
        let commit = commit.ok_or_else(|| {
            BuiltinModelError("checked transition is missing its request-bound commit".to_owned())
        })?;
        let transition = transition.ok_or_else(|| {
            BuiltinModelError("checked transition is missing its request-bound delta".to_owned())
        })?;
        if commit.transaction() != ObjectClosure::from_version(transaction.version()) {
            return Err(BuiltinModelError(
                "request-bound commit transaction does not match the transition".to_owned(),
            ));
        }
        // `into_checked` is only available on an already checked commit. This
        // therefore binds closure admission to the exact provenance detail
        // (including the request id) that will be published by the owner.
        let checked_transition = transition.checked();
        let checked_commit = commit.clone().into_checked();
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
        return WorkspaceClosure::from_checked_transition_with_registry(
            manifest,
            &checked_transition,
            Some(&checked_commit),
            objects,
            &registry,
        )
        .map_err(|error| BuiltinModelError(format!("{error:?}")));
    }
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
    WorkspaceClosure::from_checked_manifest_with_registry(manifest, objects, &registry)
        .map_err(|error| BuiltinModelError(format!("{error:?}")))
}

fn transition_closure_lazy(
    manifest: &WorkspaceManifest,
    base_relation_object: TypedObject,
    changed_nodes: &[backend_engine::CanonicalNode<BuiltinWorkspaceRelation>],
    transaction: TransactionId,
    commit: &Commit,
    transition: &WorkspaceDelta,
    intent: &BuiltinIntent,
) -> Result<WorkspaceClosure, BuiltinModelError> {
    let authority_key = ObjectKey::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE);
    let mut objects = vec![TypedObject::from_value(&authority_key, AUTHORITY_VALUE)];
    objects.push(base_relation_object);
    for node in changed_nodes {
        objects.push(
            TypedObject::from_state_root(node.commitment(), node).map_err(|error| {
                BuiltinModelError(format!("materialize changed relation node: {error:?}"))
            })?,
        );
    }
    let intent_bytes = intent.encode()?;
    let intent_key = ObjectKey::<profile::BuiltinIntentSchema>::from_value(&intent_bytes);
    objects.push(TypedObject::from_value(&intent_key, &intent_bytes));
    let transaction_bytes = transaction.as_bytes();
    let transaction_key = ObjectKey::<TransactionSchema>::from_value(&transaction_bytes[..]);
    objects.push(TypedObject::from_value(
        &transaction_key,
        &transaction_bytes[..],
    ));
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
    // `base_relation_object` came back through the durable object boundary.
    // Its bytes are still an untrusted wire object, so the generic object
    // version verifier cannot admit them as a relation. Reuse the workspace's
    // typed relation registry for the entire mixed closure.
    let objects = ClosureManifest::new_with_registry(objects, &registry)
        .map_err(|error| BuiltinModelError(format!("build lazy transition closure: {error:?}")))?;
    let checked_transition = transition.checked();
    let checked_commit = commit.clone().into_checked();
    WorkspaceClosure::from_checked_transition_root_only_with_registry(
        manifest,
        &checked_transition,
        Some(&checked_commit),
        objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("admit lazy transition closure: {error:?}")))
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

fn admit_delta(
    untrusted: UntrustedWorkspaceDelta,
    base: &WorkspaceManifest,
    target: &WorkspaceManifest,
    transition: RelationTransition,
) -> Result<WorkspaceDelta, BuiltinModelError> {
    untrusted
        .admit(base, target, vec![transition])
        .map_err(|error| BuiltinModelError(error.to_string()))
}

fn genesis() -> Result<WorkspaceHead, BuiltinModelError> {
    let relation = workspace_relation(None)?;
    let manifest = workspace_manifest(&relation)?;
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
    let closure = transition_closure(
        &manifest,
        &relation,
        None,
        Some(transaction),
        Some(&commit),
        Some(&empty_delta),
        None,
    )?;
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
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
            backend_engine::object_version(b"view-state-source"),
        );
        let expected_observation = producer.admit().map_err(|error| error.clone())?;
        if workspace.root() != self.workspace_root
            || view.basis().root != self.source_root
            || view.basis().object != backend_engine::object_version(b"view-state-source")
            || !view.is_coherent()
            || view.capability().is_none()
            || view.capability().is_some_and(|capability| {
                capability.producer_identity() != expected_observation.producer_identity()
                    || capability.context() != expected_observation.context()
                    || capability.evidence_digest() != expected_observation.evidence_digest()
            })
            || view
                .coverage()
                .iter()
                .any(|coverage| !coverage.is_complete())
        {
            return Err("builtin view is not bound to its checked source scope".to_owned());
        }
        Ok(())
    }
}

fn builtin_view_capability_for_workspace(
    workspace: &WorkspaceSnapshot,
) -> Result<backend_engine::CoverageCapability, BuiltinModelError> {
    let object = backend_engine::object_version(b"view-state-source");
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
    let source_object = backend_engine::object_version(b"view-state-source");
    let basis = backend_engine::Basis::new(source_root, source_object);
    let package = backend_engine::package_key("backend-builtin");
    let symbol = backend_engine::symbol_key("backend::Builtin");
    let rows = vec![
        Row::new(RowId::Package(package), basis, "backend-builtin"),
        Row::in_package(RowId::Symbol(symbol), basis, package, "backend::Builtin"),
    ];
    let view = ViewRoot::new_checked(
        backend_engine::view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        vec![backend_engine::ViewCoverage::Complete],
        builtin_view_capability_for_workspace(workspace)?,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    let cursor = backend_engine::Cursor::for_view_root(&view);
    Ok((view, cursor))
}

#[cfg(test)]
fn initial_view() -> Result<(ViewRoot, backend_engine::Cursor), BuiltinModelError> {
    let head = genesis()?;
    let snapshot = head.snapshot();
    initial_view_for_workspace(&snapshot)
}

#[cfg(test)]
fn test_builtin_view_capability() -> Result<backend_engine::CoverageCapability, BuiltinModelError> {
    let head = genesis()?;
    let snapshot = head.snapshot();
    builtin_view_capability_for_workspace(&snapshot)
}

fn workspace_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    daemon
        .engine()
        .daemon()
        .owner()
        .head()
        .closure()
        .manifest()
        .objects()
        .iter()
        .find_map(|object| BuiltinIntent::decode(object.bytes()).ok())
        .map_or(Ok(None), |intent| Ok(Some(intent)))
}

fn view_for_workspace(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
) -> Result<ViewRoot, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let (initial, _) = initial_view_for_workspace(&snapshot)?;
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let root = relation
        .root_node()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let row_count = usize::try_from(root.row_count())
        .map_err(|_| BuiltinModelError("workspace package count overflows usize".to_owned()))?;
    if row_count > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package relation exceeds rebuild row bound".to_owned(),
        ));
    }
    let capacity = row_count
        .checked_add(initial.rows().len())
        .ok_or_else(|| BuiltinModelError("workspace view row capacity overflow".to_owned()))?;
    let mut rows = Vec::with_capacity(capacity);
    rows.extend(initial.rows().iter().cloned());
    let builtin_package = backend_engine::PackageKey::from_value("backend-builtin");
    let capability = builtin_view_capability_for_workspace(&snapshot)?;
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        for (package_bytes, record) in page.entries() {
            let package = backend_engine::PackageKey::from_value(record.label());
            let label_bytes = record.label().len();
            if label_bytes > BuiltinPackageRecord::MAX_LABEL_BYTES
                || package.to_bytes() != *package_bytes
            {
                return Err(BuiltinModelError(
                    "workspace package record key does not match its canonical coordinate"
                        .to_owned(),
                ));
            }
            let row = Row::new(RowId::Package(package), initial.basis(), record.label());
            if package == builtin_package {
                if let Some(existing) = rows
                    .iter_mut()
                    .find(|existing| existing.id == RowId::Package(package))
                {
                    *existing = row;
                } else {
                    rows.push(row);
                }
            } else {
                rows.push(row);
            }
        }
        let Some(next) = page.next().copied() else {
            break;
        };
        after = Some(next);
    }
    let encoded_label_bytes = rows
        .iter()
        .try_fold(0usize, |total, row| total.checked_add(row.label.len()))
        .ok_or_else(|| BuiltinModelError("workspace view bytes overflow".to_owned()))?;
    if encoded_label_bytes > MAX_REBUILD_BYTES {
        return Err(BuiltinModelError(
            "workspace view exceeds rebuild byte bound".to_owned(),
        ));
    }
    ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        rows,
        initial.coverage().to_vec(),
        capability,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))
}

fn publish_builtin_package(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
) -> Result<(), BuiltinModelError> {
    let current = daemon.engine().daemon().library().view().clone();
    let intent = workspace_intent(daemon)?.ok_or_else(|| {
        BuiltinModelError("workspace commit has no authoritative package intent".to_owned())
    })?;
    let package_row = current.row(RowId::Package(intent.package));
    if (intent.is_add()
        && package_row
            .as_ref()
            .is_some_and(|row| row.label == intent.label))
        || (!intent.is_add() && package_row.is_none())
    {
        return Ok(());
    }
    let delta = if intent.is_add() {
        ViewDelta::Upsert {
            row: Row::new(
                RowId::Package(intent.package),
                current.basis(),
                &intent.label,
            ),
        }
    } else {
        ViewDelta::Remove {
            id: RowId::Package(intent.package),
        }
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let workspace_root = snapshot.root();
    let capability = builtin_view_capability_for_workspace(&snapshot)?;
    let prepared = current
        .prepare(delta, capability)
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
            view,
            cursor,
            &admission,
            Some(backend_engine::CursorEvent::View {
                delta: Box::new(committed),
            }),
        )
        .map_err(|error| BuiltinModelError(format!("{error:?}")))
}

/// Starts the compiled locald profile. It does all startup work before the
/// listener is bound, so an invalid durable directory cannot look ready.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "startup must complete all durable admission before binding"
)]
pub fn run(config: ProcessConfig) -> ExitCode {
    let result = (|| {
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
        let dispatcher = builtin_dispatcher(product_secret, Arc::clone(&profile))
            .map_err(ProcessError::Profile)?;
        let relation_registry = RelationAdmissionRegistry::new()
            .with_relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| {
                ProcessError::Profile(format!("register builtin relation: {error:?}"))
            })?;
        let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
            &config.workspace,
            BuiltinModel,
            genesis().map_err(|error| ProcessError::Profile(error.to_string()))?,
            dispatcher,
            DaemonConfig::default(),
            relation_registry,
        )
        .map_err(|error| ProcessError::Profile(error.to_string()))?;
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
            let view = view_for_workspace(&daemon)
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
        let owner =
            daemon.into_owner_with_admission(command_adapter, NoCompletionAdmission, replication);
        Ok(crate::process::run_process(owner, config))
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("backend-locald: {error}");
            ExitCode::from(70)
        }
    }
}

fn command_adapter(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    body: &[u8],
) -> Result<Vec<u8>, BuiltinModelError> {
    // The library's strict deserializer admits identity-bearing commands only
    // when their producer certificate carries canonical preimages. Health is
    // identity-free, so arbitrary bounded request correlation IDs remain valid.
    let request = backend_engine::decode_command_dto(body)
        .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
    let request_id = request.request_id;
    let request_certificate = request.certificate().cloned();
    let command = request.command;
    let (reply, certificate) = match command {
        Command::Add { package } => {
            let label = certified_package_label(request_certificate.as_ref(), package)?;
            let intent = BuiltinIntent::add(package, label.clone())?;
            commit_builtin_intent(daemon, request_id, &intent)
                .map_err(|error| BuiltinModelError(format!("commit package intent: {error}")))?;
            publish_builtin_package(daemon)
                .map_err(|error| BuiltinModelError(format!("publish package view: {error}")))?;
            let intent_id = backend_engine::intent_id("request_package", package.as_bytes());
            let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
                id: backend_engine::encode_id(intent_id.as_bytes()),
                token: "request_package".to_owned(),
                payload: package.as_bytes().to_vec().into_boxed_slice(),
            });
            (CommandReply::Added(intent_id), Some(certificate))
        }
        Command::Remove { package } => {
            let label = certified_package_label(request_certificate.as_ref(), package)?;
            let intent = BuiltinIntent::remove(package, label.clone())?;
            commit_builtin_intent(daemon, request_id, &intent)
                .map_err(|error| BuiltinModelError(format!("commit package intent: {error}")))?;
            publish_builtin_package(daemon)
                .map_err(|error| BuiltinModelError(format!("publish package view: {error}")))?;
            let intent_id = backend_engine::intent_id("remove_package", package.as_bytes());
            let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
                id: backend_engine::encode_id(intent_id.as_bytes()),
                token: "remove_package".to_owned(),
                payload: package.as_bytes().to_vec().into_boxed_slice(),
            });
            (CommandReply::Removed(intent_id), Some(certificate))
        }
        command => {
            let reply = match daemon.engine().daemon().library().execute(command.clone()) {
                Ok(reply) => reply,
                Err(error) => CommandReply::Error(error.to_string()),
            };
            let certificate = projection::reply_certificate(
                &command,
                &reply,
                daemon.engine().daemon().library().view(),
                daemon.engine().daemon().library().cursor(),
                request_certificate,
            )?;
            (reply, certificate)
        }
    };
    let mut reply = match reply {
        CommandReply::Health(root) => backend_engine::ReplyDto::health(
            request_id,
            root,
            daemon.engine().daemon().library().cursor(),
        ),
        reply => backend_engine::ReplyDto::new(request_id, reply),
    };
    if let Some(certificate) = certificate {
        reply = reply.with_certificate(certificate);
    }
    backend_engine::encode_reply_dto(&reply).map_err(BuiltinModelError)
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

fn commit_builtin_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    request_id: u64,
    intent: &BuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let _ = intent.encode()?;
    let request = BuiltinModel.request_id(intent);
    let expected = daemon.engine().daemon().owner().head().expectation();
    let receiver = daemon
        .client()
        .request(
            request_id,
            crate::Request::Commit {
                request,
                expected,
                intent: intent.clone(),
            },
        )
        .map_err(|error| BuiltinModelError(format!("queue builtin intent: {error:?}")))?;
    if !daemon.serve_one() {
        return Err(BuiltinModelError(
            "builtin intent owner did not make progress".to_owned(),
        ));
    }
    match crate::service::wait_for_daemon_reply(daemon, &receiver)
        .map_err(|error| BuiltinModelError(error.to_string()))?
    {
        backend_engine::DaemonReply::Commit(Ok(_)) => Ok(()),
        backend_engine::DaemonReply::Commit(Err(error)) => {
            Err(BuiltinModelError(error.to_string()))
        }
        _ => Err(BuiltinModelError(
            "builtin intent was sent to the wrong owner lane".to_owned(),
        )),
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
