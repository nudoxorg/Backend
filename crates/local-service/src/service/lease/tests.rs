//! Deterministic tests for lease retention, expiry and reset hydration.
//!
//! Every test drives the real [`LeaseHost`] and [`LeaseTable`] against a
//! scripted daemon and a [`ManualClock`]: time moves only when a test says so,
//! including *inside* a daemon round trip or a page encode, which is how the
//! "work outlived the lease" schedules are made exact instead of racy.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::host::{EventBatch, LeaseHost, LeaseSource};
use super::identity::{OwnerBootNonce, OwnerLeaseIdentity};
use super::limits::SubscriptionLeaseLimits;
use super::state::{
    Continuation, EventCredit, Lease, LeasePhase, LeaseRefusal, PageCredit, PagePlan, PagesLeft,
    ReleaseReason, ResetFault, ResetPage,
};
use super::table::LeaseTable;
use crate::protocol::{EngineStatus, FrameLimits, ProtocolError, ResponseFrame, encode_response};
use backend_client::lease_contract::{BOOTSTRAP_LEASE, LeaseMs, PUBLICATION_LEASE, ResetPages};
use backend_client::monotonic::{Deadline, ManualClock, MonotonicClock};
use backend_engine::{
    Basis, Cursor, CursorEvent, CursorResetReason, Frontier, Lane, LocalSubscriptionId,
    LocalSubscriptionOperation, LocalSubscriptionRequest, LocalSubscriptionResponse, Reason, Row,
    RowId, SubscriptionReply, TransportLimits, ViewCoverage, ViewPageCursor, ViewRoot,
    object_version, symbol_key, view_key, view_state_root,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

pub(super) const SECOND: Duration = Duration::from_secs(1);
const NANOSECOND: Duration = Duration::from_nanos(1);
const CREDIT: usize = 64;

pub(super) fn term() -> Duration {
    PUBLICATION_LEASE.duration()
}

fn refused(refusal: LeaseRefusal) -> ProtocolError {
    refusal.into()
}

// ---------------------------------------------------------------- fixtures

/// A root of one symbol row per label, in the library-view recipe the
/// production page certificate requires.
pub(super) fn reset_root(labels: &[&str]) -> ViewRoot {
    let source = view_state_root(&[]);
    let basis = Basis::new(source, object_version(b"retained-reset-root"));
    ViewRoot::new_incomplete(
        view_key(b"library-view-v1"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
        labels
            .iter()
            .map(|label| Row::new(RowId::Symbol(symbol_key(label)), basis, *label))
            .collect(),
        vec![ViewCoverage::Unavailable {
            lane: Lane::Exact,
            reason: Reason::Unconfigured,
        }],
    )
    .expect("reset root")
}

pub(super) fn reset_reply(labels: &[&str]) -> SubscriptionReply {
    let root = reset_root(labels);
    SubscriptionReply::ResetWithRoot {
        credit: 1,
        cursor: Cursor::for_view_root(&root).encode_control(),
        root: Box::new(root),
        reason: CursorResetReason::Gap,
    }
}

/// What the scripted daemon does on its next `reset_page` call.
#[derive(Debug)]
pub(super) enum PageScript {
    /// Serve the page from the retained root.
    Serve,
    /// Serve it, but the clock moves while the page is being encoded.
    ServeAfter(Duration),
    /// Fail to produce or encode the page.
    Fail(ProtocolError),
    /// Fail, but only after the clock moved while the page was being built.
    FailAfter(Duration, ProtocolError),
    /// Return a page with no rows.
    Empty,
    /// Return a page whose continuation is the cursor it was asked for.
    Repeating,
    /// Return a page for a different cursor than the one requested.
    Misaddressed,
}

pub(super) struct ScriptedSource {
    clock: Arc<ManualClock>,
    owner_cursor: Cursor,
    pub(super) replies: VecDeque<Result<SubscriptionReply, ProtocolError>>,
    /// Clock movement during the next daemon round trip.
    during_subscribe: Cell<Option<Duration>>,
    pages: RefCell<VecDeque<PageScript>>,
    pub(super) subscribe_calls: Cell<usize>,
    pub(super) pages_built: Cell<usize>,
    pub(super) events_error: Option<ProtocolError>,
    /// Serve pages with the production certifier instead of a scripted one.
    production: Cell<bool>,
    /// Clock movement per page produced, to model a slow encode.
    page_cost: Cell<Duration>,
}

impl ScriptedSource {
    pub(super) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            clock,
            owner_cursor: Cursor::for_view_root(&reset_root(&[])),
            replies: VecDeque::new(),
            during_subscribe: Cell::new(None),
            pages: RefCell::new(VecDeque::new()),
            subscribe_calls: Cell::new(0),
            pages_built: Cell::new(0),
            events_error: None,
            production: Cell::new(false),
            page_cost: Cell::new(Duration::ZERO),
        }
    }

    /// Serves every page with the production certifier. The retained root must
    /// then be a certified one (see `publication_tests`).
    pub(super) fn use_production_pager(&self) {
        self.production.set(true);
    }

    /// Makes every page take `cost` of clock time to produce.
    pub(super) fn set_page_cost(&self, cost: Duration) {
        self.page_cost.set(cost);
    }

    /// Makes the owner report `cursor` as its current one.
    pub(super) fn set_owner_cursor(&mut self, cursor: Cursor) {
        self.owner_cursor = cursor;
    }

    /// The cursor bytes the owner currently reports.
    pub(super) fn owner_cursor_bytes(&self) -> Box<[u8]> {
        self.owner_cursor.encode_control()
    }

    fn page_token(cursor: ViewPageCursor) -> Box<[u8]> {
        format!("token:{:?}", cursor.after())
            .into_bytes()
            .into_boxed_slice()
    }
}

impl LeaseSource for ScriptedSource {
    fn subscribe(
        &mut self,
        _request_id: u64,
        _cursor: &[u8],
        _credit: usize,
    ) -> Result<SubscriptionReply, ProtocolError> {
        self.subscribe_calls.set(self.subscribe_calls.get() + 1);
        if let Some(elapsed) = self.during_subscribe.take() {
            self.clock.advance(elapsed);
        }
        self.replies
            .pop_front()
            .expect("the test scripted a daemon reply")
    }

    fn owner_cursor(&self) -> Result<Cursor, ProtocolError> {
        Ok(self.owner_cursor)
    }

    fn event_batch(
        &self,
        requested: &[u8],
        _credit: usize,
        _target: &[u8],
        _events: &[CursorEvent],
    ) -> Result<EventBatch, ProtocolError> {
        match &self.events_error {
            Some(error) => Err(error.clone()),
            None => Ok(EventBatch {
                previous: requested.into(),
                payload: Box::from(b"events".as_slice()),
            }),
        }
    }

    fn reset_page(&self, plan: &PagePlan, credit: PageCredit) -> Result<ResetPage, ProtocolError> {
        self.pages_built.set(self.pages_built.get() + 1);
        if self.production.get() {
            let page = super::host::production_reset_page(plan, credit)?;
            self.clock.advance(self.page_cost.get());
            return Ok(page);
        }
        let script = self
            .pages
            .borrow_mut()
            .pop_front()
            .unwrap_or(PageScript::Serve);
        let page = plan
            .root
            .page(plan.requested, credit.get())
            .expect("the retained root serves its own continuation");
        let continuation = |cursor: ViewPageCursor| Continuation {
            cursor,
            token: Self::page_token(cursor),
        };
        let served = ResetPage {
            served: page.cursor(),
            rows: page.rows().len(),
            next: page.next().map(continuation),
            payload: format!("page:{}", self.pages_built.get())
                .into_bytes()
                .into_boxed_slice(),
        };
        match script {
            PageScript::Serve => Ok(served),
            PageScript::ServeAfter(elapsed) => {
                self.clock.advance(elapsed);
                Ok(served)
            }
            PageScript::Fail(error) => Err(error),
            PageScript::FailAfter(elapsed, error) => {
                self.clock.advance(elapsed);
                Err(error)
            }
            PageScript::Empty => Ok(ResetPage { rows: 0, ..served }),
            PageScript::Repeating => Ok(ResetPage {
                next: Some(continuation(plan.requested)),
                ..served
            }),
            PageScript::Misaddressed => Ok(ResetPage {
                served: ViewPageCursor::first(&reset_root(&["elsewhere"])),
                ..served
            }),
        }
    }
}

pub(super) struct Harness {
    pub(super) clock: Arc<ManualClock>,
    pub(super) table: LeaseTable,
    pub(super) source: ScriptedSource,
    next_request: u64,
}

impl Harness {
    pub(super) fn new(limits: SubscriptionLeaseLimits) -> Self {
        let clock = ManualClock::new();
        let table = LeaseTable::new(limits, clock.clone());
        let source = ScriptedSource::new(clock.clone());
        Self {
            clock,
            table,
            source,
            next_request: 0,
        }
    }

    pub(super) fn with_limits(
        max_active: usize,
        max_term: Duration,
        max_reset_pages: usize,
        max_reset_window: Duration,
    ) -> Self {
        Self::new(
            SubscriptionLeaseLimits::new(max_active, max_term, max_reset_pages, max_reset_window)
                .expect("test limits"),
        )
    }

    pub(super) fn script(&mut self, reply: SubscriptionReply) {
        self.source.replies.push_back(Ok(reply));
    }

    pub(super) fn script_pages(&self, pages: impl IntoIterator<Item = PageScript>) {
        self.source.pages.borrow_mut().extend(pages);
    }

    pub(super) fn call(
        &mut self,
        operation: LocalSubscriptionOperation,
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.next_request += 1;
        let request_id = self.next_request;
        let request = LocalSubscriptionRequest {
            request_id,
            operation,
        };
        match LeaseHost::new(&mut self.table, &mut self.source).handle(request_id, request)? {
            EngineStatus::Subscription(response) => Ok(response),
            other => panic!("lease operations answer with subscription responses: {other:?}"),
        }
    }

    fn call_prepared(
        &mut self,
        operation: LocalSubscriptionOperation,
        prepare: &mut dyn FnMut(LocalSubscriptionResponse) -> Result<Vec<u8>, ProtocolError>,
    ) -> Result<Vec<u8>, ProtocolError> {
        self.next_request += 1;
        let request_id = self.next_request;
        LeaseHost::new(&mut self.table, &mut self.source).handle_prepared(
            request_id,
            LocalSubscriptionRequest {
                request_id,
                operation,
            },
            prepare,
        )
    }

    pub(super) fn open_with(
        &mut self,
        reply: SubscriptionReply,
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.script(reply);
        self.call(LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit,
            lease_ms,
        })
    }

    /// Opens a quiet lease and returns it with the cursor it holds.
    pub(super) fn open_quiet(&mut self) -> (LocalSubscriptionId, Box<[u8]>) {
        let response = self
            .open_with(
                SubscriptionReply::Accepted { credit: CREDIT },
                CREDIT,
                PUBLICATION_LEASE.get(),
            )
            .expect("open");
        let LocalSubscriptionResponse::Opened {
            lease,
            cursor,
            credit,
            lease_ms,
            ..
        } = response
        else {
            panic!("a quiet open answers Opened: {response:?}");
        };
        assert_eq!(credit, CREDIT);
        assert_eq!(
            lease_ms,
            PUBLICATION_LEASE.get(),
            "granted exactly as asked"
        );
        (lease, cursor)
    }

    /// Opens a lease whose first reply is a reset of `labels`, one row a page.
    pub(super) fn open_reset(
        &mut self,
        labels: &[&str],
    ) -> Result<(LocalSubscriptionId, Option<Box<[u8]>>), ProtocolError> {
        let response = self.open_with(reset_reply(labels), 1, PUBLICATION_LEASE.get())?;
        let LocalSubscriptionResponse::SnapshotPage {
            lease, page, next, ..
        } = response
        else {
            panic!("a reset open answers SnapshotPage: {response:?}");
        };
        assert!(page.is_empty(), "the first page carries no request token");
        Ok((lease, next))
    }

    pub(super) fn page(
        &mut self,
        lease: LocalSubscriptionId,
        token: &[u8],
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.call(LocalSubscriptionOperation::Page {
            lease,
            page: token.into(),
            credit: 1,
        })
    }

    /// Fetches a page and returns the continuation it announces.
    pub(super) fn next_page(
        &mut self,
        lease: LocalSubscriptionId,
        token: &[u8],
    ) -> Option<Box<[u8]>> {
        match self.page(lease, token).expect("page") {
            LocalSubscriptionResponse::SnapshotPage { next, page, .. } => {
                assert_eq!(page.as_ref(), token);
                next
            }
            other => panic!("a page answers SnapshotPage: {other:?}"),
        }
    }

    pub(super) fn resume(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: &[u8],
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.call(LocalSubscriptionOperation::Resume {
            lease,
            cursor: cursor.into(),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        })
    }

    pub(super) fn renew(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: &[u8],
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.call(LocalSubscriptionOperation::Renew {
            lease,
            cursor: cursor.into(),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        })
    }

    pub(super) fn ack(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: &[u8],
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.call(LocalSubscriptionOperation::Ack {
            lease,
            cursor: cursor.into(),
        })
    }

    pub(super) fn cancel(
        &mut self,
        lease: LocalSubscriptionId,
    ) -> Result<LocalSubscriptionResponse, ProtocolError> {
        self.call(LocalSubscriptionOperation::Cancel { lease })
    }

    pub(super) fn weak_root(&self, lease: LocalSubscriptionId) -> Weak<ViewRoot> {
        match self.table.get(lease).expect("retained lease").phase() {
            LeasePhase::Hydrating(hydration) => Arc::downgrade(hydration.root()),
            LeasePhase::Live => panic!("a live lease retains no reset root"),
        }
    }

    pub(super) fn try_open_quiet(
        &mut self,
    ) -> Result<(LocalSubscriptionId, Box<[u8]>), ProtocolError> {
        match self.open_with(
            SubscriptionReply::Accepted { credit: CREDIT },
            CREDIT,
            PUBLICATION_LEASE.get(),
        )? {
            LocalSubscriptionResponse::Opened { lease, cursor, .. } => Ok((lease, cursor)),
            other => panic!("a quiet open answers Opened: {other:?}"),
        }
    }

    pub(super) fn released(&self, reason: ReleaseReason) -> u64 {
        self.table.released().count(reason)
    }

    /// The table's earliest-expiry hint must never exceed the true earliest
    /// retention deadline, or a due lease would be missed by the idle poll.
    pub(super) fn assert_hint_is_a_lower_bound(&self) {
        match (self.table.earliest(), self.table.exact_earliest()) {
            (_, None) => {}
            (Some(hint), Some(exact)) => assert!(hint <= exact, "hint {hint:?} > exact {exact:?}"),
            (None, Some(exact)) => panic!("retained lease due at {exact:?} but no hint"),
        }
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new(SubscriptionLeaseLimits::default())
    }
}

fn root_gone(root: &Weak<ViewRoot>) -> bool {
    root.upgrade().is_none()
}

// ------------------------------------------------- identity (moved intact)

#[test]
fn owner_restart_rejects_old_subscription_lease_and_reacquires_distinct_identity() {
    let cursor = b"same-cursor-after-restart";
    let live = |table: &mut LeaseTable, id| {
        let lease = Lease::grant(
            table.now(),
            PUBLICATION_LEASE,
            cursor.as_slice().into(),
            EventCredit::new(1).expect("credit"),
            LeasePhase::Live,
        )
        .expect("lease");
        table.install(id, lease).expect("install");
    };
    let mut previous_table = Harness::default().table;
    previous_table.identity().boot_nonce = Some(OwnerBootNonce([0x11; 32]));
    let previous = previous_table
        .allocate_id(17, cursor)
        .expect("first owner lease");
    live(&mut previous_table, previous);
    assert!(previous_table.get(previous).is_some());

    // A new process can serve the same endpoint path and receive the same
    // request ID and cursor. Its active map starts empty, and its OS-minted
    // boot namespace must make the first new lease distinct from the old one.
    let mut restarted_table = Harness::default().table;
    restarted_table.identity().boot_nonce = Some(OwnerBootNonce([0x22; 32]));
    assert!(restarted_table.get(previous).is_none());
    let reacquired = restarted_table
        .allocate_id(17, cursor)
        .expect("new owner lease");
    assert_ne!(previous, reacquired);
    live(&mut restarted_table, reacquired);
    assert!(restarted_table.get(reacquired).is_some());
    assert!(restarted_table.get(previous).is_none());
    assert_ne!(
        restarted_table
            .allocate_id(17, cursor)
            .expect("next owner lease"),
        reacquired
    );
}

#[test]
fn subscription_lease_counter_never_wraps_to_reissue_an_old_identity() {
    let mut identity = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x33; 32])),
        next_nonce: u64::MAX,
    };
    assert_eq!(
        identity.allocate(17, b"cursor", |_| false),
        Err(ProtocolError::LeaseIdsExhausted)
    );
}

#[test]
fn an_identity_an_active_lease_already_holds_is_never_reissued() {
    let mut identity = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x44; 32])),
        next_nonce: 0,
    };
    let first = identity
        .allocate(1, b"c", |_| false)
        .expect("first identity");
    let held = BTreeSet::from([first]);
    let mut replay = OwnerLeaseIdentity {
        boot_nonce: Some(OwnerBootNonce([0x44; 32])),
        next_nonce: 0,
    };
    let second = replay
        .allocate(1, b"c", |lease| held.contains(lease))
        .expect("a different identity");
    assert_ne!(first, second, "a held identity was skipped, not reissued");
}

// ----------------------------------------------------------- pure arithmetic

#[test]
fn event_and_page_credit_are_bounded_at_both_ends() {
    assert!(EventCredit::new(0).is_none());
    assert_eq!(EventCredit::new(1).map(EventCredit::get), Some(1));
    assert_eq!(
        EventCredit::new(backend_engine::MAX_SUBSCRIPTION_EVENTS).map(EventCredit::get),
        Some(backend_engine::MAX_SUBSCRIPTION_EVENTS)
    );
    assert!(EventCredit::new(backend_engine::MAX_SUBSCRIPTION_EVENTS + 1).is_none());
    assert!(EventCredit::new(usize::MAX).is_none());
    assert!(PageCredit::new(0).is_none());
    assert!(PageCredit::new(backend_engine::MAX_SNAPSHOT_PAGE_ROWS).is_some());
    assert!(PageCredit::new(backend_engine::MAX_SNAPSHOT_PAGE_ROWS + 1).is_none());
    assert!(PageCredit::new(usize::MAX).is_none());
    let one = EventCredit::new(1).expect("one");
    let max = EventCredit::new(backend_engine::MAX_SUBSCRIPTION_EVENTS).expect("max");
    assert!(
        max.checked_add(one).is_none(),
        "credit addition cannot exceed its bound"
    );
    assert!(max.checked_add(max).is_none());
}

#[test]
fn an_open_whose_exact_outer_reply_does_not_fit_does_not_retain_or_consume_a_lease() {
    let mut harness = Harness::default();
    harness.script(SubscriptionReply::Accepted { credit: CREDIT });
    let limits = FrameLimits {
        max_frame: 96,
        max_cursor: 96,
        max_frames_per_connection: 8,
        transport: TransportLimits {
            max_frame: 96,
            max_chunk: 96,
            ..TransportLimits::default()
        },
    };
    let result = harness.call_prepared(
        LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        },
        &mut |response| {
            encode_response(
                &ResponseFrame::Engine {
                    request_id: 1,
                    status: EngineStatus::Subscription(response),
                },
                limits,
            )
        },
    );
    assert_eq!(result, Err(ProtocolError::FrameTooLarge));
    assert_eq!(harness.table.len(), 0, "no undisclosed lease was installed");
    assert_eq!(
        harness.table.identity().next_nonce,
        0,
        "the identity ordinal is committed only with the lease"
    );
}

#[test]
fn an_unencodable_page_reply_leaves_the_exact_continuation_unconsumed() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let token = token.expect("continuation");
    let (cursor, credit, pages_left, window_ends) = {
        let retained = harness.table.get(lease).expect("retained reset");
        let LeasePhase::Hydrating(hydration) = retained.phase() else {
            panic!("reset is hydrating");
        };
        (
            retained.cursor().to_vec(),
            retained.credit().get(),
            hydration.pages_left(),
            hydration.window_ends(),
        )
    };
    let limits = FrameLimits {
        max_frame: 96,
        max_cursor: 96,
        max_frames_per_connection: 8,
        transport: TransportLimits {
            max_frame: 96,
            max_chunk: 96,
            ..TransportLimits::default()
        },
    };
    let result = harness.call_prepared(
        LocalSubscriptionOperation::Page {
            lease,
            page: token.clone(),
            credit: 1,
        },
        &mut |response| {
            encode_response(
                &ResponseFrame::Engine {
                    request_id: 2,
                    status: EngineStatus::Subscription(response),
                },
                limits,
            )
        },
    );
    assert_eq!(result, Err(ProtocolError::FrameTooLarge));
    let retained = harness.table.get(lease).expect("lease retained");
    assert_eq!(retained.cursor(), cursor.as_slice());
    assert_eq!(retained.credit().get(), credit);
    let LeasePhase::Hydrating(hydration) = retained.phase() else {
        panic!("unencodable page cannot finish the reset");
    };
    assert_eq!(hydration.pages_left(), pages_left);
    assert_eq!(hydration.window_ends(), window_ends);
    assert!(
        matches!(
            harness.page(lease, &token),
            Ok(LocalSubscriptionResponse::SnapshotPage { .. })
        ),
        "the owner still accepts the exact undisclosed continuation"
    );
}

#[test]
fn page_credit_is_retained_exactly_and_invalid_credit_preserves_the_old_state() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let token = token.expect("continuation");
    let before = harness.table.get(lease).expect("lease").credit().get();
    let (pages_left, window_ends) = {
        let LeasePhase::Hydrating(hydration) = harness.table.get(lease).expect("lease").phase()
        else {
            panic!("reset is hydrating");
        };
        (hydration.pages_left(), hydration.window_ends())
    };

    for invalid in [0, backend_engine::MAX_SNAPSHOT_PAGE_ROWS + 1, usize::MAX] {
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Page {
                lease,
                page: token.clone(),
                credit: invalid,
            }),
            Err(refused(LeaseRefusal::PageCreditBounds))
        );
        let retained = harness.table.get(lease).expect("refusal preserves lease");
        assert_eq!(retained.credit().get(), before);
        let LeasePhase::Hydrating(hydration) = retained.phase() else {
            panic!("invalid credit cannot finish the reset");
        };
        assert_eq!(hydration.pages_left(), pages_left);
        assert_eq!(hydration.window_ends(), window_ends);
    }

    let admitted = harness
        .call(LocalSubscriptionOperation::Page {
            lease,
            page: token,
            credit: backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
        })
        .expect("maximum valid page credit");
    assert!(matches!(
        admitted,
        LocalSubscriptionResponse::SnapshotPage { credit, .. }
            if credit == backend_engine::MAX_SNAPSHOT_PAGE_ROWS
    ));
    assert_eq!(
        harness
            .table
            .get(lease)
            .expect("lease retained")
            .credit()
            .get(),
        backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
        "the page's exact admitted credit becomes the model credit"
    );
}

#[test]
fn the_page_budget_counts_down_without_wrapping_and_refuses_an_unfinishable_reset() {
    let pages = |total: u16| ResetPages::new(total).expect("nonzero");
    // A one-page budget fits a reset that is one page, and nothing longer.
    assert_eq!(
        PagesLeft::after_first(pages(1), false).map(PagesLeft::get),
        Ok(0)
    );
    assert_eq!(
        PagesLeft::after_first(pages(1), true),
        Err(ResetFault::PagesExhausted)
    );
    // With a budget of three, the third page may be served only if it is last.
    let after_first = PagesLeft::after_first(pages(3), true).expect("two left");
    assert_eq!(after_first.get(), 2);
    let after_second = after_first.after_page(true).expect("one left");
    assert_eq!(after_second.get(), 1);
    assert_eq!(
        after_second.after_page(true),
        Err(ResetFault::PagesExhausted)
    );
    assert_eq!(after_second.after_page(false).map(PagesLeft::get), Ok(0));
    // Spending a page that is not there is an error, never an underflow.
    let spent = after_second.after_page(false).expect("spent");
    assert_eq!(spent.after_page(false), Err(ResetFault::PagesExhausted));
}

// ------------------------------------------------------------ term and expiry

#[test]
fn a_lease_is_retained_until_the_instant_its_term_ends_and_not_a_nanosecond_longer() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - NANOSECOND);
    assert!(
        harness.ack(lease, &cursor).is_ok(),
        "alive one tick before its deadline"
    );
    harness.clock.advance(NANOSECOND);
    assert_eq!(
        harness.ack(lease, &cursor),
        Err(refused(LeaseRefusal::UnknownLease)),
        "due at its own deadline, reclaimed before the request is served"
    );
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
    assert_eq!(harness.table.len(), 0);
}

#[test]
fn the_idle_poll_reclaims_an_abandoned_lease_with_no_request_at_all() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_quiet();
    let before = harness.clock.now();
    assert_eq!(harness.table.reclaim_due(before), 0);
    harness.clock.advance(term() - NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 0);
    assert!(harness.table.get(lease).is_some());
    harness.clock.advance(NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
    assert!(harness.table.get(lease).is_none());
    assert_eq!(
        harness.table.earliest(),
        None,
        "the hint is exact after a sweep"
    );
    // Nothing left to reclaim: the next poll is a single comparison.
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 0);
}

#[test]
fn an_abandoned_multipage_reset_root_is_freed_by_the_idle_poll_and_cannot_be_resumed() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["first", "second"]).expect("reset");
    assert!(
        token.is_some(),
        "two rows at one row a page need a second page"
    );
    let root = harness.weak_root(lease);
    assert!(!root_gone(&root));
    harness.clock.advance(term());
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
    assert!(root_gone(&root), "the retained root left owner memory");
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
    assert_eq!(
        harness.resume(lease, b"reset-target"),
        Err(refused(LeaseRefusal::UnknownLease))
    );
}

#[test]
fn work_that_outlives_a_lease_cannot_resurrect_it() {
    // The daemon round trip of a Resume takes longer than the lease has left.
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - SECOND);
    harness.source.during_subscribe.set(Some(2 * SECOND));
    harness.script(SubscriptionReply::Accepted { credit: CREDIT });
    assert_eq!(
        harness.resume(lease, &cursor),
        Err(refused(LeaseRefusal::Expired))
    );
    assert!(
        harness.table.get(lease).is_none(),
        "the late reply installed nothing"
    );
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
}

#[test]
fn an_open_is_granted_its_full_term_from_the_moment_it_commits() {
    // A slow daemon round trip must not eat into the term the holder is told.
    let mut harness = Harness::default();
    harness.source.during_subscribe.set(Some(3 * SECOND));
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - NANOSECOND);
    assert!(harness.ack(lease, &cursor).is_ok());
    harness.clock.advance(NANOSECOND);
    assert!(harness.ack(lease, &cursor).is_err());
}

#[test]
fn renew_racing_expiry_wins_one_tick_before_the_deadline_and_loses_at_it() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - NANOSECOND);
    assert!(harness.renew(lease, &cursor).is_ok());
    // The renewal granted a whole new term from that instant.
    harness.clock.advance(term() - NANOSECOND);
    assert!(harness.ack(lease, &cursor).is_ok());
    harness.clock.advance(NANOSECOND);
    assert_eq!(
        harness.renew(lease, &cursor),
        Err(refused(LeaseRefusal::UnknownLease)),
        "a renewal arriving at the deadline cannot resurrect the lease"
    );
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
    assert_eq!(harness.table.len(), 0);
}

#[test]
fn cancel_racing_expiry_is_a_cancel_before_the_deadline_and_an_expiry_at_it() {
    let mut harness = Harness::default();
    let (early, _) = harness.open_quiet();
    harness.clock.advance(term() - NANOSECOND);
    assert!(matches!(
        harness.cancel(early),
        Ok(LocalSubscriptionResponse::Cancelled { lease, .. }) if lease == early
    ));
    assert_eq!(harness.released(ReleaseReason::Cancelled), 1);

    let (late, _) = harness.open_quiet();
    harness.clock.advance(term());
    assert_eq!(
        harness.cancel(late),
        Err(refused(LeaseRefusal::UnknownLease))
    );
    assert_eq!(harness.released(ReleaseReason::Cancelled), 1);
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
    assert_eq!(
        harness.table.released().total(),
        2,
        "each lease released once"
    );
}

#[test]
fn cancelling_twice_releases_once() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_quiet();
    assert!(harness.cancel(lease).is_ok());
    assert_eq!(
        harness.cancel(lease),
        Err(refused(LeaseRefusal::UnknownLease))
    );
    assert_eq!(harness.table.released().total(), 1);
}

#[test]
fn ack_and_credit_never_move_a_deadline() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - SECOND);
    assert!(harness.ack(lease, &cursor).is_ok());
    assert!(
        harness
            .call(LocalSubscriptionOperation::Credit { lease, credit: 8 })
            .is_ok()
    );
    harness.clock.advance(SECOND);
    assert_eq!(
        harness.ack(lease, &cursor),
        Err(refused(LeaseRefusal::UnknownLease)),
        "neither Ack nor Credit extended the lease past its original term"
    );
}

#[test]
fn resume_and_renew_each_grant_a_fresh_term() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    for operation in 0..2 {
        harness.clock.advance(term() - SECOND);
        if operation == 0 {
            harness.script(SubscriptionReply::Accepted { credit: CREDIT });
            assert!(matches!(
                harness.resume(lease, &cursor),
                Ok(LocalSubscriptionResponse::Resumed { lease_ms, .. }) if lease_ms == PUBLICATION_LEASE.get()
            ));
        } else {
            assert!(matches!(
                harness.renew(lease, &cursor),
                Ok(LocalSubscriptionResponse::Renewed { lease_ms, .. }) if lease_ms == PUBLICATION_LEASE.get()
            ));
        }
    }
    harness.clock.advance(term() - NANOSECOND);
    assert!(harness.ack(lease, &cursor).is_ok());
}

#[test]
fn a_renewal_or_resume_at_the_wrong_cursor_refuses_without_extending() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() - SECOND);
    assert_eq!(
        harness.renew(lease, b"another-cursor"),
        Err(refused(LeaseRefusal::RenewCursorMismatch))
    );
    assert_eq!(
        harness.resume(lease, b"another-cursor"),
        Err(refused(LeaseRefusal::ResumeCursorMismatch))
    );
    assert_eq!(
        harness.ack(lease, b"another-cursor"),
        Err(refused(LeaseRefusal::AckCursorMismatch))
    );
    harness.clock.advance(SECOND);
    assert!(
        harness.ack(lease, &cursor).is_err(),
        "no refusal extended the term"
    );
}

#[test]
fn credit_is_bounded_and_a_refused_addition_leaves_the_credit_unchanged() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_quiet();
    let add = |harness: &mut Harness, credit| {
        harness.call(LocalSubscriptionOperation::Credit { lease, credit })
    };
    assert!(matches!(
        add(&mut harness, 100),
        Ok(LocalSubscriptionResponse::Renewed { credit: 164, .. })
    ));
    assert_eq!(
        add(&mut harness, 100),
        Err(refused(LeaseRefusal::CreditBounds)),
        "164 + 100 exceeds the 256-event bound"
    );
    // The refusal must not have kept the rejected sum: exactly 92 more fit.
    assert!(matches!(
        add(&mut harness, 92),
        Ok(LocalSubscriptionResponse::Renewed { credit: 256, .. })
    ));
    assert_eq!(
        add(&mut harness, 0),
        Err(refused(LeaseRefusal::CreditBounds))
    );
    assert_eq!(
        add(&mut harness, usize::MAX),
        Err(refused(LeaseRefusal::CreditBounds))
    );
}

#[test]
fn the_reported_remaining_term_is_never_zero() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_quiet();
    harness.clock.advance(term() - NANOSECOND);
    // One nanosecond left rounds up to the wire's millisecond: a zero term
    // would not even be encodable.
    assert!(matches!(
        harness.call(LocalSubscriptionOperation::Credit { lease, credit: 1 }),
        Ok(LocalSubscriptionResponse::Renewed { lease_ms: 1, .. })
    ));
}

// -------------------------------------------------------- negotiation inputs

#[test]
fn a_term_is_granted_exactly_up_to_the_cap_and_refused_beyond_it() {
    let mut harness = Harness::default();
    let cap = SubscriptionLeaseLimits::default().max_term().as_millis();
    let cap = u64::try_from(cap).expect("cap fits");
    for refused_term in [0, cap + 1, u64::MAX] {
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Open {
                cursor: Box::new([]),
                credit: 1,
                lease_ms: refused_term,
            }),
            Err(refused(LeaseRefusal::LeaseBounds)),
            "{refused_term} ms"
        );
    }
    assert_eq!(
        harness.source.subscribe_calls.get(),
        0,
        "refused before any daemon round trip"
    );
    let granted = harness
        .open_with(SubscriptionReply::Accepted { credit: 1 }, 1, cap)
        .expect("the cap itself is granted");
    assert!(
        matches!(granted, LocalSubscriptionResponse::Opened { lease_ms, .. } if lease_ms == cap)
    );
}

#[test]
fn the_hard_ceiling_term_does_not_overflow_the_deadline() {
    let mut harness = Harness::with_limits(1, Duration::from_hours(1), 1, SECOND);
    let hour = 3_600_000;
    let response = harness
        .open_with(SubscriptionReply::Accepted { credit: 1 }, 1, hour)
        .expect("an hour is granted");
    assert!(
        matches!(response, LocalSubscriptionResponse::Opened { lease_ms, .. } if lease_ms == hour)
    );
    harness.clock.advance(Duration::from_hours(1) - NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 0);
    harness.clock.advance(NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
}

#[test]
fn credit_outside_one_to_the_event_bound_is_refused_on_open_resume_and_renew() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    for credit in [0, backend_engine::MAX_SUBSCRIPTION_EVENTS + 1, usize::MAX] {
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Open {
                cursor: Box::new([]),
                credit,
                lease_ms: PUBLICATION_LEASE.get(),
            }),
            Err(refused(LeaseRefusal::LeaseBounds))
        );
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Resume {
                lease,
                cursor: cursor.clone(),
                credit,
                lease_ms: PUBLICATION_LEASE.get(),
            }),
            Err(refused(LeaseRefusal::LeaseBounds))
        );
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Renew {
                lease,
                cursor: cursor.clone(),
                credit,
                lease_ms: PUBLICATION_LEASE.get(),
            }),
            Err(refused(LeaseRefusal::LeaseBounds))
        );
    }
    assert!(
        harness.table.get(lease).is_some(),
        "refusals do not release"
    );
}

#[test]
fn page_credit_outside_one_to_the_page_bound_is_refused() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b"]).expect("reset");
    let token = token.expect("second page");
    for credit in [0, backend_engine::MAX_SNAPSHOT_PAGE_ROWS + 1, usize::MAX] {
        assert_eq!(
            harness.call(LocalSubscriptionOperation::Page {
                lease,
                page: token.clone(),
                credit,
            }),
            Err(refused(LeaseRefusal::PageCreditBounds))
        );
    }
    assert!(harness.table.get(lease).is_some());
}

#[test]
fn a_request_whose_correlation_differs_from_its_envelope_is_refused() {
    let mut harness = Harness::default();
    let request = LocalSubscriptionRequest {
        request_id: 6,
        operation: LocalSubscriptionOperation::Cancel {
            lease: LocalSubscriptionId::from_bytes([1; 16]),
        },
    };
    assert_eq!(
        LeaseHost::new(&mut harness.table, &mut harness.source).handle(5, request),
        Err(ProtocolError::InvalidControl(
            "subscription request correlation mismatch"
        ))
    );
}

#[test]
fn a_forged_lease_identity_is_unknown_to_every_operation() {
    let mut harness = Harness::default();
    let (_real, cursor) = harness.open_quiet();
    let forged = LocalSubscriptionId::from_bytes([0xAB; 16]);
    let unknown = Err(refused(LeaseRefusal::UnknownLease));
    assert_eq!(harness.resume(forged, &cursor), unknown);
    assert_eq!(harness.renew(forged, &cursor), unknown);
    assert_eq!(harness.ack(forged, &cursor), unknown);
    assert_eq!(harness.cancel(forged), unknown);
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Credit {
            lease: forged,
            credit: 1
        }),
        unknown
    );
    assert_eq!(harness.page(forged, b"token"), unknown);
    assert_eq!(harness.table.len(), 1, "forgery touched nothing");
    assert_eq!(harness.table.released().total(), 0);
}

// ------------------------------------------------------------------ capacity

#[test]
fn the_owner_never_retains_more_leases_than_its_limit_and_spends_no_daemon_trip_refusing() {
    let mut harness = Harness::with_limits(3, Duration::from_mins(1), 8, Duration::from_mins(1));
    for _ in 0..3 {
        harness.open_quiet();
    }
    let calls = harness.source.subscribe_calls.get();
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        }),
        Err(ProtocolError::Backpressure)
    );
    assert_eq!(harness.source.subscribe_calls.get(), calls);
    assert_eq!(harness.table.len(), 3);
    // Capacity returns the moment a lease expires.
    harness.clock.advance(term());
    harness.open_quiet();
    assert_eq!(harness.table.len(), 1);
}

#[test]
fn a_lease_table_at_the_hard_cap_fills_refuses_and_drains_in_one_sweep() {
    let mut harness = Harness::with_limits(1024, Duration::from_mins(1), 8, Duration::from_mins(1));
    let mut leases = BTreeSet::new();
    for _ in 0..1024 {
        leases.insert(harness.open_quiet().0);
    }
    assert_eq!(leases.len(), 1024, "every identity is distinct");
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        }),
        Err(ProtocolError::Backpressure)
    );
    harness.assert_hint_is_a_lower_bound();
    harness.clock.advance(term());
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1024);
    assert_eq!(harness.released(ReleaseReason::Expired), 1024);
    assert_eq!(harness.table.len(), 0);
}

#[test]
fn the_table_itself_refuses_an_install_past_its_limit_whatever_its_callers_do() {
    let mut harness = Harness::with_limits(1, Duration::from_mins(1), 8, Duration::from_mins(1));
    let now = harness.table.now();
    let lease = |harness: &Harness| {
        Lease::grant(
            harness.table.now(),
            PUBLICATION_LEASE,
            Box::from(b"c".as_slice()),
            EventCredit::new(1).expect("credit"),
            LeasePhase::Live,
        )
        .expect("lease")
    };
    let first = LocalSubscriptionId::from_bytes([1; 16]);
    let second = LocalSubscriptionId::from_bytes([2; 16]);
    let granted = lease(&harness);
    assert_eq!(harness.table.install(first, granted), Ok(()));
    let granted = lease(&harness);
    assert_eq!(
        harness.table.install(second, granted),
        Err(LeaseRefusal::CapacityFull)
    );
    // `reserve` is only the early exit before a daemon trip.
    assert_eq!(harness.table.reserve(now), Err(LeaseRefusal::CapacityFull));
}

// ------------------------------------------------------------ reset hydration

#[test]
fn a_multipage_reset_hydrates_page_by_page_and_every_page_grants_a_fresh_term() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let first = token.expect("first continuation");
    let opened_at = harness.clock.now();
    {
        let LeasePhase::Hydrating(hydration) = harness.table.get(lease).expect("lease").phase()
        else {
            panic!("a three-page reset hydrates");
        };
        // The first page spent one of the page budget; the window is absolute.
        assert_eq!(hydration.pages_left().get(), ResetPages::LIMIT.get() - 1);
        assert_eq!(
            hydration.window_ends(),
            Deadline::after(
                opened_at,
                SubscriptionLeaseLimits::default().max_reset_window()
            )
            .expect("window")
        );
    }
    // Three pages, each fetched 8 s apart: 24 s in all, far past one 10 s
    // term, yet every page proved liveness so the lease is still retained.
    harness.clock.advance(8 * SECOND);
    let second = harness
        .next_page(lease, &first)
        .expect("second continuation");
    harness.clock.advance(8 * SECOND);
    assert!(
        harness.next_page(lease, &second).is_none(),
        "the third page is the last"
    );
    harness.clock.advance(8 * SECOND);
    let target = harness
        .table
        .get(lease)
        .expect("lease survived 24 s of slow hydration")
        .cursor()
        .to_vec();
    assert!(matches!(
        harness.table.get(lease).expect("lease").phase(),
        LeasePhase::Live
    ));
    assert!(
        harness.ack(lease, &target).is_ok(),
        "the hydrated root can now be acknowledged"
    );
    assert_eq!(harness.table.released().total(), 0);
}

#[test]
fn a_replayed_page_neither_advances_nor_extends_the_lease() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let first = token.expect("first continuation");
    let built = harness.source.pages_built.get();
    let second = harness
        .next_page(lease, &first)
        .expect("second continuation");
    harness.clock.advance(9 * SECOND);
    assert_eq!(
        harness.page(lease, &first),
        Err(refused(LeaseRefusal::PageReplay)),
        "the continuation was consumed"
    );
    assert_eq!(
        harness.source.pages_built.get(),
        built + 1,
        "a replay builds no page"
    );
    harness.clock.advance(SECOND);
    assert_eq!(
        harness.page(lease, &second),
        Err(refused(LeaseRefusal::UnknownLease)),
        "the replay did not extend the term the genuine page granted"
    );
}

#[test]
fn a_forged_stale_or_empty_page_token_is_refused_and_changes_nothing() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let first = token.expect("first continuation");
    harness.clock.advance(9 * SECOND);
    for forged in [&b""[..], b"token:forged", &first[..first.len() - 1]] {
        assert_eq!(
            harness.page(lease, forged),
            Err(refused(LeaseRefusal::PageReplay))
        );
    }
    assert_eq!(
        harness.source.pages_built.get(),
        1,
        "only the first page was ever built"
    );
    harness.clock.advance(SECOND);
    assert!(
        harness.page(lease, &first).is_err(),
        "refusals did not extend the lease: it expired at its original deadline"
    );
}

#[test]
fn a_page_for_a_lease_with_no_pending_reset_is_refused() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_quiet();
    assert_eq!(
        harness.page(lease, b"token"),
        Err(refused(LeaseRefusal::NoPendingReset))
    );
    assert!(harness.table.get(lease).is_some());
}

#[test]
fn a_reset_that_is_one_page_retains_no_root() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["only"]).expect("reset");
    assert!(token.is_none());
    assert!(matches!(
        harness.table.get(lease).expect("lease").phase(),
        LeasePhase::Live
    ));
    let (lease, token) = harness.open_reset(&[]).expect("empty reset");
    assert!(
        token.is_none(),
        "an empty root is one page that carries the descriptor"
    );
    assert!(harness.table.get(lease).is_some());
}

#[test]
fn only_page_and_cancel_are_meaningful_while_a_reset_hydrates() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b"]).expect("reset");
    token.expect("hydrating");
    let target = harness.table.get(lease).expect("lease").cursor().to_vec();
    assert_eq!(
        harness.ack(lease, &target),
        Err(refused(LeaseRefusal::HydrationInProgress)),
        "acknowledging a root not yet held would be a lie"
    );
    assert_eq!(
        harness.renew(lease, &target),
        Err(refused(LeaseRefusal::HydrationInProgress))
    );
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Credit { lease, credit: 1 }),
        Err(refused(LeaseRefusal::HydrationInProgress))
    );
    assert_eq!(
        harness.resume(lease, &target),
        Err(refused(LeaseRefusal::ResumeCursorMismatch))
    );
    // None of the refusals released or extended anything.
    harness.clock.advance(term());
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
}

#[test]
fn renewing_cannot_pin_a_reset_root_that_is_not_progressing() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_reset(&["a", "b"]).expect("reset");
    let root = harness.weak_root(lease);
    let target = harness.table.get(lease).expect("lease").cursor().to_vec();
    for _ in 0..5 {
        harness.clock.advance(term() / 2);
        let _ = harness.renew(lease, &target);
    }
    assert!(
        root_gone(&root),
        "five refused renewals over 25 s did not keep the root"
    );
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
}

#[test]
fn cancelling_a_hydrating_lease_frees_its_root_exactly_once() {
    let mut harness = Harness::default();
    let (lease, _) = harness.open_reset(&["a", "b"]).expect("reset");
    let root = harness.weak_root(lease);
    assert!(harness.cancel(lease).is_ok());
    assert!(root_gone(&root));
    assert_eq!(
        harness.cancel(lease),
        Err(refused(LeaseRefusal::UnknownLease))
    );
    assert_eq!(harness.released(ReleaseReason::Cancelled), 1);
    assert_eq!(harness.table.released().total(), 1);
}

// ----------------------------------------------- the two bounds a holder cannot extend

#[test]
fn a_slow_drip_of_valid_pages_is_ended_by_the_absolute_reset_window() {
    // Term 10 s, window 30 s. A page every 8 s keeps renewing the term, but
    // the window never moves.
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 100, Duration::from_secs(30));
    let labels: Vec<String> = (0..10).map(|n| format!("row-{n}")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let (lease, token) = harness.open_reset(&labels).expect("reset");
    let root = harness.weak_root(lease);
    let mut token = token.expect("continuation");
    for _ in 0..3 {
        harness.clock.advance(8 * SECOND);
        token = harness.next_page(lease, &token).expect("another page");
    }
    // 24 s in: the term was just renewed, the window has 6 s left.
    harness.clock.advance(8 * SECOND);
    assert_eq!(
        harness.page(lease, &token),
        Err(refused(LeaseRefusal::UnknownLease)),
        "the window elapsed although the lease term was still live"
    );
    assert!(root_gone(&root));
    assert_eq!(harness.released(ReleaseReason::ResetWindowElapsed), 1);
    assert_eq!(harness.released(ReleaseReason::Expired), 0);
}

#[test]
fn the_reset_window_elapsing_during_a_page_encode_withholds_that_page() {
    // A 5 s window inside a 10 s term, so only the window is due when the
    // encode returns at 7 s.
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 100, Duration::from_secs(5));
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let root = harness.weak_root(lease);
    harness.script_pages([PageScript::ServeAfter(6 * SECOND)]);
    harness.clock.advance(SECOND);
    assert_eq!(
        harness.page(lease, &token.expect("continuation")),
        Err(ProtocolError::ResetDeadlineExceeded)
    );
    assert!(root_gone(&root));
    assert_eq!(harness.released(ReleaseReason::ResetWindowElapsed), 1);
}

#[test]
fn a_reset_that_needs_more_pages_than_its_budget_is_refused_at_open() {
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 1, Duration::from_mins(1));
    assert_eq!(
        harness
            .open_reset(&["a", "b"])
            .expect_err("two pages exceed a budget of one"),
        ProtocolError::ResetPageBudgetExhausted
    );
    assert_eq!(
        harness.table.len(),
        0,
        "nothing was retained for an unservable reset"
    );
    // A reset that fits the budget exactly is admitted.
    assert!(harness.open_reset(&["only"]).is_ok());
}

#[test]
fn a_reset_that_would_outrun_its_page_budget_stops_the_moment_it_cannot_finish() {
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 3, Duration::from_mins(1));
    let (lease, token) = harness
        .open_reset(&["a", "b", "c", "d", "e"])
        .expect("reset");
    let root = harness.weak_root(lease);
    let second = harness
        .next_page(lease, &token.expect("p2"))
        .expect("p3 announced");
    // The third page (the last the budget allows) announces a fourth.
    assert_eq!(
        harness.page(lease, &second),
        Err(ProtocolError::ResetPageBudgetExhausted)
    );
    assert!(
        root_gone(&root),
        "the unfinishable reset was released at once"
    );
    assert_eq!(harness.released(ReleaseReason::ResetPagesExhausted), 1);
    assert_eq!(
        harness.page(lease, &second),
        Err(refused(LeaseRefusal::UnknownLease))
    );
}

#[test]
fn a_reset_of_exactly_the_budgeted_pages_completes() {
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 3, Duration::from_mins(1));
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let second = harness.next_page(lease, &token.expect("p2")).expect("p3");
    assert!(harness.next_page(lease, &second).is_none());
    assert_eq!(harness.table.released().total(), 0);
}

// ------------------------------------------------------------ failure paths

#[test]
fn a_page_that_fails_to_encode_releases_the_reset_and_never_extends_the_lease() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let root = harness.weak_root(lease);
    let failure = ProtocolError::InvalidControl("snapshot page encoding");
    harness.script_pages([PageScript::Fail(failure.clone())]);
    harness.clock.advance(5 * SECOND);
    assert_eq!(
        harness.page(lease, &token.expect("continuation")),
        Err(failure)
    );
    assert!(
        root_gone(&root),
        "the failed reset's root is released at once"
    );
    assert_eq!(harness.released(ReleaseReason::ResetPageUnservable), 1);
    assert!(harness.table.get(lease).is_none());
}

#[test]
fn the_first_reset_page_failing_to_encode_retains_nothing() {
    let mut harness = Harness::default();
    harness.script_pages([PageScript::Fail(ProtocolError::InvalidControl(
        "snapshot page encoding",
    ))]);
    assert_eq!(
        harness
            .open_reset(&["a", "b"])
            .expect_err("first page failed"),
        ProtocolError::InvalidControl("snapshot page encoding")
    );
    assert_eq!(harness.table.len(), 0);
}

#[test]
fn a_resume_whose_reset_page_fails_releases_the_lease_it_resumed() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.script(reset_reply(&["a", "b"]));
    harness.script_pages([PageScript::Fail(ProtocolError::InvalidControl(
        "snapshot page encoding",
    ))]);
    assert_eq!(
        harness.resume(lease, &cursor),
        Err(ProtocolError::InvalidControl("snapshot page encoding"))
    );
    assert!(harness.table.get(lease).is_none());
    assert_eq!(harness.released(ReleaseReason::ResetPageUnservable), 1);
}

#[test]
fn a_resume_whose_event_batch_cannot_be_encoded_keeps_the_lease_but_does_not_extend_it() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.source.events_error =
        Some(ProtocolError::InvalidControl("subscription event encoding"));
    harness.script(SubscriptionReply::Events {
        credit: CREDIT,
        cursor: cursor.clone(),
        events: Box::new([]),
    });
    harness.clock.advance(term() - SECOND);
    assert_eq!(
        harness.resume(lease, &cursor),
        Err(ProtocolError::InvalidControl("subscription event encoding"))
    );
    assert!(harness.table.get(lease).is_some());
    harness.clock.advance(SECOND);
    assert_eq!(
        harness.table.reclaim_due(harness.clock.now()),
        1,
        "still due at its old deadline"
    );
}

#[test]
fn a_page_that_makes_no_progress_cannot_keep_a_lease_alive() {
    for stall in [
        PageScript::Empty,
        PageScript::Repeating,
        PageScript::Misaddressed,
    ] {
        let mut harness = Harness::default();
        let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
        let root = harness.weak_root(lease);
        harness.script_pages([stall]);
        assert_eq!(
            harness.page(lease, &token.expect("continuation")),
            Err(refused(LeaseRefusal::PageStalled))
        );
        assert!(root_gone(&root));
        assert_eq!(harness.released(ReleaseReason::ResetPageUnservable), 1);
    }
}

#[test]
fn a_lease_expiring_while_its_page_is_being_encoded_is_not_extended_by_that_page() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let root = harness.weak_root(lease);
    harness.clock.advance(term() - SECOND);
    harness.script_pages([PageScript::ServeAfter(2 * SECOND)]);
    assert_eq!(
        harness.page(lease, &token.expect("continuation")),
        Err(refused(LeaseRefusal::Expired))
    );
    assert!(root_gone(&root));
    assert_eq!(harness.released(ReleaseReason::Expired), 1);
}

#[test]
fn an_unconfirmed_reply_kind_cannot_install_a_lease() {
    let mut harness = Harness::default();
    harness.script(SubscriptionReply::Reset { credit: CREDIT });
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        }),
        Err(ProtocolError::InvalidControl(
            "subscription reset omitted its replacement root"
        ))
    );
    assert_eq!(harness.table.len(), 0);
}

// ----------------------------------------------------------------- shutdown

#[test]
fn closing_releases_every_lease_and_root_and_admits_no_later_one() {
    let mut harness = Harness::default();
    let (reset, _) = harness.open_reset(&["a", "b"]).expect("reset");
    let (quiet, _) = harness.open_quiet();
    let root = harness.weak_root(reset);
    harness.table.close();
    assert!(root_gone(&root));
    assert_eq!(harness.table.len(), 0);
    assert_eq!(harness.table.earliest(), None);
    assert_eq!(harness.released(ReleaseReason::OwnerClosed), 2);
    // After close there is nothing to resume and nothing to open.
    let calls = harness.source.subscribe_calls.get();
    assert_eq!(
        harness.cancel(quiet),
        Err(refused(LeaseRefusal::UnknownLease))
    );
    harness.script(SubscriptionReply::Accepted { credit: CREDIT });
    assert_eq!(
        harness.call(LocalSubscriptionOperation::Open {
            cursor: Box::new([]),
            credit: CREDIT,
            lease_ms: PUBLICATION_LEASE.get(),
        }),
        Err(ProtocolError::Closed)
    );
    assert_eq!(
        harness.source.subscribe_calls.get(),
        calls,
        "no daemon trip after close"
    );
    assert_eq!(harness.table.len(), 0);
}

#[test]
fn a_successor_for_a_released_lease_is_dropped_not_resurrected() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    assert!(harness.cancel(lease).is_ok());
    let successor = Lease::grant(
        harness.table.now(),
        PUBLICATION_LEASE,
        cursor.clone(),
        EventCredit::new(1).expect("credit"),
        LeasePhase::Live,
    )
    .expect("lease");
    assert!(!harness.table.replace(lease, successor));
    assert!(harness.table.get(lease).is_none());
    harness.table.close();
    let successor = Lease::grant(
        harness.table.now(),
        PUBLICATION_LEASE,
        cursor,
        EventCredit::new(1).expect("credit"),
        LeasePhase::Live,
    )
    .expect("lease");
    assert!(!harness.table.replace(lease, successor));
}

// ------------------------------------------------------- expiry bookkeeping

#[test]
fn a_renewed_lease_survives_the_stale_hint_then_expires_at_its_new_deadline() {
    let mut harness = Harness::default();
    let (lease, cursor) = harness.open_quiet();
    harness.clock.advance(term() / 2);
    assert!(harness.renew(lease, &cursor).is_ok());
    // The hint still names the original deadline; reaching it triggers one
    // exact scan that retains the renewed lease and corrects the hint.
    harness.clock.advance(term() / 2);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 0);
    assert!(harness.table.get(lease).is_some());
    assert_eq!(harness.table.earliest(), harness.table.exact_earliest());
    harness.clock.advance(term() / 2);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
}

#[test]
fn the_expiry_hint_is_always_a_lower_bound_across_a_mixed_schedule() {
    let mut harness = Harness::default();
    let mut live = Vec::new();
    for step in 0..40_u32 {
        harness.assert_hint_is_a_lower_bound();
        match step % 5 {
            0 | 1 => live.push(harness.open_quiet()),
            2 => {
                if let Some((lease, cursor)) = live.first().cloned() {
                    let _ = harness.renew(lease, &cursor);
                }
            }
            3 => {
                if let Some((lease, _)) = live.pop() {
                    let _ = harness.cancel(lease);
                }
            }
            _ => harness.clock.advance(3 * SECOND),
        }
    }
    harness.assert_hint_is_a_lower_bound();
}

#[test]
fn the_release_reasons_are_closed_and_indexed_without_collision() {
    let indexes: BTreeSet<usize> = ReleaseReason::ALL
        .iter()
        .map(|reason| reason.index())
        .collect();
    assert_eq!(indexes.len(), ReleaseReason::ALL.len());
    assert_eq!(
        indexes.into_iter().max(),
        Some(ReleaseReason::ALL.len() - 1)
    );
}

#[test]
fn refusal_messages_are_the_stable_wire_texts_clients_match_on() {
    // The observer treats any protocol rejection as "reacquire"; hosts and
    // logs match on these exact strings, so they must not drift silently.
    assert_eq!(
        refused(LeaseRefusal::UnknownLease),
        ProtocolError::InvalidControl("unknown subscription lease")
    );
    assert_eq!(
        refused(LeaseRefusal::Expired),
        ProtocolError::InvalidControl("subscription lease expired")
    );
    assert_eq!(
        refused(LeaseRefusal::PageReplay),
        ProtocolError::InvalidControl("snapshot page continuation mismatch or replay")
    );
    assert_eq!(
        refused(LeaseRefusal::CapacityFull),
        ProtocolError::Backpressure
    );
    assert_eq!(refused(LeaseRefusal::OwnerClosed), ProtocolError::Closed);
    assert_eq!(
        ProtocolError::from(ResetFault::WindowElapsed),
        ProtocolError::ResetDeadlineExceeded
    );
    assert_eq!(
        ProtocolError::from(ResetFault::PagesExhausted),
        ProtocolError::ResetPageBudgetExhausted
    );
    assert_eq!(
        ProtocolError::from(ResetFault::DeadlineUnavailable),
        ProtocolError::LeaseDeadlineUnavailable
    );
}

#[test]
fn the_terms_the_observer_requests_fit_the_owner_defaults() {
    let limits = SubscriptionLeaseLimits::default();
    for requested in [PUBLICATION_LEASE, BOOTSTRAP_LEASE] {
        assert_eq!(limits.grant_term(requested.get()), Some(requested));
    }
    assert_eq!(LeaseMs::new(1).map(LeaseMs::get), Some(1));
}

// -------------------------------------- bounds that must hold under every schedule

#[test]
fn the_idle_poll_frees_a_reset_root_at_its_window_even_while_the_lease_term_runs() {
    // A 5 s window inside a 10 s term: when the window ends nothing a holder
    // does is pending, and the root must go without waiting for the term.
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 100, Duration::from_secs(5));
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    token.expect("hydrating");
    let root = harness.weak_root(lease);
    harness.clock.advance(Duration::from_secs(5) - NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 0);
    harness.clock.advance(NANOSECOND);
    assert_eq!(harness.table.reclaim_due(harness.clock.now()), 1);
    assert!(root_gone(&root));
    assert_eq!(harness.released(ReleaseReason::ResetWindowElapsed), 1);
    assert_eq!(harness.released(ReleaseReason::Expired), 0);
}

#[test]
fn a_lease_that_expires_while_its_page_fails_to_encode_is_released_exactly_once() {
    let mut harness = Harness::default();
    let (lease, token) = harness.open_reset(&["a", "b", "c"]).expect("reset");
    let root = harness.weak_root(lease);
    harness.clock.advance(term() - SECOND);
    let failure = ProtocolError::InvalidControl("snapshot page encoding");
    harness.script_pages([PageScript::FailAfter(2 * SECOND, failure.clone())]);
    assert_eq!(
        harness.page(lease, &token.expect("continuation")),
        Err(failure)
    );
    assert!(root_gone(&root));
    assert_eq!(
        harness.table.released().total(),
        1,
        "one lease, one release"
    );
    assert_eq!(harness.released(ReleaseReason::ResetPageUnservable), 1);
    assert_eq!(
        harness.table.reclaim_due(harness.clock.now()),
        0,
        "nothing left to sweep"
    );
}

#[test]
fn a_resume_whose_reset_needs_more_pages_than_the_budget_releases_the_lease() {
    let mut harness = Harness::with_limits(4, Duration::from_mins(1), 1, Duration::from_mins(1));
    let (lease, cursor) = harness.open_quiet();
    harness.script(reset_reply(&["a", "b"]));
    assert_eq!(
        harness.resume(lease, &cursor),
        Err(ProtocolError::ResetPageBudgetExhausted)
    );
    assert!(harness.table.get(lease).is_none());
    assert_eq!(harness.released(ReleaseReason::ResetPagesExhausted), 1);
}

#[test]
fn an_open_answered_with_events_holds_the_lease_at_the_batch_cursor_and_resume_advances_it() {
    let mut harness = Harness::default();
    let batch_cursor: Box<[u8]> = Box::from(b"after-the-batch".as_slice());
    let response = harness
        .open_with(
            SubscriptionReply::Events {
                credit: CREDIT,
                cursor: batch_cursor.clone(),
                events: Box::new([]),
            },
            CREDIT,
            PUBLICATION_LEASE.get(),
        )
        .expect("open");
    let LocalSubscriptionResponse::Batch {
        lease,
        cursor,
        credit,
        ..
    } = response
    else {
        panic!("an open answered with events answers a batch: {response:?}");
    };
    assert_eq!(cursor, batch_cursor);
    assert_eq!(credit, CREDIT);
    assert!(
        harness.ack(lease, &batch_cursor).is_ok(),
        "the holder's cursor is the batch's"
    );
    // A resume from that cursor is answered by the next batch and moves the
    // lease's cursor with it.
    let next_cursor: Box<[u8]> = Box::from(b"after-the-next-batch".as_slice());
    harness.script(SubscriptionReply::Events {
        credit: CREDIT,
        cursor: next_cursor.clone(),
        events: Box::new([]),
    });
    assert!(matches!(
        harness.resume(lease, &batch_cursor),
        Ok(LocalSubscriptionResponse::Batch { cursor, .. }) if cursor == next_cursor
    ));
    assert_eq!(
        harness.ack(lease, &batch_cursor),
        Err(refused(LeaseRefusal::AckCursorMismatch)),
        "the old cursor is no longer the lease's"
    );
    assert!(harness.ack(lease, &next_cursor).is_ok());
}
