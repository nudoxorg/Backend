//! Pinned, authenticated membership without hydrated object payloads.

use super::{ClosureId, ObjectId, StoreError, TypedObject};
use crate::{
    ArtifactClosureClaim, ClosureCompositionBudget, ClosureMembershipChange, DurableManifest,
    FileStore, PinnedStoredClosureReceipt,
};
use std::sync::Arc;

const MEMBER_PAGE_SIZE: usize = 128;

/// An exact durable closure index and the pins protecting its member objects.
///
/// Cloning this handle shares authenticated index evidence and its lifetime
/// pin. It never enumerates or hydrates the closure's object payloads.
#[derive(Clone, Debug)]
pub struct DurableClosureManifest {
    store: FileStore,
    index: DurableManifest,
    pin: Arc<MembershipPin>,
    budget: ClosureCompositionBudget,
    cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
    allocation_budget: Option<crate::PhysicalAllocationBudget>,
}

#[derive(Debug)]
struct MembershipPin {
    receipt: crate::StoredClosureReceipt,
    _lease: crate::durable::ClosureMembershipLease,
}

impl PartialEq for DurableClosureManifest {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id() && self.object_count() == other.object_count()
    }
}

impl Eq for DurableClosureManifest {}

impl DurableClosureManifest {
    /// Opens the exact index admitted by an affine storage receipt.
    ///
    /// The receipt is consumed and retained, rather than replaced by a raw
    /// closure ID or an unpinned cloned metadata description.
    pub fn from_pinned(
        store: &FileStore,
        receipt: PinnedStoredClosureReceipt,
        budget: ClosureCompositionBudget,
    ) -> Result<Self, StoreError> {
        let reader_pin = store.pin_garbage_collection()?;
        let admitted = receipt.receipt();
        let index = store.open_closure(admitted.closure())?;
        if index.id() != admitted.closure() || index.object_count() != admitted.object_count() {
            return Err(StoreError::Corrupt);
        }
        let lease = store.lease_closure_membership(index.id())?;
        // Registration is complete while both global admission barriers are
        // held. Every clone now retains an exact per-closure reachability
        // lease, allowing GC of unrelated interrupted admissions.
        drop(receipt);
        drop(reader_pin);
        Ok(Self {
            store: store.clone(),
            index,
            pin: Arc::new(MembershipPin {
                receipt: admitted,
                _lease: lease,
            }),
            budget,
            cancellation: None,
            allocation_budget: None,
        })
    }

    /// Associates the live admission's cancellation flag with bounded visits.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Retains an explicit actual filesystem-allocation policy through rebind
    /// and the durable pre-HEAD validation gate.
    pub fn with_physical_allocation_budget(
        mut self,
        budget: crate::PhysicalAllocationBudget,
    ) -> Result<Self, StoreError> {
        budget.admit(&self.store)?;
        self.allocation_budget = Some(budget);
        Ok(self)
    }

    pub(crate) fn validate_physical_allocation(&self) -> Result<(), StoreError> {
        self.validate_physical_allocation_in(&self.store)
    }

    pub(crate) fn validate_physical_allocation_in(
        &self,
        store: &FileStore,
    ) -> Result<(), StoreError> {
        if let Some(budget) = self.allocation_budget {
            budget.admit(store)?;
        }
        Ok(())
    }

    fn check_cancellation(&self) -> Result<(), StoreError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            Err(StoreError::Io(
                "workspace membership admission cancelled".to_owned(),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn rebind_controls(
        &self,
        previous: &super::ClosureManifest,
        next: &super::WorkspaceClosure,
    ) -> Result<Self, StoreError> {
        let controls = next.control_manifest();
        self.check_cancellation()?;
        if controls.objects().len() > 128 || previous.objects().len() > 128 {
            return Err(StoreError::Bounds);
        }
        self.store.stage_workspace_frontier(next)?;
        let mut changes = Vec::new();
        for object in previous.objects() {
            if !controls.contains_object_id(object.id()) {
                changes.push(ClosureMembershipChange::remove(object.id()));
            }
        }
        for object in controls.objects() {
            if !self.contains(object.id())? {
                changes.push(ClosureMembershipChange::add(object.id()));
            }
        }
        let claim = Some(ArtifactClosureClaim::from_id(self.id()));
        let receipt = if let Some(flag) = &self.cancellation {
            self.store.compose_workspace_closure_index_cancellable(
                claim,
                &changes,
                self.budget,
                flag,
            )?
        } else {
            self.store
                .compose_workspace_closure_index(claim, &changes, self.budget)?
        };
        let mut rebound = Self::from_pinned(&self.store, receipt, self.budget)?;
        rebound.cancellation.clone_from(&self.cancellation);
        rebound.allocation_budget = self.allocation_budget;
        rebound.validate_physical_allocation()?;
        Ok(rebound)
    }

    /// Extends an admitted evidence scope by only a bounded typed frontier.
    pub fn with_control_frontier(
        &self,
        next: &super::WorkspaceClosure,
    ) -> Result<Self, StoreError> {
        let empty = super::ClosureManifest::new(Vec::new())?;
        self.rebind_controls(&empty, next)
    }

    /// Returns the complete membership identity, without opening objects.
    #[must_use]
    pub fn id(&self) -> ClosureId {
        self.index.id()
    }

    /// Returns the authenticated complete member count.
    #[must_use]
    pub fn object_count(&self) -> u64 {
        self.index.object_count()
    }

    /// Classifies a control schema through this store's immutable admission
    /// registry, without reading any member objects or relation descendants.
    #[must_use]
    pub fn is_relation_schema(&self, schema: backend_version::SchemaIdentity) -> bool {
        self.store.relation_registry().contains_schema(schema)
    }

    /// Proves exact membership without reading an object's payload.
    pub fn contains(&self, id: ObjectId) -> Result<bool, StoreError> {
        self.index.contains_object_id(id)
    }

    /// Reads one member only after its exact index membership is proved.
    pub fn get(&self, id: ObjectId) -> Result<Option<TypedObject>, StoreError> {
        self.index.get(id)
    }

    /// Visits authenticated member IDs in canonical order with bounded pages.
    /// No object vector or payload hydration is performed.
    pub fn visit_ids(
        &self,
        mut visit: impl FnMut(ObjectId) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut after = None;
        let mut count = 0_u64;
        loop {
            self.check_cancellation()?;
            let page = self.index.page_ids(after, MEMBER_PAGE_SIZE)?;
            for &id in page.object_ids() {
                self.check_cancellation()?;
                if after.is_some_and(|previous| id <= previous) {
                    return Err(StoreError::Corrupt);
                }
                visit(id)?;
                after = Some(id);
                count = count.checked_add(1).ok_or(StoreError::Bounds)?;
                if count > self.object_count() {
                    return Err(StoreError::Corrupt);
                }
            }
            if page.next().is_none() {
                break;
            }
            if page.next() != after {
                return Err(StoreError::Corrupt);
            }
        }
        if count != self.object_count() {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }

    /// Returns the immutable receipt while this handle keeps its pin alive.
    #[must_use]
    pub fn receipt(&self) -> crate::StoredClosureReceipt {
        self.pin.receipt
    }
}
