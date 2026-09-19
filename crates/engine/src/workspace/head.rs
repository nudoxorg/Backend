//! Immutable checked workspace head and snapshots.

use super::lazy::{WorkspaceRelationError, WorkspaceRelationHandle};
use super::owner::WorkspaceError;
use super::transition::{PreparedTransition, TransactionId, TransitionWork};
use crate::journal::ChainHash;
use crate::schema::{RecordId, WorkspaceLog};
use backend_store::{FileStore, ObjectId, RelationAdmissionRegistry, WorkspaceClosure};
use backend_version::{
    CheckedCommit, CommitProvenance, ObjectClosure as VersionObjectClosure, WorkspaceManifest,
    WorkspaceRoot, commit_capability, commit_checked,
};
use std::sync::Arc;

/// Exact selected head expectation retained by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeadExpectation {
    root: WorkspaceRoot,
    sequence: u64,
}

impl HeadExpectation {
    /// Creates an expectation from an admitted root and sequence.
    #[must_use]
    pub const fn new(root: WorkspaceRoot, sequence: u64) -> Self {
        Self { root, sequence }
    }

    /// Returns the admitted root.
    #[must_use]
    pub const fn root(self) -> WorkspaceRoot {
        self.root
    }

    /// Returns the selected sequence.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// Immutable state retained by a selected head and all snapshots derived from
/// it.  The large checked closure is shared by `Arc`; taking a base guard or a
/// model snapshot therefore never copies the closure's object bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
struct HeadState {
    transition: Arc<PreparedTransition>,
    sequence: u64,
    journal_sequence: u64,
    chain: ChainHash<WorkspaceLog>,
    record: RecordId<WorkspaceLog>,
    owner_epoch: u64,
}

/// Snapshot exposed to a pure workspace model.  Every state value is already
/// a checked backend-version/store capability.  Cloning a snapshot is an
/// O(1) immutable-state guard operation.
#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    state: Arc<HeadState>,
    store: Option<Arc<FileStore>>,
}

impl PartialEq for WorkspaceSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

impl Eq for WorkspaceSnapshot {}

impl WorkspaceSnapshot {
    /// Returns the checked visible root derived from the manifest.
    #[must_use]
    pub fn root(&self) -> WorkspaceRoot {
        self.state.transition.target()
    }

    /// Returns the checked workspace root immediately preceding this
    /// snapshot's selected transition.
    #[must_use]
    pub fn transition_base(&self) -> WorkspaceRoot {
        self.state.transition.base()
    }

    /// Returns the exact checked base and target roots for one relation lane
    /// in this snapshot's selected transition. The roots are copied from the
    /// checked base/target manifests, so callers cannot substitute a root
    /// merely because its workspace digest happens to match.
    #[must_use]
    pub fn transition_relation_roots<R: backend_version::CanonicalRelation>(
        &self,
    ) -> Option<(
        [u8; backend_version::ID_BYTES],
        [u8; backend_version::ID_BYTES],
    )> {
        let schema = backend_version::SchemaIdentity::of_relation::<R>();
        let base = self
            .state
            .transition
            .base_manifest()
            .relations()
            .iter()
            .find(|binding| binding.schema() == schema)
            .map(|binding| binding.root());
        let target = self
            .manifest()
            .relations()
            .iter()
            .find(|binding| binding.schema() == schema)
            .map(|binding| binding.root());
        base.zip(target)
    }

    /// Returns the checked workspace manifest.
    #[must_use]
    pub fn manifest(&self) -> &WorkspaceManifest {
        self.state.transition.manifest()
    }

    /// Returns the checked history commit.
    #[must_use]
    pub fn commit(&self) -> &CheckedCommit {
        self.state.transition.commit()
    }

    /// Returns the complete immutable object closure.
    #[must_use]
    pub fn closure(&self) -> &WorkspaceClosure {
        self.state.transition.closure()
    }

    /// Returns the durable publication sequence.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.state.sequence
    }

    /// Returns the owner epoch that selected this snapshot.
    #[must_use]
    pub fn owner_epoch(&self) -> u64 {
        self.state.owner_epoch
    }

    /// Returns the compact checked work facts recorded for this publication.
    /// Product planners use these facts to choose a delta or scoped rebuild;
    /// they never infer change size from a caller supplied root label.
    #[must_use]
    pub fn transition_work(&self) -> TransitionWork {
        self.state.transition.work()
    }

    pub(crate) fn with_store(mut self, store: Arc<FileStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Opens one relation selected by the checked workspace manifest through
    /// the owner's authenticated lazy node loader. The returned capability
    /// owns the store clone and never borrows mutable owner state.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn relation<R: backend_version::CanonicalRelation>(
        &self,
    ) -> Result<WorkspaceRelationHandle<R>, WorkspaceRelationError> {
        let store = self
            .store
            .clone()
            .ok_or(WorkspaceRelationError::StoreUnavailable)?;
        let schema = backend_version::SchemaIdentity::of_relation::<R>();
        let binding = self
            .manifest()
            .relations()
            .iter()
            .find(|binding| binding.schema() == schema)
            .ok_or(WorkspaceRelationError::MissingRelation)?;
        WorkspaceRelationHandle::open(store, binding.root())
    }

    /// Alias emphasizing the lazy, read-only relation capability.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn lazy_relation<R: backend_version::CanonicalRelation>(
        &self,
    ) -> Result<WorkspaceRelationHandle<R>, WorkspaceRelationError> {
        self.relation::<R>()
    }
}

/// Checked selected workspace head.
///
/// A head is an immutable pointer.  Cloning it for a publication base, a
/// snapshot, or a recovery candidate shares the complete checked closure and
/// canonical encodings through one `Arc<HeadState>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceHead {
    state: Arc<HeadState>,
}

impl WorkspaceHead {
    /// Creates a checked genesis head.  The root and commit are derived from
    /// the admitted manifest and a deterministic genesis transaction.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn genesis(
        manifest: WorkspaceManifest,
        closure: WorkspaceClosure,
    ) -> Result<Self, WorkspaceError> {
        Self::genesis_with_registry(manifest, closure, &RelationAdmissionRegistry::default())
    }

    /// Creates a checked genesis head using the store's relation admission
    /// registry.  Custom product relations must use this entry point so the
    /// initial closure is validated with the same canonical decoders used by
    /// later publications and recovery.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the manifest, closure, bootstrap
    /// transition, or relation objects fail checked admission.
    pub fn genesis_with_registry(
        manifest: WorkspaceManifest,
        closure: WorkspaceClosure,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        if !manifest.is_checked() || manifest.root() != closure.root() {
            return Err(WorkspaceError::UnverifiedManifest);
        }
        let root = manifest.root();
        let request = [0; 32];
        let transaction = TransactionId::derive(0, root, request, 0);
        let authority = manifest
            .authority_closure()
            .ok_or(WorkspaceError::UnverifiedManifest)?;
        let provenance = CommitProvenance::new(
            authority,
            VersionObjectClosure::from_version(transaction.version()),
            b"backend.engine.genesis.v1".to_vec(),
        );
        let commit = commit_checked(&manifest, Vec::new(), provenance)
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        // A genesis transition has no relation delta.  Constructing it through
        // the owner is unnecessary; the checked head keeps the same typed
        // target/commit/closure tuple and uses a private bootstrap marker.
        let delta = backend_version::workspace_delta(&manifest, &manifest, Vec::new())
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        let checked_delta = delta.into_checked();
        let checked_commit = commit_capability(
            &manifest,
            commit.parents().to_vec(),
            CommitProvenance::new(
                commit.authority(),
                commit.transaction(),
                commit.provenance().to_vec(),
            ),
        )
        .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        let base_manifest = manifest.clone();
        let transition = PreparedTransition::from_checked_with_registry(
            crate::workspace::transition::CheckedTransitionInput {
                request,
                transaction,
                base: &base_manifest,
                manifest,
                delta: checked_delta,
                commit: checked_commit,
                closure,
                registry,
            },
        )?;
        Ok(Self {
            state: Arc::new(HeadState {
                transition: Arc::new(transition),
                sequence: 0,
                journal_sequence: 0,
                chain: ChainHash::genesis(),
                record: RecordId::from_payload(&[]),
                owner_epoch: 0,
            }),
        })
    }

    pub(super) fn from_transition(
        transition: PreparedTransition,
        sequence: u64,
        journal_sequence: u64,
        chain: ChainHash<WorkspaceLog>,
        record: RecordId<WorkspaceLog>,
        owner_epoch: u64,
    ) -> Self {
        Self {
            state: Arc::new(HeadState {
                transition: Arc::new(transition),
                sequence,
                journal_sequence,
                chain,
                record,
                owner_epoch,
            }),
        }
    }

    pub(super) fn from_shared_transition(
        transition: Arc<PreparedTransition>,
        sequence: u64,
        journal_sequence: u64,
        chain: ChainHash<WorkspaceLog>,
        record: RecordId<WorkspaceLog>,
        owner_epoch: u64,
    ) -> Self {
        Self {
            state: Arc::new(HeadState {
                transition,
                sequence,
                journal_sequence,
                chain,
                record,
                owner_epoch,
            }),
        }
    }

    pub(super) fn with_owner_epoch(&self, owner_epoch: u64) -> Self {
        let mut state = (*self.state).clone();
        state.owner_epoch = owner_epoch;
        Self {
            state: Arc::new(state),
        }
    }

    /// Returns the derived workspace root.
    #[must_use]
    pub fn root(&self) -> WorkspaceRoot {
        self.state.transition.target()
    }

    /// Returns the checked target manifest.
    #[must_use]
    pub fn manifest(&self) -> &WorkspaceManifest {
        self.state.transition.manifest()
    }

    /// Returns the checked commit.
    #[must_use]
    pub fn commit(&self) -> &CheckedCommit {
        self.state.transition.commit()
    }

    /// Returns the complete checked closure.
    #[must_use]
    pub fn closure(&self) -> &WorkspaceClosure {
        self.state.transition.closure()
    }

    pub(super) fn request(&self) -> [u8; 32] {
        self.state.transition.request()
    }

    pub(super) fn transition_shared(&self) -> Arc<PreparedTransition> {
        Arc::clone(&self.state.transition)
    }

    pub(super) fn catalog_descriptor(&self) -> Option<ObjectId> {
        self.state.transition.catalog_descriptor()
    }

    /// Returns the monotonic visible sequence.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.state.sequence
    }

    /// Returns the owner epoch.
    #[must_use]
    pub fn owner_epoch(&self) -> u64 {
        self.state.owner_epoch
    }

    /// Returns the compact checked work facts recorded for this head.
    #[must_use]
    pub fn transition_work(&self) -> TransitionWork {
        self.state.transition.work()
    }

    /// Returns the chain tip selected by the head.
    #[must_use]
    pub fn chain(&self) -> ChainHash<WorkspaceLog> {
        self.state.chain
    }

    /// Returns the exact journal sequence of the selected frame.
    #[must_use]
    pub fn journal_sequence(&self) -> u64 {
        self.state.journal_sequence
    }

    /// Returns the checked caller expectation.
    #[must_use]
    pub fn expectation(&self) -> HeadExpectation {
        HeadExpectation::new(self.root(), self.sequence())
    }

    /// Creates a model snapshot.
    #[must_use]
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            state: Arc::clone(&self.state),
            store: None,
        }
    }
}
