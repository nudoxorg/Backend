//! Compiled profile identities, model, validator, and dispatcher.
//!
//! This module is the profile composition boundary. It owns the registered
//! relation recipe and its checked authority/semantic contracts; process and
//! transport state stay in the parent façade and the replication module.

use super::{
    AUTHORITY_VALUE, Arc, AuthorityVersionSchema, Blake3AuthorityVerifier, Budget, ClosureManifest,
    Commit, CommitProvenance, CompleteSemanticCoverage, CompositeAdmissionValidator,
    DependencyManifest, DispatchError, Dispatcher, ECHO_AUTHORITY_SECRET, ObjectClosure, ObjectKey,
    ObjectVersion, OutputVersion, PreparedTransition, RelationAdmissionRegistry,
    RelationTransition, RemoteAuthorityPolicy, ResourceVector, Scheduler, Schema,
    SemanticCoverageAdmissionError, SemanticCoverageBinding, TransactionId, TypedObject,
    UntrustedSemanticCoverageClaim, WorkspaceClosure, WorkspaceDelta, WorkspaceManifest,
    WorkspaceModel, WorkspaceSnapshot, admit_delta, admit_manifest, fmt, transition_closure_lazy,
    workspace_manifest_from_root,
};
use backend_engine::{
    LazyPreparedUpdate, SemanticCoverageValidator, TransitionWork, WorkspaceRelationHandle,
};

pub(super) use backend_engine::builtin::{
    ECHO_OUTPUT_BYTES, PRODUCT_OUTPUT_BYTES, ProductSourceRelation as ProductRelation,
    Profile as BuiltinProfile, ProfileDescriptor, ProfileIds, execution_input_basis,
    execution_manifest, execution_resources, output_bytes, product_dependency_manifest,
    product_input_bytes, product_input_version, product_source_fixture_with_authority,
    profile_descriptor,
};

/// Product source relation aliases keep locald as a thin composition layer.
pub(super) type BuiltinWorkspaceRelation = backend_engine::ProductSourceRelation;
pub(super) type BuiltinPackageRecord = backend_engine::ProductSourceRecord;

fn prepare_source_update(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    key: &[u8; 32],
    before: Option<&BuiltinPackageRecord>,
    after: Option<BuiltinPackageRecord>,
) -> Result<LazyPreparedUpdate<BuiltinWorkspaceRelation>, BuiltinModelError> {
    match (before.is_some(), after) {
        (false, Some(value)) => relation.prepare_insert(key, value),
        (true, Some(value)) => relation.prepare_change(key, Some(value)),
        (true, None) => relation.prepare_remove(key),
        (false, None) => {
            return Err(BuiltinModelError(
                "package source transition is a no-op".to_owned(),
            ));
        }
    }
    .map_err(|error| BuiltinModelError(format!("prepare package source delta: {error}")))
}

/// One authoritative package intent carried through the workspace owner.
///
/// The canonical bytes are retained in the checked closure, so recovery can
/// reconstruct the exact package operation before rebuilding its relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinIntent {
    pub(super) operation: BuiltinIntentOperation,
    pub(super) package: backend_engine::PackageKey,
    pub(super) label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BuiltinIntentOperation {
    Add,
    Remove,
}

impl BuiltinIntent {
    const VERSION: u8 = 1;
    const ADD: u8 = 1;
    const REMOVE: u8 = 2;
    const MAX_LABEL_BYTES: usize = 4096;

    /// Creates an intent that adds or replaces one package coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error when the package label exceeds the bounded intent size.
    pub fn add(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
    ) -> Result<Self, BuiltinModelError> {
        Self::new(BuiltinIntentOperation::Add, package, label)
    }

    /// Creates an intent that removes one package coordinate.
    ///
    /// # Errors
    ///
    /// Returns an error when the package label exceeds the bounded intent size.
    pub fn remove(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
    ) -> Result<Self, BuiltinModelError> {
        Self::new(BuiltinIntentOperation::Remove, package, label)
    }

    fn new(
        operation: BuiltinIntentOperation,
        package: backend_engine::PackageKey,
        label: impl Into<String>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        if label.len() > Self::MAX_LABEL_BYTES {
            return Err(BuiltinModelError(
                "package intent label is too large".to_owned(),
            ));
        }
        Ok(Self {
            operation,
            package,
            label,
        })
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, BuiltinModelError> {
        let label = self.label.as_bytes();
        if label.len() > Self::MAX_LABEL_BYTES {
            return Err(BuiltinModelError(
                "package intent label is too large".to_owned(),
            ));
        }
        let mut bytes = Vec::with_capacity(1 + 1 + 32 + 4 + label.len());
        bytes.extend_from_slice(b"BPI1");
        bytes.push(Self::VERSION);
        bytes.push(match self.operation {
            BuiltinIntentOperation::Add => Self::ADD,
            BuiltinIntentOperation::Remove => Self::REMOVE,
        });
        bytes.extend_from_slice(self.package.as_bytes());
        let label_len = u32::try_from(label.len())
            .map_err(|_| BuiltinModelError("package intent label is too large".to_owned()))?;
        bytes.extend_from_slice(&label_len.to_be_bytes());
        bytes.extend_from_slice(label);
        Ok(bytes)
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let label = self.label.as_bytes();
        let mut bytes = Vec::with_capacity(42 + label.len());
        bytes.extend_from_slice(b"BPI1");
        bytes.push(Self::VERSION);
        bytes.push(match self.operation {
            BuiltinIntentOperation::Add => Self::ADD,
            BuiltinIntentOperation::Remove => Self::REMOVE,
        });
        bytes.extend_from_slice(self.package.as_bytes());
        let label_len = u32::try_from(label.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&label_len.to_be_bytes());
        bytes.extend_from_slice(label);
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, BuiltinModelError> {
        if bytes.len() < 42 || &bytes[..4] != b"BPI1" || bytes[4] != Self::VERSION {
            return Err(BuiltinModelError(
                "malformed builtin package intent".to_owned(),
            ));
        }
        let operation = match bytes[5] {
            Self::ADD => BuiltinIntentOperation::Add,
            Self::REMOVE => BuiltinIntentOperation::Remove,
            _ => {
                return Err(BuiltinModelError(
                    "unknown builtin package operation".to_owned(),
                ));
            }
        };
        let encoded_package: [u8; 32] = bytes[6..38]
            .try_into()
            .map_err(|_| BuiltinModelError("malformed package intent key".to_owned()))?;
        let label_len = u32::from_be_bytes(
            bytes[38..42]
                .try_into()
                .map_err(|_| BuiltinModelError("malformed package intent length".to_owned()))?,
        ) as usize;
        if label_len > Self::MAX_LABEL_BYTES || bytes.len() != 42 + label_len {
            return Err(BuiltinModelError(
                "malformed package intent label".to_owned(),
            ));
        }
        let label = String::from_utf8(bytes[42..].to_vec())
            .map_err(|_| BuiltinModelError("package intent label is not utf8".to_owned()))?;
        // The wire record is untrusted. Recompute the typed package key from
        // the canonical coordinate and compare it with the encoded identity;
        // a raw digest never becomes a PackageKey by itself.
        let package = backend_engine::PackageKey::from_value(label.as_str());
        if package.to_bytes() != encoded_package {
            return Err(BuiltinModelError(
                "package intent key does not match its canonical coordinate".to_owned(),
            ));
        }
        Self::new(operation, package, label)
    }

    pub(super) fn is_add(&self) -> bool {
        matches!(self.operation, BuiltinIntentOperation::Add)
    }

    pub(super) fn record(&self) -> Result<BuiltinPackageRecord, BuiltinModelError> {
        BuiltinPackageRecord::new(self.label.clone()).map_err(BuiltinModelError)
    }
}

impl backend_engine::queue::QueueSized for BuiltinIntent {
    fn queue_bytes(&self) -> usize {
        42usize.saturating_add(self.label.len())
    }
}

/// Failure while constructing or admitting the checked builtin workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinModelError(pub(super) String);

impl fmt::Display for BuiltinModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BuiltinModelError {}

/// The built in workspace model used by the local daemon.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinModel;

impl WorkspaceModel for BuiltinModel {
    type Intent = BuiltinIntent;
    type Error = BuiltinModelError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        let bytes = intent.canonical_bytes();
        ObjectVersion::<BuiltinIntentSchema>::from_value(&bytes).to_bytes()
    }

    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        let intent = intent.clone();
        let relation = base
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
        let package_bytes = intent.package.to_bytes();
        let before = relation
            .lookup(&package_bytes)
            .map_err(|error| BuiltinModelError(format!("read package source: {error}")))?;
        let after = if intent.is_add() {
            Some(intent.record()?)
        } else {
            None
        };
        if before == after {
            return Err(BuiltinModelError(
                "builtin package intent is a no-op".to_owned(),
            ));
        }
        let update = prepare_source_update(&relation, &package_bytes, before.as_ref(), after)?;
        let base_manifest = workspace_manifest_from_root(&relation.root_handle())?;
        if base.manifest() != &base_manifest {
            return Err(BuiltinModelError(
                "workspace base manifest mismatch".to_owned(),
            ));
        }
        let target_manifest = workspace_manifest_from_root(&update.target_root())?;
        let request = self.request_id(&intent);
        let changed_nodes = update.changed_nodes().to_vec();
        let relation_base_object = relation
            .root_object()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let work = TransitionWork::from_lazy(1, update.work()).map_err(BuiltinModelError)?;
        let relation_delta = update.into_delta();
        let transition = RelationTransition::from_delta(&relation_delta);
        let delta = WorkspaceDelta::new(&base_manifest, &target_manifest, vec![transition])
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let provenance = CommitProvenance::from_versions(
            ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
            transaction.version(),
            request,
        );
        let commit = backend_engine::commit_checked(&target_manifest, Vec::new(), provenance)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let closure = transition_closure_lazy(
            &target_manifest,
            relation_base_object,
            &changed_nodes,
            transaction,
            &commit,
            &delta,
            &intent,
        )?;
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
        let intent_bytes = intent.encode()?;
        let intent_key = ObjectKey::<BuiltinIntentSchema>::from_value(&intent_bytes);
        let intent_object = TypedObject::from_value(&intent_key, &intent_bytes);
        let transition = PreparedTransition::new_with_registry(
            request,
            transaction,
            delta,
            commit,
            closure,
            &registry,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))?;
        transition
            .retain_objects([intent_object], &registry)
            .map(|transition| transition.with_work(work))
            .map_err(|error| BuiltinModelError(error.to_string()))
    }

    fn admit_persisted(
        &self,
        _persisted: &backend_engine::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        Err(BuiltinModelError(
            "persisted builtin transitions require an owned durable relation loader".to_owned(),
        ))
    }

    fn admit_persisted_with_store(
        &self,
        persisted: &backend_engine::PersistedTransition,
        store: &backend_engine::FileStore,
    ) -> Result<PreparedTransition, Self::Error> {
        admit_persisted_transition(persisted, store)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "admission keeps the authenticated persisted transition phases together"
)]
fn admit_persisted_transition(
    persisted: &backend_engine::PersistedTransition,
    store: &backend_engine::FileStore,
) -> Result<PreparedTransition, BuiltinModelError> {
    let persisted_intent = persisted
        .closure_manifest()
        .objects()
        .iter()
        .find_map(|object| BuiltinIntent::decode(object.bytes()).ok())
        .ok_or_else(|| {
            BuiltinModelError("persisted builtin transition has no package intent".to_owned())
        })?;
    let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let target_root = untrusted_manifest
        .relations()
        .first()
        .map(|binding| binding.root())
        .ok_or_else(|| {
            BuiltinModelError("persisted builtin manifest has no relation".to_owned())
        })?;
    let package_bytes = persisted_intent.package.to_bytes();
    let target_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, target_root)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let untrusted_delta = WorkspaceDelta::decode_untrusted(persisted.delta_bytes())
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let base_root = untrusted_delta
        .relations()
        .first()
        .map(RelationTransition::base)
        .ok_or_else(|| BuiltinModelError("persisted builtin delta has no relation".to_owned()))?;
    let base_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, base_root)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let before = base_tree
        .lookup(&package_bytes)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let after = target_tree
        .lookup(&package_bytes)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    if before == after {
        return Err(BuiltinModelError(
            "persisted builtin transition is a no-op".to_owned(),
        ));
    }
    let update = prepare_source_update(&base_tree, &package_bytes, before.as_ref(), after.clone())?;
    let work = TransitionWork::from_lazy(1, update.work()).map_err(BuiltinModelError)?;
    if update.target().root().to_bytes() != target_root {
        return Err(BuiltinModelError(
            "persisted builtin target root does not match its package change".to_owned(),
        ));
    }
    let target_manifest = workspace_manifest_from_root(&target_tree.root_handle())?;
    let base_manifest = workspace_manifest_from_root(&base_tree.root_handle())?;
    let relation_delta = update.delta();
    let manifest = admit_manifest(untrusted_manifest, &target_manifest)?;
    if manifest != target_manifest {
        return Err(BuiltinModelError(
            "persisted builtin manifest changed during typed admission".to_owned(),
        ));
    }

    let untrusted_delta = WorkspaceDelta::decode_untrusted(persisted.delta_bytes())
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let delta = admit_delta(
        untrusted_delta,
        &base_manifest,
        &manifest,
        RelationTransition::from_delta(&relation_delta),
    )?;

    let untrusted_commit = Commit::decode_untrusted(persisted.commit_bytes())
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let authority = ObjectClosure::from_version(
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
    );
    let transaction = ObjectClosure::from_version(persisted.transaction().version());
    let provenance = untrusted_commit
        .provenance()
        .clone()
        .admit(authority, transaction)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    if provenance.detail() != persisted.request() {
        return Err(BuiltinModelError(
            "persisted builtin commit detail does not match its request".to_owned(),
        ));
    }
    let commit = untrusted_commit
        .admit(&manifest, provenance)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let checked_delta = delta.clone().into_checked();
    let checked_commit = commit.clone().into_checked();
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
    // The durable pack intentionally omits relation descendants and root
    // object IDs. Re-admit the selected root from the store's typed relation
    // handle, then let the root-only closure retain only this frontier.
    let target_root_object = target_tree
        .root_object()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut persisted_objects = persisted.closure_manifest().objects().to_vec();
    if !persisted_objects
        .iter()
        .any(|object| object.id() == target_root_object.id())
    {
        persisted_objects.push(target_root_object);
        persisted_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    }
    let persisted_objects = ClosureManifest::new(persisted_objects)
        .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    let closure = WorkspaceClosure::from_checked_transition_root_only_with_registry(
        &manifest,
        &checked_delta,
        Some(&checked_commit),
        persisted_objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("{error:?}")))?;
    let intent_bytes = persisted_intent.encode()?;
    let intent_key = ObjectKey::<BuiltinIntentSchema>::from_value(&intent_bytes);
    let intent_object = TypedObject::from_value(&intent_key, &intent_bytes);
    let transition = PreparedTransition::new_with_registry(
        persisted.request(),
        persisted.transaction(),
        delta,
        commit,
        closure,
        &registry,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))?;
    transition
        .retain_objects([intent_object], &registry)
        .map(|transition| transition.with_work(work))
        .map_err(|error| BuiltinModelError(error.to_string()))
}

#[derive(Debug)]
pub(super) struct BuiltinIntentSchema;

impl Schema for BuiltinIntentSchema {
    const DOMAIN: u8 = 0x96;
    const TYPE: u16 = 2;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Output/CAS adapter for the compiled profile.  The validator checks the
/// exact bytes and returns retaining publication evidence, including the
/// shared owner used by remote admission.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct BuiltinOutputValidator;

impl backend_engine::OutputAdmissionValidator for BuiltinOutputValidator {
    fn validate(
        &self,
        output: OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<backend_engine::RetainedOutput, DispatchError> {
        builtin_output_check(output, bytes, semantic)?;
        Ok(backend_engine::RetainedOutput::from_bytes(output, bytes))
    }

    fn validate_shared(
        &self,
        output: OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<backend_engine::RetainedOutput, DispatchError> {
        builtin_output_check(output, bytes.as_slice(), semantic)?;
        Ok(backend_engine::RetainedOutput::from_shared(
            output,
            Arc::clone(bytes),
        ))
    }
}

#[derive(Clone, Debug)]
pub(super) struct BuiltinSemanticAuthority {
    profile: Arc<ProfileDescriptor>,
}
pub(super) type BuiltinValidator =
    CompositeAdmissionValidator<BuiltinOutputValidator, BuiltinSemanticAuthority>;
pub(super) type BuiltinAuthorityVerifier = Blake3AuthorityVerifier;

pub(super) fn builtin_output_check(
    output: OutputVersion,
    bytes: &[u8],
    semantic: &CompleteSemanticCoverage,
) -> Result<(), DispatchError> {
    let is_product =
        bytes.len() == PRODUCT_OUTPUT_BYTES.len() + 64 && bytes.starts_with(PRODUCT_OUTPUT_BYTES);
    let is_echo = bytes == ECHO_OUTPUT_BYTES;
    if !semantic.is_complete()
        || (!is_product && !is_echo)
        || OutputVersion::from_value(bytes) != output
    {
        return Err(DispatchError::OutputMismatch);
    }
    Ok(())
}

impl SemanticCoverageValidator for BuiltinSemanticAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_claim(self.profile.ids, binding, claim)
    }

    fn validate_manifest(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_manifest(
            self.profile.ids,
            binding,
            claim,
            manifest,
        )
    }
}

pub(super) fn builtin_validator(profile: Arc<ProfileDescriptor>) -> BuiltinValidator {
    CompositeAdmissionValidator::new(BuiltinOutputValidator, BuiltinSemanticAuthority { profile })
}

pub(super) fn builtin_dispatcher(
    product_secret: Option<[u8; 32]>,
    profile: Arc<ProfileDescriptor>,
) -> Result<Dispatcher<BuiltinValidator, BuiltinAuthorityVerifier>, String> {
    let ids = profile.ids;
    let max_output = ids
        .output_prefix
        .len()
        .saturating_add(if ids.include_basis { 64 } else { 0 }) as u64;
    let budget = Budget {
        operations: 64,
        bytes: 64 * 1024,
        hedges: 0,
        resources: ResourceVector {
            cpu_millis: 64 * 100,
            memory_bytes: 64 * 1024,
            network_bytes: 64 * max_output,
            storage_bytes: 64 * max_output,
        },
    };
    let scheduler = Scheduler::with_envelopes(backend_engine::EnvelopeBudgets::uniform(budget));
    let mut authority = Blake3AuthorityVerifier::new();
    match profile.kind {
        BuiltinProfile::Product => {
            let secret = product_secret.ok_or_else(|| {
                "production profile requires a configured authority verifier".to_owned()
            })?;
            authority.insert_key(ids.authority.to_bytes(), secret);
        }
        BuiltinProfile::EchoFixture => {
            authority.insert_key(ids.authority.to_bytes(), ECHO_AUTHORITY_SECRET);
        }
    }
    Ok(Dispatcher::with_scheduler(
        scheduler,
        builtin_validator(profile),
        authority,
        RemoteAuthorityPolicy::Signed,
    ))
}
