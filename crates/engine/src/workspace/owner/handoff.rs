//! Exclusive writer transfer while the admitted read head remains available.
//!
//! Preparation and durable selection run on the holder of `WorkspaceWriter`.
//! The owner loop retains the old checked head and grants one exact candidate.
//! Nothing here clones a lease, reopens OWNER.lock, or shares a mutable owner.

use super::{
    Arc, CatalogState, DurablePublication, HeadExpectation, PublicationStatus, WorkspaceError,
    WorkspaceHead, WorkspaceModel, WorkspaceOwner,
};
use crate::workspace::catalog::state_from_durable_manifest;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Binding {
    base: HeadExpectation,
    closure: [u8; 32],
    epoch: u64,
    fence: [u8; 32],
    nonce: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Reservation {
    binding: Binding,
    granted: Option<WorkspaceCandidateClaim>,
}

/// A fixed-width observation of a privately prepared candidate. Its fields
/// are only minted by the exclusive writer; it does not authorize publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceCandidateClaim {
    binding: Binding,
    request: [u8; 32],
    target: backend_version::WorkspaceRoot,
    transaction: super::TransactionId,
    sequence: u64,
}

/// The sole owner's single-use approval of one prepared candidate. This is
/// deliberately neither Clone nor Copy. Cancellation loses arbitration once
/// this grant has been issued: the durable result must be reconciled/installed.
#[derive(Debug)]
pub struct PublishGrant {
    claim: WorkspaceCandidateClaim,
}

/// Privately durable immutable bytes, still unselected. Only a matching grant
/// can move this capability through the existing physical publication law.
#[derive(Debug)]
pub struct PreparedWorkspaceCandidate {
    claim: WorkspaceCandidateClaim,
    durable: DurablePublication,
}

impl PreparedWorkspaceCandidate {
    /// Borrows the exact compact claim to send to the owner for arbitration.
    #[must_use]
    pub const fn claim(&self) -> WorkspaceCandidateClaim {
        self.claim
    }
}

/// One exact selected result returned with its exclusively owned writer.
/// Creating this value never changes the serving owner's admitted head.
#[derive(Debug)]
pub struct CommittedWorkspaceCandidate {
    claim: WorkspaceCandidateClaim,
    status: PublicationStatus,
}

impl CommittedWorkspaceCandidate {
    /// Returns the existing post-selection acknowledgement status.
    #[must_use]
    pub const fn status(&self) -> PublicationStatus {
        self.status
    }
}

/// Worker-verified observation that a granted attempt did not select any new
/// root. This is not cancellation: the caller retains the real failure.
#[derive(Debug)]
pub struct UnselectedWorkspaceCandidate {
    claim: WorkspaceCandidateClaim,
}

/// Old checked head retained after an O(1) installation. Send it back to the
/// worker for retirement so a large last-Arc drop cannot stall the owner loop.
#[must_use]
#[derive(Debug)]
pub struct RetiredWorkspaceHead {
    _head: WorkspaceHead,
    _catalog: CatalogState,
}

/// The unique writer owns the actual kernel lease and diagnostic journal for
/// its entire lifetime. Its private owner is inaccessible to callers; no
/// competing write or GC can execute through the serving owner while reserved.
pub struct WorkspaceWriter<M: WorkspaceModel> {
    owner: WorkspaceOwner<M>,
    binding: Binding,
    prepared: Option<WorkspaceCandidateClaim>,
    granted: bool,
}

impl<M: WorkspaceModel> std::fmt::Debug for WorkspaceWriter<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceWriter")
            .field("binding", &self.binding)
            .field("prepared", &self.prepared)
            .field("granted", &self.granted)
            .finish()
    }
}

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Transfers the sole writer while retaining this exact admitted read
    /// head. The caller must keep the returned writer until it is reinstalled
    /// or explicitly returned after failure; dropping it releases the lease.
    ///
    /// # Errors
    /// Refuses a concurrent reservation, stale lease, or exhausted nonce.
    pub fn reserve_writer(&mut self) -> Result<WorkspaceWriter<M>, WorkspaceError> {
        self.writer()?.lease.assert_current()?;
        if self.handoff.is_some() {
            return Err(WorkspaceError::WriterReserved);
        }
        let nonce = self
            .next_handoff
            .checked_add(1)
            .ok_or(WorkspaceError::Bounds)?;
        let binding = Binding {
            base: self.head.expectation(),
            closure: *self.head.closure().membership_id().as_bytes(),
            epoch: self.lease.epoch(),
            fence: self.lease.fence(),
            nonce,
        };
        let writer = self.writer.take().ok_or(WorkspaceError::WriterReserved)?;
        self.next_handoff = nonce;
        self.handoff = Some(Reservation {
            binding,
            granted: None,
        });
        Ok(WorkspaceWriter {
            owner: WorkspaceOwner {
                model: Arc::clone(&self.model),
                directory: self.directory.clone(),
                lease: self.lease.clone(),
                writer: Some(writer),
                handoff: None,
                next_handoff: nonce,
                store: Arc::clone(&self.store),
                head: self.head.clone(),
                catalog: self.catalog.clone(),
                faults: Arc::clone(&self.faults),
                gc_roots: Arc::clone(&self.gc_roots),
                next_gc_pin: Arc::clone(&self.next_gc_pin),
            },
            binding,
            prepared: None,
            granted: false,
        })
    }

    /// Arbitrates cancellation against publication using only the retained
    /// checked head and private fixed-width binding. It performs no store HEAD
    /// read, fsync, relation hydration, or candidate projection.
    ///
    /// # Errors
    /// Refuses cancellation, foreign/stale claims, or a second grant.
    pub fn grant_candidate(
        &mut self,
        claim: WorkspaceCandidateClaim,
        cancellation: &AtomicBool,
    ) -> Result<PublishGrant, WorkspaceError> {
        let reservation = self.handoff.as_mut().ok_or(WorkspaceError::HeadConflict)?;
        if self.writer.is_some()
            || reservation.binding != claim.binding
            || reservation.granted.is_some()
            || self.head.expectation() != claim.binding.base
            || *self.head.closure().membership_id().as_bytes() != claim.binding.closure
            || self.lease.epoch() != claim.binding.epoch
            || self.lease.fence() != claim.binding.fence
        {
            return Err(WorkspaceError::HeadConflict);
        }
        if cancellation.load(Ordering::Acquire) {
            return Err(WorkspaceError::PublicationCancelled);
        }
        reservation.granted = Some(claim);
        Ok(PublishGrant { claim })
    }

    /// Installs a committed head and returns the prior head for off-thread
    /// retirement. All verification and installation is fixed-width/Arc work;
    /// durability and recovery occurred on the unique writer beforehand.
    ///
    /// # Errors
    /// Refuses a stale/foreign writer or result. On refusal the writer is
    /// returned to the caller, so the live lease is never discarded.
    pub fn install_candidate(
        &mut self,
        mut writer: WorkspaceWriter<M>,
        committed: CommittedWorkspaceCandidate,
    ) -> Result<RetiredWorkspaceHead, (WorkspaceWriter<M>, WorkspaceError)> {
        let valid = self.handoff.is_some_and(|reservation| {
            reservation.binding == writer.binding && reservation.granted == Some(committed.claim)
        }) && writer.binding == committed.claim.binding
            && self.writer.is_none()
            && self.head.expectation() == writer.binding.base
            && writer.owner.head.root() == committed.claim.target
            && writer.owner.head.request_identity() == committed.claim.request
            && writer.owner.head.sequence() == committed.claim.sequence;
        if !valid {
            return Err((writer, WorkspaceError::HeadConflict));
        }
        let authority = match writer.owner.writer.take() {
            Some(authority) => authority,
            None => return Err((writer, WorkspaceError::WriterReserved)),
        };
        let prior_head = std::mem::replace(&mut self.head, writer.owner.head.clone());
        let prior_catalog = std::mem::replace(&mut self.catalog, writer.owner.catalog.clone());
        self.writer = Some(authority);
        self.handoff = None;
        Ok(RetiredWorkspaceHead {
            _head: prior_head,
            _catalog: prior_catalog,
        })
    }

    /// Returns an unselected writer after preparation failure/cancellation.
    /// Once a grant was issued this route is closed: callers must reconcile
    /// physical publication and install the truthful selected result instead.
    ///
    /// # Errors
    /// Returns ownership intact for foreign, stale, or already granted work.
    pub fn return_unselected_writer(
        &mut self,
        mut writer: WorkspaceWriter<M>,
    ) -> Result<(), (WorkspaceWriter<M>, WorkspaceError)> {
        let valid = self.handoff.is_some_and(|reservation| {
            reservation.binding == writer.binding && reservation.granted.is_none()
        }) && self.writer.is_none()
            && self.head == writer.owner.head
            && !writer.granted;
        if !valid {
            return Err((writer, WorkspaceError::HeadConflict));
        }
        self.writer = writer.owner.writer.take();
        self.handoff = None;
        Ok(())
    }
    /// Returns the writer after a granted attempt was checked on the worker
    /// and proved still physically unselected. No store read runs on the actor.
    pub fn return_failed_writer(
        &mut self,
        mut writer: WorkspaceWriter<M>,
        unselected: UnselectedWorkspaceCandidate,
    ) -> Result<(), (WorkspaceWriter<M>, WorkspaceError)> {
        let valid = self.handoff.is_some_and(|reservation| {
            reservation.binding == writer.binding && reservation.granted == Some(unselected.claim)
        }) && self.writer.is_none()
            && writer.binding == unselected.claim.binding
            && self.head == writer.owner.head
            && writer.granted;
        if !valid {
            return Err((writer, WorkspaceError::HeadConflict));
        }
        self.writer = writer.owner.writer.take();
        self.handoff = None;
        Ok(())
    }
}

impl<M: WorkspaceModel> WorkspaceWriter<M> {
    /// Prepares and persists one candidate on the worker. Unwind is caught
    /// while borrowing this writer, so the lease remains available for return.
    /// No root is selected and the owner loop continues serving the old head.
    pub fn prepare(
        &mut self,
        intent: M::Intent,
    ) -> Result<PreparedWorkspaceCandidate, WorkspaceError> {
        if self.prepared.is_some() || self.granted {
            return Err(WorkspaceError::WriterReserved);
        }
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.owner
                .prepare(self.binding.base, intent)
                .and_then(|prepared| self.owner.durable(prepared))
        }))
        .map_err(|_| WorkspaceError::Model("workspace preparation worker panicked".to_owned()))??;
        let claim = WorkspaceCandidateClaim {
            binding: self.binding,
            request: prepared.data.transition.request(),
            target: prepared.target(),
            transaction: prepared.transaction(),
            sequence: if prepared.data.existing_published.is_some() {
                self.binding.base.sequence()
            } else {
                self.binding
                    .base
                    .sequence()
                    .checked_add(1)
                    .ok_or(WorkspaceError::Bounds)?
            },
        };
        self.prepared = Some(claim);
        Ok(PreparedWorkspaceCandidate {
            claim,
            durable: prepared,
        })
    }

    /// Consumes the single-use owner grant at the existing store publication
    /// linearization point. Cancellation is deliberately absent after grant.
    /// On error or unwind, reconcile on this same lease before classifying the
    /// result; a physically selected root can never become Cancelled.
    pub fn publish(
        &mut self,
        candidate: PreparedWorkspaceCandidate,
        grant: PublishGrant,
    ) -> Result<CommittedWorkspaceCandidate, WorkspaceError> {
        if self.granted
            || self.prepared != Some(candidate.claim)
            || grant.claim != candidate.claim
            || candidate.claim.binding != self.binding
        {
            return Err(WorkspaceError::HeadConflict);
        }
        self.granted = true;
        let claim = candidate.claim;
        let attempted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.owner.publish(candidate.durable)
        }));
        match attempted {
            Ok(Ok(published)) => Ok(CommittedWorkspaceCandidate {
                claim,
                status: published.status(),
            }),
            Ok(Err(error)) => self.reconcile(claim, error),
            Err(_) => self.reconcile(
                claim,
                WorkspaceError::Model("workspace publication worker panicked".to_owned()),
            ),
        }
    }

    /// Proves on the same exclusive lease that a failed granted attempt did
    /// not select a root. If the durable head changed, this refuses: callers
    /// must reconcile/install it instead of clearing the reservation.
    pub fn prove_unselected(&self) -> Result<UnselectedWorkspaceCandidate, WorkspaceError> {
        if !self.granted {
            return Err(WorkspaceError::HeadConflict);
        }
        let claim = self.prepared.ok_or(WorkspaceError::HeadConflict)?;
        self.owner.writer()?.lease.assert_current()?;
        let selected = self.owner.store.head().map_err(WorkspaceError::store)?;
        let matches = match selected {
            Some(head) => {
                let descriptor = head.descriptor();
                descriptor.target() == self.binding.base.root().to_bytes()
                    && descriptor.target_generation() == self.binding.base.sequence()
                    && *descriptor.closure().as_bytes() == self.binding.closure
            }
            None => self.binding.base.sequence() == 0,
        };
        if !matches {
            return Err(WorkspaceError::HeadConflict);
        }
        Ok(UnselectedWorkspaceCandidate { claim })
    }

    /// Builds a checked immutable candidate snapshot on the writer for view
    /// preparation. Its CAS objects are durable, but it confers no selection
    /// authority; only a grant and physical publication can select this head.
    pub fn candidate_snapshot(
        &self,
        candidate: &PreparedWorkspaceCandidate,
    ) -> Result<super::WorkspaceSnapshot, WorkspaceError> {
        if self.prepared != Some(candidate.claim) || candidate.claim.binding != self.binding {
            return Err(WorkspaceError::HeadConflict);
        }
        let head = WorkspaceHead::from_shared_transition(
            Arc::clone(&candidate.durable.data.transition),
            candidate.claim.sequence,
            0,
            super::ChainHash::genesis(),
            super::RecordId::from_payload(&[]),
            self.binding.epoch,
        );
        Ok(head.snapshot().with_store(Arc::clone(&self.owner.store)))
    }

    fn reconcile(
        &mut self,
        claim: WorkspaceCandidateClaim,
        prior_error: WorkspaceError,
    ) -> Result<CommittedWorkspaceCandidate, WorkspaceError> {
        self.owner.writer()?.lease.assert_current()?;
        let head = super::recover_store_head(
            &self.owner.store,
            self.owner.model.as_ref(),
            &self.owner.head,
            self.binding.epoch,
            &self.owner.faults,
        )?;
        if head.root() != claim.target
            || head.request_identity() != claim.request
            || head.sequence() != claim.sequence
        {
            return Err(prior_error);
        }
        let catalog = if let Some(descriptor) = head.catalog_descriptor() {
            let manifest = self
                .owner
                .store
                .open_closure(head.closure().membership_id())
                .map_err(WorkspaceError::store)?;
            state_from_durable_manifest(
                &self.owner.store,
                &manifest,
                descriptor,
                head.manifest().coverage(),
            )?
        } else {
            CatalogState::empty(head.manifest().coverage())
        };
        self.owner.head = head;
        self.owner.catalog = catalog;
        Ok(CommittedWorkspaceCandidate {
            claim,
            status: PublicationStatus::default().with_notification_pending(),
        })
    }
}
