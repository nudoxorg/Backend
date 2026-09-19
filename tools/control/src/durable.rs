//! Crash-safe publication and restart recovery for the control relation.
//!
//! `FileStore` owns immutable packs, relation node CAS, closure admission, and
//! the selected workspace head. This adapter supplies the control relation's
//! typed workspace manifest and keeps the in-memory scheduler root equal to
//! the last atomically selected durable root.

use std::path::Path;

use backend_store::{
    FileStore, LayoutId, OrderedMap, PackId, RelationAdmissionRegistry, encode_pack,
};

use crate::ControlError;
use crate::ledger::{ControlCommit, SchedulerLimits};
use crate::record::{ControlRelation, ControlRoot};

#[path = "durable_closure.rs"]
mod closure;
#[path = "durable_lazy.rs"]
mod lazy;

use lazy::{LazyControlPage, LazyControlPlane};

/// Default physical pack envelope used by the command adapter.
pub const DEFAULT_MAX_PACK_BYTES: usize = 4 * 1024 * 1024;
/// Maximum rows returned by one status page.
pub const MAX_STATUS_PAGE: usize = 256;

pub(super) const AUTHORITY_VALUE: u64 = 1;
const PACK_LAYOUT_DESCRIPTOR: &[u8] = b"backend-control-pack-v1";

/// A bounded status page from the selected control relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlPage {
    records: Vec<crate::WorkRecord>,
    next: Option<crate::ids::AgentWorkKey>,
}

impl ControlPage {
    fn from_lazy(page: LazyControlPage) -> Self {
        let (records, next) = page.into_parts();
        Self { records, next }
    }

    /// Returns the rows in canonical work-key order.
    #[must_use]
    pub fn records(&self) -> &[crate::WorkRecord] {
        &self.records
    }

    /// Returns the cursor to pass as `after` for the next page.
    #[must_use]
    pub const fn next(&self) -> Option<crate::ids::AgentWorkKey> {
        self.next
    }
}

/// Durable local-first control plane.
#[derive(Clone, Debug)]
pub struct DurableControlPlane {
    state: LazyControlPlane,
}

impl DurableControlPlane {
    /// Opens a control ledger and recovers its selected workspace root.
    ///
    /// # Errors
    ///
    /// Returns a typed store, bounds, authority, coverage, or corruption
    /// error when the ledger cannot be admitted.
    pub fn open(
        path: impl AsRef<Path>,
        max_pack_bytes: usize,
        limits: SchedulerLimits,
    ) -> Result<Self, ControlError> {
        if max_pack_bytes == 0 {
            return Err(ControlError::Bounds);
        }
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<ControlRelation>()
            .map_err(ControlError::from)?;
        let store = FileStore::open_with_registry(path, max_pack_bytes, registry)?;
        let authority = store
            .acquire_publication_authority()
            .map_err(|error| ControlError::Store(error.to_string()))?;
        let head = store.head()?;
        let (layout, pack) = if let Some(head) = head {
            let descriptor = head.descriptor();
            let expected_layout = LayoutId::derive(PACK_LAYOUT_DESCRIPTOR);
            if descriptor.layout() != expected_layout {
                return Err(ControlError::Corrupt);
            }
            if store.read_pack(descriptor.pack())?.layout() != expected_layout {
                return Err(ControlError::Corrupt);
            }
            (expected_layout, descriptor.pack())
        } else {
            ensure_pack(&store, max_pack_bytes)?
        };
        let state = if let Some(head) = head {
            LazyControlPlane::open(
                store.clone(),
                authority.clone(),
                &head,
                layout,
                pack,
                limits,
            )?
        } else {
            LazyControlPlane::new(store.clone(), authority.clone(), layout, pack, limits)?
        };
        Ok(Self { state })
    }

    /// Returns the exact selected control relation root.
    #[must_use]
    pub const fn root(&self) -> ControlRoot {
        self.state.root()
    }

    /// Reads one row through the selected relation root.
    ///
    /// # Errors
    ///
    /// Returns a durable store error when the selected relation node cannot
    /// be read or admitted.
    pub fn get(
        &self,
        key: crate::ids::AgentWorkKey,
    ) -> Result<Option<crate::WorkRecord>, ControlError> {
        self.state.get(key)
    }

    /// Reads one bounded status page without reopening all history.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] for an empty or oversized page, or a
    /// durable store error when the selected relation cannot be read.
    pub fn page(
        &self,
        after: Option<crate::ids::AgentWorkKey>,
        limit: usize,
    ) -> Result<ControlPage, ControlError> {
        if limit == 0 || limit > MAX_STATUS_PAGE {
            return Err(ControlError::Bounds);
        }
        self.state.page(after, limit).map(ControlPage::from_lazy)
    }

    /// Reads one bounded status page in canonical work-key order.
    ///
    /// # Errors
    ///
    /// Returns the same bounds or durable store errors as [`Self::page`].
    pub fn status_page(
        &self,
        after: Option<crate::ids::AgentWorkKey>,
        limit: usize,
    ) -> Result<ControlPage, ControlError> {
        self.page(after, limit)
    }

    /// Plans one work specification and publishes a newly admitted row.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle or durable publication error.
    pub fn plan(&mut self, spec: crate::WorkSpec) -> Result<crate::PlanResult, ControlError> {
        self.state.plan(spec)
    }

    /// Acquires an attempt and publishes any waiter/status transition.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, fencing, bounds, or durable publication error.
    pub fn admit(
        &mut self,
        key: crate::ids::AgentWorkKey,
        owner: crate::ids::Identity<crate::ids::OwnerSchema>,
        now: u64,
    ) -> Result<(crate::Admission, Option<ControlCommit>), ControlError> {
        self.state.admit(key, owner, now)
    }

    /// Renews an exact fenced attempt and publishes its new expiry.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, lifecycle, or durable publication error.
    pub fn renew<S: crate::ActiveLeaseMarker>(
        &mut self,
        lease: &crate::Lease<S>,
        now: u64,
    ) -> Result<(crate::Lease<crate::Renewed>, ControlCommit), ControlError> {
        self.state.renew(lease, now)
    }

    /// Freezes a candidate and publishes its controller receipt.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, lifecycle, identity, or durable
    /// publication error.
    pub fn freeze<S: crate::ActiveLeaseMarker>(
        &mut self,
        lease: &crate::Lease<S>,
        output: crate::ids::Identity<crate::ids::OutputSchema>,
        evidence: crate::ids::Identity<crate::ids::EvidenceSchema>,
        now: u64,
    ) -> Result<(crate::Lease<crate::Frozen>, ControlCommit), ControlError> {
        self.state.freeze(lease, output, evidence, now)
    }

    /// Publishes an independent evaluator verdict over a frozen candidate.
    ///
    /// # Errors
    ///
    /// Returns an independence, lifecycle, identity, or durable publication
    /// error.
    pub fn evaluate(
        &mut self,
        lease: &crate::Lease<crate::Frozen>,
        evaluator: crate::ids::Identity<crate::ids::OwnerSchema>,
        verdict: crate::CustodyVerdict,
        evidence: crate::ids::Identity<crate::ids::EvidenceSchema>,
        now: u64,
    ) -> Result<crate::EvaluationResult, ControlError> {
        self.state
            .evaluate(lease, evaluator, verdict, evidence, now)
    }

    /// Publishes an independent reviewer verdict over an accepted evaluation.
    ///
    /// # Errors
    ///
    /// Returns an independence, stale-custody, lifecycle, identity, or durable
    /// publication error.
    pub fn review(
        &mut self,
        candidate: &crate::Candidate<crate::Evaluated>,
        reviewer: crate::ids::Identity<crate::ids::OwnerSchema>,
        verdict: crate::CustodyVerdict,
        evidence: crate::ids::Identity<crate::ids::EvidenceSchema>,
    ) -> Result<crate::ReviewResult, ControlError> {
        self.state.review(candidate, reviewer, verdict, evidence)
    }

    /// Publishes Sol's final decision over an accepted review.
    ///
    /// # Errors
    ///
    /// Returns an independence, stale-custody, lifecycle, identity, or durable
    /// publication error.
    pub fn decide(
        &mut self,
        candidate: &crate::Candidate<crate::Reviewed>,
        sol: crate::ids::Identity<crate::ids::OwnerSchema>,
        verdict: crate::CustodyVerdict,
        evidence: crate::ids::Identity<crate::ids::EvidenceSchema>,
    ) -> Result<crate::DecisionResult, ControlError> {
        self.state.decide(candidate, sol, verdict, evidence)
    }

    /// Releases a failed attempt for bounded retry.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, lifecycle, or durable publication error.
    pub fn fail<S: crate::ActiveLeaseMarker>(
        &mut self,
        lease: &crate::Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        self.state.fail(lease)
    }

    /// Cancels an active attempt and publishes the terminal cancellation.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, lifecycle, or durable publication error.
    pub fn cancel<S: crate::ActiveLeaseMarker>(
        &mut self,
        lease: &crate::Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        self.state.cancel(lease)
    }

    /// Reaps expired leases and publishes one batched delta.
    ///
    /// # Errors
    ///
    /// Returns a corruption or durable publication error.
    pub fn recover(
        &mut self,
        now: u64,
    ) -> Result<(Vec<crate::ids::AgentWorkKey>, Option<ControlCommit>), ControlError> {
        self.state.recover(now)
    }

    /// Invalidates the complete transitive dependent slice through the
    /// reverse index.
    ///
    /// # Errors
    ///
    /// Returns a corruption or durable publication error.
    pub fn invalidate_dependency(
        &mut self,
        dependency: crate::ids::AgentWorkKey,
    ) -> Result<Option<ControlCommit>, ControlError> {
        self.state.invalidate_dependency(dependency)
    }

    /// Admits an immutable work-key claim against the selected relation.
    ///
    /// # Errors
    ///
    /// Returns a wire, unknown-work, or corruption error when the claim does
    /// not identify a row in the selected relation.
    pub fn lookup_work_key(&self, bytes: &[u8]) -> Result<crate::ids::AgentWorkKey, ControlError> {
        self.state.lookup_work_key(bytes)
    }

    /// Rehydrates a held lease after checking its current typed fence.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, unknown-work, or stale-fence error when the row
    /// is not currently held by the supplied fence.
    pub fn lease_for(
        &self,
        key: crate::ids::AgentWorkKey,
        fence: &[u8],
    ) -> Result<crate::Lease<crate::Held>, ControlError> {
        self.state.lease_for(key, fence)
    }

    /// Rehydrates a frozen lease after checking its current typed fence.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, unknown-work, or stale-fence error when the row
    /// is not frozen under the supplied fence.
    pub fn frozen_for(
        &self,
        key: crate::ids::AgentWorkKey,
        fence: &[u8],
    ) -> Result<crate::Lease<crate::Frozen>, ControlError> {
        self.state.frozen_for(key, fence)
    }

    /// Rehydrates an evaluated capability after checking its exact receipt.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, unknown-work, or stale-receipt error.
    pub fn evaluated_for(
        &self,
        key: crate::ids::AgentWorkKey,
        receipt: &[u8],
    ) -> Result<crate::Candidate<crate::Evaluated>, ControlError> {
        self.state.evaluated_for(key, receipt)
    }

    /// Rehydrates a reviewed capability after checking its exact receipt.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, unknown-work, or stale-receipt error.
    pub fn reviewed_for(
        &self,
        key: crate::ids::AgentWorkKey,
        receipt: &[u8],
    ) -> Result<crate::Candidate<crate::Reviewed>, ControlError> {
        self.state.reviewed_for(key, receipt)
    }
}

fn ensure_pack(
    store: &FileStore,
    max_pack_bytes: usize,
) -> Result<(LayoutId, PackId), ControlError> {
    let layout = LayoutId::derive(PACK_LAYOUT_DESCRIPTOR);
    let map = OrderedMap::try_empty().map_err(ControlError::from)?;
    let pack = encode_pack(&map, layout, max_pack_bytes).map_err(ControlError::from)?;
    let id = store.write_pack(&pack).map_err(ControlError::from)?;
    Ok((layout, id))
}
