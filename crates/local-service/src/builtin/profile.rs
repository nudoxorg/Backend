//! Compiled profile identities, model, validator, and dispatcher.
//!
//! This module is the profile composition boundary. It owns the registered
//! relation recipe and its checked authority/semantic contracts; process and
//! transport state stay in the parent façade and the replication module.

use super::{
    AUTHORITY_VALUE, Arc, AuthorityVersionSchema, Blake3AuthorityVerifier, Budget, Commit,
    CommitProvenance, CompleteSemanticCoverage, CompositeAdmissionValidator, DependencyManifest,
    DispatchError, Dispatcher, ECHO_AUTHORITY_SECRET, ObjectClosure, ObjectKey, ObjectVersion,
    OutputVersion, PreparedTransition, RelationAdmissionRegistry, RelationTransition,
    RemoteAuthorityPolicy, ResourceVector, Scheduler, Schema, SemanticCoverageAdmissionError,
    SemanticCoverageBinding, TransactionId, TypedObject, UntrustedSemanticCoverageClaim,
    WorkspaceClosure, WorkspaceDelta, WorkspaceManifest, WorkspaceModel, WorkspaceSnapshot,
    admit_manifest, fmt, transition_closure_lazy, workspace_manifest_from_root,
};
use backend_engine::{
    CanonicalRelation, LazyPreparedUpdate, Relation, SemanticCoverageValidator, TransitionWork,
    TreeChange, WorkspaceRelationHandle,
};

pub(super) use backend_engine::builtin::{
    ECHO_OUTPUT_BYTES, PRODUCT_OUTPUT_BYTES, ProductSourceRelation as ProductRelation,
    Profile as BuiltinProfile, ProfileDescriptor, ProfileIds, execution_input_basis,
    execution_manifest, execution_resources, product_dependency_manifest, product_input_version,
    product_source_fixture_with_authority, profile_descriptor, profile_output_len,
};

/// Product source relation aliases keep locald as a thin composition layer.
pub(super) type BuiltinWorkspaceRelation = backend_engine::ProductSourceRelation;
pub(super) type BuiltinPackageRecord = backend_engine::ProductSourceRecord;

fn prepare_source_update(
    relation: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    changes: &[BuiltinSourceChange],
) -> Result<LazyPreparedUpdate<BuiltinWorkspaceRelation>, BuiltinModelError> {
    let changes = changes
        .iter()
        .map(|change| TreeChange {
            key: change.key,
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    relation
        .prepare_update(&changes)
        .map_err(|error| BuiltinModelError(format!("prepare product source delta: {error}")))
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
    changes: Box<[BuiltinSourceChange]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BuiltinSourceChange {
    pub(super) key: [u8; 32],
    pub(super) after: Option<BuiltinPackageRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BuiltinIntentOperation {
    Add,
    Remove,
    Index,
}

impl BuiltinIntent {
    const VERSION: u8 = 2;
    const ADD: u8 = 1;
    const REMOVE: u8 = 2;
    const INDEX: u8 = 3;
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
        let label = label.into();
        let record = BuiltinPackageRecord::new(label.clone()).map_err(BuiltinModelError)?;
        Self::new(
            BuiltinIntentOperation::Add,
            package,
            label,
            vec![BuiltinSourceChange {
                key: package.to_bytes(),
                after: Some(record),
            }],
        )
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
        Self::remove_project(package, label, &[])
    }

    /// Creates an intent that removes a project and every file selected by
    /// its last admitted frontier.
    pub(super) fn remove_project(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        files: &[[u8; 32]],
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        let mut changes = Vec::with_capacity(files.len().saturating_add(1));
        changes.push(BuiltinSourceChange {
            key: package.to_bytes(),
            after: None,
        });
        changes.extend(
            files
                .iter()
                .copied()
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
        Self::new(BuiltinIntentOperation::Remove, package, label, changes)
    }

    /// Creates one atomic project-index intent from already scanned and
    /// compared file changes.
    pub(super) fn index(
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        changes: Vec<BuiltinSourceChange>,
    ) -> Result<Self, BuiltinModelError> {
        Self::new(BuiltinIntentOperation::Index, package, label, changes)
    }

    fn new(
        operation: BuiltinIntentOperation,
        package: backend_engine::PackageKey,
        label: impl Into<String>,
        mut changes: Vec<BuiltinSourceChange>,
    ) -> Result<Self, BuiltinModelError> {
        let label = label.into();
        if label.len() > Self::MAX_LABEL_BYTES {
            return Err(BuiltinModelError(
                "package intent label is too large".to_owned(),
            ));
        }
        changes.sort_by_key(|change| change.key);
        if changes.is_empty()
            || changes.len()
                > BuiltinPackageRecord::MAX_PROJECT_FILES
                    .saturating_add(BuiltinPackageRecord::MAX_PROJECT_FACTS)
                    .saturating_add(1)
            || changes
                .windows(2)
                .any(|window| window[0].key >= window[1].key)
        {
            return Err(BuiltinModelError(
                "product source intent is empty, unordered, or oversized".to_owned(),
            ));
        }
        Ok(Self {
            operation,
            package,
            label,
            changes: changes.into_boxed_slice(),
        })
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        self.encode_canonical()
    }

    fn encode_canonical(&self) -> Vec<u8> {
        let label = self.label.as_bytes();
        let mut bytes = Vec::with_capacity(46 + label.len());
        bytes.extend_from_slice(b"BPI2");
        bytes.push(Self::VERSION);
        bytes.push(match self.operation {
            BuiltinIntentOperation::Add => Self::ADD,
            BuiltinIntentOperation::Remove => Self::REMOVE,
            BuiltinIntentOperation::Index => Self::INDEX,
        });
        bytes.extend_from_slice(self.package.as_bytes());
        let label_len = u32::try_from(label.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&label_len.to_be_bytes());
        bytes.extend_from_slice(label);
        let change_count = u32::try_from(self.changes.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&change_count.to_be_bytes());
        for change in &self.changes {
            bytes.extend_from_slice(&change.key);
            match &change.after {
                Some(record) => {
                    bytes.push(1);
                    let mut encoded = Vec::new();
                    <ProductRelation as Relation>::encode_value(record, &mut encoded);
                    let length = u32::try_from(encoded.len()).unwrap_or(u32::MAX);
                    bytes.extend_from_slice(&length.to_be_bytes());
                    bytes.extend_from_slice(&encoded);
                }
                None => bytes.push(0),
            }
        }
        bytes
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        self.encode_canonical()
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, BuiltinModelError> {
        if bytes.len() < 46 || &bytes[..4] != b"BPI2" || bytes[4] != Self::VERSION {
            return Err(BuiltinModelError(
                "malformed builtin package intent".to_owned(),
            ));
        }
        let operation = match bytes[5] {
            Self::ADD => BuiltinIntentOperation::Add,
            Self::REMOVE => BuiltinIntentOperation::Remove,
            Self::INDEX => BuiltinIntentOperation::Index,
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
        if label_len > Self::MAX_LABEL_BYTES || bytes.len() < 46 + label_len {
            return Err(BuiltinModelError(
                "malformed package intent label".to_owned(),
            ));
        }
        let label_end = 42usize
            .checked_add(label_len)
            .ok_or_else(|| BuiltinModelError("malformed package intent label".to_owned()))?;
        let label = String::from_utf8(bytes[42..label_end].to_vec())
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
        let mut at = label_end;
        let change_count = read_u32(bytes, &mut at)? as usize;
        if change_count == 0
            || change_count
                > BuiltinPackageRecord::MAX_PROJECT_FILES
                    .saturating_add(BuiltinPackageRecord::MAX_PROJECT_FACTS)
                    .saturating_add(1)
        {
            return Err(BuiltinModelError(
                "malformed product source change count".to_owned(),
            ));
        }
        let mut changes = Vec::with_capacity(change_count);
        for _ in 0..change_count {
            let key: [u8; 32] = take(bytes, &mut at, 32)?
                .try_into()
                .map_err(|_| BuiltinModelError("malformed product source key".to_owned()))?;
            let after = match take(bytes, &mut at, 1)?.first().copied() {
                Some(0) => None,
                Some(1) => {
                    let length = read_u32(bytes, &mut at)? as usize;
                    let record = <ProductRelation as CanonicalRelation>::decode_value(take(
                        bytes, &mut at, length,
                    )?)
                    .map_err(|_| BuiltinModelError("malformed product source record".to_owned()))?;
                    Some(record)
                }
                _ => {
                    return Err(BuiltinModelError(
                        "malformed product source operation".to_owned(),
                    ));
                }
            };
            changes.push(BuiltinSourceChange { key, after });
        }
        if at != bytes.len() {
            return Err(BuiltinModelError(
                "trailing product source intent bytes".to_owned(),
            ));
        }
        Self::new(operation, package, label, changes)
    }

    pub(super) fn record(&self) -> Result<BuiltinPackageRecord, BuiltinModelError> {
        self.changes
            .iter()
            .find(|change| change.key == self.package.to_bytes())
            .and_then(|change| change.after.clone())
            .ok_or_else(|| BuiltinModelError("intent has no project record".to_owned()))
    }

    pub(super) fn changes(&self) -> &[BuiltinSourceChange] {
        &self.changes
    }
}

impl backend_engine::queue::QueueSized for BuiltinIntent {
    fn queue_bytes(&self) -> usize {
        self.encode().len()
    }
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, length: usize) -> Result<&'a [u8], BuiltinModelError> {
    let end = at
        .checked_add(length)
        .ok_or_else(|| BuiltinModelError("product source intent length overflow".to_owned()))?;
    let value = bytes
        .get(*at..end)
        .ok_or_else(|| BuiltinModelError("truncated product source intent".to_owned()))?;
    *at = end;
    Ok(value)
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, BuiltinModelError> {
    let value: [u8; 4] = take(bytes, at, 4)?
        .try_into()
        .map_err(|_| BuiltinModelError("malformed product source length".to_owned()))?;
    Ok(u32::from_be_bytes(value))
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
        let update = prepare_source_update(&relation, intent.changes())?;
        let changed_items = update.delta().changes().count();
        if changed_items == 0 {
            return Err(BuiltinModelError(
                "product source intent is a no-op".to_owned(),
            ));
        }
        let base_manifest = workspace_manifest_from_root(&relation.root_handle())?;
        if base.manifest() != &base_manifest {
            return Err(BuiltinModelError(
                "workspace base manifest mismatch".to_owned(),
            ));
        }
        let target_manifest = workspace_manifest_from_root(&update.target_root())?;
        let target_root = update.target().root();
        let request = self.request_id(&intent);
        let changed_nodes = update.changed_nodes().to_vec();
        let relation_base_object = relation
            .root_object()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let work =
            TransitionWork::from_lazy(changed_items, update.work()).map_err(BuiltinModelError)?;
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
            base.closure(),
            &target_manifest,
            relation_base_object,
            &changed_nodes,
            target_root,
            transaction,
            &intent,
        )?;
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
        let intent_bytes = intent.encode();
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
        .map_err(|error| BuiltinModelError(format!("construct prepared transition: {error}")))?;
        transition
            .replace_object_family([intent_object], &registry)
            .map(|transition| transition.with_work(work))
            .map_err(|error| BuiltinModelError(format!("retain current intent: {error}")))
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
            .map_err(|error| BuiltinModelError(format!("builtin persisted transition: {error}")))
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
        .map_err(|error| BuiltinModelError(format!("decode persisted manifest: {error}")))?;
    let target_root = untrusted_manifest
        .relations()
        .first()
        .map(|binding| binding.root())
        .ok_or_else(|| {
            BuiltinModelError("persisted builtin manifest has no relation".to_owned())
        })?;
    let target_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, target_root)
        .map_err(|error| BuiltinModelError(format!("open persisted target relation: {error}")))?;
    let delta_header = persisted
        .delta_header()
        .map_err(|error| BuiltinModelError(format!("decode persisted delta header: {error}")))?;
    let base_root = delta_header
        .relations()
        .first()
        .map(|relation| relation.base())
        .ok_or_else(|| BuiltinModelError("persisted builtin delta has no relation".to_owned()))?;
    let base_tree = persisted
        .relation::<BuiltinWorkspaceRelation>(store, base_root)
        .map_err(|error| BuiltinModelError(format!("open persisted base relation: {error}")))?;
    let update = prepare_source_update(&base_tree, persisted_intent.changes())?;
    let changed_items = update.delta().changes().count();
    if changed_items == 0 {
        return Err(BuiltinModelError(
            "persisted product transition is a no-op".to_owned(),
        ));
    }
    let work = TransitionWork::from_lazy(changed_items, update.work())
        .map_err(|error| BuiltinModelError(format!("admit persisted work: {error}")))?;
    if update.target().root().to_bytes() != target_root {
        return Err(BuiltinModelError(
            "persisted builtin target root does not match its package change".to_owned(),
        ));
    }
    let target_manifest = workspace_manifest_from_root(&target_tree.root_handle())
        .map_err(|error| BuiltinModelError(format!("rebuild target manifest: {error}")))?;
    let base_manifest = workspace_manifest_from_root(&base_tree.root_handle())
        .map_err(|error| BuiltinModelError(format!("rebuild base manifest: {error}")))?;
    let relation_delta = update.delta();
    let manifest = admit_manifest(untrusted_manifest, &target_manifest)
        .map_err(|error| BuiltinModelError(format!("admit persisted manifest: {error}")))?;
    if manifest != target_manifest {
        return Err(BuiltinModelError(
            "persisted builtin manifest changed during typed admission".to_owned(),
        ));
    }

    let delta = delta_header
        .admit_encoded(
            persisted.delta_bytes(),
            &base_manifest,
            &manifest,
            vec![RelationTransition::from_delta(&relation_delta)],
        )
        .map_err(|error| BuiltinModelError(format!("admit persisted delta: {error}")))?;

    let untrusted_commit = Commit::decode_untrusted(persisted.commit_bytes())
        .map_err(|error| BuiltinModelError(format!("decode persisted commit: {error}")))?;
    let authority = ObjectClosure::from_version(
        ObjectVersion::<AuthorityVersionSchema>::from_value(AUTHORITY_VALUE),
    );
    let transaction = ObjectClosure::from_version(persisted.transaction().version());
    let provenance = untrusted_commit
        .provenance()
        .clone()
        .admit(authority, transaction)
        .map_err(|error| BuiltinModelError(format!("admit persisted provenance: {error}")))?;
    if provenance.detail() != persisted.request() {
        return Err(BuiltinModelError(
            "persisted builtin commit detail does not match its request".to_owned(),
        ));
    }
    let commit = untrusted_commit
        .admit(&manifest, provenance)
        .map_err(|error| BuiltinModelError(format!("admit persisted commit: {error}")))?;
    let checked_delta = delta.clone().into_checked();
    let checked_commit = commit.clone().into_checked();
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("register builtin relation: {error:?}")))?;
    // Reopen the exact authenticated closure selected by the physical HEAD.
    // The compact transition envelope intentionally contains only recovery
    // pointers, so rebuilding a new manifest from that subset would produce a
    // different closure identity. The durable manifest remains content
    // addressed and is admitted through the store's relation registry here.
    let selected = store
        .head()
        .map_err(|error| BuiltinModelError(format!("read selected workspace head: {error:?}")))?
        .ok_or_else(|| BuiltinModelError("selected workspace head disappeared".to_owned()))?;
    let persisted_objects = store
        .read_closure(selected.descriptor().closure())
        .map_err(|error| {
            BuiltinModelError(format!("read selected workspace closure: {error:?}"))
        })?;
    let closure = WorkspaceClosure::from_checked_transition_root_only_with_registry(
        &manifest,
        &checked_delta,
        Some(&checked_commit),
        persisted_objects,
        &registry,
    )
    .map_err(|error| BuiltinModelError(format!("admit persisted closure: {error:?}")))?;
    let intent_bytes = persisted_intent.encode();
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
    .map_err(|error| BuiltinModelError(format!("rebuild persisted transition: {error}")))?;
    transition
        .replace_object_family([intent_object], &registry)
        .map(|transition| transition.with_work(work))
        .map_err(|error| BuiltinModelError(format!("retain persisted intent: {error}")))
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
    let is_product = bytes.len()
        == PRODUCT_OUTPUT_BYTES.len() + backend_engine::PRODUCT_OUTPUT_BODY_BYTES
        && bytes.starts_with(PRODUCT_OUTPUT_BYTES);
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
    attempt_lease_ticks: u64,
) -> Result<Dispatcher<BuiltinValidator, BuiltinAuthorityVerifier>, String> {
    let ids = profile.ids;
    let envelope = execution_resources(ids);
    let max_output = profile_output_len(ids) as u64;
    let budget = Budget {
        operations: 64,
        bytes: 64 * 1024,
        hedges: 0,
        resources: ResourceVector {
            cpu_millis: 64_u64.saturating_mul(envelope.cpu_millis),
            memory_bytes: 64_u64.saturating_mul(envelope.memory_bytes),
            network_bytes: 64 * max_output,
            storage_bytes: 64 * max_output,
        },
    };
    let scheduler = Scheduler::with_envelopes_and_lease_duration(
        backend_engine::EnvelopeBudgets::uniform(budget),
        attempt_lease_ticks,
    );
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
