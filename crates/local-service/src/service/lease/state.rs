//! Immutable lease state and the pure transitions between its states.
//!
//! A [`Lease`] is never edited in place. Every operation builds the lease it
//! should become and the table swaps it in, so the places that can move a
//! deadline are exactly the constructors in this file: [`Lease::grant`],
//! [`Lease::renewed`] and [`Lease::after_page`]. Nothing else can keep a lease
//! alive, which is the property the expiry rules rest on.

use super::limits::ResetLimits;
use crate::protocol::ProtocolError;
use backend_client::lease_contract::{LeaseMs, ResetPages};
use backend_client::monotonic::Deadline;
use backend_engine::{
    Cursor, CursorResetReason, MAX_SNAPSHOT_PAGE_ROWS, MAX_SUBSCRIPTION_EVENTS, ViewPageCursor,
    ViewRoot,
};
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

const _: () = assert!(MAX_SNAPSHOT_PAGE_ROWS > 0);
const _: () = assert!(MAX_SNAPSHOT_PAGE_ROWS <= MAX_SUBSCRIPTION_EVENTS);
const ONE_MILLISECOND: LeaseMs = LeaseMs::constant(1);

/// Event credit a lease retains: between one and the interactive batch bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EventCredit(NonZeroUsize);

impl EventCredit {
    pub(crate) fn new(credit: usize) -> Option<Self> {
        NonZeroUsize::new(credit)
            .filter(|credit| credit.get() <= MAX_SUBSCRIPTION_EVENTS)
            .map(Self)
    }

    pub(crate) const fn get(self) -> usize {
        self.0.get()
    }

    pub(crate) fn checked_add(self, other: Self) -> Option<Self> {
        self.0.get().checked_add(other.0.get()).and_then(Self::new)
    }

    /// A page's credit becomes the lease's retained credit. The shared
    /// nonzero representation avoids a runtime fallback; compile-time guards
    /// prove the page bound fits the event-credit range.
    fn from_page(page: PageCredit) -> Self {
        Self(page.0)
    }
}

/// Rows one snapshot page may carry: between one and the page bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageCredit(NonZeroUsize);

impl PageCredit {
    pub(crate) fn new(credit: usize) -> Option<Self> {
        NonZeroUsize::new(credit)
            .filter(|credit| credit.get() <= MAX_SNAPSHOT_PAGE_ROWS)
            .map(Self)
    }

    /// The first page of a reset is sized by the lease's event credit.
    pub(crate) fn clamped(credit: EventCredit) -> Self {
        Self(
            NonZeroUsize::new(credit.get().min(MAX_SNAPSHOT_PAGE_ROWS))
                .unwrap_or(NonZeroUsize::MIN),
        )
    }

    pub(crate) const fn get(self) -> usize {
        self.0.get()
    }
}

/// Why an operation on a lease was refused. Closed, so callers and tests name
/// the cause instead of matching message text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LeaseRefusal {
    /// No such lease is retained (never issued, cancelled, expired or released).
    UnknownLease,
    /// The lease's term elapsed while the operation was in flight.
    Expired,
    /// The requested credit or lease term is out of range.
    LeaseBounds,
    /// The requested credit is out of range.
    CreditBounds,
    /// The requested page size is out of range.
    PageCreditBounds,
    /// The resume cursor is not the lease's cursor, or a reset is pending.
    ResumeCursorMismatch,
    /// The acknowledged cursor is not the lease's cursor.
    AckCursorMismatch,
    /// The renewal cursor is not the lease's cursor.
    RenewCursorMismatch,
    /// Only `Page` and `Cancel` are meaningful while a reset hydrates.
    HydrationInProgress,
    /// A page was requested for a lease with no reset pending.
    NoPendingReset,
    /// The page token is not the exact pending continuation.
    PageReplay,
    /// The producer's page made no progress.
    PageStalled,
    /// The owner retains as many leases as it allows.
    CapacityFull,
    /// The owner has been closed.
    OwnerClosed,
}

impl LeaseRefusal {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::UnknownLease => "unknown subscription lease",
            Self::Expired => "subscription lease expired",
            Self::LeaseBounds => "subscription lease bounds",
            Self::CreditBounds => "subscription credit bounds",
            Self::PageCreditBounds => "snapshot page credit bounds",
            Self::ResumeCursorMismatch => "subscription resume cursor is not the lease cursor",
            Self::AckCursorMismatch => "subscription acknowledgement cursor mismatch",
            Self::RenewCursorMismatch => "subscription renewal cursor or lease mismatch",
            Self::HydrationInProgress => "subscription reset hydration is in progress",
            Self::NoPendingReset => "subscription has no pending snapshot",
            Self::PageReplay => "snapshot page continuation mismatch or replay",
            Self::PageStalled => "snapshot page made no progress",
            Self::CapacityFull | Self::OwnerClosed => "subscription lease unavailable",
        }
    }
}

impl From<LeaseRefusal> for ProtocolError {
    fn from(refusal: LeaseRefusal) -> Self {
        match refusal {
            LeaseRefusal::CapacityFull => Self::Backpressure,
            LeaseRefusal::OwnerClosed => Self::Closed,
            other => Self::InvalidControl(other.message()),
        }
    }
}

/// Why a reset can no longer be served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResetFault {
    /// The absolute window elapsed.
    WindowElapsed,
    /// The page budget was spent before the last page.
    PagesExhausted,
    /// The monotonic clock cannot represent the window's end.
    DeadlineUnavailable,
    /// A page made no progress.
    Stalled,
    /// The page could not be built, certified or encoded.
    Unservable(ProtocolError),
}

impl From<ResetFault> for ProtocolError {
    fn from(fault: ResetFault) -> Self {
        match fault {
            ResetFault::WindowElapsed => Self::ResetDeadlineExceeded,
            ResetFault::PagesExhausted => Self::ResetPageBudgetExhausted,
            ResetFault::DeadlineUnavailable => Self::LeaseDeadlineUnavailable,
            ResetFault::Stalled => LeaseRefusal::PageStalled.into(),
            ResetFault::Unservable(error) => error,
        }
    }
}

impl ResetFault {
    pub(crate) const fn release_reason(&self) -> ReleaseReason {
        match self {
            Self::WindowElapsed => ReleaseReason::ResetWindowElapsed,
            Self::PagesExhausted => ReleaseReason::ResetPagesExhausted,
            Self::DeadlineUnavailable | Self::Stalled | Self::Unservable(_) => {
                ReleaseReason::ResetPageUnservable
            }
        }
    }
}

/// Why the owner stopped retaining a lease. Every release names exactly one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReleaseReason {
    /// The holder cancelled it.
    Cancelled,
    /// Its term elapsed without a liveness-proving operation.
    Expired,
    /// A reset outlived its absolute window.
    ResetWindowElapsed,
    /// A reset needed more pages than its budget.
    ResetPagesExhausted,
    /// A reset page could not be produced, or produced no progress.
    ResetPageUnservable,
    /// The owner closed.
    OwnerClosed,
}

impl ReleaseReason {
    /// Every reason, in the order [`ReleaseCounts`](super::table::ReleaseCounts) stores them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Cancelled,
        Self::Expired,
        Self::ResetWindowElapsed,
        Self::ResetPagesExhausted,
        Self::ResetPageUnservable,
        Self::OwnerClosed,
    ];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Cancelled => 0,
            Self::Expired => 1,
            Self::ResetWindowElapsed => 2,
            Self::ResetPagesExhausted => 3,
            Self::ResetPageUnservable => 4,
            Self::OwnerClosed => 5,
        }
    }

    /// The error the holder of a lease released for this reason is told.
    pub(crate) fn holder_error(self) -> ProtocolError {
        match self {
            Self::ResetWindowElapsed => ProtocolError::ResetDeadlineExceeded,
            Self::ResetPagesExhausted => ProtocolError::ResetPageBudgetExhausted,
            Self::OwnerClosed => ProtocolError::Closed,
            Self::Cancelled | Self::Expired | Self::ResetPageUnservable => {
                LeaseRefusal::Expired.into()
            }
        }
    }
}

/// A failed operation: the error to send and whether the lease it targeted
/// must also be released. Keeping the two together stops an error path from
/// reporting a failure while leaving the failed state retained.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Failure {
    pub(crate) error: ProtocolError,
    pub(crate) release: Option<ReleaseReason>,
}

impl Failure {
    pub(crate) fn releasing(error: impl Into<ProtocolError>, reason: ReleaseReason) -> Self {
        Self {
            error: error.into(),
            release: Some(reason),
        }
    }

    pub(crate) fn reset(fault: ResetFault) -> Self {
        let reason = fault.release_reason();
        Self::releasing(fault, reason)
    }
}

impl From<ProtocolError> for Failure {
    fn from(error: ProtocolError) -> Self {
        Self {
            error,
            release: None,
        }
    }
}

impl From<LeaseRefusal> for Failure {
    fn from(refusal: LeaseRefusal) -> Self {
        ProtocolError::from(refusal).into()
    }
}

/// Pages a reset may still serve. Counting down is checked: spending a page
/// that is not there is an error, never a wrap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PagesLeft(u16);

impl PagesLeft {
    /// The budget after a reset's first page was served.
    pub(crate) fn after_first(total: ResetPages, more_follow: bool) -> Result<Self, ResetFault> {
        Self::remaining(total.get().saturating_sub(1), more_follow)
    }

    /// The budget after serving one more page.
    pub(crate) fn after_page(self, more_follow: bool) -> Result<Self, ResetFault> {
        let left = self.0.checked_sub(1).ok_or(ResetFault::PagesExhausted)?;
        Self::remaining(left, more_follow)
    }

    /// A page that announces a successor is refused unless the budget can
    /// also serve that successor, so a reset that cannot finish stops at once.
    fn remaining(left: u16, more_follow: bool) -> Result<Self, ResetFault> {
        if more_follow && left == 0 {
            Err(ResetFault::PagesExhausted)
        } else {
            Ok(Self(left))
        }
    }

    #[cfg(test)]
    pub(crate) const fn get(self) -> u16 {
        self.0
    }
}

/// The exact continuation the owner will accept next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation {
    pub(crate) cursor: ViewPageCursor,
    pub(crate) token: Box<[u8]>,
}

/// One produced, certified and encoded reset page.
#[derive(Debug)]
pub(crate) struct ResetPage {
    /// The page cursor the producer says it served.
    pub(crate) served: ViewPageCursor,
    /// Rows on this page.
    pub(crate) rows: usize,
    /// The continuation, if rows remain.
    pub(crate) next: Option<Continuation>,
    /// The encoded page DTO sent to the holder.
    pub(crate) payload: Box<[u8]>,
}

impl ResetPage {
    /// A continuation page must serve the cursor it was asked for, carry
    /// rows, and name a different cursor next. Anything else is a producer
    /// bug that would otherwise let an empty or repeating page keep a lease
    /// alive indefinitely.
    pub(crate) fn advances_from(&self, requested: ViewPageCursor) -> bool {
        self.served == requested
            && self.rows != 0
            && self
                .next
                .as_ref()
                .is_none_or(|next| next.cursor != requested)
    }
}

/// What the owner needs to produce the next page, copied out of the lease so
/// the slow work runs without borrowing the table.
pub(crate) struct PagePlan {
    pub(crate) root: Arc<ViewRoot>,
    pub(crate) target: Cursor,
    pub(crate) reason: CursorResetReason,
    pub(crate) requested: ViewPageCursor,
}

/// A reset whose pages the holder is still collecting.
#[derive(Clone)]
pub(crate) struct Hydration {
    root: Arc<ViewRoot>,
    target: Cursor,
    reason: CursorResetReason,
    next: Continuation,
    /// Absolute: renewing the lease never moves it.
    window_ends: Deadline,
    pages_left: PagesLeft,
}

impl Hydration {
    /// Starts hydrating after the first page, and returns the phase the lease
    /// is in afterwards: live when that page was the whole reset.
    pub(crate) fn begin_phase(
        granted_at: Instant,
        limits: ResetLimits,
        root: Arc<ViewRoot>,
        target: Cursor,
        reason: CursorResetReason,
        first: &ResetPage,
    ) -> Result<LeasePhase, ResetFault> {
        Self::begin(granted_at, limits, root, target, reason, first).map(LeasePhase::after_reset)
    }

    /// Starts hydrating after the first page, or returns `None` when that
    /// page was the whole reset.
    fn begin(
        granted_at: Instant,
        limits: ResetLimits,
        root: Arc<ViewRoot>,
        target: Cursor,
        reason: CursorResetReason,
        first: &ResetPage,
    ) -> Result<Option<Self>, ResetFault> {
        let Some(next) = first.next.clone() else {
            return Ok(None);
        };
        let pages_left = PagesLeft::after_first(limits.pages, true)?;
        let window_ends = Deadline::after(granted_at, limits.window)
            .ok_or(ResetFault::DeadlineUnavailable)?;
        Ok(Some(Self {
            root,
            target,
            reason,
            next,
            window_ends,
            pages_left,
        }))
    }

    fn after_page(
        &self,
        committed_at: Instant,
        page: &ResetPage,
    ) -> Result<Option<Self>, ResetFault> {
        if self.window_ends.is_due(committed_at) {
            return Err(ResetFault::WindowElapsed);
        }
        let pages_left = self.pages_left.after_page(page.next.is_some())?;
        Ok(page.next.clone().map(|next| Self {
            root: Arc::clone(&self.root),
            target: self.target,
            reason: self.reason,
            next,
            window_ends: self.window_ends,
            pages_left,
        }))
    }

    #[cfg(test)]
    pub(crate) const fn window_ends(&self) -> Deadline {
        self.window_ends
    }

    #[cfg(test)]
    pub(crate) const fn pages_left(&self) -> PagesLeft {
        self.pages_left
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Arc<ViewRoot> {
        &self.root
    }
}

impl fmt::Debug for Hydration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Hydration")
            .field("pages_left", &self.pages_left)
            .field("window_ends", &self.window_ends)
            .finish_non_exhaustive()
    }
}

/// Whether a lease is idle or collecting a reset.
#[derive(Clone, Debug)]
pub(crate) enum LeasePhase {
    /// No reset pending.
    Live,
    /// A reset root is retained until the last page, a failure, cancel or
    /// expiry. Boxed: a live lease, the common case, should not pay for it.
    Hydrating(Box<Hydration>),
}

impl LeasePhase {
    /// The phase after a reset page: live when it was the last, otherwise
    /// still hydrating.
    fn after_reset(hydration: Option<Hydration>) -> Self {
        hydration.map_or(Self::Live, |hydration| Self::Hydrating(Box::new(hydration)))
    }
}

/// One retained subscription lease.
#[derive(Clone, Debug)]
pub(crate) struct Lease {
    cursor: Box<[u8]>,
    credit: EventCredit,
    term: LeaseMs,
    expires: Deadline,
    phase: LeasePhase,
}

impl Lease {
    /// Grants `term` starting at `granted_at`.
    pub(crate) fn grant(
        granted_at: Instant,
        term: LeaseMs,
        cursor: Box<[u8]>,
        credit: EventCredit,
        phase: LeasePhase,
    ) -> Result<Self, Failure> {
        let expires = Deadline::after(granted_at, term.duration())
            .ok_or(ProtocolError::LeaseDeadlineUnavailable)?;
        Ok(Self {
            cursor,
            credit,
            term,
            expires,
            phase,
        })
    }

    pub(crate) fn cursor(&self) -> &[u8] {
        &self.cursor
    }

    pub(crate) const fn credit(&self) -> EventCredit {
        self.credit
    }

    #[cfg(test)]
    pub(crate) const fn phase(&self) -> &LeasePhase {
        &self.phase
    }

    /// The instant the owner stops retaining anything for this lease: the
    /// lease deadline, or the reset window when that comes first.
    pub(crate) fn retained_until(&self) -> Deadline {
        match &self.phase {
            LeasePhase::Live => self.expires,
            LeasePhase::Hydrating(hydration) => self.expires.min(hydration.window_ends),
        }
    }

    /// Why this lease must be released at `at`, if it must.
    pub(crate) fn release_due(&self, at: Instant) -> Option<ReleaseReason> {
        if self.expires.is_due(at) {
            return Some(ReleaseReason::Expired);
        }
        match &self.phase {
            LeasePhase::Hydrating(hydration) if hydration.window_ends.is_due(at) => {
                Some(ReleaseReason::ResetWindowElapsed)
            }
            LeasePhase::Live | LeasePhase::Hydrating(_) => None,
        }
    }

    /// Remaining term at `at`, rounded up to the wire's millisecond and never
    /// zero (a zero term is not encodable).
    pub(crate) fn remaining_term(&self, at: Instant) -> LeaseMs {
        let remaining = self.expires.remaining(at);
        let whole = remaining.as_millis();
        let rounded = if remaining.subsec_nanos().is_multiple_of(1_000_000) {
            whole
        } else {
            whole.saturating_add(1)
        };
        LeaseMs::new(u64::try_from(rounded).unwrap_or(u64::MAX)).unwrap_or(ONE_MILLISECOND)
    }

    fn live(&self, otherwise: LeaseRefusal) -> Result<(), LeaseRefusal> {
        match self.phase {
            LeasePhase::Live => Ok(()),
            LeasePhase::Hydrating(_) => Err(otherwise),
        }
    }

    /// A resume must name this lease's own cursor and find no reset pending.
    pub(crate) fn expect_resumable_at(&self, cursor: &[u8]) -> Result<(), LeaseRefusal> {
        self.live(LeaseRefusal::ResumeCursorMismatch)?;
        if self.cursor.as_ref() == cursor {
            Ok(())
        } else {
            Err(LeaseRefusal::ResumeCursorMismatch)
        }
    }

    /// An acknowledgement must name this lease's own cursor, and only after
    /// the last reset page: acknowledging a root not yet held is a lie.
    pub(crate) fn expect_acknowledgeable(&self, cursor: &[u8]) -> Result<(), LeaseRefusal> {
        self.live(LeaseRefusal::HydrationInProgress)?;
        if self.cursor.as_ref() == cursor {
            Ok(())
        } else {
            Err(LeaseRefusal::AckCursorMismatch)
        }
    }

    /// The lease after a renewal fenced at `cursor`, with a fresh `term`.
    pub(crate) fn renewed(
        &self,
        renewed_at: Instant,
        cursor: &[u8],
        term: LeaseMs,
        credit: EventCredit,
    ) -> Result<Self, Failure> {
        self.live(LeaseRefusal::HydrationInProgress)?;
        if self.cursor.as_ref() != cursor {
            return Err(LeaseRefusal::RenewCursorMismatch.into());
        }
        Self::grant(
            renewed_at,
            term,
            self.cursor.clone(),
            credit,
            LeasePhase::Live,
        )
    }

    /// The lease after extra event credit. Credit never moves a deadline.
    pub(crate) fn with_added_credit(&self, extra: EventCredit) -> Result<Self, LeaseRefusal> {
        self.live(LeaseRefusal::HydrationInProgress)?;
        let credit = self
            .credit
            .checked_add(extra)
            .ok_or(LeaseRefusal::CreditBounds)?;
        Ok(Self {
            credit,
            ..self.clone()
        })
    }

    /// Copies out what is needed to produce the page named by `token`. The
    /// token must be the exact pending continuation: a stale, replayed or
    /// forged one is refused here, before any work and without touching the
    /// lease.
    pub(crate) fn page_plan(&self, token: &[u8]) -> Result<PagePlan, LeaseRefusal> {
        let LeasePhase::Hydrating(hydration) = &self.phase else {
            return Err(LeaseRefusal::NoPendingReset);
        };
        if hydration.next.token.as_ref() != token {
            return Err(LeaseRefusal::PageReplay);
        }
        Ok(PagePlan {
            root: Arc::clone(&hydration.root),
            target: hydration.target,
            reason: hydration.reason,
            requested: hydration.next.cursor,
        })
    }

    /// The lease after `page` was produced, checked at `committed_at`.
    ///
    /// This is the only place hydration slides a lease deadline, and it does so
    /// only for a page the caller has already verified advances the exact
    /// pending continuation and encoded successfully.
    pub(crate) fn after_page(
        &self,
        committed_at: Instant,
        page: &ResetPage,
        credit: PageCredit,
    ) -> Result<Self, Failure> {
        let LeasePhase::Hydrating(hydration) = &self.phase else {
            return Err(LeaseRefusal::NoPendingReset.into());
        };
        let phase = LeasePhase::after_reset(
            hydration
                .after_page(committed_at, page)
                .map_err(Failure::reset)?,
        );
        Self::grant(
            committed_at,
            self.term,
            hydration.target.encode_control(),
            EventCredit::from_page(credit),
            phase,
        )
    }
}
