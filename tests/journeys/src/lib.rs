//! Black-box product journeys through the public composition seams.
//!
//! The fixtures in this crate deliberately build their identities from the
//! checked version, library, and store constructors. A journey may bridge a
//! checked value into an older transport claim only after obtaining that value
//! from the public admission API; no test manufactures a trusted root,
//! commit, receipt, coverage witness, or worker attestation from bytes.
#![deny(unsafe_code)]
#![cfg(test)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

#[cfg(unix)]
pub mod surface_matrix;

use backend_client::SubscriptionRequest;
use backend_desktop::{core::VersionedRoot, model::AppSnapshot};
use backend_engine::dispatch::{
    Blake3AuthorityVerifier, CompleteSemanticCoverage, DispatchCompletion, DispatchError,
    DispatchPlan, Dispatcher, ExpectedInput, LoopbackTransport, OutputAdmissionValidator,
    RemoteAuthorityPolicy, RemoteDispatchContract, RemoteTransport, RetainedOutput,
    SemanticCoverageAdmissionError, SemanticCoverageBinding, SemanticCoverageValidator,
    UntrustedSemanticCoverageClaim,
};
use backend_engine::effects::{EffectCodec, EffectSnapshotLimits};
use backend_engine::worker::{
    AdmittedInput, Blake3WorkerSigner, PreparedOutput, PureRecipeExecutor, PureWorkContext,
    WorkerAttestationSigner, WorkerCapabilities, WorkerEndpoint, WorkerError,
};
use backend_engine::{
    Boundary, DaemonConfig, DaemonError, DaemonReply, DispatchJournal, DispatchJournalLimits,
    DispatchRecoveryAction, EffectCoordinator, EffectError, EffectJournalPersistence, EffectKey,
    EffectPersistence, EffectPhase, EffectSink, EffectSpec, HeadExpectation, JournalLimits,
    PreparedTransition, QueueBudget, QueueError, QueueSized, SinkApply, SinkError, SinkObservation,
    TransactionId, TransactionSchema, ViewBindingState, WireCertificate, WireClaim, WorkspaceHead,
    WorkspaceModel, WorkspaceOwner, WorkspaceSnapshot, effect_key,
};
use backend_execution::{
    Admission, AttemptManager, AuthorityVersion, Budget, CompletionCost, CostObservation,
    CostSnapshot, EnvelopeBudgets, HedgeSide, LocalCapability, LocalState, ObservationError,
    OutputEquivalence, OutputLookup, PlacementClass, ReadManifestId, RecipeId, RemoteCapability,
    RemoteState, ResourceVector, ScheduleRequest, Scheduler, VersionedWorkIdentity, WorkInterner,
};
use backend_library::{
    Basis, Command, CommandDto, CommittedViewDelta, CoverageCapability, Cursor, CursorEvent,
    CursorRead, CursorResetReason, DocumentQuery, Frontier, Intent, IntentError, IntentLog,
    Library, LibraryError, Row, RowId, ViewDelta, ViewRoot, object_version, package_key,
    symbol_key, view_key, view_state_root,
};
use backend_replication::{
    AuthorityEpoch, CancelAttempt, CapabilityManifest, Fence, RecipeCapability, ReplicationError,
    ResourceEnvelope, RevocationVersion, SchemaDescriptor, SparseCoverage, TransportLimits,
    TransportMessage, VersionRange, WireAuthorityPolicy, WireIdentity, WireRecipeRequest,
    WireRecipeResult, WorkspaceRootClaim,
};
use backend_semantic::{DependencyManifest, FacetKind, Read, Recipe};
use backend_store::{
    ClosureManifest, FileStore, LayoutId, OrderedMap, RelationAdmissionRegistry, StoredValue,
    TypedObject, WorkspaceClosure, decode_pack, encode_pack,
};
use backend_version::{
    AuthorityScopeClaim, BasisBinding, CommitProvenance, CoverageWitness, IdContext, MapChange,
    ObjectClosure as VersionObjectClosure, ObjectKey, ObjectVersion, ProducerObservationClaims,
    ProducerObservationVerifier, Relation, RelationBinding, RelationState, Schema, ScopeRoot,
    UntrustedId, UntrustedProducerObservation, WorkspaceManifest, admit_complete_scope,
    admit_producer_observation, commit_checked, prepare_delta, workspace_delta,
};
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

struct JourneyCoverageVerifier(ScopeRoot);

impl ProducerObservationVerifier for JourneyCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let scope = self.0;
        let expected_identity = *scope.as_bytes();
        let expected_evidence = scope.as_bytes();
        if observation.producer_identity() != expected_identity
            || observation.context() != expected_identity
            || observation.scope_root() != scope
            || observation.evidence() != expected_evidence
        {
            return Err("invalid journey producer observation");
        }
        Ok(ProducerObservationClaims::new(
            expected_identity,
            scope,
            expected_identity,
            *blake3::hash(expected_evidence).as_bytes(),
        ))
    }
}

fn library_coverage(object: backend_library::SemanticObject) -> CoverageCapability {
    let scope = ScopeRoot::from_bytes(object.to_bytes());
    let declared = AuthorityScopeClaim::from_object_version(object);
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            *scope.as_bytes(),
            scope,
            *scope.as_bytes(),
            scope.as_bytes().to_vec(),
        ),
        &JourneyCoverageVerifier(scope),
    )
    .expect("journey producer observation");
    CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(declared, observation).expect("journey coverage"),
        scope.as_bytes().to_vec(),
    )
    .expect("journey coverage evidence")
}

/// Authority scope used by the complete typed workspace fixture.
#[derive(Debug)]
struct JourneyAuthority;

impl Schema for JourneyAuthority {
    const DOMAIN: u8 = 0x92;
    const TYPE: u16 = 0x0001;
    type Value = u64;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

/// Immutable workspace basis used by the fixture.
#[derive(Debug)]
struct JourneyBasis;

impl Schema for JourneyBasis {
    const DOMAIN: u8 = 0x92;
    const TYPE: u16 = 0x0002;
    type Value = u64;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

/// One authoritative relation in the fixture workspace.
#[derive(Debug)]
struct JourneyRelation;

impl Relation for JourneyRelation {
    const DOMAIN: u8 = 0x92;
    const TYPE: u16 = 0x0010;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl backend_version::CanonicalRelation for JourneyRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
        let bytes: [u8; 8] = bytes
            .try_into()
            .map_err(|_| backend_version::RelationDecodeError::Malformed)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
        Self::decode_key(bytes)
    }
}

/// A durable workspace intent whose base and target values are checked by the
/// model before it crosses into the engine's owner protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
struct JourneyIntent {
    base: u64,
    target: u64,
    request: [u8; 32],
}

impl JourneyIntent {
    fn new(base: u64, target: u64) -> Self {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&base.to_be_bytes());
        bytes.extend_from_slice(&target.to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.journey.intent.v1\0");
        hasher.update(&bytes);
        Self {
            base,
            target,
            request: *hasher.finalize().as_bytes(),
        }
    }
}

impl QueueSized for JourneyIntent {
    fn queue_bytes(&self) -> usize {
        // This is the model's encoded intent footprint. Queue callers never
        // supply an independent byte charge for a product request.
        16 + self.request.len()
    }
}

type JourneyDispatcher = Dispatcher<JourneyAdmissionValidator, Blake3AuthorityVerifier>;
type JourneyLocald =
    backend_locald::Locald<JourneyModel, JourneyAdmissionValidator, Blake3AuthorityVerifier>;
type JourneyDaemon =
    backend_engine::Daemon<JourneyModel, JourneyAdmissionValidator, Blake3AuthorityVerifier>;

fn open_journey_owner(path: &std::path::Path) -> WorkspaceOwner<JourneyModel> {
    WorkspaceOwner::open_with_registry(path, JourneyModel, journey_genesis(), journey_registry())
        .unwrap_or_else(|error| panic!("open journey owner: {error:?}"))
}

fn open_journey_locald(
    path: &std::path::Path,
    dispatcher: JourneyDispatcher,
    config: DaemonConfig,
) -> JourneyLocald {
    backend_locald::Locald::open_with_dispatcher_and_registry(
        path,
        JourneyModel,
        journey_genesis(),
        dispatcher,
        config,
        journey_registry(),
    )
    .unwrap_or_else(|error| panic!("open journey locald: {error:?}"))
}

#[allow(clippy::unnecessary_wraps)]
fn admit_process_view(_workspace: &WorkspaceSnapshot, _view: &ViewRoot) -> Result<(), String> {
    Ok(())
}

#[derive(Clone, Debug)]
struct JourneyPlanningError(String);

impl fmt::Display for JourneyPlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JourneyPlanningError {}

/// Admits a fixture observation through the same producer verifier boundary
/// used by the product paths.
fn complete_scope_equality_fixture(scope: u64) -> CoverageWitness {
    let version = ObjectVersion::<JourneyAuthority>::from_value(&scope);
    let declared = AuthorityScopeClaim::from_object_version(version);
    let observed = declared.scope_root();
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            *observed.as_bytes(),
            observed,
            *observed.as_bytes(),
            observed.as_bytes().to_vec(),
        ),
        &JourneyCoverageVerifier(observed),
    )
    .expect("producer observation");
    CoverageWitness::Complete(
        admit_complete_scope(declared, observation)
            .unwrap_or_else(|error| panic!("admit complete scope: {error}")),
    )
}

fn relation_state(value: u64) -> RelationState<JourneyRelation> {
    RelationState::from_entries([(1, value)], complete_scope_equality_fixture(1))
        .unwrap_or_else(|error| panic!("relation state: {error}"))
}

fn checked_manifest(value: u64) -> WorkspaceManifest {
    let state = relation_state(value);
    let basis = ObjectVersion::<JourneyBasis>::from_value(&1);
    let authority = ObjectVersion::<JourneyAuthority>::from_value(&1);
    WorkspaceManifest::new_checked(
        1,
        vec![RelationBinding::from_state(&state)],
        vec![BasisBinding::from_version(basis)],
        VersionObjectClosure::from_version(authority),
        complete_scope_equality_fixture(1),
    )
    .unwrap_or_else(|error| panic!("checked workspace manifest: {error}"))
}

fn journey_registry() -> RelationAdmissionRegistry {
    RelationAdmissionRegistry::default()
        .with_relation::<JourneyRelation>()
        .unwrap_or_else(|error| panic!("journey relation registry: {error:?}"))
}

fn journey_health_certificate(root: &ViewRoot, cursor: Cursor) -> WireCertificate {
    let mut certificate = WireCertificate::new().with_claim(WireClaim::KeyBytes {
        schema: backend_engine::WireSchema::ViewRecipe,
        id: backend_engine::encode_id(root.recipe().as_bytes()),
        value: b"library-view-v1".to_vec().into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Version {
        schema: backend_engine::WireSchema::ViewVersion,
        id: backend_engine::encode_id(root.version().as_bytes()),
        value: backend_engine::view_version_preimage(
            root.recipe(),
            root.basis(),
            root.frontier(),
            root.root(),
            root.coverage(),
        )
        .into_boxed_slice(),
    });
    certificate = certificate.with_claim(WireClaim::Root {
        schema: backend_engine::WireSchema::ViewRelation,
        id: backend_engine::encode_id(root.root().as_bytes()),
        canonical: root
            .canonical_relation_bytes()
            .unwrap_or_else(|error| panic!("health relation certificate: {error:?}"))
            .into_boxed_slice(),
    });
    if root.root() != root.basis().root {
        certificate = certificate.with_claim(WireClaim::Root {
            schema: backend_engine::WireSchema::ViewRelation,
            id: backend_engine::encode_id(root.basis().root.as_bytes()),
            canonical: backend_engine::canonical_empty::<backend_engine::ViewRelation>()
                .as_bytes()
                .to_vec()
                .into_boxed_slice(),
        });
    }
    certificate = certificate.with_claim(WireClaim::Version {
        schema: backend_engine::WireSchema::Object,
        id: backend_engine::encode_id(root.basis().object.as_bytes()),
        value: b"library-source-v1".to_vec().into_boxed_slice(),
    });
    if let Some(producer) = root.capability() {
        let source = backend_engine::encode_id(root.basis().object.as_bytes());
        certificate = certificate.with_claim(WireClaim::Coverage {
            scope: source.clone(),
            observed: source,
            producer: backend_engine::encode_id(&producer.producer_identity()),
            context: backend_engine::encode_id(&producer.context()),
            evidence: producer.evidence().to_vec().into_boxed_slice(),
        });
    }
    certificate = certificate.with_claim(WireClaim::Key {
        schema: backend_engine::WireSchema::Branch,
        id: backend_engine::encode_id(root.basis().branch.as_bytes()),
        value: "main".to_owned(),
    });
    certificate
        .with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Log,
            id: backend_engine::encode_id(root.basis().log.as_bytes()),
            value: "library".to_owned(),
        })
        .with_claim(WireClaim::Cursor {
            recipe: backend_engine::encode_id(cursor.recipe().as_bytes()),
            version: backend_engine::encode_id(cursor.version().as_bytes()),
            branch: backend_engine::encode_id(cursor.branch().as_bytes()),
            log: backend_engine::encode_id(cursor.log().as_bytes()),
            schema: cursor.schema(),
            root: backend_engine::encode_id(cursor.root().as_bytes()),
            sequence: cursor.sequence(),
        })
}

fn workspace_closure(
    workspace: &WorkspaceManifest,
    relation: &RelationState<JourneyRelation>,
    transition: Option<&backend_version::CheckedWorkspaceTransition>,
    commit: Option<&backend_version::CheckedCommit>,
    transaction: Option<TransactionId>,
) -> WorkspaceClosure {
    let authority = 1u64;
    let authority_key = ObjectKey::<JourneyAuthority>::from_value(&authority);
    let basis = 1u64;
    let basis_key = ObjectKey::<JourneyBasis>::from_value(&basis);
    let mut objects = vec![
        TypedObject::from_value(&authority_key, &authority),
        TypedObject::from_value(&basis_key, &basis),
    ];
    objects.push(
        TypedObject::from_relation_state(relation)
            .unwrap_or_else(|error| panic!("relation object: {error:?}")),
    );
    if let Some(transaction) = transaction {
        let bytes = transaction.as_bytes();
        let key = ObjectKey::<TransactionSchema>::from_value(&bytes[..]);
        objects.push(TypedObject::from_value(&key, &bytes[..]));
    }
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let registry = journey_registry();
    let manifest = ClosureManifest::new_with_registry(objects, &registry)
        .unwrap_or_else(|error| panic!("checked workspace closure: {error:?}"));
    let result = match transition {
        Some(transition) => WorkspaceClosure::from_checked_transition_with_registry(
            workspace, transition, commit, manifest, &registry,
        ),
        None => {
            WorkspaceClosure::from_checked_manifest_with_registry(workspace, manifest, &registry)
        }
    };
    result.unwrap_or_else(|error| panic!("admit workspace closure: {error:?}"))
}

fn journey_genesis() -> WorkspaceHead {
    let manifest = checked_manifest(0);
    let transaction = TransactionId::derive(0, manifest.root(), [0; 32], 0);
    let delta = workspace_delta(&manifest, &manifest, Vec::new())
        .unwrap_or_else(|error| panic!("checked genesis delta: {error}"));
    let checked_delta = delta.into_checked();
    let provenance = CommitProvenance::from_versions(
        ObjectVersion::<JourneyAuthority>::from_value(&1),
        transaction.version(),
        b"backend.engine.genesis.v1".to_vec(),
    );
    let checked_commit = checked_delta
        .clone()
        .commit(Vec::new(), provenance)
        .unwrap_or_else(|error| panic!("checked genesis commit: {error}"));
    let closure = workspace_closure(
        &manifest,
        &relation_state(0),
        Some(&checked_delta),
        Some(&checked_commit),
        Some(transaction),
    );
    let registry = journey_registry();
    WorkspaceHead::genesis_with_registry(manifest, closure, &registry)
        .unwrap_or_else(|error| panic!("checked workspace genesis: {error}"))
}

#[test]
fn checked_manifest_closure_admits_the_typed_authority_object() {
    let manifest = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        ObjectVersion::<JourneyAuthority>::from_value(&1),
        complete_scope_equality_fixture(1),
    )
    .unwrap_or_else(|error| panic!("checked authority-only manifest: {error}"));
    let authority = 1u64;
    let key = ObjectKey::<JourneyAuthority>::from_value(&authority);
    let objects = ClosureManifest::new(vec![TypedObject::from_value(&key, &authority)])
        .unwrap_or_else(|error| panic!("authority object closure: {error:?}"));
    let closure = WorkspaceClosure::from_checked_manifest(&manifest, objects)
        .unwrap_or_else(|error| panic!("admit authority-only closure: {error:?}"));
    assert_eq!(closure.root(), manifest.root());
    assert_eq!(closure.binding().root(), &manifest.root().to_bytes());
}

fn typed_delta(
    base: u64,
    target: u64,
) -> Result<backend_version::Delta<JourneyRelation>, JourneyPlanningError> {
    prepare_delta(
        &relation_state(base),
        if base == target {
            Vec::new()
        } else {
            vec![MapChange {
                key: 1,
                before: Some(base),
                after: Some(target),
            }]
        },
    )
    .map_err(|error| JourneyPlanningError(error.to_string()))
}

#[derive(Clone, Copy, Debug, Default)]
struct JourneyModel;

impl WorkspaceModel for JourneyModel {
    type Intent = JourneyIntent;
    type Error = JourneyPlanningError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        intent.request
    }

    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        let base_manifest = checked_manifest(intent.base);
        if base.root() != base_manifest.root() {
            return Err(JourneyPlanningError(
                "intent base does not match the admitted workspace snapshot".to_owned(),
            ));
        }

        let target_manifest = checked_manifest(intent.target);
        let typed_delta = typed_delta(intent.base, intent.target)?;
        let transition = if intent.base == intent.target {
            Vec::new()
        } else {
            vec![
                backend_version::RelationTransition::try_from_delta(&typed_delta)
                    .map_err(|error| JourneyPlanningError(error.to_string()))?,
            ]
        };
        let checked_delta = workspace_delta(&base_manifest, &target_manifest, transition)
            .map_err(|error| JourneyPlanningError(format!("{error:?}")))?;

        let authority = ObjectVersion::<JourneyAuthority>::from_value(&1);
        let provenance =
            CommitProvenance::from_versions(authority, transaction.version(), intent.request);
        let commit = commit_checked(&target_manifest, Vec::new(), provenance.clone())
            .map_err(|error| JourneyPlanningError(error.to_string()))?;

        let checked_transition = checked_delta.clone().into_checked();
        let checked_commit = checked_transition
            .clone()
            .commit(Vec::new(), provenance.clone())
            .map_err(|error| JourneyPlanningError(error.to_string()))?;
        let closure = workspace_closure(
            &target_manifest,
            &relation_state(intent.target),
            Some(&checked_transition),
            Some(&checked_commit),
            Some(transaction),
        );
        PreparedTransition::new_with_registry(
            intent.request,
            transaction,
            checked_delta,
            commit,
            closure,
            &journey_registry(),
        )
        .map_err(|error| JourneyPlanningError(error.to_string()))
    }

    fn admit_persisted(
        &self,
        persisted: &backend_engine::workspace::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        admit_persisted_impl(persisted, None)
    }

    fn admit_persisted_with_store(
        &self,
        persisted: &backend_engine::workspace::PersistedTransition,
        store: &FileStore,
    ) -> Result<PreparedTransition, Self::Error> {
        let root = checked_manifest_root(persisted)?;
        let evidence = store
            .read_relation_node_claim::<JourneyRelation>(
                UntrustedId::<JourneyRelation>::from_wire(
                    root.as_bytes(),
                    IdContext::relation::<JourneyRelation>(),
                )
                .map_err(|error| JourneyPlanningError(format!("{error:?}")))?,
            )
            .map_err(|error| JourneyPlanningError(format!("{error:?}")))?;
        let object = TypedObject::from_state_root(evidence.root(), evidence.node())
            .map_err(|error| JourneyPlanningError(format!("{error:?}")))?;
        admit_persisted_impl(persisted, Some(object))
    }
}

fn checked_manifest_root(
    persisted: &backend_engine::workspace::PersistedTransition,
) -> Result<backend_version::StateRoot<JourneyRelation>, JourneyPlanningError> {
    let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
        .map_err(|error| JourneyPlanningError(error.to_string()))?;
    for value in 0..=4 {
        let candidate = checked_manifest(value);
        let admitted = untrusted_manifest.clone().admit_checked(
            vec![RelationBinding::from_state(&relation_state(value))],
            vec![BasisBinding::from_version(
                ObjectVersion::<JourneyBasis>::from_value(&1),
            )],
            VersionObjectClosure::from_version(ObjectVersion::<JourneyAuthority>::from_value(&1)),
            complete_scope_equality_fixture(1),
        );
        if admitted.is_ok_and(|manifest| manifest == candidate) {
            return Ok(relation_state(value).root());
        }
    }
    Err(JourneyPlanningError(
        "unknown persisted target manifest".to_owned(),
    ))
}

fn admit_persisted_closure(
    persisted: &backend_engine::workspace::PersistedTransition,
    relation_object: Option<TypedObject>,
    target_manifest: &WorkspaceManifest,
    checked_transition: &backend_version::CheckedWorkspaceTransition,
    checked_commit: &backend_version::CheckedCommit,
) -> Result<WorkspaceClosure, JourneyPlanningError> {
    let mut closure_manifest = persisted.closure_manifest().clone();
    if let Some(relation_object) = relation_object {
        let change = backend_store::ManifestChange::insert(&relation_object)
            .map_err(|error| JourneyPlanningError(format!("{error:?}")))?;
        closure_manifest = closure_manifest
            .prepare_delta(&[change])
            .map_err(|error| JourneyPlanningError(format!("{error:?}")))?
            .commit();
    }
    WorkspaceClosure::from_checked_transition_with_registry(
        target_manifest,
        checked_transition,
        Some(checked_commit),
        closure_manifest,
        &journey_registry(),
    )
    .map_err(|error| JourneyPlanningError(format!("{error:?}")))
}

fn admit_persisted_impl(
    persisted: &backend_engine::workspace::PersistedTransition,
    relation_object: Option<TypedObject>,
) -> Result<PreparedTransition, JourneyPlanningError> {
    let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
        .map_err(|error| JourneyPlanningError(error.to_string()))?;
    let (target, admitted_manifest) = (0..=4)
        .find_map(|value| {
            let candidate = checked_manifest(value);
            let admitted = untrusted_manifest.clone().admit_checked(
                vec![RelationBinding::from_state(&relation_state(value))],
                vec![BasisBinding::from_version(
                    ObjectVersion::<JourneyBasis>::from_value(&1),
                )],
                VersionObjectClosure::from_version(ObjectVersion::<JourneyAuthority>::from_value(
                    &1,
                )),
                complete_scope_equality_fixture(1),
            );
            match admitted {
                Ok(admitted) if admitted == candidate => Some((value, admitted)),
                _ => None,
            }
        })
        .ok_or_else(|| JourneyPlanningError("unknown persisted target manifest".to_owned()))?;
    let target_manifest = admitted_manifest;

    let untrusted_delta =
        backend_version::WorkspaceDelta::decode_untrusted(persisted.delta_bytes())
            .map_err(|error| JourneyPlanningError(error.to_string()))?;
    let base = (0..=4)
        .find(|value| checked_manifest(*value).root().to_bytes() == untrusted_delta.base())
        .ok_or_else(|| JourneyPlanningError("unknown persisted base root".to_owned()))?;
    if checked_manifest(target).root().to_bytes() != untrusted_delta.target() {
        return Err(JourneyPlanningError(
            "persisted delta target differs from manifest".to_owned(),
        ));
    }
    let typed_delta = typed_delta(base, target)?;
    let relations = if base == target {
        if !untrusted_delta.relations().is_empty() {
            return Err(JourneyPlanningError(
                "no-op persisted delta contains a relation transition".to_owned(),
            ));
        }
        Vec::new()
    } else {
        let relation = untrusted_delta
            .relations()
            .first()
            .ok_or_else(|| JourneyPlanningError("persisted delta omitted relation".to_owned()))?
            .admit_delta(&typed_delta)
            .map_err(|error| JourneyPlanningError(error.to_string()))?;
        if untrusted_delta.relations().len() != 1 {
            return Err(JourneyPlanningError(
                "persisted delta contains an unexpected relation".to_owned(),
            ));
        }
        vec![relation]
    };
    let checked_delta = untrusted_delta
        .admit(&checked_manifest(base), &target_manifest, relations)
        .map_err(|error| JourneyPlanningError(error.to_string()))?;

    let untrusted_commit = backend_version::Commit::decode_untrusted(persisted.commit_bytes())
        .map_err(|error| JourneyPlanningError(error.to_string()))?;
    let authority =
        VersionObjectClosure::from_version(ObjectVersion::<JourneyAuthority>::from_value(&1));
    let provenance = CommitProvenance::admit_untrusted(
        untrusted_commit.provenance().clone(),
        authority,
        VersionObjectClosure::from_version(persisted.transaction().version()),
    )
    .map_err(|error| JourneyPlanningError(format!("{error:?}")))?;
    let commit = untrusted_commit
        .clone()
        .admit(&target_manifest, provenance.clone())
        .map_err(|error| JourneyPlanningError(error.to_string()))?;
    let expected_request = JourneyIntent::new(base, target).request;
    if expected_request != persisted.request() {
        return Err(JourneyPlanningError(
            "persisted request does not match its checked transition".to_owned(),
        ));
    }
    let checked_transition = checked_delta.clone().into_checked();
    let checked_commit = checked_transition
        .clone()
        .commit(Vec::new(), provenance.clone())
        .map_err(|error| JourneyPlanningError(error.to_string()))?;
    let closure = admit_persisted_closure(
        persisted,
        relation_object,
        &target_manifest,
        &checked_transition,
        &checked_commit,
    )?;
    PreparedTransition::new_with_registry(
        persisted.request(),
        persisted.transaction(),
        checked_delta,
        commit,
        closure,
        &journey_registry(),
    )
    .map_err(|error| JourneyPlanningError(error.to_string()))
}

fn unique_directory(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let path = std::env::temp_dir().join(format!("backend-journey-{label}-{nanos}"));
    std::fs::create_dir_all(&path)
        .unwrap_or_else(|error| panic!("create journey directory: {error}"));
    path
}

fn daemon_config() -> DaemonConfig {
    DaemonConfig {
        commands: QueueBudget::new(8, 4096),
        replication: QueueBudget::new(8, 4096),
        completions: QueueBudget::new(8, 4096),
        subscriptions: QueueBudget::new(8, 4096),
    }
}

fn local_query_state(daemon: &mut JourneyLocald, request_id: u64) -> backend_engine::QueryState {
    let receiver = daemon
        .client()
        .request(request_id, backend_locald::Request::Query)
        .unwrap_or_else(|error| panic!("queue query: {error:?}"));
    assert!(daemon.serve_one());
    match receiver
        .recv()
        .unwrap_or_else(|error| panic!("query reply: {error}"))
    {
        DaemonReply::Query(state) => match *state {
            Ok(state) => state,
            Err(error) => panic!("query response: {error:?}"),
        },
        other => panic!("unexpected query response: {other:?}"),
    }
}

fn local_query(daemon: &mut JourneyLocald, request_id: u64) -> WorkspaceSnapshot {
    let state = local_query_state(daemon, request_id);
    assert!(state.view.is_coherent());
    state.workspace
}

fn local_commit(
    daemon: &mut JourneyLocald,
    request_id: u64,
    snapshot: &WorkspaceSnapshot,
    intent: JourneyIntent,
) -> WorkspaceHead {
    let receiver = daemon
        .client()
        .request(
            request_id,
            backend_locald::Request::Commit {
                request: intent.request,
                expected: HeadExpectation::new(snapshot.root(), snapshot.sequence()),
                intent,
            },
        )
        .unwrap_or_else(|error| panic!("queue commit: {error:?}"));
    assert!(daemon.serve_one());
    match receiver
        .recv()
        .unwrap_or_else(|error| panic!("commit reply: {error}"))
    {
        DaemonReply::Commit(Ok(head)) => head,
        other => panic!("unexpected commit response: {other:?}"),
    }
}

#[test]
fn locald_commit_reopens_from_the_filesystem_head_and_checked_closure() {
    let path = unique_directory("locald-reopen");
    let committed_root = {
        let mut daemon = open_journey_locald(&path, journey_dispatcher(), daemon_config());
        let snapshot = local_query(&mut daemon, 1);
        let head = local_commit(&mut daemon, 2, &snapshot, JourneyIntent::new(0, 1));
        assert_eq!(head.sequence(), snapshot.sequence() + 1);
        head.root()
    };

    let mut reopened = open_journey_locald(&path, journey_dispatcher(), daemon_config());
    let recovered = local_query(&mut reopened, 3);
    assert_eq!(recovered.root(), committed_root);
    assert_eq!(recovered.sequence(), 1);
    drop(reopened);
    std::fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup locald: {error}"));
}

#[test]
fn locald_replays_an_acknowledged_intent_without_advancing_the_head() {
    let path = unique_directory("locald-idempotent-replay");
    let mut daemon = open_journey_locald(&path, journey_dispatcher(), daemon_config());
    let snapshot = local_query(&mut daemon, 4);
    let intent = JourneyIntent::new(0, 1);
    let first = local_commit(&mut daemon, 5, &snapshot, intent.clone());
    let replay = local_commit(&mut daemon, 6, &snapshot, intent);
    assert_eq!(replay.root(), first.root());
    assert_eq!(replay.sequence(), first.sequence());
    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup idempotency locald: {error}"));
}

#[test]
fn owner_fault_at_head_selection_recovers_one_coherent_root() {
    let path = unique_directory("owner-crash");
    let mut owner = open_journey_owner(&path);
    let snapshot = owner.snapshot();
    let intent = JourneyIntent::new(0, 1);
    let prepared = owner
        .prepare(
            HeadExpectation::new(snapshot.root(), snapshot.sequence()),
            intent,
        )
        .unwrap_or_else(|error| panic!("prepare: {error:?}"));
    let durable = owner
        .durable(prepared)
        .unwrap_or_else(|error| panic!("durable: {error:?}"));
    owner.faults().arm(Boundary::HeadSelection);
    let published = owner.publish(durable).unwrap_or_else(|error| {
        panic!("publish with pending head-selection acknowledgement: {error:?}")
    });
    assert!(published.status().head_selection_pending());
    drop(owner);

    let recovered = open_journey_owner(&path);
    assert_eq!(recovered.head().sequence(), 1);
    assert_eq!(recovered.head().root(), checked_manifest(1).root());
    drop(recovered);
    std::fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup owner: {error}"));
}

#[test]
fn intent_replay_is_idempotent_and_payload_conflict_is_rejected() {
    let mut log = IntentLog::new();
    let first = Intent::request_package(package_key("journey/one"));
    let same = first.clone();
    let conflicting = match first {
        Intent::RequestPackage { request, .. } => Intent::RequestPackage {
            id: package_key("journey/two"),
            request,
        },
        other => panic!("unexpected intent fixture: {other:?}"),
    };
    assert!(log.append(same.clone()).unwrap().inserted);
    assert!(!log.append(same).unwrap().inserted);
    assert!(matches!(
        log.append(conflicting),
        Err(IntentError::IdempotencyConflict { .. })
    ));
    assert_eq!(log.len(), 1);
}

#[test]
fn exact_root_query_accepts_current_source_and_rejects_a_real_stale_view_root() {
    let (library, capability) = checked_library();
    let symbol = symbol_key("journey::query");
    let basis = library.view().basis();
    let stale = library.revision_root();
    let (library, _) = advance_library(
        library,
        ViewDelta::Upsert {
            row: Row::new(RowId::Symbol(symbol), basis, "exact root"),
        },
        &capability,
    );

    let source = library.revision_root();
    let document = library
        .document(DocumentQuery::new(symbol, source))
        .unwrap_or_else(|error| panic!("current-root query: {error}"));
    assert_eq!(document.text(), "exact root");

    assert_ne!(stale, source);
    assert!(matches!(
        library.document(DocumentQuery::new(symbol, stale)),
        Err(LibraryError::WrongBasis { expected, observed })
            if expected == source && observed == stale
    ));
}

#[test]
fn workspace_commit_keeps_the_old_library_view_pinned_to_its_source_root() {
    let (old_library, _) = checked_library();
    let old_view = old_library.view().clone();
    let old_revision = old_library.revision_root();

    let path = unique_directory("workspace-library-coherence");
    let mut daemon = open_journey_locald(&path, journey_dispatcher(), daemon_config());
    let initial = local_query_state(&mut daemon, 20);
    let daemon_old_view = initial.view.clone();
    let daemon_old_source = initial.binding.source_root();
    let snapshot = initial.workspace;
    let head = local_commit(&mut daemon, 22, &snapshot, JourneyIntent::new(0, 1));
    let query = local_query_state(&mut daemon, 23);
    assert_eq!(query.view.basis().root, daemon_old_view.basis().root);
    assert_eq!(query.binding.source_root(), daemon_old_source);
    assert_eq!(query.binding.workspace_root(), head.root());
    assert_eq!(query.binding.state(), ViewBindingState::Stale);
    assert!(!query.binding.is_current());

    // Derive a new product source root from the admitted workspace commit's
    // root through the library's canonical relation helper.
    let workspace_root_text = backend_library::encode_id(&head.root().to_bytes());
    let new_source = view_state_root(&[("workspace-commit".to_owned(), workspace_root_text)]);
    assert_ne!(new_source, old_view.basis().root);

    let new_object = object_version(&head.root().to_bytes());
    let new_capability = library_coverage(new_object);
    let basis = Basis::new(new_source, new_object);
    let frontier = Frontier::new(
        basis.branch,
        basis.log,
        basis.schema,
        basis.root,
        head.sequence(),
    );
    let new_view = ViewRoot::empty_checked(
        view_key(b"library-view-v1"),
        basis,
        frontier,
        new_capability.clone(),
    )
    .unwrap_or_else(|error| panic!("new workspace view: {error:?}"));
    let new_cursor = Cursor::for_view_root(&new_view);
    let new_library = Library::from_view(new_view, new_cursor)
        .unwrap_or_else(|error| panic!("new library projection: {error:?}"));

    let symbol = symbol_key("journey::workspace-coherence");
    let row = Row::new(RowId::Symbol(symbol), basis, "workspace coherence");
    let (new_library, _) = advance_library(new_library, ViewDelta::Upsert { row }, &new_capability);
    let new_revision = new_library.revision_root();

    assert_eq!(old_view.root(), old_revision);
    assert_ne!(new_revision, old_revision);
    assert_eq!(new_library.view().basis().root, new_source);
    assert!(matches!(
        new_library.document(DocumentQuery::new(symbol, old_revision)),
        Err(LibraryError::WrongBasis { expected, observed })
            if expected == new_revision && observed == old_revision
    ));

    let mismatched_cursor = Cursor::for_view(
        old_view.recipe(),
        old_view.version(),
        Frontier::new(
            old_view.frontier().branch,
            old_view.frontier().log,
            old_view.frontier().schema,
            new_source,
            head.sequence(),
        ),
    );
    assert!(Library::from_view(old_view, mismatched_cursor).is_err());

    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup coherence locald: {error}"));
}

fn checked_library() -> (Library, CoverageCapability) {
    let source = object_version(b"library-source-v1");
    let capability = library_coverage(source);
    (
        Library::with_coverage(capability.clone())
            .unwrap_or_else(|error| panic!("checked library: {error}")),
        capability,
    )
}

/// Advances a test projection through the same checked immutable-root seam
/// used by an engine publisher. The production `Library` remains read-only;
/// this fixture owns the replacement projection locally for the next step.
fn advance_library(
    library: Library,
    delta: ViewDelta,
    capability: &CoverageCapability,
) -> (Library, CommittedViewDelta) {
    let prepared = library
        .view()
        .prepare(delta, capability.clone())
        .unwrap_or_else(|error| panic!("prepare library view: {error:?}"));
    library
        .commit(prepared)
        .unwrap_or_else(|error| panic!("commit library view: {error:?}"))
}

#[test]
fn committed_view_delta_advances_the_exact_root_and_cursor_event() {
    let (library, capability) = checked_library();
    let base = library.view().clone();
    let row = Row::new(
        RowId::Symbol(symbol_key("journey::delta")),
        base.basis(),
        "delta",
    );
    let (library, committed) = advance_library(library, ViewDelta::Upsert { row }, &capability);
    assert_eq!(committed.base_root(), base.root());
    assert_eq!(committed.target(), library.view().root());
    assert_ne!(committed.target(), committed.base_root());
    assert_eq!(library.cursor().sequence(), 1);
    let applied = committed
        .clone()
        .apply_to(&base)
        .unwrap_or_else(|error| panic!("apply committed delta: {error:?}"));
    assert_eq!(applied, *library.view());
}

#[test]
fn bounded_subscription_returns_real_events_and_resets_on_a_gap() {
    let (mut library, capability) = checked_library();
    let base = library.view().clone();
    let mut events = Vec::new();
    for name in ["one", "two"] {
        let symbol = format!("journey::{name}");
        let row = Row::new(RowId::Symbol(symbol_key(&symbol)), base.basis(), name);
        let (next, committed) = advance_library(library, ViewDelta::Upsert { row }, &capability);
        library = next;
        events.push(CursorEvent::View {
            delta: Box::new(committed),
        });
    }

    let source = library.cursor();
    let mut bounded = backend_library::CursorSub::for_view(&base, 1);
    let first = bounded
        .read(&source, &events, library.view().clone())
        .unwrap_or_else(|error| panic!("bounded event read: {error:?}"));
    assert!(matches!(
        first,
        CursorRead::Events { ref events, .. } if events.len() == 1
    ));

    let mut gapped = backend_library::CursorSub::for_view(&base, 4);
    let reset = gapped
        .read(&source, &[], library.view().clone())
        .unwrap_or_else(|error| panic!("gap reset: {error:?}"));
    assert!(matches!(
        reset,
        CursorRead::Reset {
            reason: CursorResetReason::Gap,
            ..
        }
    ));
}

#[test]
fn desktop_rekeys_one_snapshot_without_copying_unchanged_branches() {
    let (library, capability) = checked_library();
    let base = library.view().clone();
    let desktop = AppSnapshot::empty(VersionedRoot::from_revision(
        7,
        backend_library::Cursor::at(base.root(), 0),
        0,
    ));
    let shelf = desktop.shelf() as *const _;
    let documents = desktop.documents() as *const _;
    let row = Row::new(
        RowId::Symbol(symbol_key("journey::desktop")),
        base.basis(),
        "desktop",
    );
    let (library, _) = advance_library(library, ViewDelta::Upsert { row }, &capability);
    let rekeyed = desktop.with_key(
        VersionedRoot::from_revision(7, backend_library::Cursor::at(library.view().root(), 1), 0),
        None,
    );
    assert_eq!(rekeyed.root(), library.view().root());
    assert!(core::ptr::eq(shelf, rekeyed.shelf()));
    assert!(core::ptr::eq(documents, rekeyed.documents()));
    assert!(
        SubscriptionRequest::new(library.cursor(), backend_library::MAX_SUBSCRIPTION_EVENTS)
            .is_ok()
    );
}

fn store_coverage() -> CoverageWitness {
    let version = ObjectVersion::<JourneyAuthority>::from_value(&1);
    let declared = AuthorityScopeClaim::from_object_version(version);
    let observed = declared.scope_root();
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new(
            *observed.as_bytes(),
            observed,
            *observed.as_bytes(),
            observed.as_bytes().to_vec(),
        ),
        &JourneyCoverageVerifier(observed),
    )
    .expect("producer observation");
    CoverageWitness::Complete(
        admit_complete_scope(declared, observation)
            .unwrap_or_else(|error| panic!("store scope: {error}")),
    )
}

#[test]
fn semantic_manifest_uses_admitted_scope_and_store_round_trips_the_same_bytes() {
    let source = object_version(b"library-source-v1");
    let capability = library_coverage(source);
    let manifest = Recipe::Names
        .manifest(capability.scope_root())
        .unwrap_or_else(|error| panic!("semantic manifest: {error}"));
    assert!(manifest.invalidated_by(&[Read::exact(FacetKind::Name, capability.scope_root(),)]));
    assert!(!manifest.invalidated_by(&[Read::exact(
        FacetKind::Documentation,
        capability.scope_root(),
    )]));

    let value = StoredValue::new(b"canonical value".to_vec(), 1, Vec::new());
    let map = OrderedMap::try_from_iter_with_coverage(
        [(b"journey-key".to_vec(), value.clone())],
        store_coverage(),
    )
    .unwrap_or_else(|error| panic!("store map: {error:?}"));
    let pack = encode_pack(&map, LayoutId::derive(b"journey-layout"), 4096)
        .unwrap_or_else(|error| panic!("encode pack: {error:?}"));
    let decoded = decode_pack(&pack).unwrap_or_else(|error| panic!("decode pack: {error:?}"));
    assert_eq!(decoded.state_root(), map.state_root());
    assert_eq!(decoded.get(b"journey-key"), Some(&value));

    let path = unique_directory("store-reopen");
    let store =
        FileStore::open(&path, 4096).unwrap_or_else(|error| panic!("open file store: {error:?}"));
    let root = store
        .publish(&map, LayoutId::derive(b"journey-layout"))
        .unwrap_or_else(|error| panic!("publish map: {error:?}"));
    drop(store);
    let reopened =
        FileStore::open(&path, 4096).unwrap_or_else(|error| panic!("reopen file store: {error:?}"));
    let recovered = reopened
        .recover()
        .unwrap_or_else(|error| panic!("recover map: {error:?}"))
        .unwrap_or_else(|| panic!("missing recovered map"));
    assert_eq!(recovered.state_root(), root);
    std::fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup store: {error}"));
}

#[test]
fn locald_command_lane_reports_backpressure_from_the_request_footprint() {
    let path = unique_directory("queue");
    let config = DaemonConfig {
        commands: QueueBudget::new(8, 32),
        ..daemon_config()
    };
    let mut daemon = open_journey_locald(&path, journey_dispatcher(), config);
    let first = daemon
        .client()
        .request(1, backend_locald::Request::Query)
        .unwrap_or_else(|error| panic!("first query: {error:?}"));
    assert!(matches!(
        daemon.client().request(2, backend_locald::Request::Query),
        Err(backend_locald::LocaldError::Queue(QueueError::Bytes))
    ));
    drop(first);
    daemon.close();
    assert!(matches!(
        daemon.client().request(3, backend_locald::Request::Query),
        Err(backend_locald::LocaldError::Queue(QueueError::Closed))
    ));
    drop(daemon);
    std::fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup queue: {error}"));
}

/// Output and semantic admission are kept independent in the journey fixture.
/// A remote result must carry bytes that match the expected immutable output,
/// while its complete semantic witness must be admitted against the exact
/// recipe, read manifest, and authority binding.
#[derive(Clone, Debug)]
struct JourneyAdmissionValidator {
    output: Box<[u8]>,
}

#[derive(Clone, Copy, Debug)]
struct JourneyExecutor;

impl PureRecipeExecutor for JourneyExecutor {
    fn recipe_id(&self) -> RecipeId {
        RecipeId::from_value(b"journey-recipe")
    }

    fn execute(
        &self,
        _invocation: &backend_engine::worker::AdmittedInvocation,
        context: &mut PureWorkContext<'_>,
    ) -> Result<PreparedOutput, WorkerError> {
        context.charge_cpu(1)?;
        if context.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        let bytes = b"journey-remote-output".to_vec().into_boxed_slice();
        let coverage =
            SparseCoverage::complete(bytes.len() as u64).map_err(WorkerError::Replication)?;
        Ok(PreparedOutput { bytes, coverage })
    }
}

impl OutputAdmissionValidator for JourneyAdmissionValidator {
    fn validate(
        &self,
        output: backend_execution::OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        if !semantic.is_complete() || bytes != self.output.as_ref() {
            return Err(DispatchError::OutputMismatch);
        }
        if backend_execution::OutputVersion::from_value(bytes) != output {
            return Err(DispatchError::OutputMismatch);
        }
        Ok(RetainedOutput::from_bytes(output, bytes))
    }

    fn validate_shared(
        &self,
        output: backend_execution::OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        if !semantic.is_complete() || bytes.as_slice() != self.output.as_ref() {
            return Err(DispatchError::OutputMismatch);
        }
        if backend_execution::OutputVersion::from_value(bytes.as_slice()) != output {
            return Err(DispatchError::OutputMismatch);
        }
        Ok(RetainedOutput::from_shared(output, Arc::clone(bytes)))
    }
}

impl SemanticCoverageValidator for JourneyAdmissionValidator {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if !claim.asserted_complete()
            || claim.scope() != 1
            || claim.witness() != b"journey-semantic-witness"
            || claim.read_manifest() != binding.read_manifest()
            || claim.authority() != binding.authority()
        {
            return Err(SemanticCoverageAdmissionError::Rejected);
        }
        Ok(())
    }

    fn validate_manifest(
        &self,
        _binding: &SemanticCoverageBinding,
        _claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if manifest.is_reuse_ready() {
            Ok(())
        } else {
            Err(SemanticCoverageAdmissionError::Rejected)
        }
    }
}

fn journey_dispatcher() -> JourneyDispatcher {
    let mut authority = Blake3AuthorityVerifier::new();
    let identity = execution_identity(0);
    authority.insert_key(identity.authority.to_bytes(), [0xa5; 32]);
    let dispatcher = Dispatcher::with_scheduler(
        journey_scheduler(),
        JourneyAdmissionValidator {
            output: b"journey-remote-output".to_vec().into_boxed_slice(),
        },
        authority,
        RemoteAuthorityPolicy::Signed,
    );
    // A daemon's owner model must be seeded by completions before optional
    // remote work can outrank a ready local path. This keeps the process
    // journey on the same authenticated cost path as a long-lived locald.
    warm_route_costs(&dispatcher, true);
    dispatcher
}

fn journey_scheduler() -> Scheduler {
    let budgets = EnvelopeBudgets::uniform(Budget {
        operations: 8,
        bytes: 64 * 1024,
        hedges: 4,
        resources: ResourceVector {
            cpu_millis: 100,
            memory_bytes: 4096,
            network_bytes: 4096,
            storage_bytes: 4096,
        },
    });
    Scheduler::with_components(
        Admission::with_envelopes(budgets),
        Arc::new(AttemptManager::with_lease_duration(16)),
        Arc::new(OutputLookup::with_limits(32, 64 * 1024)),
        WorkInterner::new(4096, 256),
    )
}

fn execution_identity(value: u64) -> VersionedWorkIdentity<JourneyRelation> {
    VersionedWorkIdentity::new(
        RecipeId::from_value(b"journey-recipe"),
        relation_state(value).root(),
        ReadManifestId::from_value(b"journey-read-manifest"),
        AuthorityVersion::from_value(b"journey-execution-authority"),
        OutputEquivalence::from_value(b"journey-output-equivalence"),
    )
}

fn execution_limits() -> TransportLimits {
    TransportLimits {
        max_frame: 64 * 1024,
        max_chunk: 1024,
        max_object: 4096,
        max_objects: 64,
        max_ranges: 1,
        max_capabilities: 32,
        max_key_bytes: 1024,
        max_inputs: 8,
    }
}

fn execution_capabilities(identity: &VersionedWorkIdentity<JourneyRelation>) -> CapabilityManifest {
    let recipe = WireIdentity::from_typed(&identity.recipe);
    CapabilityManifest {
        protocol: VersionRange::new(1, 1).unwrap_or_else(|error| panic!("protocol range: {error}")),
        schemas: vec![SchemaDescriptor::of::<
            backend_replication::ImmutableObjectSchema,
        >()],
        recipes: vec![RecipeCapability {
            recipe,
            versions: VersionRange::new(1, 1)
                .unwrap_or_else(|error| panic!("recipe range: {error}")),
        }],
        max_object: 4096,
        max_chunk: 1024,
        max_frame: 64 * 1024,
        max_ranges: 32,
        max_resources: ResourceEnvelope::UNBOUNDED,
    }
}

fn execution_contract(
    semantic: CompleteSemanticCoverage,
    output: &[u8],
    limits: TransportLimits,
) -> RemoteDispatchContract {
    let input = backend_replication::ObjectVersion::from_value(b"journey-input");
    RemoteDispatchContract {
        input_basis: checked_manifest(0).root(),
        inputs: vec![ExpectedInput::from_typed(&input)],
        semantic,
        resources: ResourceEnvelope {
            cpu_millis: 10,
            memory_bytes: 128,
            network_bytes: (b"journey-input".len() + output.len()) as u64,
            storage_bytes: 128,
            output_bytes: output.len() as u64,
            processes: 1,
            wall_millis: 100,
        },
        authority_epoch: AuthorityEpoch(1),
        revocation_version: RevocationVersion(1),
        authority_policy: RemoteAuthorityPolicy::Signed,
        limits,
        expected_receipt: None,
    }
}

fn execution_schedule_request(
    identity: VersionedWorkIdentity<JourneyRelation>,
    class: PlacementClass,
    fallback: bool,
    hedge: bool,
    capabilities: &backend_replication::NegotiatedCapabilities,
    output_len: usize,
) -> ScheduleRequest<JourneyRelation> {
    let key = identity.work_key();
    let local = LocalCapability::admit(
        &identity,
        LocalState::Ready,
        0,
        100,
        &move |candidate: &VersionedWorkIdentity<JourneyRelation>, state| {
            if candidate.work_key() == key && state == LocalState::Ready {
                Ok(())
            } else {
                Err(ObservationError::Rejected)
            }
        },
    )
    .unwrap_or_else(|error| panic!("local capability: {error}"));
    let remote = RemoteCapability::admit(
        &identity,
        capabilities,
        0,
        100,
        &move |candidate: &VersionedWorkIdentity<JourneyRelation>,
               caps: &backend_replication::NegotiatedCapabilities| {
            if candidate.work_key() == key && !caps.recipes.is_empty() {
                Ok(RemoteState::Warm)
            } else {
                Err(ObservationError::Rejected)
            }
        },
    )
    .unwrap_or_else(|error| panic!("remote capability: {error}"));
    let costs = CostSnapshot::admit(
        &identity,
        CostObservation {
            local: 100,
            remote: CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            observed_at: 0,
            expires_at: 100,
            confidence_per_mille: 1_000,
        },
        &move |candidate: &VersionedWorkIdentity<JourneyRelation>,
               observation: &CostObservation| {
            if candidate.work_key() == key && observation.confidence_per_mille >= 500 {
                Ok(())
            } else {
                Err(ObservationError::Rejected)
            }
        },
    )
    .unwrap_or_else(|error| panic!("cost observation: {error}"));
    let resources = ResourceVector {
        cpu_millis: 10,
        memory_bytes: 128,
        network_bytes: (b"journey-input".len() + output_len) as u64,
        storage_bytes: 128,
    };
    let request = ScheduleRequest::new(identity, 1, 1, class, local, remote, costs)
        .with_charge(output_len as u64, resources)
        .pure(true)
        .hedge(hedge);
    if fallback {
        request.with_fallback(Some(1))
    } else {
        request
    }
}

fn wire_request_for(
    plan: &DispatchPlan<JourneyRelation>,
    contract: &RemoteDispatchContract,
) -> WireRecipeRequest {
    let scheduled = match plan {
        DispatchPlan::Scheduled(scheduled) => scheduled,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => panic!("scheduled plan expected"),
    };
    let identity = scheduled.identity();
    let expected = backend_engine::dispatch::request_expectation(scheduled, contract)
        .unwrap_or_else(|error| panic!("request expectation: {error}"));
    WireRecipeRequest {
        attempt: expected.attempt,
        recipe: WireIdentity::from_typed(&identity.recipe),
        work_key: WireIdentity::from_typed(&identity.work_key()),
        inputs: contract.inputs.iter().map(|input| input.wire).collect(),
        read_manifest: WireIdentity::from_typed(&identity.read_manifest),
        scope: contract.semantic.expectation(&identity).scope,
        authority: WireAuthorityPolicy {
            id: WireIdentity::from_typed(&identity.authority),
            minimum_epoch: contract.authority_epoch,
            revocation_version: contract.revocation_version,
        },
        resources: contract.resources,
        fence: expected.fence,
        cancellation: expected.cancellation,
        input_basis: WorkspaceRootClaim::from_bytes(contract.input_basis.to_bytes()),
    }
}

fn warm_route_costs(dispatcher: &JourneyDispatcher, remote_faster: bool) {
    // Optional remote placement is admitted only after the owner has enough
    // real completion history for this coarse route class. These ten bounded
    // warmup jobs model the history a long-lived locald process accumulates;
    // they never inject a caller supplied cost snapshot.
    for value in 10..15 {
        let identity = execution_identity(value);
        let output = b"journey-remote-output".to_vec();
        let limits = execution_limits();
        let capabilities = execution_capabilities(&identity);
        let negotiated = capabilities
            .negotiate(&capabilities, limits)
            .unwrap_or_else(|error| panic!("warmup capability negotiation: {error}"));
        let mut request = execution_schedule_request(
            identity,
            PlacementClass::LocalPreferred,
            false,
            false,
            &negotiated,
            output.len(),
        );
        request.now = u64::from(!remote_faster);
        let claim = UntrustedSemanticCoverageClaim::complete_claim(
            &identity,
            1,
            b"journey-semantic-witness".to_vec(),
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        let semantic = dispatcher
            .admit_semantic_with_manifest(
                &identity,
                claim,
                DependencyManifest::new(Vec::new())
                    .unwrap_or_else(|error| panic!("warmup manifest: {error:?}")),
            )
            .unwrap_or_else(|error| panic!("warmup local semantic: {error}"));
        let plan = dispatcher
            .plan(request)
            .unwrap_or_else(|error| panic!("warmup local plan: {error}"));
        let contract = execution_contract(semantic.clone(), &output, limits);
        let result = execute_journey_worker(
            &identity,
            &plan,
            &contract,
            semantic.clone(),
            limits,
            HedgeSide::Local,
        );
        dispatcher
            .complete_local(plan, &result.output_bytes, semantic, 1)
            .unwrap_or_else(|error| panic!("warmup local completion: {error}"));
    }
    for value in 20..25 {
        let identity = execution_identity(value);
        let output = b"journey-remote-output".to_vec();
        let limits = execution_limits();
        let capabilities = execution_capabilities(&identity);
        let negotiated = capabilities
            .negotiate(&capabilities, limits)
            .unwrap_or_else(|error| panic!("warmup capability negotiation: {error}"));
        let mut request = execution_schedule_request(
            identity,
            PlacementClass::RemoteRequired,
            false,
            false,
            &negotiated,
            output.len(),
        );
        request.now = u64::from(remote_faster);
        let claim = UntrustedSemanticCoverageClaim::complete_claim(
            &identity,
            1,
            b"journey-semantic-witness".to_vec(),
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        let semantic = dispatcher
            .admit_semantic_with_manifest(
                &identity,
                claim,
                DependencyManifest::new(Vec::new())
                    .unwrap_or_else(|error| panic!("warmup manifest: {error:?}")),
            )
            .unwrap_or_else(|error| panic!("warmup remote semantic: {error}"));
        let contract = execution_contract(semantic.clone(), &output, limits);
        let plan = dispatcher
            .plan(request)
            .unwrap_or_else(|error| panic!("warmup remote plan: {error}"));
        let result = execute_journey_worker(
            &identity,
            &plan,
            &contract,
            semantic.clone(),
            limits,
            HedgeSide::Remote,
        );
        let ticket = dispatcher
            .dispatch_ticket(plan, contract)
            .unwrap_or_else(|error| panic!("warmup remote ticket: {error}"));
        dispatcher
            .complete_remote_ticket(ticket, result, 1)
            .unwrap_or_else(|error| panic!("warmup remote completion: {error}"));
    }
}

fn execute_journey_worker(
    identity: &VersionedWorkIdentity<JourneyRelation>,
    plan: &DispatchPlan<JourneyRelation>,
    contract: &RemoteDispatchContract,
    semantic: CompleteSemanticCoverage,
    limits: TransportLimits,
    side: HedgeSide,
) -> WireRecipeResult {
    let scheduled = match plan {
        DispatchPlan::Scheduled(scheduled) => scheduled,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            panic!("worker requires a scheduled attempt")
        }
    };
    let expected = backend_engine::dispatch::request_expectation(scheduled, contract)
        .unwrap_or_else(|error| panic!("worker request expectation: {error}"));
    let wire_request = wire_request_for(plan, contract);
    let capabilities = WorkerCapabilities {
        recipes: vec![identity.recipe],
        max_scope: semantic.scope(),
        max_resources: contract.resources,
    };
    let signer = Blake3WorkerSigner::new(identity.authority.to_bytes(), [0xa5; 32]);
    let endpoint = WorkerEndpoint::with_signer(capabilities, JourneyExecutor, signer, limits);
    let inputs = contract
        .inputs
        .iter()
        .map(|input| AdmittedInput {
            identity: input.wire,
            bytes: b"journey-input".to_vec().into_boxed_slice(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let cancellation = scheduled
        .cancellation(side)
        .unwrap_or_else(|| panic!("worker cancellation token"));
    endpoint
        .execute(
            &wire_request,
            &expected,
            contract.input_basis,
            identity.recipe,
            identity.read_manifest,
            identity.authority,
            identity.output_equivalence,
            identity.work_key(),
            inputs,
            semantic,
            contract.revocation_version,
            &cancellation,
            contract.resources,
        )
        .unwrap_or_else(|error| panic!("execute journey worker: {error}"))
}

struct RemoteJourneyFixture {
    dispatcher: Dispatcher<JourneyAdmissionValidator, Blake3AuthorityVerifier>,
    identity: VersionedWorkIdentity<JourneyRelation>,
    request: ScheduleRequest<JourneyRelation>,
    plan: DispatchPlan<JourneyRelation>,
    contract: RemoteDispatchContract,
    wire_request: WireRecipeRequest,
    wire_result: WireRecipeResult,
    semantic: CompleteSemanticCoverage,
    output: Vec<u8>,
    capabilities: CapabilityManifest,
    limits: TransportLimits,
}

fn remote_journey_fixture(
    class: PlacementClass,
    fallback: bool,
    hedge: bool,
) -> RemoteJourneyFixture {
    remote_journey_fixture_for(0, class, fallback, hedge)
}

fn remote_journey_fixture_for(
    value: u64,
    class: PlacementClass,
    fallback: bool,
    hedge: bool,
) -> RemoteJourneyFixture {
    let identity = execution_identity(value);
    let output = b"journey-remote-output".to_vec();
    let limits = execution_limits();
    let capabilities = execution_capabilities(&identity);
    let negotiated = capabilities
        .negotiate(&capabilities, limits)
        .unwrap_or_else(|error| panic!("negotiate remote capability: {error}"));
    let request =
        execution_schedule_request(identity, class, fallback, hedge, &negotiated, output.len());
    let mut authority = Blake3AuthorityVerifier::new();
    authority.insert_key(identity.authority.to_bytes(), [0xa5; 32]);
    let validator = JourneyAdmissionValidator {
        output: output.clone().into_boxed_slice(),
    };
    let dispatcher = Dispatcher::with_scheduler(
        journey_scheduler(),
        validator,
        authority,
        RemoteAuthorityPolicy::Signed,
    );
    if hedge || fallback {
        warm_route_costs(&dispatcher, true);
    }
    let claim = UntrustedSemanticCoverageClaim::complete_claim(
        &identity,
        1,
        b"journey-semantic-witness".to_vec(),
        AuthorityEpoch(1),
        RevocationVersion(1),
    );
    let semantic = dispatcher
        .admit_semantic_with_manifest(
            &identity,
            claim,
            DependencyManifest::new(Vec::new())
                .unwrap_or_else(|error| panic!("journey dependency manifest: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("admit semantic coverage: {error}"));
    let contract = execution_contract(semantic.clone(), &output, limits);
    let plan = dispatcher
        .plan(request)
        .unwrap_or_else(|error| panic!("plan remote journey: {error}"));
    let wire_request = wire_request_for(&plan, &contract);
    let side = match &plan {
        DispatchPlan::Scheduled(scheduled)
            if scheduled.decision() == backend_execution::PlacementDecision::Local =>
        {
            HedgeSide::Local
        }
        DispatchPlan::Scheduled(_) => HedgeSide::Remote,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            panic!("fresh journey work must be scheduled")
        }
    };
    let wire_result =
        execute_journey_worker(&identity, &plan, &contract, semantic.clone(), limits, side);
    RemoteJourneyFixture {
        dispatcher,
        identity,
        request,
        plan,
        contract,
        wire_request,
        wire_result,
        semantic,
        output,
        capabilities,
        limits,
    }
}

fn assert_remote_semantic_claim_rejections(fixture: &RemoteJourneyFixture) {
    for (scope, witness) in [
        (1, b"wrong-semantic-witness".as_slice()),
        (2, b"journey-semantic-witness".as_slice()),
    ] {
        let claim = UntrustedSemanticCoverageClaim::complete_claim(
            &fixture.identity,
            scope,
            witness.to_vec(),
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        assert!(matches!(
            fixture.dispatcher.admit_semantic(&fixture.identity, claim),
            Err(DispatchError::AuthorityRejected)
        ));
    }
    assert!(matches!(&fixture.plan, DispatchPlan::Scheduled(_)));
}

fn assert_remote_ticket_rejections() {
    let mut stale = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    stale.wire_result.fence =
        Fence::from_bytes([0x44; 32]).unwrap_or_else(|error| panic!("stale fence: {error}"));
    let stale_ticket = stale
        .dispatcher
        .dispatch_ticket(stale.plan, stale.contract.clone())
        .unwrap_or_else(|error| panic!("bind stale ticket: {error:?}"));
    assert!(matches!(
        stale
            .dispatcher
            .complete_remote_ticket(stale_ticket, stale.wire_result, 1),
        Err(DispatchError::TicketMismatch)
    ));

    let mut corrupt = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    Arc::make_mut(&mut corrupt.wire_result.output_bytes)[0] ^= 1;
    let corrupt_ticket = corrupt
        .dispatcher
        .dispatch_ticket(corrupt.plan, corrupt.contract.clone())
        .unwrap_or_else(|error| panic!("bind corrupt ticket: {error:?}"));
    assert!(matches!(
        corrupt
            .dispatcher
            .complete_remote_ticket(corrupt_ticket, corrupt.wire_result, 1),
        Err(DispatchError::OutputMismatch)
    ));

    let mut incomplete = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    incomplete.wire_result.byte_coverage =
        SparseCoverage::new(1).unwrap_or_else(|error| panic!("empty byte coverage: {error}"));
    incomplete.wire_result.attestation = Some(
        Blake3WorkerSigner::new(incomplete.identity.authority.to_bytes(), [0xa5; 32])
            .sign(&incomplete.wire_result.attestation_material())
            .unwrap_or_else(|error| panic!("resign incomplete coverage: {error}")),
    );
    let incomplete_ticket = incomplete
        .dispatcher
        .dispatch_ticket(incomplete.plan, incomplete.contract.clone())
        .unwrap_or_else(|error| panic!("bind incomplete ticket: {error:?}"));
    let incomplete_result =
        incomplete
            .dispatcher
            .complete_remote_ticket(incomplete_ticket, incomplete.wire_result, 1);
    assert!(
        matches!(incomplete_result, Err(DispatchError::IncompleteCoverage)),
        "unexpected incomplete coverage handling: {incomplete_result:?}"
    );

    let mut wrong_key = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let wrong_identity = execution_identity(1);
    wrong_key.wire_result.work_key = WireIdentity::from_typed(&wrong_identity.work_key());
    let wrong_key_ticket = wrong_key
        .dispatcher
        .dispatch_ticket(wrong_key.plan, wrong_key.contract.clone())
        .unwrap_or_else(|error| panic!("bind wrong-key ticket: {error:?}"));
    let wrong_key_result =
        wrong_key
            .dispatcher
            .complete_remote_ticket(wrong_key_ticket, wrong_key.wire_result, 1);
    assert!(
        matches!(&wrong_key_result, Err(DispatchError::TicketMismatch)),
        "unexpected wrong-key result: {wrong_key_result:?}"
    );
}

fn assert_remote_identity_rejections() {
    let mut wrong_authority = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let authority = AuthorityVersion::from_value(b"wrong-executor-authority");
    wrong_authority.wire_result.authority.id = WireIdentity::from_typed(&authority);
    let wrong_authority_ticket = wrong_authority
        .dispatcher
        .dispatch_ticket(wrong_authority.plan, wrong_authority.contract.clone())
        .unwrap_or_else(|error| panic!("bind wrong-authority ticket: {error:?}"));
    assert!(matches!(
        wrong_authority.dispatcher.complete_remote_ticket(
            wrong_authority_ticket,
            wrong_authority.wire_result,
            1,
        ),
        Err(DispatchError::Replication(
            ReplicationError::IdentityMismatch
        ))
    ));

    let mut bad_attestation = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let mut attestation = bad_attestation
        .wire_result
        .attestation
        .unwrap_or_else(|| panic!("signed worker result"));
    attestation.0[0] ^= 1;
    bad_attestation.wire_result.attestation = Some(attestation);
    assert!(matches!(&bad_attestation.plan, DispatchPlan::Scheduled(_)));
    let bad_attestation_ticket = bad_attestation
        .dispatcher
        .dispatch_ticket(bad_attestation.plan, bad_attestation.contract.clone())
        .unwrap_or_else(|error| panic!("bind bad-attestation ticket: {error:?}"));
    assert!(matches!(
        bad_attestation.dispatcher.complete_remote_ticket(
            bad_attestation_ticket,
            bad_attestation.wire_result,
            1,
        ),
        Err(DispatchError::Replication(
            ReplicationError::InvalidAttestation
        ))
    ));
}

#[test]
fn authenticated_remote_result_uses_the_same_bounded_transport_and_publishes() {
    let fixture = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let (mut sender, mut receiver) = LoopbackTransport::pair(2);
    sender.set_peer_capabilities(fixture.capabilities.clone());
    let negotiated = sender
        .negotiate(&fixture.capabilities, fixture.limits)
        .unwrap_or_else(|error| panic!("negotiate transport: {error}"));
    assert!(!negotiated.recipes.is_empty());
    sender
        .send(
            TransportMessage::WireRecipeRequest(fixture.wire_request.clone()),
            fixture.limits,
        )
        .unwrap_or_else(|error| panic!("send remote request: {error}"));
    assert!(matches!(
        receiver
            .recv()
            .unwrap_or_else(|error| panic!("receive remote request: {error}")),
        Some(TransportMessage::WireRecipeRequest(_))
    ));
    sender
        .send(
            TransportMessage::WireRecipeResult(Box::new(fixture.wire_result.clone())),
            fixture.limits,
        )
        .unwrap_or_else(|error| panic!("send remote result: {error}"));
    let received_result = match receiver
        .recv()
        .unwrap_or_else(|error| panic!("receive remote result: {error}"))
    {
        Some(TransportMessage::WireRecipeResult(result)) => result,
        other => panic!("unexpected remote result: {other:?}"),
    };
    assert_eq!(*received_result, fixture.wire_result);

    let ticket = fixture
        .dispatcher
        .dispatch_ticket(fixture.plan, fixture.contract.clone())
        .unwrap_or_else(|error| panic!("bind authenticated remote ticket: {error:?}"));
    let completion = fixture
        .dispatcher
        .complete_remote_ticket(ticket, (*received_result).clone(), 1)
        .unwrap_or_else(|error| panic!("complete authenticated remote result: {error}"));
    assert!(matches!(completion, DispatchCompletion::Accepted(receipt)
        if receipt.output() == backend_execution::OutputVersion::from_value(fixture.output.as_slice())));

    sender.disconnect();
    assert_eq!(
        sender.send(
            TransportMessage::WireRecipeRequest(fixture.wire_request),
            fixture.limits,
        ),
        Err(ReplicationError::Disconnected)
    );
    sender.reconnect();
}

#[test]
fn remote_stale_corrupt_late_wrong_coverage_and_wrong_key_results_are_rejected() {
    let semantic_rejected = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    assert_remote_semantic_claim_rejections(&semantic_rejected);

    assert_remote_ticket_rejections();
    assert_remote_identity_rejections();

    let late = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let late_ticket = late
        .dispatcher
        .dispatch_ticket(late.plan, late.contract.clone())
        .unwrap_or_else(|error| panic!("bind late ticket: {error:?}"));
    late.dispatcher
        .complete_remote_ticket(late_ticket, late.wire_result.clone(), 1)
        .unwrap_or_else(|error| panic!("complete first remote result: {error}"));
    let reused = late
        .dispatcher
        .plan_with_semantic(late.request, &late.semantic)
        .unwrap_or_else(|error| panic!("reuse exact remote result: {error}"));
    assert!(matches!(&reused, DispatchPlan::Reused(output)
        if output.output() == backend_execution::OutputVersion::from_value(late.output.as_slice())));
    assert!(matches!(
        late.dispatcher
            .dispatch_ticket(reused, late.contract.clone()),
        Err(DispatchError::AuthorityRejected)
    ));
}

#[test]
fn remote_outage_activates_the_reserved_local_fallback_and_cancellation_stops_work() {
    let fallback = remote_journey_fixture(PlacementClass::RemoteOptional, true, false);
    let (mut transport, _peer) = LoopbackTransport::pair(1);
    transport.set_peer_capabilities(fallback.capabilities.clone());
    transport.disconnect();
    assert_eq!(
        transport.send(
            TransportMessage::WireRecipeRequest(fallback.wire_request.clone()),
            fallback.limits,
        ),
        Err(ReplicationError::Disconnected)
    );
    let scheduled = match fallback.plan {
        DispatchPlan::Scheduled(scheduled) => scheduled,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            panic!("fallback must schedule remote work")
        }
    };
    assert_eq!(
        scheduled.decision(),
        backend_execution::PlacementDecision::Remote
    );
    let mut scheduled = scheduled;
    scheduled
        .activate_fallback(1)
        .unwrap_or_else(|error| panic!("activate offline local fallback: {error}"));
    let completion = fallback
        .dispatcher
        .complete_local(
            DispatchPlan::Scheduled(scheduled),
            &fallback.output,
            fallback.semantic,
            2,
        )
        .unwrap_or_else(|error| panic!("complete local fallback: {error}"));
    assert!(
        matches!(completion, DispatchCompletion::Accepted(receipt) if receipt.decision() == backend_execution::PlacementDecision::Local)
    );

    let cancelled = remote_journey_fixture(PlacementClass::RemoteRequired, false, false);
    let observation = cancelled
        .dispatcher
        .cancellation(&cancelled.plan, HedgeSide::Remote)
        .unwrap_or_else(|| panic!("remote cancellation observation"));
    let retry_request = cancelled.request;
    cancelled.dispatcher.cancel(cancelled.plan);
    assert!(observation.is_cancelled());
    assert!(matches!(
        cancelled.dispatcher.plan(retry_request),
        Ok(DispatchPlan::Scheduled(_))
    ));
}

#[test]
fn local_execution_publishes_once_then_exact_reuse_is_a_noop() {
    let fixture = remote_journey_fixture(PlacementClass::LocalPreferred, false, false);
    let RemoteJourneyFixture {
        dispatcher,
        request,
        plan,
        semantic,
        output: expected_output,
        wire_result,
        ..
    } = fixture;
    let scheduled = match plan {
        DispatchPlan::Scheduled(scheduled) => scheduled,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            panic!("fresh local work must be scheduled")
        }
    };
    assert_eq!(
        scheduled.decision(),
        backend_execution::PlacementDecision::Local
    );
    assert_eq!(
        wire_result.output_bytes.as_slice(),
        expected_output.as_slice()
    );
    let completion = dispatcher
        .complete_local(
            DispatchPlan::Scheduled(scheduled),
            &wire_result.output_bytes,
            semantic.clone(),
            1,
        )
        .unwrap_or_else(|error| panic!("complete local execution: {error}"));
    assert!(matches!(
        completion,
        DispatchCompletion::Accepted(receipt)
            if receipt.decision() == backend_execution::PlacementDecision::Local
                && receipt.output()
                    == backend_execution::OutputVersion::from_value(expected_output.as_slice())
    ));
    let reused = dispatcher
        .plan_with_semantic(request, &semantic)
        .unwrap_or_else(|error| panic!("plan exact local reuse: {error}"));
    assert!(
        matches!(
            &reused,
            DispatchPlan::Reused(output_version)
                if output_version.output()
                    == backend_execution::OutputVersion::from_value(expected_output.as_slice())
        ),
        "reused plan: {reused:?}"
    );
}

#[test]
fn local_and_remote_hedge_publishes_one_valid_result_and_cancels_the_loser() {
    let fixture = remote_journey_fixture(PlacementClass::LocalPreferred, false, true);
    let RemoteJourneyFixture {
        dispatcher,
        identity,
        request,
        plan,
        contract,
        semantic,
        output: expected_output,
        limits,
        ..
    } = fixture;
    let scheduled = match &plan {
        DispatchPlan::Scheduled(scheduled) => scheduled,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => {
            panic!("fresh hedge work must be scheduled")
        }
    };
    assert_eq!(
        scheduled.decision(),
        backend_execution::PlacementDecision::Hedge
    );
    let remote_cancel = dispatcher
        .cancellation(&plan, HedgeSide::Remote)
        .unwrap_or_else(|| panic!("hedge has a remote cancellation token"));
    let local_result = execute_journey_worker(
        &identity,
        &plan,
        &contract,
        semantic.clone(),
        limits,
        HedgeSide::Local,
    );
    assert_eq!(
        local_result.output_bytes.as_slice(),
        expected_output.as_slice()
    );
    let completion = dispatcher
        .complete_local(plan, &local_result.output_bytes, semantic.clone(), 1)
        .unwrap_or_else(|error| panic!("complete winning local hedge: {error}"));
    assert!(matches!(
        completion,
        DispatchCompletion::Accepted(receipt)
            if receipt.decision() == backend_execution::PlacementDecision::Hedge
    ));
    assert!(remote_cancel.is_cancelled());
    assert!(matches!(
        dispatcher
            .plan_with_semantic(request, &semantic)
            .unwrap_or_else(|error| panic!("reuse hedged output: {error}")),
        DispatchPlan::Reused(output)
            if output.output() == backend_execution::OutputVersion::from_value(expected_output.as_slice())
    ));
}

#[test]
fn locald_replication_completion_and_subscription_lanes_use_admitted_inputs() {
    let path = unique_directory("locald-control-lanes");
    let mut daemon = open_journey_locald(&path, journey_dispatcher(), daemon_config());
    let complete_library =
        Library::with_coverage(library_coverage(object_version(b"library-source-v1")))
            .unwrap_or_else(|error| panic!("complete control-lane library: {error}"));
    daemon
        .engine_mut()
        .daemon_mut()
        .publish_view(
            complete_library.view().clone(),
            complete_library.cursor(),
            &admit_process_view,
            None,
        )
        .unwrap_or_else(|error| panic!("publish complete control-lane view: {error}"));
    let fixture = remote_journey_fixture(PlacementClass::LocalPreferred, false, false);
    let daemon_plan = daemon
        .engine()
        .plan_with_semantic(fixture.request, &fixture.semantic)
        .unwrap_or_else(|error| panic!("plan locald completion: {error}"));
    let completion = daemon
        .engine()
        .daemon()
        .dispatcher()
        .complete_local(daemon_plan, &fixture.output, fixture.semantic.clone(), 1)
        .unwrap_or_else(|error| panic!("admit completion for locald: {error}"));
    let receipt = match completion {
        DispatchCompletion::Accepted(receipt) => receipt,
        DispatchCompletion::Reused(_) | DispatchCompletion::Waiting(_) => {
            panic!("new remote completion expected")
        }
    };

    let replicated = daemon
        .client()
        .request(
            11,
            backend_locald::Request::Replicate(Box::new(TransportMessage::Capabilities(
                fixture.capabilities.clone(),
            ))),
        )
        .unwrap_or_else(|error| panic!("queue replication: {error:?}"));
    assert!(daemon.serve_one());
    assert!(matches!(
        replicated
            .recv()
            .unwrap_or_else(|error| panic!("replication reply: {error}")),
        DaemonReply::Replicated(Ok(backend_engine::ReplicationReply::Validated { bytes }))
            if bytes > 0
    ));

    let completed = daemon
        .client()
        .request(
            12,
            backend_locald::Request::Complete(
                backend_engine::daemon::CompletionNotice::from_receipt(&receipt),
            ),
        )
        .unwrap_or_else(|error| panic!("queue completion: {error:?}"));
    assert!(daemon.serve_one());
    assert!(matches!(
        completed
            .recv()
            .unwrap_or_else(|error| panic!("completion reply: {error}")),
        DaemonReply::Completed(Ok(()))
    ));

    let subscribed = daemon
        .client()
        .request(
            13,
            backend_locald::Request::Subscribe {
                cursor: daemon.engine().daemon().cursor_bytes(),
                credit: 2,
            },
        )
        .unwrap_or_else(|error| panic!("queue subscription: {error:?}"));
    assert!(daemon.serve_one());
    let subscription = subscribed
        .recv()
        .unwrap_or_else(|error| panic!("subscription reply: {error}"));
    assert!(
        matches!(
            subscription,
            DaemonReply::Subscribed(Ok(backend_engine::SubscriptionReply::Accepted {
                credit: 2
            }))
        ),
        "unexpected subscription reply: {subscription:?}"
    );

    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup control-lane locald: {error}"));
}

#[cfg(unix)]
#[test]
fn cli_and_mcp_framed_clients_share_one_locald_service_adapter() {
    let path = unique_directory("framed-service");
    let mut daemon = open_journey_locald(&path, journey_dispatcher(), daemon_config());
    let complete_library =
        Library::with_coverage(library_coverage(object_version(b"library-source-v1")))
            .unwrap_or_else(|error| panic!("complete process library: {error}"));
    daemon
        .engine_mut()
        .daemon_mut()
        .publish_view(
            complete_library.view().clone(),
            complete_library.cursor(),
            &admit_process_view,
            None,
        )
        .unwrap_or_else(|error| panic!("publish complete process view: {error}"));
    let request = CommandDto::new(41, Command::Health);
    let expected_command = request.clone();
    let owner = daemon.into_owner(move |daemon: &mut JourneyLocald, body: &[u8]| {
        let request =
            CommandDto::decode_against(body, &expected_command).map_err(|error| error.clone())?;
        let mut reply = daemon.engine().daemon().library().execute_dto(request);
        if let backend_library::CommandReply::Health(root) = &reply.reply {
            let cursor = daemon.engine().daemon().library().cursor();
            reply = backend_engine::ReplyDto::health(reply.request_id, root.clone(), cursor)
                .with_certificate(journey_health_certificate(root, cursor));
        }
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    });
    let service = Arc::new(Mutex::new(
        backend_locald::LocaldService::new(owner, backend_locald::FrameLimits::default())
            .unwrap_or_else(|error| panic!("create locald service: {error}")),
    ));
    let mut expected = service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .owner()
        .daemon()
        .engine()
        .daemon()
        .library()
        .execute_dto(request.clone());
    if let backend_library::CommandReply::Health(root) = &expected.reply {
        let cursor = service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owner()
            .daemon()
            .engine()
            .daemon()
            .library()
            .cursor();
        expected = backend_engine::ReplyDto::health(expected.request_id, root.clone(), cursor)
            .with_certificate(journey_health_certificate(root, cursor));
    }

    let (client_stream, server_stream) = std::os::unix::net::UnixStream::pair()
        .unwrap_or_else(|error| panic!("cli stream pair: {error}"));
    let service_for_cli = Arc::clone(&service);
    let server = std::thread::spawn(move || {
        let mut stream = server_stream;
        service_for_cli
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .serve_one_frame(&mut stream)
            .unwrap_or_else(|error| panic!("serve cli frame: {error}"));
    });
    let mut cli = backend_cli::UnixCommandTransport::from_stream(client_stream);
    let cli_reply = cli
        .request_against(&request, &expected)
        .unwrap_or_else(|error| panic!("cli framed request: {error}"));
    server
        .join()
        .unwrap_or_else(|_| panic!("cli server thread"));

    let (client_stream, server_stream) = std::os::unix::net::UnixStream::pair()
        .unwrap_or_else(|error| panic!("mcp stream pair: {error}"));
    let service_for_mcp = Arc::clone(&service);
    let server = std::thread::spawn(move || {
        let mut stream = server_stream;
        service_for_mcp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .serve_one_frame(&mut stream)
            .unwrap_or_else(|error| panic!("serve mcp frame: {error}"));
    });
    let mut mcp = backend_mcp::UnixCommandTransport::from_stream(client_stream);
    let mcp_reply = mcp
        .request_against(&request, &expected)
        .unwrap_or_else(|error| panic!("mcp framed request: {error}"));
    server
        .join()
        .unwrap_or_else(|_| panic!("mcp server thread"));
    assert_eq!(cli_reply, mcp_reply);
    // The owner answers `Health` with a constant-size readiness report; the
    // whole-view `Health` shape is the compatibility spelling a legacy peer
    // may still send. Both are the same command, and this case is about the
    // two transports agreeing, not about which of the two shapes arrived.
    assert!(
        matches!(
            cli_reply.reply,
            backend_library::CommandReply::Readiness(_) | backend_library::CommandReply::Health(_)
        ),
        "a framed health request answered with something other than health: {:?}",
        cli_reply.reply
    );

    drop(service);
    std::fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup framed service: {error}"));
}

#[cfg(not(unix))]
#[test]
fn cli_and_mcp_framed_clients_share_one_locald_service_adapter() {}

/// Small typed effect fixture used by the checkpoint/compaction journey.  The
/// sink is deliberately deterministic: the test is about journal generation
/// selection and recovery, so no external effect is allowed to add timing or
/// outcome ambiguity to the observation.
#[derive(Clone, Copy, Debug, Default)]
struct JourneyEffectSpec;

impl EffectSpec for JourneyEffectSpec {
    type Intent = Arc<[u8]>;
    type Request = Arc<[u8]>;
    type Receipt = Arc<[u8]>;

    fn key(&self, intent: &Self::Intent) -> EffectKey {
        effect_key(intent.as_ref())
    }

    fn request(&self, intent: &Self::Intent) -> Self::Request {
        Arc::clone(intent)
    }

    fn validate_receipt(
        &self,
        key: EffectKey,
        request: &Self::Request,
        receipt: &Self::Receipt,
    ) -> Result<(), EffectError> {
        if effect_key(request.as_ref()) == key && receipt.as_ref() == request.as_ref() {
            Ok(())
        } else {
            Err(EffectError::InvalidReceipt(
                "journey effect receipt does not bind to request".to_owned(),
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct JourneyEffectCodec;

impl EffectCodec<JourneyEffectSpec> for JourneyEffectCodec {
    fn encode_intent(&self, intent: &Arc<[u8]>) -> Result<Vec<u8>, EffectError> {
        Ok(intent.to_vec())
    }

    fn decode_intent(&self, bytes: &[u8]) -> Result<Arc<[u8]>, EffectError> {
        Ok(Arc::from(bytes))
    }

    fn encode_receipt(&self, receipt: &Arc<[u8]>) -> Result<Vec<u8>, EffectError> {
        Ok(receipt.to_vec())
    }

    fn decode_receipt(&self, bytes: &[u8]) -> Result<Arc<[u8]>, EffectError> {
        Ok(Arc::from(bytes))
    }
}

#[derive(Default)]
struct JourneyEffectSink;

impl EffectSink<JourneyEffectSpec> for JourneyEffectSink {
    fn apply(
        &mut self,
        _key: EffectKey,
        request: Arc<[u8]>,
    ) -> Result<SinkApply<Arc<[u8]>>, SinkError> {
        Ok(SinkApply::Confirmed(request))
    }

    fn reconcile(&mut self, _key: EffectKey) -> Result<SinkObservation<Arc<[u8]>>, SinkError> {
        Ok(SinkObservation::Absent)
    }
}

fn assert_forged_checkpoint_selection_is_rejected(path: &std::path::Path, limits: JournalLimits) {
    let pointer = path
        .parent()
        .unwrap_or_else(|| panic!("journal path has no parent"))
        .join("journal.snapshot.pointer");
    let mut pointer_bytes = std::fs::read(&pointer)
        .unwrap_or_else(|error| panic!("read selected snapshot pointer: {error}"));
    pointer_bytes[0] ^= 1;
    std::fs::write(&pointer, pointer_bytes)
        .unwrap_or_else(|error| panic!("forge selected snapshot pointer: {error}"));
    assert!(
        EffectJournalPersistence::<JourneyEffectSpec, JourneyEffectCodec>::open_streaming(
            path,
            JourneyEffectCodec,
            limits,
        )
        .is_err(),
        "forged checkpoint selection must be rejected"
    );
}

fn assert_checkpoint_marker(path: &std::path::Path) {
    assert!(
        std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("compacted journal metadata: {error}"))
            .len()
            > 0,
        "compaction must leave an authenticated generation marker"
    );
}

#[test]
fn checkpoint_compaction_retains_effect_audit_and_rejects_forged_selection() {
    let root = unique_directory("effect-checkpoint");
    let path = root.join("journal");
    let limits = JournalLimits {
        max_frames: 32,
        max_bytes: 1024 * 1024,
    };
    let (persistence, _) = EffectJournalPersistence::<JourneyEffectSpec, JourneyEffectCodec>::open(
        &path,
        JourneyEffectCodec,
    )
    .unwrap_or_else(|error| panic!("open effect journal: {error}"));
    let mut coordinator = EffectCoordinator::new(JourneyEffectSpec, persistence, JourneyEffectSink);
    let pending: Arc<[u8]> = Arc::from(&b"pending-after-checkpoint"[..]);
    let pending_handle = coordinator
        .prepare(Arc::clone(&pending))
        .unwrap_or_else(|error| panic!("prepare pending effect: {error}"));
    let confirmed: Arc<[u8]> = Arc::from(&b"confirmed-before-checkpoint"[..]);
    let confirmed_handle = coordinator
        .prepare(Arc::clone(&confirmed))
        .unwrap_or_else(|error| panic!("prepare confirmed effect: {error}"));
    coordinator
        .execute(confirmed_handle.key)
        .unwrap_or_else(|error| panic!("execute confirmed effect: {error}"));
    assert_eq!(pending_handle.phase, EffectPhase::Prepared);
    assert_eq!(
        coordinator.state(confirmed_handle.key),
        Some(EffectPhase::Confirmed)
    );

    let recovered = coordinator
        .persistence_mut()
        .replay(&JourneyEffectSpec)
        .unwrap_or_else(|error| panic!("replay before checkpoint: {error}"));
    let receipt = coordinator
        .persistence_mut()
        .checkpoint_and_compact(
            &JourneyEffectSpec,
            recovered,
            EffectSnapshotLimits {
                max_entries: 8,
                max_bytes: 64 * 1024,
            },
            limits,
        )
        .unwrap_or_else(|error| panic!("checkpoint and compact effect journal: {error}"));
    assert_ne!(receipt.record, [0; 32]);
    assert_checkpoint_marker(&path);
    drop(coordinator);

    let (mut reopened, scan) =
        EffectJournalPersistence::<JourneyEffectSpec, JourneyEffectCodec>::open_streaming(
            &path,
            JourneyEffectCodec,
            limits,
        )
        .unwrap_or_else(|error| panic!("reopen compacted effect journal: {error}"));
    assert_eq!(
        scan.frames_scanned, 1,
        "warm recovery scans only the marker"
    );
    let warm = reopened
        .replay_with_limits(&JourneyEffectSpec, limits)
        .unwrap_or_else(|error| panic!("replay compacted effect journal: {error}"));
    assert!(warm.iter().any(|entry| {
        entry.intent.as_ref() == pending.as_ref() && entry.state.phase() == EffectPhase::Prepared
    }));
    assert!(warm.iter().any(|entry| {
        entry.intent.as_ref() == confirmed.as_ref() && entry.state.phase() == EffectPhase::Confirmed
    }));

    let tail: Arc<[u8]> = Arc::from(&b"tail-after-compaction"[..]);
    let tail_key = effect_key(tail.as_ref());
    reopened
        .prepared(tail_key, &tail)
        .unwrap_or_else(|error| panic!("append post-compaction tail: {error}"));
    drop(reopened);
    let (reopened, _) =
        EffectJournalPersistence::<JourneyEffectSpec, JourneyEffectCodec>::open_streaming(
            &path,
            JourneyEffectCodec,
            limits,
        )
        .unwrap_or_else(|error| panic!("reopen compacted tail: {error}"));
    let with_tail = reopened
        .replay_with_limits(&JourneyEffectSpec, limits)
        .unwrap_or_else(|error| panic!("replay compacted tail: {error}"));
    assert_eq!(
        with_tail.len(),
        3,
        "compaction must retain the audit entries"
    );
    assert!(
        with_tail
            .iter()
            .any(|entry| entry.intent.as_ref() == tail.as_ref())
    );
    drop(reopened);

    // Mutating the selected pointer must fail closed. A replacement journal
    // is useful only when its snapshot selection remains authenticated.
    assert_forged_checkpoint_selection_is_rejected(&path, limits);
    std::fs::remove_dir_all(root)
        .unwrap_or_else(|error| panic!("cleanup effect checkpoint journey: {error}"));
}

/// Scripted transport used only at the daemon's public remote seam.  The
/// queue is shared with the test so frames can be injected after the daemon
/// owns a pending ticket, while the daemon still performs all negotiation,
/// correlation, admission, fallback, and cancellation work itself.
#[derive(Clone, Debug, Default)]
struct ScriptedRemoteEvents {
    sent: Vec<TransportMessage>,
    cancelled: Vec<CancelAttempt>,
}

struct ScriptedRemote {
    peer: CapabilityManifest,
    inbound: Arc<Mutex<VecDeque<TransportMessage>>>,
    events: Arc<Mutex<ScriptedRemoteEvents>>,
}

type ScriptedRemoteParts = (
    ScriptedRemote,
    Arc<Mutex<VecDeque<TransportMessage>>>,
    Arc<Mutex<ScriptedRemoteEvents>>,
);

impl ScriptedRemote {
    fn new(peer: CapabilityManifest, inbound: VecDeque<TransportMessage>) -> ScriptedRemoteParts {
        let inbound = Arc::new(Mutex::new(inbound));
        let events = Arc::new(Mutex::new(ScriptedRemoteEvents::default()));
        (
            Self {
                peer,
                inbound: Arc::clone(&inbound),
                events: Arc::clone(&events),
            },
            inbound,
            events,
        )
    }
}

impl RemoteTransport for ScriptedRemote {
    fn negotiate(
        &mut self,
        local: &CapabilityManifest,
        limits: TransportLimits,
    ) -> Result<backend_replication::NegotiatedCapabilities, ReplicationError> {
        local.negotiate(&self.peer, limits)
    }

    fn send(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        if message.estimated_size() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.events
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?
            .sent
            .push(message);
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<TransportMessage>, ReplicationError> {
        self.inbound
            .lock()
            .map_err(|_| ReplicationError::Disconnected)
            .map(|mut inbound| inbound.pop_front())
    }

    fn cancel(&mut self, cancellation: CancelAttempt) -> Result<(), ReplicationError> {
        self.events
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?
            .cancelled
            .push(cancellation);
        Ok(())
    }
}

fn combined_execution_capabilities(
    first: &CapabilityManifest,
    second: &CapabilityManifest,
) -> CapabilityManifest {
    let mut combined = first.clone();
    combined.recipes.extend(second.recipes.iter().copied());
    combined.recipes.sort_by_key(|recipe| recipe.recipe);
    combined.recipes.dedup_by_key(|recipe| recipe.recipe);
    combined
}

fn complete_daemon_fallback(
    daemon: &mut JourneyDaemon,
    second: &mut RemoteJourneyFixture,
    events: &Mutex<ScriptedRemoteEvents>,
) {
    second.request.now = 13;
    let fallback_plan = daemon
        .dispatcher()
        .plan(second.request)
        .unwrap_or_else(|error| panic!("plan remote fallback attempt: {error}"));
    let fallback_decision = match &fallback_plan {
        DispatchPlan::Scheduled(scheduled) => format!("{:?}", scheduled.decision()),
        DispatchPlan::Reused(_) => "Reused".to_string(),
        DispatchPlan::Waiting(_) => "Waiting".to_string(),
    };
    let ticket = daemon
        .dispatch_remote_ticket(fallback_plan, second.contract.clone(), &second.capabilities)
        .unwrap_or_else(|error| {
            panic!("send fallback ticket: {error:?}; decision={fallback_decision}")
        });
    let fallback_result = daemon.fallback_remote(ticket, &second.output, 13);
    assert!(
        matches!(&fallback_result, Ok(DispatchCompletion::Accepted(_))),
        "unexpected daemon fallback result: {fallback_result:?}"
    );
    let events = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        events
            .cancelled
            .iter()
            .any(|cancel| cancel.attempt.get() != 0),
        "fallback must send an authenticated remote cancellation"
    );
    assert!(daemon.replication_backlog_len() <= 1);
}

#[test]
fn daemon_owns_remote_ticket_correlation_backpressure_fallback_and_reusable_bytes() {
    let path = unique_directory("daemon-remote-ticket");
    let owner = open_journey_owner(&path);
    let config = DaemonConfig {
        // One pending remote attempt is the explicit bounded capacity under
        // test. The byte budget remains large enough for one canonical frame.
        replication: QueueBudget::new(1, 8 * 1024),
        ..daemon_config()
    };
    let mut daemon = backend_engine::Daemon::new(owner, journey_dispatcher(), config);
    let first = remote_journey_fixture_for(0, PlacementClass::RemoteRequired, false, false);
    // Fallback is a local reserve attached to an opportunistic remote route;
    // a remote-required request intentionally has no local fallback.
    let mut second = remote_journey_fixture_for(1, PlacementClass::RemoteOptional, true, false);
    let peer = combined_execution_capabilities(&first.capabilities, &second.capabilities);
    let mut wrong = first.wire_result.clone();
    wrong.attempt = backend_replication::AttemptId::new(2)
        .unwrap_or_else(|error| panic!("out-of-order attempt: {error}"));
    let inbound = VecDeque::from([
        TransportMessage::WireRecipeResult(Box::new(wrong)),
        TransportMessage::WireRecipeResult(Box::new(first.wire_result.clone())),
    ]);
    let (remote, inbound, events) = ScriptedRemote::new(peer, inbound);
    daemon.set_remote_transport(Box::new(remote));

    // Build the plan from the daemon's scheduler. The fixture's independent
    // dispatcher is used only for typed semantic/output material and to
    // produce a valid worker result for this daemon-owned plan below.
    let first_plan = daemon
        .dispatcher()
        .plan(first.request)
        .unwrap_or_else(|error| panic!("daemon plan first remote attempt: {error}"));
    let first_wire_result = execute_journey_worker(
        &first.identity,
        &first_plan,
        &first.contract,
        first.semantic.clone(),
        first.limits,
        HedgeSide::Remote,
    );
    {
        let mut inbound = inbound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inbound.pop_back();
        inbound.push_back(TransportMessage::WireRecipeResult(Box::new(
            first_wire_result.clone(),
        )));
    }
    let pending = daemon
        .dispatch_remote_pending(first_plan, first.contract.clone(), &first.capabilities)
        .unwrap_or_else(|error| panic!("register daemon-owned remote ticket: {error:?}"));
    assert_eq!(daemon.replication_backlog_len(), 0);

    // A second ticket cannot exceed the daemon's configured pending count.
    // Dropping the second plan on this error also releases its scheduler
    // reservation, which the fallback path below checks by scheduling it
    // again after the first ticket completes.
    let second_plan = daemon
        .dispatcher()
        .plan(second.request)
        .unwrap_or_else(|error| panic!("daemon plan bounded second attempt: {error}"));
    let second_decision = match &second_plan {
        DispatchPlan::Scheduled(scheduled) => format!("{:?}", scheduled.decision()),
        DispatchPlan::Reused(_) => "Reused".to_string(),
        DispatchPlan::Waiting(_) => "Waiting".to_string(),
    };
    let second_pending =
        daemon.dispatch_remote_pending(second_plan, second.contract.clone(), &second.capabilities);
    assert!(
        matches!(&second_pending, Err(DaemonError::Backpressure)),
        "unexpected bounded second attempt result: {second_pending:?}; decision={second_decision}"
    );

    // The result for a different attempt is retained for diagnostics while
    // the receive loop continues to the valid frame for this ticket.
    let completed = daemon.receive_remote_pending(pending, 1);
    assert!(
        matches!(completed, Ok(DispatchCompletion::Accepted(_))),
        "unexpected pending result handling: {completed:?}"
    );
    assert_eq!(daemon.replication_backlog_len(), 1);
    assert!(matches!(
        daemon.drain_remote_quarantine(),
        Some(TransportMessage::WireRecipeResult(_))
    ));

    // A late duplicate cannot republish. It is buffered as an unmatched
    // frame, leaving the accepted output's exact canonical bytes reusable.
    inbound
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(TransportMessage::WireRecipeResult(Box::new(
            first_wire_result,
        )));
    assert!(matches!(
        daemon.receive_remote(12),
        Err(DaemonError::RemoteResultUnmatched)
    ));
    let reused = daemon
        .dispatcher()
        .plan_with_semantic(first.request, &first.semantic)
        .unwrap_or_else(|error| panic!("plan retained result reuse: {error}"));
    match reused {
        DispatchPlan::Reused(output) => {
            assert_eq!(output.canonical_bytes(), first.output.as_slice());
        }
        DispatchPlan::Scheduled(_) | DispatchPlan::Waiting(_) => {
            panic!("accepted output was not retained for reuse")
        }
    }

    // The second identity is schedulable after its dropped reservation. Its
    // typed ticket owns the fallback/cancel command and publishes local bytes.
    complete_daemon_fallback(&mut daemon, &mut second, &events);
    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup remote-ticket journey: {error}"));
}

#[test]
fn daemon_prepares_fallback_custody_without_transmitting_a_recipe() {
    let path = unique_directory("daemon-prepared-fallback");
    let owner = open_journey_owner(&path);
    let mut daemon = backend_engine::Daemon::new(owner, journey_dispatcher(), daemon_config());
    let fixture = remote_journey_fixture_for(7, PlacementClass::RemoteOptional, true, false);
    let (remote, _inbound, events) =
        ScriptedRemote::new(fixture.capabilities.clone(), VecDeque::new());
    daemon.set_remote_transport(Box::new(remote));

    let plan = daemon
        .dispatcher()
        .plan(fixture.request)
        .unwrap_or_else(|error| panic!("plan prepared fallback: {error}"));
    let key = daemon
        .prepare_remote_pending(plan, fixture.contract.clone())
        .unwrap_or_else(|error| panic!("retain prepared fallback: {error:?}"));
    {
        let observed = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            observed.sent.is_empty(),
            "fallback preparation must not expose a recipe without its closure"
        );
        assert!(observed.cancelled.is_empty());
    }

    let completion = daemon.fallback_remote_pending(key, Arc::new(fixture.output.clone()), 13);
    assert!(matches!(completion, Ok(DispatchCompletion::Accepted(_))));
    let observed = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(observed.sent.is_empty());
    assert_eq!(observed.cancelled.len(), 1);
    assert_eq!(observed.cancelled[0].attempt, key.attempt());
    drop(observed);
    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup prepared-fallback journey: {error}"));
}

#[test]
fn daemon_reopens_pending_remote_dispatch_into_fenced_fallback() {
    let path = unique_directory("daemon-remote-restart");
    let journal_path = path.join(backend_engine::daemon::DISPATCH_JOURNAL_FILE);
    let first = remote_journey_fixture_for(4, PlacementClass::RemoteRequired, false, false);
    let peer = first.capabilities.clone();
    let (remote, _inbound, _events) = ScriptedRemote::new(peer, VecDeque::new());
    let owner = open_journey_owner(&path);
    let (journal, _recovery) =
        DispatchJournal::open(&journal_path, DispatchJournalLimits::default())
            .unwrap_or_else(|error| panic!("open dispatch journal: {error:?}"));
    let mut daemon = backend_engine::Daemon::new_with_dispatch_journal(
        owner,
        journey_dispatcher(),
        daemon_config(),
        journal,
        1,
    )
    .unwrap_or_else(|error| panic!("compose durable daemon: {error:?}"));
    daemon.set_remote_transport(Box::new(remote));
    let plan = daemon
        .dispatcher()
        .plan(first.request)
        .unwrap_or_else(|error| panic!("plan pending restart attempt: {error}"));
    let pending = daemon
        .dispatch_remote_pending(plan, first.contract.clone(), &first.capabilities)
        .unwrap_or_else(|error| panic!("admit pending restart attempt: {error:?}"));
    let journal = daemon
        .dispatch_journal()
        .unwrap_or_else(|| panic!("durable daemon journal missing"));
    let key = backend_engine::DispatchAttemptKey::new(
        pending.work_key().to_bytes(),
        pending.attempt().get(),
    )
    .unwrap_or_else(|error| panic!("pending journal key: {error}"));
    let before = journal
        .snapshot()
        .unwrap_or_else(|error| panic!("snapshot pending journal: {error:?}"));
    assert_eq!(
        before.get(key).map(|attempt| attempt.phase),
        Some(backend_engine::DispatchPhase::Executing)
    );
    drop(daemon);

    let owner = open_journey_owner(&path);
    let (journal, _recovery) =
        DispatchJournal::open(&journal_path, DispatchJournalLimits::default())
            .unwrap_or_else(|error| panic!("reopen dispatch journal: {error:?}"));
    let daemon = backend_engine::Daemon::new_with_dispatch_journal(
        owner,
        journey_dispatcher(),
        daemon_config(),
        journal,
        1,
    )
    .unwrap_or_else(|error| panic!("recover durable daemon: {error:?}"));
    assert!(daemon.recovered_dispatch().any(|action| matches!(
        action,
        DispatchRecoveryAction::Fallback { attempt, .. } if attempt.key() == key
    )));
    let recovered = daemon
        .dispatch_journal()
        .and_then(|journal| journal.snapshot().ok())
        .and_then(|recovery| recovery.get(key).cloned())
        .unwrap_or_else(|| panic!("recovered pending attempt missing"));
    assert_eq!(recovered.phase, backend_engine::DispatchPhase::Fallback);
    drop(daemon);
    std::fs::remove_dir_all(path)
        .unwrap_or_else(|error| panic!("cleanup remote-restart journey: {error}"));
}
