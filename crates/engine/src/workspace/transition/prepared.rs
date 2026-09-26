//! Admission and publication of one checked workspace transition.
//!
//! Transaction identity, payload schemas, and the persisted envelope stay
//! with the transition module. This module binds a checked commit to the
//! store closure the owner publishes.

use super::super::owner::WorkspaceError;
use super::super::pack;
use super::{
    CheckedTransitionInput, MAX_RECORD_BYTES, PersistedTransition, PreparedTransition,
    TRANSITION_PAYLOAD_COUNT, TransactionId, TransitionWork, consume_commit,
    materialize_transition_closure_with_ids,
};
use backend_store::{
    FileStore, ManifestChange, ObjectId, RelationAdmissionRegistry, StoreError, TypedObject,
    WorkspaceClosure,
};
use backend_version::{
    CheckedCommit, CheckedWorkspaceTransition, Commit, CommitProvenance,
    ObjectClosure as VersionObjectClosure, SchemaIdentity, WorkspaceDelta, WorkspaceManifest,
    WorkspaceRoot, commit_capability,
};
use std::sync::Arc;

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
    pub(in crate::workspace) fn with_catalog_descriptor(mut self, descriptor: ObjectId) -> Self {
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

    pub(in crate::workspace) const fn catalog_descriptor(&self) -> Option<ObjectId> {
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
    pub(in crate::workspace) fn payload_object_ids(&self) -> [ObjectId; TRANSITION_PAYLOAD_COUNT] {
        self.payloads
    }

    /// Returns the fixed, direct object frontier needed to reconstruct the
    /// model's typed closure after restart. Relation descendants remain in
    /// the store's authenticated CAS and are loaded through lazy handles.
    pub(in crate::workspace) fn auxiliary_object_ids(&self) -> &[ObjectId] {
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
        let (layout, pack) = pack::workspace_pack(self, MAX_RECORD_BYTES)?;
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
