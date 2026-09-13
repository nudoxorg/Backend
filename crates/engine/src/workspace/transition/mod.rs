//! Checked workspace transition and persisted publication envelope.

use super::lazy::{WorkspaceRelationError, WorkspaceRelationHandle};
use super::owner::WorkspaceError;
use backend_store::{
    ClosureManifest, FileStore, ManifestChange, ObjectId, RelationAdmissionRegistry, StoreError,
    TypedObject, WorkspaceClosure,
};
use backend_version::{
    CheckedCommit, CheckedWorkspaceTransition, Commit, CommitProvenance, LazyTreeWork,
    ObjectClosure as VersionObjectClosure, ObjectVersion, Schema, SchemaIdentity, WorkspaceDelta,
    WorkspaceManifest, WorkspaceRoot, commit_capability,
};
use blake3::Hasher;
use std::fmt;
use std::sync::Arc;

const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

#[inline(never)]
fn consume_commit(commit: Commit) {
    // Checked capabilities are reconstructed from the commit before the
    // owning constructor boundary releases its input.
    std::hint::black_box(commit);
}

mod payload;

pub(crate) use payload::materialize_transition_closure_with_registry;
pub(crate) use payload::{
    TRANSITION_PAYLOAD_COUNT, materialize_transition_closure_with_ids,
    persisted_from_store_manifest,
};

/// Engine-owned schema for deterministic transaction objects.
#[derive(Debug)]
pub struct TransactionSchema;

impl Schema for TransactionSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Schema for the request identity retained in the store-owned workspace
/// closure.  These small typed objects let the lower store HEAD reconstruct
/// a publication without treating the diagnostic journal as an authority.
#[derive(Debug)]
struct RequestPayloadSchema;

impl Schema for RequestPayloadSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 2;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Schema for the canonical target manifest bytes retained in a workspace
/// closure.  The bytes are produced by the checked version capability.
#[derive(Debug)]
struct ManifestPayloadSchema;

impl Schema for ManifestPayloadSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 3;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Schema for the exact checked workspace transition bytes retained in a
/// workspace closure.
#[derive(Debug)]
struct DeltaPayloadSchema;

impl Schema for DeltaPayloadSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 4;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Schema for the exact checked history commit bytes retained in a workspace
/// closure.
#[derive(Debug)]
struct CommitPayloadSchema;

impl Schema for CommitPayloadSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 5;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Schema for the deterministic owner transaction bytes.  The checked commit
/// already references this identity, while this envelope gives store-head
/// recovery a bounded typed way to recover the owner transaction ID.
#[derive(Debug)]
struct TransactionPayloadSchema;

impl Schema for TransactionPayloadSchema {
    const DOMAIN: u8 = 0x86;
    const TYPE: u16 = 6;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Typed transaction object version used in commit provenance.
pub type TransactionVersion = ObjectVersion<TransactionSchema>;

/// Deterministic idempotency identity for one owner epoch, base, request, and
/// attempt.  Its bytes are the version of the canonical transaction object.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionId([u8; 32]);

impl TransactionId {
    /// Derives an identity without wall-clock or process-local counters.
    #[must_use]
    pub fn derive(owner_epoch: u64, base: WorkspaceRoot, request: [u8; 32], attempt: u64) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(b"backend.engine.transaction.v3\0");
        hasher.update(&owner_epoch.to_be_bytes());
        hasher.update(&base.to_bytes());
        hasher.update(&request);
        hasher.update(&attempt.to_be_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns a typed transaction object version derived from this exact ID.
    #[must_use]
    pub fn version(self) -> TransactionVersion {
        ObjectVersion::from_value(&self.0[..])
    }

    /// Returns fixed-width bytes for diagnostics and persistence.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Compact checked work facts for the selected transition.
///
/// A model records these facts while it owns the typed lazy update. They are
/// retained with the immutable transition so the composition layer can make
/// a route/refresh decision from observed work rather than a fixture constant;
/// recovery models recompute the same facts while re-admitting the transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransitionWork {
    items: u64,
    nodes: u64,
    bytes: u64,
}

impl TransitionWork {
    /// Creates bounded work facts from a checked model update.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn from_lazy(changed_items: usize, work: LazyTreeWork) -> Result<Self, String> {
        Ok(Self {
            items: u64::try_from(changed_items)
                .map_err(|_| "changed item count overflow".to_owned())?,
            nodes: u64::try_from(work.rebuilt_nodes)
                .map_err(|_| "changed node count overflow".to_owned())?,
            bytes: u64::try_from(work.emitted_bytes)
                .map_err(|_| "changed byte count overflow".to_owned())?,
        })
    }

    /// Creates explicit facts for a checked non-tree transition.
    #[must_use]
    pub const fn new(changed_items: u64, changed_nodes: u64, changed_bytes: u64) -> Self {
        Self {
            items: changed_items,
            nodes: changed_nodes,
            bytes: changed_bytes,
        }
    }

    /// Returns the exact logical item count observed by the model.
    #[must_use]
    pub const fn changed_items(self) -> u64 {
        self.items
    }

    /// Returns the number of canonical nodes emitted by the model update.
    #[must_use]
    pub const fn changed_nodes(self) -> u64 {
        self.nodes
    }

    /// Returns bytes emitted in the changed canonical frontier.
    #[must_use]
    pub const fn changed_bytes(self) -> u64 {
        self.bytes
    }
}

/// Checked target transition returned by a workspace model.
#[derive(Clone, Eq, PartialEq)]
pub struct PreparedTransition {
    request: [u8; 32],
    transaction: TransactionId,
    base: WorkspaceRoot,
    base_manifest: WorkspaceManifest,
    manifest: WorkspaceManifest,
    delta: CheckedWorkspaceTransition,
    commit: CheckedCommit,
    closure: WorkspaceClosure,
    persisted: Arc<PersistedTransition>,
    payloads: [ObjectId; TRANSITION_PAYLOAD_COUNT],
    auxiliary: Arc<[ObjectId]>,
    catalog_descriptor: Option<ObjectId>,
    work: TransitionWork,
}

pub(crate) struct CheckedTransitionInput<'a> {
    pub(crate) request: [u8; 32],
    pub(crate) transaction: TransactionId,
    pub(crate) base: &'a WorkspaceManifest,
    pub(crate) manifest: WorkspaceManifest,
    pub(crate) delta: CheckedWorkspaceTransition,
    pub(crate) commit: CheckedCommit,
    pub(crate) closure: WorkspaceClosure,
    pub(crate) registry: &'a RelationAdmissionRegistry,
}

impl fmt::Debug for PreparedTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedTransition")
            .field("request", &self.request)
            .field("transaction", &self.transaction)
            .field("base", &self.base)
            .field("manifest", &self.manifest)
            .field("relation_count", &self.delta.relation_count())
            .field("parent_count", &self.commit.parent_count())
            .field("closure", &self.closure)
            .finish_non_exhaustive()
    }
}

impl PreparedTransition {
    /// Binds checked version and store capabilities to one owner transaction.
    ///
    /// # Errors
    ///
    /// Rejects mismatched target roots, closure roots, authority provenance,
    /// transaction provenance, or noncanonical transition state.
    pub fn new(
        request: [u8; 32],
        transaction: TransactionId,
        delta: WorkspaceDelta,
        commit: Commit,
        closure: WorkspaceClosure,
    ) -> Result<Self, WorkspaceError> {
        let manifest = delta.target_manifest().clone();
        if delta.target() != commit.root() || delta.target() != closure.root() {
            return Err(WorkspaceError::TransitionMismatch);
        }
        let authority = manifest
            .authority_closure()
            .ok_or(WorkspaceError::UnverifiedManifest)?;
        if commit.authority() != authority {
            return Err(WorkspaceError::TransitionMismatch);
        }
        if commit.transaction() != VersionObjectClosure::from_version(transaction.version()) {
            return Err(WorkspaceError::TransactionMismatch);
        }
        let provenance = CommitProvenance::new(
            commit.authority(),
            commit.transaction(),
            commit.provenance().to_vec(),
        );
        let checked_commit = commit_capability(&manifest, commit.parents().to_vec(), provenance)
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        consume_commit(commit);
        let base_manifest = delta.base_manifest().clone();
        Self::from_checked(
            request,
            transaction,
            &base_manifest,
            manifest,
            delta.into_checked(),
            checked_commit,
            closure,
        )
    }

    /// Binds a checked transition while using the same relation admission
    /// registry as the owning store.  Product relations are intentionally
    /// absent from the default registry, so callers that persist a custom
    /// relation must use this constructor at the model boundary.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the checked transition, commit, or
    /// closure does not agree, or when a relation object fails admission.
    pub fn new_with_registry(
        request: [u8; 32],
        transaction: TransactionId,
        delta: WorkspaceDelta,
        commit: Commit,
        closure: WorkspaceClosure,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let manifest = delta.target_manifest().clone();
        if delta.target() != commit.root() || delta.target() != closure.root() {
            return Err(WorkspaceError::TransitionMismatch);
        }
        let authority = manifest
            .authority_closure()
            .ok_or(WorkspaceError::UnverifiedManifest)?;
        if commit.authority() != authority {
            return Err(WorkspaceError::TransitionMismatch);
        }
        if commit.transaction() != VersionObjectClosure::from_version(transaction.version()) {
            return Err(WorkspaceError::TransactionMismatch);
        }
        let provenance = CommitProvenance::new(
            commit.authority(),
            commit.transaction(),
            commit.provenance().to_vec(),
        );
        let checked_commit = commit_capability(&manifest, commit.parents().to_vec(), provenance)
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        consume_commit(commit);
        let base_manifest = delta.base_manifest().clone();
        Self::from_checked_with_registry(CheckedTransitionInput {
            request,
            transaction,
            base: &base_manifest,
            manifest,
            delta: delta.into_checked(),
            commit: checked_commit,
            closure,
            registry,
        })
    }

    /// Constructs a publication from opaque checked version capabilities.
    /// The closure is rechecked against every manifest, transition, commit,
    /// authority, and transaction reference before it is retained.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn from_checked(
        request: [u8; 32],
        transaction: TransactionId,
        base: &WorkspaceManifest,
        manifest: WorkspaceManifest,
        delta: CheckedWorkspaceTransition,
        commit: CheckedCommit,
        closure: WorkspaceClosure,
    ) -> Result<Self, WorkspaceError> {
        Self::from_checked_with_registry(CheckedTransitionInput {
            request,
            transaction,
            base,
            manifest,
            delta,
            commit,
            closure,
            registry: &RelationAdmissionRegistry::default(),
        })
    }

    /// Constructs a publication from opaque checked version capabilities and
    /// an explicit relation registry owned by the store.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the checked transition, commit, or
    /// closure does not agree, or when a relation object fails admission.
    pub(crate) fn from_checked_with_registry(
        input: CheckedTransitionInput<'_>,
    ) -> Result<Self, WorkspaceError> {
        let CheckedTransitionInput {
            request,
            transaction,
            base,
            manifest,
            delta,
            commit,
            closure,
            registry,
        } = input;
        if !manifest.is_checked() || !base.is_checked() || manifest.root() != closure.root() {
            return Err(WorkspaceError::TransitionMismatch);
        }
        if !delta.binds_base(base)
            || !delta.binds_target(&manifest)
            || !commit.binds_target(&manifest)
        {
            return Err(WorkspaceError::TransitionMismatch);
        }
        let manifest_bytes = manifest.encode();
        let delta_bytes = delta.encode();
        let commit_bytes = commit.encode();
        let (closure, payloads) = materialize_transition_closure_with_ids(
            &manifest,
            &delta,
            &commit,
            transaction,
            request,
            &closure,
            registry,
        )?;
        let closure_bytes = closure
            .manifest()
            .encode(MAX_RECORD_BYTES)
            .map_err(WorkspaceError::store)?;
        // Retain only the manifest's non-relation roots in the fixed
        // recovery index. Relation nodes, including the selected root, are
        // reopened through the store's typed relation reference index;
        // placing their object IDs in this index would make recovery probe a
        // relation-node identity through the ordinary object-CAS path. The
        // model re-adds the selected relation root to the transient closure
        // before typed admission.
        let mut closure_refs = manifest
            .closure_refs()
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        closure_refs.extend(
            delta
                .closure_refs()
                .map_err(|error| WorkspaceError::Version(error.to_string()))?,
        );
        closure_refs.extend(commit.closure_refs());
        let mut auxiliary = closure
            .manifest()
            .objects()
            .iter()
            .filter(|object| !payloads.contains(&object.id()))
            .filter(|object| {
                closure_refs.iter().any(|reference| {
                    reference.kind() != backend_version::ClosureKind::Relation
                        && reference.schema() == object.schema()
                        && reference.version() == *object.version()
                })
            })
            .map(TypedObject::id)
            .collect::<Vec<_>>();
        auxiliary.sort_unstable();
        auxiliary.dedup();
        let persisted = Arc::new(PersistedTransition::from_parts(
            request,
            transaction,
            manifest_bytes.into_boxed_slice(),
            delta_bytes.into_boxed_slice(),
            commit_bytes.into_boxed_slice(),
            closure.manifest().clone(),
            closure_bytes.into_boxed_slice(),
        ));
        Ok(Self {
            request,
            transaction,
            base: base.root(),
            base_manifest: base.clone(),
            manifest,
            delta,
            commit,
            closure,
            persisted,
            payloads,
            auxiliary: Arc::from(auxiliary),
            catalog_descriptor: None,
            work: TransitionWork::default(),
        })
    }

    /// Binds an optional durable derived-output descriptor to the fixed
    /// workspace pack index. The descriptor is an immutable object already
    /// admitted into the target closure; this setter is crate-private so a
    /// caller cannot make an arbitrary catalog claim visible in the pack.
    pub(super) fn with_catalog_descriptor(mut self, descriptor: ObjectId) -> Self {
        if self.closure.manifest().contains_object_id(descriptor) {
            let mut auxiliary = self.auxiliary.to_vec();
            if !self.payloads.contains(&descriptor) && !auxiliary.contains(&descriptor) {
                auxiliary.push(descriptor);
                auxiliary.sort_unstable();
                self.auxiliary = Arc::from(auxiliary);
            }
            self.catalog_descriptor = Some(descriptor);
        }
        self
    }

    /// Retains checked immutable objects that belong to an already selected
    /// side relation (for example the derived-output catalog descriptor)
    /// while preparing a source transition. The objects are admitted into the
    /// target closure through the store's typed registry; callers cannot add
    /// an arbitrary digest or bypass transition/root checks.
    /// Extends this checked closure with already admitted immutable objects.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] if an object cannot be admitted into the
    /// closure or the resulting checked transition no longer binds.
    pub fn retain_objects(
        self,
        objects: impl IntoIterator<Item = TypedObject>,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let objects = objects.into_iter().collect::<Vec<_>>();
        self.retain_objects_inner(&objects, registry, None)
    }

    /// Replaces one auxiliary object schema family with its current members.
    ///
    /// This is the bounded generational form of [`Self::retain_objects`]. All
    /// supplied objects must share one schema. Older objects of that schema
    /// are removed in the same closure-index delta, while unrelated authority,
    /// relation, and catalog objects remain shared.
    ///
    /// # Errors
    /// Returns [`WorkspaceError`] when the input is empty, spans schemas, or
    /// the resulting closure no longer binds the checked transition.
    pub fn replace_object_family(
        self,
        objects: impl IntoIterator<Item = TypedObject>,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let objects = objects.into_iter().collect::<Vec<_>>();
        let schema = objects
            .first()
            .map(TypedObject::schema)
            .ok_or(WorkspaceError::Corrupt("empty object family"))?;
        if objects.iter().any(|object| object.schema() != schema) {
            return Err(WorkspaceError::Corrupt("object family spans schemas"));
        }
        self.retain_objects_inner(&objects, registry, Some(schema))
    }

    fn retain_objects_inner(
        self,
        objects: &[TypedObject],
        registry: &RelationAdmissionRegistry,
        replace_schema: Option<SchemaIdentity>,
    ) -> Result<Self, WorkspaceError> {
        let retained_ids = objects.iter().map(TypedObject::id).collect::<Vec<_>>();
        let mut changes = Vec::new();
        if let Some(schema) = replace_schema {
            for previous in self.closure.manifest().objects() {
                if previous.schema() == schema && !retained_ids.contains(&previous.id()) {
                    changes.push(ManifestChange::delete(previous).map_err(WorkspaceError::store)?);
                }
            }
        }
        for object in objects {
            if !self.closure.manifest().contains_object_id(object.id()) {
                changes.push(ManifestChange::insert(object).map_err(WorkspaceError::store)?);
            }
        }
        let mut rebuilt = if changes.is_empty() {
            self
        } else {
            changes.sort_by_key(ManifestChange::key);
            let manifest = self
                .closure
                .manifest()
                .prepare_delta(&changes)
                .map_err(WorkspaceError::store)?
                .commit();
            let closure = self
                .closure
                .rebind_checked_transition_with_registry(
                    &self.manifest,
                    &self.delta,
                    Some(&self.commit),
                    manifest,
                    registry,
                )
                .map_err(WorkspaceError::store)?;
            let mut rebuilt = Self::from_checked_with_registry(CheckedTransitionInput {
                request: self.request,
                transaction: self.transaction,
                base: &self.base_manifest,
                manifest: self.manifest,
                delta: self.delta,
                commit: self.commit,
                closure,
                registry,
            })?;
            rebuilt.catalog_descriptor = self.catalog_descriptor;
            rebuilt.work = self.work;
            rebuilt
        };
        if rebuilt
            .catalog_descriptor
            .is_some_and(|descriptor| !rebuilt.closure.manifest().contains_object_id(descriptor))
        {
            rebuilt.catalog_descriptor = None;
        }
        let mut auxiliary = rebuilt
            .auxiliary
            .iter()
            .copied()
            .filter(|id| rebuilt.closure.manifest().contains_object_id(*id))
            .collect::<Vec<_>>();
        for object_id in retained_ids {
            if !rebuilt.payloads.contains(&object_id) && !auxiliary.contains(&object_id) {
                auxiliary.push(object_id);
            }
        }
        auxiliary.sort_unstable();
        rebuilt.auxiliary = Arc::from(auxiliary);
        Ok(rebuilt)
    }

    pub(super) const fn catalog_descriptor(&self) -> Option<ObjectId> {
        self.catalog_descriptor
    }

    /// Client idempotency identity.
    #[must_use]
    pub const fn request(&self) -> [u8; 32] {
        self.request
    }

    /// Deterministic transaction identity.
    #[must_use]
    pub const fn transaction(&self) -> TransactionId {
        self.transaction
    }

    /// Checked base root.
    #[must_use]
    pub const fn base(&self) -> WorkspaceRoot {
        self.base
    }

    /// Checked base manifest retained by this transition. Keeping the exact
    /// capability avoids reconstructing or guessing provenance when a
    /// derived catalog update extends an already selected closure.
    #[must_use]
    pub const fn base_manifest(&self) -> &WorkspaceManifest {
        &self.base_manifest
    }

    /// Checked target root derived by the version layer.
    #[must_use]
    pub fn target(&self) -> WorkspaceRoot {
        self.manifest.root()
    }

    /// Checked workspace transition.
    #[must_use]
    pub const fn delta(&self) -> &CheckedWorkspaceTransition {
        &self.delta
    }

    /// Checked target manifest.
    #[must_use]
    pub const fn manifest(&self) -> &WorkspaceManifest {
        &self.manifest
    }

    /// Checked commit bound to the target manifest and transaction provenance.
    #[must_use]
    pub const fn commit(&self) -> &CheckedCommit {
        &self.commit
    }

    /// Complete store closure bound to the target root.
    #[must_use]
    pub const fn closure(&self) -> &WorkspaceClosure {
        &self.closure
    }

    pub(crate) fn persisted(&self) -> &PersistedTransition {
        &self.persisted
    }

    /// Shares the canonical persisted bytes across a publication boundary.
    /// Cloning this handle is O(1); the transition payload is serialized only
    /// when the checked transition is first admitted.
    pub(crate) fn persisted_shared(&self) -> Arc<PersistedTransition> {
        Arc::clone(&self.persisted)
    }

    /// Attaches work facts obtained from the model's checked path-copy update.
    /// The facts are metadata only; roots and closure references remain
    /// derived from the checked version/store capabilities.
    #[must_use]
    pub fn with_work(mut self, work: TransitionWork) -> Self {
        self.work = work;
        self
    }

    /// Returns the compact checked work facts retained with this transition.
    #[must_use]
    pub const fn work(&self) -> TransitionWork {
        self.work
    }

    /// Returns the immutable object identities that carry the exact checked
    /// transition envelope.  The workspace pack stores these fixed-width
    /// pointers so recovery can fetch the five required payloads directly by
    /// identity instead of scanning the complete closure export.
    pub(super) fn payload_object_ids(&self) -> [ObjectId; TRANSITION_PAYLOAD_COUNT] {
        self.payloads
    }

    /// Returns the fixed, direct object frontier needed to reconstruct the
    /// model's typed closure after restart. Relation descendants remain in
    /// the store's authenticated CAS and are loaded through lazy handles.
    pub(super) fn auxiliary_object_ids(&self) -> &[ObjectId] {
        &self.auxiliary
    }

    pub(crate) fn store_publication(
        &self,
        store: &FileStore,
        base: Option<backend_store::PublicationBase>,
    ) -> Result<backend_store::WorkspaceFilePrepared, StoreError> {
        // The recovery pack names this bounded non-relation frontier
        // directly. Persist those exact objects before publishing the pack,
        // including retained catalog descriptors that are unchanged from the
        // logical closure's point of view but may not yet exist in this
        // physical store. Relation roots and descendants keep using the
        // path-copied relation writer below.
        for object_id in self.auxiliary_object_ids() {
            let object = self
                .closure
                .manifest()
                .objects()
                .iter()
                .find(|object| object.id() == *object_id)
                .ok_or(StoreError::Corrupt)?;
            store.write_object(object)?;
        }
        // The physical pack is an authenticated index of the checked closure.
        // Object bytes are written once by the store's closure typestate;
        // `verify_workspace_pack` repeats the fixed index derivation on every
        // recovery and publication ambiguity check.
        let (layout, pack) = super::pack::workspace_pack(self, MAX_RECORD_BYTES)?;
        let pack_id = store.write_pack(&pack)?;
        // Pass the checked closure capability intact. Reconstructing it from
        // its logical manifest would discard the private path-copy frontier
        // needed to publish newly split relation children with their parent.
        // `PreparedTransition` already bound this closure to the exact
        // manifest, delta, and commit; the store re-admits its object grammar
        // and root/closure binding before preparing the durable write.
        store.prepare_workspace_publication(
            self.manifest.root(),
            layout,
            pack_id,
            self.closure.clone(),
            base,
        )
    }
}

/// Canonical bytes supplied to the model at restart.  The owner never turns
/// this value directly into a head: the model must re-admit it against typed
/// relation states and the store closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedTransition {
    request: [u8; 32],
    transaction: TransactionId,
    manifest: Arc<[u8]>,
    delta: Arc<[u8]>,
    commit: Arc<[u8]>,
    closure: Arc<ClosureManifest>,
    closure_bytes: Arc<[u8]>,
}

impl PersistedTransition {
    pub(super) fn from_parts(
        request: [u8; 32],
        transaction: TransactionId,
        manifest: Box<[u8]>,
        delta: Box<[u8]>,
        commit: Box<[u8]>,
        closure: ClosureManifest,
        closure_bytes: Box<[u8]>,
    ) -> Self {
        Self {
            request,
            transaction,
            manifest: Arc::from(manifest),
            delta: Arc::from(delta),
            commit: Arc::from(commit),
            closure: Arc::new(closure),
            closure_bytes: Arc::from(closure_bytes),
        }
    }

    /// Returns the request identity carried by the record.
    #[must_use]
    pub const fn request(&self) -> [u8; 32] {
        self.request
    }

    /// Returns the deterministic transaction identity carried by the record.
    #[must_use]
    pub const fn transaction(&self) -> TransactionId {
        self.transaction
    }

    /// Returns the canonical manifest bytes for typed admission.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest
    }

    /// Returns the canonical delta bytes for typed admission.
    #[must_use]
    pub fn delta_bytes(&self) -> &[u8] {
        &self.delta
    }

    /// Reads the fixed identities of the durable delta without copying its
    /// potentially large canonical change body.
    ///
    /// # Errors
    ///
    /// Returns a bounded workspace decode error when the stored envelope is
    /// malformed or exceeds the engine record limit.
    pub fn delta_header(
        &self,
    ) -> Result<backend_version::UntrustedWorkspaceDeltaHeader, backend_version::WorkspaceDecodeError>
    {
        WorkspaceDelta::decode_persisted_header(&self.delta, MAX_RECORD_BYTES)
    }

    /// Returns the canonical commit bytes for typed admission.
    #[must_use]
    pub fn commit_bytes(&self) -> &[u8] {
        &self.commit
    }

    /// Returns the admitted immutable closure manifest.
    #[must_use]
    pub fn closure_manifest(&self) -> &ClosureManifest {
        &self.closure
    }

    /// Opens a relation root referenced by this persisted transition through
    /// an owned store-backed capability. The caller supplies only an
    /// untrusted fixed-width root; the relation schema, canonical node bytes,
    /// and root digest are admitted before a handle is returned.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn relation<R: backend_version::CanonicalRelation>(
        &self,
        store: &FileStore,
        root: [u8; 32],
    ) -> Result<WorkspaceRelationHandle<R>, WorkspaceRelationError> {
        WorkspaceRelationHandle::open(Arc::new(store.clone()), root)
    }

    pub(super) fn closure_bytes(&self) -> &[u8] {
        &self.closure_bytes
    }
}
