//! Checked workspace transition and persisted publication envelope.

use super::lazy::{WorkspaceRelationError, WorkspaceRelationHandle};
use backend_store::{
    ClosureManifest, FileStore, ObjectId, RelationAdmissionRegistry, WorkspaceClosure,
};
use backend_version::{
    CheckedCommit, CheckedWorkspaceTransition, Commit, LazyTreeWork, ObjectVersion, Schema,
    WorkspaceDelta, WorkspaceManifest, WorkspaceRoot,
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
mod prepared;

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
