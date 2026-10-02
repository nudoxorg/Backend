//! The leased-subscription protocol: Open, Resume, Credit, Ack, Renew, Cancel
//! and Page, run against a [`LeaseTable`] and a [`LeaseSource`].
//!
//! # What may keep a lease alive
//!
//! A lease's deadline moves in exactly three places, each of which proves the
//! holder is making progress:
//!
//! * `Open` and `Resume` grant a fresh term once the owner has produced the
//!   reply it will send;
//! * `Renew` fences the holder's exact cursor and grants a fresh term;
//! * a `Page` that is the *exact* pending continuation, whose page advances,
//!   and which encoded successfully, grants a fresh term.
//!
//! `Credit` and `Ack` never move a deadline, and a replayed, stale, forged or
//! failed request moves nothing. A reset is also bounded by a page budget and
//! an absolute window that no operation can extend, so a holder that drips one
//! page per term still cannot pin a root past the window.
//!
//! Every operation reads the clock when it begins and again when it commits.
//! Work that outlives a lease (a slow daemon round trip, a slow page encode)
//! therefore finds the lease gone instead of resurrecting it.

use super::super::subscription;
use super::state::{
    EventCredit, Failure, Hydration, Lease, LeasePhase, LeaseRefusal, PageCredit, PagePlan,
    ReleaseReason, ResetFault, ResetPage,
};
use super::table::LeaseTable;
use crate::protocol::{EngineStatus, ProtocolError};
use backend_client::lease_contract::LeaseMs;
use backend_engine::{
    Cursor, CursorEvent, LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResponse, SubscriptionReply, ViewPageCursor, ViewRoot,
};
use std::sync::Arc;
use std::time::Instant;

/// One encoded batch of events.
pub(crate) struct EventBatch {
    /// The cursor the batch starts from.
    pub(crate) previous: Box<[u8]>,
    /// The encoded subscription payload.
    pub(crate) payload: Box<[u8]>,
}

/// Everything the lease protocol needs from the daemon it fronts.
///
/// The lease rules never touch the daemon directly, so a test can script the
/// replies, make a page fail to encode, or move the clock mid-encode, and the
/// production owner supplies the real daemon through the same four calls.
pub(crate) trait LeaseSource {
    /// Asks the daemon what a subscriber at `cursor` should receive next.
    fn subscribe(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
    ) -> Result<SubscriptionReply, ProtocolError>;

    /// The daemon's current cursor.
    fn owner_cursor(&self) -> Result<Cursor, ProtocolError>;

    /// Certifies and encodes the events a subscriber at `requested` receives.
    fn event_batch(
        &self,
        requested: &[u8],
        credit: usize,
        target: &[u8],
        events: &[CursorEvent],
    ) -> Result<EventBatch, ProtocolError>;

    /// Builds, certifies and encodes one reset page. The default is the
    /// production path; it needs no daemon, only the retained root.
    fn reset_page(&self, plan: &PagePlan, credit: PageCredit) -> Result<ResetPage, ProtocolError> {
        production_reset_page(plan, credit)
    }
}

pub(crate) fn production_reset_page(
    plan: &PagePlan,
    credit: PageCredit,
) -> Result<ResetPage, ProtocolError> {
    let page = subscription::snapshot_page(
        &plan.root,
        plan.target,
        plan.reason,
        plan.requested,
        credit.get(),
    )?;
    let next = page.page().next();
    let token = page
        .next_token()
        .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
    let payload = backend_engine::encode_snapshot_page_dto(&page)
        .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
    let next = match (next, token) {
        (Some(cursor), Some(token)) => Some(super::state::Continuation { cursor, token }),
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => {
            return Err(ProtocolError::InvalidControl(
                "reset continuation omitted its token",
            ));
        }
    };
    Ok(ResetPage {
        served: page.page().cursor(),
        rows: page.page().rows().len(),
        next,
        payload: payload.into_boxed_slice(),
    })
}

/// Whether a reply attaches a brand-new lease or re-attaches a retained one.
#[derive(Clone, Copy)]
enum Attachment {
    Open(LocalSubscriptionId),
    Resume(LocalSubscriptionId),
}

impl Attachment {
    const fn lease(self) -> LocalSubscriptionId {
        match self {
            Self::Open(lease) | Self::Resume(lease) => lease,
        }
    }
}

/// The lease a request operates on, if it names one.
fn target_lease(operation: &LocalSubscriptionOperation) -> Option<LocalSubscriptionId> {
    match operation {
        LocalSubscriptionOperation::Open { .. } => None,
        LocalSubscriptionOperation::Resume { lease, .. }
        | LocalSubscriptionOperation::Credit { lease, .. }
        | LocalSubscriptionOperation::Ack { lease, .. }
        | LocalSubscriptionOperation::Renew { lease, .. }
        | LocalSubscriptionOperation::Cancel { lease }
        | LocalSubscriptionOperation::Page { lease, .. } => Some(*lease),
    }
}

/// Runs lease requests against one table and one daemon.
pub(crate) struct LeaseHost<'a, S> {
    table: &'a mut LeaseTable,
    source: &'a mut S,
}

impl<'a, S: LeaseSource> LeaseHost<'a, S> {
    pub(crate) fn new(table: &'a mut LeaseTable, source: &'a mut S) -> Self {
        Self { table, source }
    }

    /// Handles one correlated lease request.
    ///
    /// # Errors
    /// Returns the typed refusal or fault. A fault that makes a retained
    /// reset unservable has already released the lease it targeted.
    pub(crate) fn handle(
        &mut self,
        request_id: u64,
        request: LocalSubscriptionRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        if request.request_id != request_id {
            return Err(ProtocolError::InvalidControl(
                "subscription request correlation mismatch",
            ));
        }
        let target = target_lease(&request.operation);
        // Housekeeping first: whatever is due is gone before this request can
        // observe, count against, or be served by it.
        let began = self.table.now();
        self.table.reclaim_due(began);
        match self.dispatch(request_id, request.operation) {
            Ok(response) => Ok(EngineStatus::Subscription(response)),
            Err(failure) => {
                if let (Some(reason), Some(lease)) = (failure.release, target) {
                    self.table.release(lease, reason);
                }
                Err(failure.error)
            }
        }
    }

    fn dispatch(
        &mut self,
        request_id: u64,
        operation: LocalSubscriptionOperation,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        match operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } => self.open(request_id, &cursor, credit, lease_ms),
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.resume(request_id, lease, &cursor, credit, lease_ms),
            LocalSubscriptionOperation::Credit { lease, credit } => {
                self.credit(request_id, lease, credit)
            }
            LocalSubscriptionOperation::Ack { lease, cursor } => {
                self.acknowledge(request_id, lease, cursor)
            }
            LocalSubscriptionOperation::Renew {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.renew(request_id, lease, cursor, credit, lease_ms),
            LocalSubscriptionOperation::Cancel { lease } => self.cancel(request_id, lease),
            LocalSubscriptionOperation::Page {
                lease,
                page,
                credit,
            } => self.page(request_id, lease, page, credit),
        }
    }

    fn open(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let credit = EventCredit::new(credit).ok_or(LeaseRefusal::LeaseBounds)?;
        let term = self.table.grant_term(lease_ms)?;
        let at = self.table.now();
        self.table.reserve(at)?;
        let lease = self.table.allocate_id(request_id, cursor)?;
        let reply = self.source.subscribe(request_id, cursor, credit.get())?;
        self.attach(Attachment::Open(lease), request_id, cursor, term, reply)
    }

    fn resume(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let credit = EventCredit::new(credit).ok_or(LeaseRefusal::LeaseBounds)?;
        let term = self.table.grant_term(lease_ms)?;
        let at = self.table.now();
        self.table.active(lease, at)?.expect_resumable_at(cursor)?;
        let reply = self.source.subscribe(request_id, cursor, credit.get())?;
        self.attach(Attachment::Resume(lease), request_id, cursor, term, reply)
    }

    /// Turns the daemon's reply into the lease it grants and the response
    /// that tells the holder about it.
    fn attach(
        &mut self,
        attachment: Attachment,
        request_id: u64,
        requested: &[u8],
        term: LeaseMs,
        reply: SubscriptionReply,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let lease = attachment.lease();
        match reply {
            SubscriptionReply::Accepted { credit } => {
                let credit = granted_credit(credit)?;
                let cursor = self.source.owner_cursor()?.encode_control();
                self.commit(attachment, cursor.clone(), credit, term, |_| {
                    Ok(LeasePhase::Live)
                })?;
                let (credit, lease_ms) = (credit.get(), term.get());
                Ok(match attachment {
                    Attachment::Open(_) => LocalSubscriptionResponse::Opened {
                        request_id,
                        lease,
                        cursor,
                        credit,
                        lease_ms,
                    },
                    Attachment::Resume(_) => LocalSubscriptionResponse::Resumed {
                        request_id,
                        lease,
                        cursor,
                        credit,
                        lease_ms,
                    },
                })
            }
            SubscriptionReply::Events {
                credit,
                cursor: target,
                events,
            } => {
                let credit = granted_credit(credit)?;
                let batch = self
                    .source
                    .event_batch(requested, credit.get(), &target, &events)?;
                self.commit(attachment, target.clone(), credit, term, |_| {
                    Ok(LeasePhase::Live)
                })?;
                Ok(LocalSubscriptionResponse::Batch {
                    request_id,
                    lease,
                    previous: batch.previous,
                    cursor: target,
                    credit: credit.get(),
                    payload: batch.payload,
                })
            }
            SubscriptionReply::ResetWithRoot {
                credit,
                cursor: target,
                root,
                reason,
            } => {
                let credit = granted_credit(credit)?;
                let target_cursor = subscription::decode_cursor_for_service(&target, &root)?;
                let root: Arc<ViewRoot> = Arc::from(root);
                let plan = PagePlan {
                    root: Arc::clone(&root),
                    target: target_cursor,
                    reason,
                    requested: ViewPageCursor::first(&root),
                };
                let first = self
                    .source
                    .reset_page(&plan, PageCredit::clamped(credit))
                    .map_err(|error| Failure::reset(ResetFault::Unservable(error)))?;
                let limits = self.table.limits().reset();
                let next = first.next.as_ref().map(|next| next.token.clone());
                self.commit(attachment, target.clone(), credit, term, |granted_at| {
                    Hydration::begin_phase(granted_at, limits, root, target_cursor, reason, &first)
                        .map_err(Failure::reset)
                })?;
                Ok(LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next,
                    credit: credit.get(),
                    payload: first.payload,
                })
            }
            SubscriptionReply::Reset { .. } => Err(ProtocolError::InvalidControl(
                "subscription reset omitted its replacement root",
            )
            .into()),
        }
    }

    /// Stores the lease an `Open`/`Resume` granted. The term starts now, at
    /// commit, not when the request arrived: the holder was not told about the
    /// grant until the work that preceded it finished.
    fn commit(
        &mut self,
        attachment: Attachment,
        cursor: Box<[u8]>,
        credit: EventCredit,
        term: LeaseMs,
        phase: impl FnOnce(Instant) -> Result<LeasePhase, Failure>,
    ) -> Result<(), Failure> {
        let granted_at = self.table.now();
        if let Attachment::Resume(lease) = attachment {
            // The daemon round trip and the page encode may have outlived it.
            self.table.active(lease, granted_at)?;
        }
        let phase = phase(granted_at)?;
        let granted = Lease::grant(granted_at, term, cursor, credit, phase)?;
        match attachment {
            Attachment::Open(lease) => self.table.install(lease, granted)?,
            Attachment::Resume(lease) => {
                if !self.table.replace(lease, granted) {
                    return Err(LeaseRefusal::UnknownLease.into());
                }
            }
        }
        Ok(())
    }

    fn credit(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        credit: usize,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let credit = EventCredit::new(credit).ok_or(LeaseRefusal::CreditBounds)?;
        let at = self.table.now();
        let current = self.table.active(lease, at)?;
        let updated = current.with_added_credit(credit)?;
        let response = LocalSubscriptionResponse::Renewed {
            request_id,
            lease,
            cursor: updated.cursor().into(),
            credit: updated.credit().get(),
            lease_ms: updated.remaining_term(at).get(),
        };
        self.table.replace(lease, updated);
        Ok(response)
    }

    fn acknowledge(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let at = self.table.now();
        self.table
            .active(lease, at)?
            .expect_acknowledgeable(&cursor)?;
        Ok(LocalSubscriptionResponse::Acked {
            request_id,
            lease,
            cursor,
        })
    }

    fn renew(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let credit = EventCredit::new(credit).ok_or(LeaseRefusal::LeaseBounds)?;
        let term = self.table.grant_term(lease_ms)?;
        let at = self.table.now();
        let renewed = self
            .table
            .active(lease, at)?
            .renewed(at, &cursor, term, credit)?;
        self.table.replace(lease, renewed);
        Ok(LocalSubscriptionResponse::Renewed {
            request_id,
            lease,
            cursor,
            credit: credit.get(),
            lease_ms: term.get(),
        })
    }

    fn cancel(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        if self.table.release(lease, ReleaseReason::Cancelled) {
            Ok(LocalSubscriptionResponse::Cancelled { request_id, lease })
        } else {
            Err(LeaseRefusal::UnknownLease.into())
        }
    }

    /// Serves the next page of a pending reset.
    ///
    /// The order is the point. The exact token is checked first and a mismatch
    /// touches nothing. The page is then produced with no lease state changed,
    /// so a failure there releases the reset rather than leaving a
    /// half-advanced one. Only after the page exists, advances and has encoded
    /// is the clock read again and the lease granted a fresh term.
    fn page(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        token: Box<[u8]>,
        credit: usize,
    ) -> Result<LocalSubscriptionResponse, Failure> {
        let credit = PageCredit::new(credit).ok_or(LeaseRefusal::PageCreditBounds)?;
        let began = self.table.now();
        let plan = self.table.active(lease, began)?.page_plan(&token)?;
        let page = self
            .source
            .reset_page(&plan, credit)
            .map_err(|error| Failure::reset(ResetFault::Unservable(error)))?;
        if !page.advances_from(plan.requested) {
            return Err(Failure::reset(ResetFault::Stalled));
        }
        let committed = self.table.now();
        let next = self
            .table
            .active(lease, committed)?
            .after_page(committed, &page, credit)?;
        self.table.replace(lease, next);
        Ok(LocalSubscriptionResponse::SnapshotPage {
            request_id,
            lease,
            page: token,
            next: page.next.map(|next| next.token),
            credit: credit.get(),
            payload: page.payload,
        })
    }
}

fn granted_credit(credit: usize) -> Result<EventCredit, Failure> {
    EventCredit::new(credit)
        .ok_or(LeaseRefusal::CreditBounds)
        .map_err(Failure::from)
}
