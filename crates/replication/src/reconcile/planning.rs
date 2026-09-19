//! Reconciliation planning against a typed root summary.

use std::{collections::BTreeMap, fmt};

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    ReplicationError, SchemaWireObjectKey, SchemaWireObjectVersion, SparseCoverage, TransferId,
    TransportLimits, WorkspaceRoot,
};

use super::merkle::{
    MerkleDelta, MerkleDiffCursor, MerklePageSource, MerkleReconciler, MerkleRoot, ReconcileBudget,
};
use super::request::ObjectRequest;
use super::summary::RootSummary;

/// Starts bounded Merkle closure reconciliation for two exact immutable
/// roots. The returned cursor can be retained across transport reconnects;
/// no flat root map is materialized by this path.
///
/// # Errors
///
/// Returns a schema or budget error when the roots cannot be reconciled under
/// the supplied traversal limits.
pub fn reconcile_merkle(
    local_root: MerkleRoot,
    remote_root: MerkleRoot,
    budget: ReconcileBudget,
) -> Result<MerkleReconciler, ReplicationError> {
    MerkleReconciler::new(local_root, remote_root, budget)
}

/// One missing or changed immutable object discovered by closure
/// reconciliation. The key and version remain wire claims until the runtime
/// admits them against its own typed object registry.
pub struct ClosureObjectRequest<T: Schema = crate::ImmutableObjectSchema> {
    /// Transfer identity allocated by the closure planner.
    pub transfer: TransferId,
    /// Untrusted logical object key bytes.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted complete object version claim.
    pub version: SchemaWireObjectVersion<T>,
    /// Canonical encoded object length.
    pub len: u64,
}
impl<T: Schema> Clone for ClosureObjectRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
        }
    }
}
impl<T: Schema> fmt::Debug for ClosureObjectRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClosureObjectRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .finish()
    }
}
impl<T: Schema> PartialEq for ClosureObjectRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
    }
}
impl<T: Schema> Eq for ClosureObjectRequest<T> {}
impl<T: Schema> ClosureObjectRequest<T> {
    /// Validates the claim contexts and transfer/object limits.
    ///
    /// # Errors
    ///
    /// Returns an identity, context, or object-size error when the request is
    /// malformed or exceeds `limits`.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        Ok(())
    }

    /// Admits this claim against exact typed object identities retained by the
    /// execution owner and creates a normal resumable object request.
    ///
    /// # Errors
    ///
    /// Returns an identity, context, bounds, or request-construction error
    /// when the wire claim differs from the typed object.
    pub fn admit_against(
        self,
        key: VersionObjectKey<T>,
        version: VersionObjectVersion<T>,
        limits: TransportLimits,
    ) -> Result<ObjectRequest<T>, ReplicationError> {
        self.validate(limits)?;
        let expected_key = crate::claim_schema_object_key(key).map_err(ReplicationError::from)?;
        let expected_version =
            crate::claim_schema_object_version(version).map_err(ReplicationError::from)?;
        if self.key != expected_key || self.version != expected_version {
            return Err(ReplicationError::IdentityMismatch);
        }
        ObjectRequest::whole(self.transfer, key, version, self.len, limits.max_ranges)
    }
}

/// A bounded changed-only closure page ready for object transfer dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureSyncPage<T: Schema = crate::ImmutableObjectSchema> {
    /// Remote root that all returned object claims are subordinate to.
    pub root: MerkleRoot,
    /// Changed leaf rows, retained for policy and diagnostics.
    pub deltas: Vec<MerkleDelta>,
    /// Wire-safe requests for remote rows not already present locally.
    pub requests: Vec<ClosureObjectRequest<T>>,
    /// Cursor for the next bounded closure page.
    pub next: Option<ClosureSyncCursor>,
}

/// A closure root that has completed Merkle reconciliation.
///
/// The root commitment itself is cheap to copy, but callers must not bind a
/// remote root into an invocation while a repair cursor is still live. This
/// capability can only be created by [`ClosureSync::into_bound_root`] after
/// every unequal subtree has been exhausted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BoundClosureRoot {
    root: MerkleRoot,
}
impl BoundClosureRoot {
    /// Returns the reconciled remote closure root.
    #[must_use]
    pub const fn root(self) -> MerkleRoot {
        self.root
    }
}

/// Durable continuation for remote prelude dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureSyncCursor {
    /// Merkle closure frontier and page offsets.
    pub merkle: MerkleDiffCursor,
    /// Next transfer id to allocate for a missing object.
    pub next_transfer: TransferId,
}

/// Local-first closure synchronizer used as a prelude to remote execution.
///
/// A warm root completes without requesting a byte. A changed root emits only
/// changed leaf rows and object requests; the invocation owner can transfer
/// those objects into its local CAS before sending a recipe request. The
/// cursor binds the exact remote root and remains bounded by `ReconcileBudget`.
pub struct ClosureSync<T: Schema = crate::ImmutableObjectSchema> {
    root: MerkleRoot,
    reconciler: MerkleReconciler,
    next_transfer: TransferId,
    limits: TransportLimits,
    marker: std::marker::PhantomData<fn() -> T>,
}
impl<T: Schema> fmt::Debug for ClosureSync<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClosureSync")
            .field("root", &self.root)
            .field("reconciler", &self.reconciler)
            .field("next_transfer", &self.next_transfer)
            .field("limits", &self.limits)
            .finish()
    }
}
impl<T: Schema> ClosureSync<T> {
    /// Starts a closure prelude for one exact remote root.
    ///
    /// # Errors
    ///
    /// Returns a schema, identifier, budget, or transport-limit error when the
    /// prelude cannot be initialized.
    pub fn new(
        local_root: MerkleRoot,
        remote_root: MerkleRoot,
        next_transfer: TransferId,
        budget: ReconcileBudget,
        limits: TransportLimits,
    ) -> Result<Self, ReplicationError> {
        limits.validate()?;
        if next_transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if remote_root.schema() != local_root.schema() {
            return Err(ReplicationError::IdentityContext);
        }
        Ok(Self {
            root: remote_root,
            reconciler: MerkleReconciler::new(local_root, remote_root, budget)?,
            next_transfer,
            limits,
            marker: std::marker::PhantomData,
        })
    }

    /// Restores a bounded closure prelude after reconnect or process restart.
    ///
    /// # Errors
    ///
    /// Returns a schema, cursor, identifier, budget, or limit error when the
    /// continuation is stale or exceeds the new negotiated bounds.
    pub fn from_cursor(
        cursor: ClosureSyncCursor,
        budget: ReconcileBudget,
        limits: TransportLimits,
    ) -> Result<Self, ReplicationError> {
        limits.validate()?;
        if cursor.next_transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        let (local_root, remote_root) = cursor.merkle.roots();
        if local_root.schema() != remote_root.schema() {
            return Err(ReplicationError::IdentityContext);
        }
        Ok(Self {
            root: remote_root,
            reconciler: MerkleReconciler::from_cursor(cursor.merkle, budget)?,
            next_transfer: cursor.next_transfer,
            limits,
            marker: std::marker::PhantomData,
        })
    }

    /// Returns a durable bounded continuation.
    #[must_use]
    pub fn cursor(&self) -> ClosureSyncCursor {
        ClosureSyncCursor {
            merkle: self.reconciler.cursor(),
            next_transfer: self.next_transfer,
        }
    }

    /// Returns the exact remote root commitment carried by this prelude.
    /// Use [`Self::into_bound_root`] before binding it to an invocation.
    #[must_use]
    pub const fn remote_root(&self) -> MerkleRoot {
        self.root
    }

    /// Consumes a completed prelude and returns a root capability suitable for
    /// invocation admission.
    ///
    /// Keeping this as a consuming transition prevents a caller from holding
    /// a mutable synchronizer and a supposedly final root at the same time.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Incomplete`] when any unequal closure
    /// subtree remains on the bounded continuation frontier.
    pub fn into_bound_root(self) -> Result<BoundClosureRoot, ReplicationError> {
        if !self.is_complete() {
            return Err(ReplicationError::Incomplete);
        }
        Ok(BoundClosureRoot { root: self.root })
    }

    /// Returns whether closure repair has completed and the root can be bound.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.reconciler.is_complete()
    }

    /// Traverses one bounded page and emits only remote rows absent or stale
    /// locally. No object payload is loaded by this prelude.
    ///
    /// # Errors
    ///
    /// Returns a source, proof, identity, transfer-id, or budget error when a
    /// closure page cannot be admitted or the request page exceeds its bound.
    pub fn step<L: MerklePageSource, R: MerklePageSource>(
        &mut self,
        local: &mut L,
        remote: &mut R,
        budget: ReconcileBudget,
    ) -> Result<ClosureSyncPage<T>, ReplicationError> {
        let page = self.reconciler.step(local, remote, budget)?;
        let deltas = page.deltas;
        let mut requests = Vec::new();
        for delta in &deltas {
            let Some(remote_object) = delta.remote.as_ref() else {
                continue;
            };
            if delta.key != remote_object.key {
                return Err(ReplicationError::IdentityMismatch);
            }
            // Merkle reconciliation already suppresses exact equality. The
            // explicit condition keeps this planner robust if a future
            // source emits a diagnostic equal row.
            if delta.local.as_ref() == Some(remote_object) {
                continue;
            }
            let transfer = self.next_transfer;
            self.next_transfer = TransferId::new(
                transfer
                    .get()
                    .checked_add(1)
                    .ok_or(ReplicationError::Overflow)?,
            )?;
            let key = crate::schema_object_key_claim::<T>(&remote_object.key_id)
                .map_err(ReplicationError::from)?;
            let version = crate::schema_object_version_claim::<T>(&remote_object.version)
                .map_err(ReplicationError::from)?;
            let request = ClosureObjectRequest {
                transfer,
                key,
                version,
                len: remote_object.len,
            };
            request.validate(self.limits)?;
            requests.push(request);
        }
        let next = page.next.map(|merkle| ClosureSyncCursor {
            merkle,
            next_transfer: self.next_transfer,
        });
        Ok(ClosureSyncPage {
            root: self.root,
            deltas,
            requests,
            next,
        })
    }
}

/// Result of exact object reconciliation under one workspace root.
pub struct Reconciliation<T: Schema = crate::ImmutableObjectSchema> {
    /// Missing or mismatched immutable objects.
    pub missing: Vec<ObjectRequest<T>>,
    /// Whether the exact workspace root is already selected locally.
    pub root_present: bool,
}
impl<T: Schema> Clone for Reconciliation<T> {
    fn clone(&self) -> Self {
        Self {
            missing: self.missing.clone(),
            root_present: self.root_present,
        }
    }
}
impl<T: Schema> fmt::Debug for Reconciliation<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reconciliation")
            .field("missing", &self.missing)
            .field("root_present", &self.root_present)
            .finish()
    }
}
impl<T: Schema> PartialEq for Reconciliation<T> {
    fn eq(&self, other: &Self) -> bool {
        self.missing == other.missing && self.root_present == other.root_present
    }
}
impl<T: Schema> Eq for Reconciliation<T> {}

/// Compares a typed root summary with local object versions.
///
/// # Errors
///
/// Returns an error when the root summary or supplied transport limits are
/// invalid.
pub fn reconcile<T: Schema>(
    root: &RootSummary<T>,
    local_root: Option<WorkspaceRoot>,
    local: &BTreeMap<VersionObjectKey<T>, VersionObjectVersion<T>>,
    transfer_seed: TransferId,
    limits: TransportLimits,
) -> Result<Reconciliation<T>, ReplicationError> {
    root.validate(limits)?;
    let empty = SparseCoverage::new(limits.max_ranges)?;
    let mut next_transfer = transfer_seed;
    let mut missing = Vec::new();
    for object in root.objects() {
        if local.get(&object.key) == Some(&object.version) {
            continue;
        }
        missing.push(ObjectRequest {
            transfer: next_transfer,
            key: object.key,
            version: object.version,
            len: object.len,
            ranges: empty.clone(),
        });
        next_transfer = TransferId::new(
            next_transfer
                .get()
                .checked_add(1)
                .ok_or(ReplicationError::Overflow)?,
        )?;
    }
    Ok(Reconciliation {
        missing,
        root_present: local_root == Some(root.workspace()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use super::super::merkle::{MerkleObject, MerklePage, MerklePageBody, NodeDigest, PageCursor};

    #[derive(Debug, PartialEq)]
    struct Bytes;
    impl Schema for Bytes {
        const DOMAIN: u8 = 0x7b;
        const TYPE: u16 = 31;
        type Value = [u8];

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    #[derive(Default)]
    struct Source {
        pages: BTreeMap<NodeDigest, MerklePage>,
    }
    impl MerklePageSource for Source {
        fn page(
            &mut self,
            request: super::super::merkle::MerklePageRequest,
        ) -> Result<MerklePage, ReplicationError> {
            self.pages
                .get(&request.node)
                .cloned()
                .ok_or(ReplicationError::CorruptFrame)
        }
    }

    fn budget() -> ReconcileBudget {
        ReconcileBudget {
            max_pages: 8,
            max_pending: 16,
            max_deltas: 8,
            max_page_items: 4,
            max_key_bytes: 32,
        }
    }

    fn limits() -> TransportLimits {
        TransportLimits {
            max_frame: 1024,
            max_chunk: 16,
            max_object: 64,
            max_objects: 16,
            max_ranges: 8,
            max_capabilities: 8,
            max_key_bytes: 32,
            max_inputs: 8,
        }
    }

    fn object(key: u8, version: u8, len: u64) -> MerkleObject {
        let value = vec![version; usize::try_from(len).expect("test length")];
        MerkleObject {
            key: vec![key],
            key_id: *VersionObjectKey::<Bytes>::from_value(&[key]).as_bytes(),
            version: *VersionObjectVersion::<Bytes>::from_value(&value).as_bytes(),
            len,
        }
    }

    #[test]
    fn warm_root_has_zero_object_requests() {
        let root = MerkleRoot::new(1, NodeDigest([1; 32]));
        let mut sync = ClosureSync::<Bytes>::new(
            root,
            root,
            TransferId::new(10).expect("transfer"),
            budget(),
            limits(),
        )
        .expect("sync");
        let mut local = Source::default();
        let mut remote = Source::default();
        let page = sync.step(&mut local, &mut remote, budget()).expect("step");
        assert!(page.deltas.is_empty());
        assert!(page.requests.is_empty());
        assert!(page.next.is_none());
        assert!(sync.is_complete());
        assert_eq!(sync.into_bound_root().expect("bound root").root(), root);
    }

    #[test]
    fn incomplete_prelude_cannot_bind_a_remote_root() {
        let local = MerkleRoot::new(1, NodeDigest([6; 32]));
        let remote = MerkleRoot::new(1, NodeDigest([7; 32]));
        let sync = ClosureSync::<Bytes>::new(
            local,
            remote,
            TransferId::new(10).expect("transfer"),
            budget(),
            limits(),
        )
        .expect("sync");
        assert_eq!(sync.into_bound_root(), Err(ReplicationError::Incomplete));
    }

    #[test]
    fn cold_root_and_one_node_delta_emit_only_remote_object_claims() {
        let local_node = NodeDigest([2; 32]);
        let remote_node = NodeDigest([3; 32]);
        let local_root = MerkleRoot::new(1, local_node);
        let remote_root = MerkleRoot::new(1, remote_node);
        let local_page = MerklePage {
            root: local_root,
            node: local_node,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(1, 1, 4)]),
        };
        let remote_page = MerklePage {
            root: remote_root,
            node: remote_node,
            level: 0,
            cursor: PageCursor::origin(),
            next: None,
            body: MerklePageBody::Leaf(vec![object(1, 9, 5), object(2, 2, 3)]),
        };
        let mut local = Source::default();
        local.pages.insert(local_node, local_page);
        let mut remote = Source::default();
        remote.pages.insert(remote_node, remote_page);
        let mut sync = ClosureSync::<Bytes>::new(
            local_root,
            remote_root,
            TransferId::new(10).expect("transfer"),
            budget(),
            limits(),
        )
        .expect("sync");
        let page = sync.step(&mut local, &mut remote, budget()).expect("step");
        assert_eq!(page.requests.len(), 2);
        assert_eq!(page.requests[0].transfer, TransferId::new(10).expect("id"));
        assert_eq!(page.requests[0].len, 5);
        assert_eq!(page.requests[1].len, 3);
        assert!(page.next.is_none());
        assert!(sync.is_complete());
        let typed_key = VersionObjectKey::<Bytes>::from_value(&[1]);
        let typed_version = VersionObjectVersion::<Bytes>::from_value(&[9; 5]);
        let request = page.requests[0]
            .clone()
            .admit_against(typed_key, typed_version, limits())
            .expect("typed request");
        assert_eq!(request.len, 5);
    }

    #[test]
    fn missing_or_corrupt_closure_page_never_marks_root_warm() {
        let local_node = NodeDigest([4; 32]);
        let remote_node = NodeDigest([5; 32]);
        let local_root = MerkleRoot::new(1, local_node);
        let remote_root = MerkleRoot::new(1, remote_node);
        let mut local = Source::default();
        local.pages.insert(
            local_node,
            MerklePage {
                root: local_root,
                node: local_node,
                level: 0,
                cursor: PageCursor::origin(),
                next: None,
                body: MerklePageBody::Leaf(vec![object(1, 1, 1)]),
            },
        );
        let mut remote = Source::default();
        let mut sync = ClosureSync::<Bytes>::new(
            local_root,
            remote_root,
            TransferId::new(10).expect("transfer"),
            budget(),
            limits(),
        )
        .expect("sync");
        assert_eq!(
            sync.step(&mut local, &mut remote, budget()),
            Err(ReplicationError::CorruptFrame)
        );
        assert!(!sync.is_complete());
    }
}
