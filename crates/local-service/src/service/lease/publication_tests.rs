//! The owner's lease protocol and the observer's reset driver, run against
//! each other over a real listener, a real socket and the production page
//! certifier, with one injected clock.
//!
//! These are the tests of the *agreement* between the two crates: that the
//! term the observer asks for is the term the owner grants and honors, that a
//! reset the observer is willing to wait for is one the owner keeps serving,
//! and that when either side gives up the other reclaims what it holds.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::host::LeaseHost;
use super::limits::SubscriptionLeaseLimits;
use super::state::ReleaseReason;
use super::table::{LeaseTable, ReleaseCounts};
use super::tests::{SECOND, ScriptedSource};
use crate::listener::{
    FilesystemPeerPolicy, ListenerConfig, ListenerShutdown, PeerPolicy, PeerPolicyError,
    UnixListenerService,
};
use crate::protocol::{EngineRequest, EngineStatus, FrameLimits, ProtocolError};
use crate::service::{LocaldService, OwnerService};
use crate::test_support::socket_path;
use backend_client::LocalSubscriptionTransport;
use backend_client::lease_contract::{PUBLICATION_CREDIT, PUBLICATION_LEASE, max_reset_time};
use backend_client::monotonic::{ManualClock, MonotonicClock};
use backend_engine::{
    AuthorityScopeClaim, Basis, CoverageCapability, Cursor, CursorResetReason, Frontier,
    ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ScopeRoot,
    SubscriptionReply, UntrustedProducerObservation, ViewCoverage, ViewRoot, admit_complete_scope,
    admit_producer_observation, object_version, symbol_key, view_key, view_state_root,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- fixtures

struct FixtureVerifier;
impl ProducerObservationVerifier for FixtureVerifier {
    type Error = &'static str;
    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

/// A root of `rows` symbol rows whose complete coverage a producer certified:
/// the shape the production page certifier requires and an observer admits.
fn certified_root(rows: usize, sequence: u64) -> Arc<ViewRoot> {
    let basis = Basis::new(
        view_state_root(&[]),
        object_version(crate::builtin::VIEW_SOURCE_VALUE),
    );
    let scope = ScopeRoot::from_bytes(basis.object.to_bytes());
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new([7; 32], scope, [9; 32], b"e2e-evidence".to_vec()),
        &FixtureVerifier,
    )
    .expect("observation");
    let capability = CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(basis.object),
            observation,
        )
        .expect("source scope"),
        b"e2e-evidence".to_vec(),
    )
    .expect("coverage");
    let rows = (0..rows)
        .map(|index| {
            let label = format!("symbol-{index:06}");
            Row::new(RowId::Symbol(symbol_key(&label)), basis, label)
        })
        .collect();
    Arc::new(
        ViewRoot::new_checked(
            view_key(b"library-view-v1"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, sequence),
            rows,
            vec![ViewCoverage::Complete],
            capability,
        )
        .expect("certified root"),
    )
}

fn reset_to(root: &Arc<ViewRoot>, sequence: u64) -> SubscriptionReply {
    SubscriptionReply::ResetWithRoot {
        credit: PUBLICATION_CREDIT,
        cursor: Cursor::for_view_root_at(root, sequence).encode_control(),
        root: Box::new(ViewRoot::clone(root)),
        reason: CursorResetReason::Gap,
    }
}

/// What the test observes of the owner thread.
#[derive(Clone, Copy, Debug, Default)]
struct Ledger {
    active: usize,
    released: ReleaseCounts,
}

struct PublicationOwner {
    table: LeaseTable,
    source: ScriptedSource,
    ledger: Arc<Mutex<Ledger>>,
}

impl PublicationOwner {
    fn publish(&self) {
        *self.ledger.lock().expect("ledger") = Ledger {
            active: self.table.len(),
            released: self.table.released(),
        };
    }
}

impl OwnerService for PublicationOwner {
    fn command(&mut self, _body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        Err(ProtocolError::CommandExecution(
            "lease owner takes no commands".to_owned(),
        ))
    }

    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let EngineRequest::Subscription(request) = request else {
            return Err(ProtocolError::InvalidControl("lease requests only"));
        };
        let result = LeaseHost::new(&mut self.table, &mut self.source).handle(request_id, request);
        self.publish();
        result
    }

    fn serve_one(&mut self) -> bool {
        let now = self.table.now();
        self.table.reclaim_due(now);
        self.publish();
        false
    }

    fn close(&mut self) {
        self.table.close();
        self.publish();
    }
}

#[derive(Debug)]
struct CountingPolicy {
    accepted: Arc<AtomicUsize>,
}

impl PeerPolicy for CountingPolicy {
    fn authorize(&self, stream: &backend_engine::LocalStream) -> Result<(), PeerPolicyError> {
        self.accepted.fetch_add(1, Ordering::SeqCst);
        FilesystemPeerPolicy.authorize(stream)
    }
}

/// An owner thread behind a real private endpoint.
struct Wire {
    clock: Arc<ManualClock>,
    ledger: Arc<Mutex<Ledger>>,
    endpoint: PathBuf,
    accepted: Arc<AtomicUsize>,
    shutdown: ListenerShutdown,
    thread: Option<JoinHandle<()>>,
}

impl Wire {
    fn start(
        clock: &Arc<ManualClock>,
        limits: SubscriptionLeaseLimits,
        script: impl FnOnce(&mut ScriptedSource),
    ) -> Self {
        let endpoint = socket_path("publication");
        let mut source = ScriptedSource::new(clock.clone());
        source.use_production_pager();
        script(&mut source);
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let owner = PublicationOwner {
            table: LeaseTable::new(limits, clock.clone()),
            source,
            ledger: Arc::clone(&ledger),
        };
        let accepted = Arc::new(AtomicUsize::new(0));
        let mut config = ListenerConfig::new(&endpoint);
        config.poll_interval = Duration::from_millis(1);
        config.idle_timeout = None;
        let service = LocaldService::new(owner, FrameLimits::default()).expect("service");
        let listener = UnixListenerService::bind_with_peer_policy(
            service,
            config,
            Arc::new(CountingPolicy {
                accepted: Arc::clone(&accepted),
            }),
        )
        .expect("bind the lease endpoint");
        let shutdown = listener.shutdown_handle();
        let mut listener = listener;
        let thread = std::thread::spawn(move || {
            let _ = listener.run();
        });
        Self {
            clock: Arc::clone(clock),
            ledger,
            endpoint,
            accepted,
            shutdown,
            thread: Some(thread),
        }
    }

    fn client(&self) -> LocalSubscriptionTransport {
        LocalSubscriptionTransport::connect(&self.endpoint)
            .expect("an authenticated connection to the owner")
            .with_clock(self.clock.clone())
    }

    fn ledger(&self) -> Ledger {
        *self.ledger.lock().expect("ledger")
    }

    /// Waits (in real time, bounded) for the owner thread to reach a state.
    fn wait_until(&self, what: &str, reached: impl Fn(&Ledger) -> bool) -> Ledger {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ledger = self.ledger();
            if reached(&ledger) {
                return ledger;
            }
            assert!(
                Instant::now() < deadline,
                "owner never reached: {what}: {ledger:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.shutdown.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// -------------------------------------------------------------------- tests

#[test]
fn the_defaults_each_side_ships_admit_the_other_with_room_to_spare() {
    // The observer's longest reset is shorter than the owner's reset window,
    // and its term is within the owner's: so a reset the observer will wait
    // for is one the owner keeps serving.
    let limits = SubscriptionLeaseLimits::default();
    assert!(max_reset_time() < limits.max_reset_window());
    assert!(PUBLICATION_LEASE.duration() <= limits.max_term());
    assert_eq!(
        limits.max_reset_pages(),
        backend_client::lease_contract::ResetPages::LIMIT.get()
    );
}

#[test]
fn a_slow_but_legitimate_multipage_reset_completes_within_the_allowance_it_earned() {
    let clock = ManualClock::new();
    let target = certified_root(192, 1);
    let wire = Wire::start(&clock, SubscriptionLeaseLimits::default(), |source| {
        source.replies.push_back(Ok(reset_to(&target, 1)));
        // Three 64-row pages, each taking 6 s to produce: the reset lasts
        // longer than the 10 s base allowance, inside the 10 s + 3 x 2 s the
        // first descriptor earned, and every page renews a 10 s lease term.
        source.set_page_cost(6 * SECOND);
    });
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let started = clock.now();
    let state = client
        .acquire_publications(Arc::clone(&base), cursor, &|| false)
        .expect("a slow reset inside its allowance is admitted");
    assert!(clock.now() - started >= 18 * SECOND, "three pages of 6 s");
    assert_eq!(state.root().row_count(), 192);
    assert_eq!(state.root().root(), target.root());
    assert_eq!(state.cursor(), Cursor::for_view_root_at(&target, 1));
    let ledger = wire.ledger();
    assert_eq!(
        ledger.released.total(),
        0,
        "the owner kept the lease throughout"
    );
    assert_eq!(ledger.active, 1);
}

#[test]
fn a_reset_slower_than_its_allowance_is_abandoned_and_its_lease_released_on_the_same_socket() {
    let clock = ManualClock::new();
    // 256 rows are four pages: an 18 s allowance. Pages take 9 s each.
    let target = certified_root(256, 1);
    let wire = Wire::start(&clock, SubscriptionLeaseLimits::default(), |source| {
        source.replies.push_back(Ok(reset_to(&target, 1)));
        source.set_page_cost(9 * SECOND);
    });
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let error = client
        .acquire_publications(Arc::clone(&base), cursor, &|| false)
        .err()
        .expect("the reset is slower than it earned");
    assert_eq!(
        error,
        backend_client::ClientError::Protocol(
            "publication reset exceeded its time budget".to_owned()
        )
    );
    let ledger = wire.wait_until("the abandoned lease to be cancelled", |ledger| {
        ledger.released.total() == 1
    });
    assert_eq!(ledger.released.count(ReleaseReason::Cancelled), 1);
    assert_eq!(ledger.active, 0, "the retained reset root was released");
    assert_eq!(
        wire.accepted.load(Ordering::SeqCst),
        1,
        "the cancel used the socket it was opened on; nothing was re-dialled"
    );
}

#[test]
fn when_the_cancel_cannot_be_delivered_the_owner_reclaims_the_lease_on_its_own() {
    let clock = ManualClock::new();
    let target = certified_root(256, 1);
    let wire = Wire::start(&clock, SubscriptionLeaseLimits::default(), |source| {
        source.replies.push_back(Ok(reset_to(&target, 1)));
    });
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    // The holder withdraws after the first page and its socket is cut, so the
    // terminal cancel has nowhere to go.
    let interrupt = client.interrupt_handle().expect("exact socket");
    let polls = AtomicUsize::new(0);
    let withdrawn = AtomicBool::new(false);
    let error = client
        .acquire_publications(Arc::clone(&base), cursor, &|| {
            if polls.fetch_add(1, Ordering::SeqCst) >= 4 && !withdrawn.swap(true, Ordering::SeqCst)
            {
                interrupt.interrupt();
            }
            withdrawn.load(Ordering::SeqCst)
        })
        .err()
        .expect("withdrawn mid-reset");
    drop(error);
    let held = wire.wait_until("the owner to hold the abandoned reset", |ledger| {
        ledger.active == 1
    });
    assert_eq!(held.released.total(), 0, "no cancel reached the owner");
    // Nothing else ever touches the lease: only the owner's own expiry can
    // reclaim it, and its idle poll does so with no request in flight.
    clock.advance(PUBLICATION_LEASE.duration());
    let reclaimed = wire.wait_until("the owner to reclaim the abandoned lease", |ledger| {
        ledger.active == 0
    });
    assert_eq!(reclaimed.released.count(ReleaseReason::Expired), 1);
    assert_eq!(reclaimed.released.count(ReleaseReason::Cancelled), 0);
}

// ------------------------------------------------- a dishonest or ill-sized owner

/// Wraps a real owner and rewrites the answer to `Page` requests, to stand in
/// for a producer that replays or substitutes pages.
struct RewritingOwner {
    inner: PublicationOwner,
    rewrite: Box<
        dyn FnMut(
                u64,
                backend_engine::LocalSubscriptionResponse,
            ) -> backend_engine::LocalSubscriptionResponse
            + Send,
    >,
}

impl OwnerService for RewritingOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        self.inner.command(body)
    }

    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let status = self.inner.engine(request_id, request)?;
        Ok(match status {
            EngineStatus::Subscription(response) => {
                EngineStatus::Subscription((self.rewrite)(request_id, response))
            }
            other => other,
        })
    }

    fn serve_one(&mut self) -> bool {
        self.inner.serve_one()
    }

    fn close(&mut self) {
        self.inner.close();
    }
}

fn rewriting_wire(
    clock: &Arc<ManualClock>,
    script: impl FnOnce(&mut ScriptedSource),
    rewrite: impl FnMut(
        u64,
        backend_engine::LocalSubscriptionResponse,
    ) -> backend_engine::LocalSubscriptionResponse
    + Send
    + 'static,
) -> RewritingWire {
    let endpoint = socket_path("rewrite");
    let mut source = ScriptedSource::new(clock.clone());
    source.use_production_pager();
    script(&mut source);
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let owner = RewritingOwner {
        inner: PublicationOwner {
            table: LeaseTable::new(SubscriptionLeaseLimits::default(), clock.clone()),
            source,
            ledger: Arc::clone(&ledger),
        },
        rewrite: Box::new(rewrite),
    };
    let mut config = ListenerConfig::new(&endpoint);
    config.poll_interval = Duration::from_millis(1);
    config.idle_timeout = None;
    let service = LocaldService::new(owner, FrameLimits::default()).expect("service");
    let mut listener = UnixListenerService::bind(service, config).expect("bind");
    let shutdown = listener.shutdown_handle();
    let thread = std::thread::spawn(move || {
        let _ = listener.run();
    });
    RewritingWire {
        clock: Arc::clone(clock),
        ledger,
        endpoint,
        shutdown,
        thread: Some(thread),
    }
}

struct RewritingWire {
    clock: Arc<ManualClock>,
    ledger: Arc<Mutex<Ledger>>,
    endpoint: PathBuf,
    shutdown: ListenerShutdown,
    thread: Option<JoinHandle<()>>,
}

impl RewritingWire {
    fn client(&self) -> LocalSubscriptionTransport {
        LocalSubscriptionTransport::connect(&self.endpoint)
            .expect("an authenticated connection")
            .with_clock(self.clock.clone())
    }
}

impl Drop for RewritingWire {
    fn drop(&mut self) {
        self.shutdown.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn protocol_error(message: &str) -> backend_client::ClientError {
    backend_client::ClientError::Protocol(message.to_owned())
}

#[test]
fn a_replayed_page_is_refused_at_once_and_buys_no_time() {
    let clock = ManualClock::new();
    let target = certified_root(192, 1);
    let first = Arc::new(Mutex::new(None));
    let remembered = Arc::clone(&first);
    let wire = rewriting_wire(
        &clock,
        |source| source.replies.push_back(Ok(reset_to(&target, 1))),
        move |request_id, response| {
            use backend_engine::LocalSubscriptionResponse::SnapshotPage;
            let mut first = remembered.lock().expect("first page");
            match (&*first, &response) {
                (None, SnapshotPage { .. }) => {
                    *first = Some(response.clone());
                    response
                }
                // Every later page is the first page again.
                (
                    Some(SnapshotPage {
                        lease,
                        page,
                        next,
                        credit,
                        payload,
                        ..
                    }),
                    SnapshotPage { .. },
                ) => SnapshotPage {
                    request_id,
                    lease: *lease,
                    page: page.clone(),
                    next: next.clone(),
                    credit: *credit,
                    payload: payload.clone(),
                },
                _ => response,
            }
        },
    );
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let started = clock.now();
    let error = client
        .acquire_publications(base, cursor, &|| false)
        .err()
        .expect("a replayed page is not a page");
    assert_eq!(error, protocol_error("publication reset page mismatch"));
    assert_eq!(
        clock.now(),
        started,
        "the replay moved no time and was not retried"
    );
}

#[test]
fn a_page_from_a_different_root_cannot_continue_a_reset() {
    let clock = ManualClock::new();
    let target = certified_root(256, 1);
    let other = certified_root(320, 1);
    let other_for_rewrite = Arc::clone(&other);
    let calls = Arc::new(AtomicUsize::new(0));
    let wire = rewriting_wire(
        &clock,
        |source| source.replies.push_back(Ok(reset_to(&target, 1))),
        move |request_id, response| {
            use backend_engine::LocalSubscriptionResponse::SnapshotPage;
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return response;
            }
            // The continuation is honoured in form, but the page comes from a
            // root with a different row count.
            let SnapshotPage {
                lease,
                page,
                credit,
                ..
            } = response
            else {
                return response;
            };
            let plan = super::state::PagePlan {
                root: Arc::clone(&other_for_rewrite),
                target: Cursor::for_view_root_at(&other_for_rewrite, 1),
                reason: CursorResetReason::Gap,
                requested: backend_engine::ViewPageCursor::first(&other_for_rewrite),
            };
            let substituted = super::host::production_reset_page(
                &plan,
                super::state::PageCredit::new(PUBLICATION_CREDIT).expect("credit"),
            )
            .expect("a page of the other root");
            SnapshotPage {
                request_id,
                lease,
                page,
                next: substituted.next.map(|next| next.token),
                credit,
                payload: substituted.payload,
            }
        },
    );
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let error = client
        .acquire_publications(base, cursor, &|| false)
        .err()
        .expect("a substituted page is refused");
    // The descriptor's row count is the first thing that gives it away; even
    // had it matched, the hydrator would refuse a page for another root.
    assert_eq!(
        error,
        protocol_error("publication reset changed its row budget")
    );
}

#[test]
fn an_owner_that_cannot_grant_the_requested_term_refuses_it_and_retains_nothing() {
    let clock = ManualClock::new();
    let wire = Wire::start(
        &clock,
        SubscriptionLeaseLimits::new(8, Duration::from_secs(5), 64, Duration::from_mins(1))
            .expect("limits"),
        |source| {
            source.replies.push_back(Ok(SubscriptionReply::Accepted {
                credit: PUBLICATION_CREDIT,
            }));
        },
    );
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let error = client
        .acquire_publications(base, cursor, &|| false)
        .err()
        .expect("a 5 s owner cannot grant the observer's 10 s term");
    assert!(
        matches!(&error, backend_client::ClientError::Protocol(message) if message.contains("subscription lease bounds")),
        "{error:?}"
    );
    assert_eq!(wire.ledger().active, 0);
}

#[test]
fn an_owner_whose_page_budget_is_smaller_than_the_reset_needs_stops_it_and_the_observer_gives_up() {
    let clock = ManualClock::new();
    let target = certified_root(192, 1);
    let wire = Wire::start(
        &clock,
        SubscriptionLeaseLimits::new(8, Duration::from_mins(1), 2, Duration::from_mins(1))
            .expect("limits"),
        |source| source.replies.push_back(Ok(reset_to(&target, 1))),
    );
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let mut client = wire.client();
    let error = client
        .acquire_publications(base, cursor, &|| false)
        .err()
        .expect("three pages exceed a budget of two");
    assert!(
        matches!(&error, backend_client::ClientError::Protocol(message) if message.contains("page budget")),
        "{error:?}"
    );
    let ledger = wire.wait_until("the unfinishable reset to be released", |ledger| {
        ledger.released.total() == 1
    });
    assert_eq!(ledger.released.count(ReleaseReason::ResetPagesExhausted), 1);
    assert_eq!(ledger.active, 0);
}

#[test]
fn renewing_at_the_cadence_the_owner_granted_keeps_a_quiet_lease_alive_and_skipping_one_does_not() {
    let clock = ManualClock::new();
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let wire = Wire::start(&clock, SubscriptionLeaseLimits::default(), |source| {
        source.set_owner_cursor(cursor);
        source.replies.push_back(Ok(SubscriptionReply::Accepted {
            credit: PUBLICATION_CREDIT,
        }));
    });
    let mut client = wire.client();
    let mut state = client
        .acquire_publications(base, cursor, &|| false)
        .expect("a quiet lease");
    for _ in 0..10 {
        clock.advance(state.renew_after());
        client
            .renew_publications(&mut state)
            .expect("on-time renewal");
    }
    assert_eq!(
        wire.ledger().active,
        1,
        "50 s of renewals outlived the 10 s term"
    );
    // Skipping renewals for a whole term loses it, and the loss is typed.
    clock.advance(PUBLICATION_LEASE.duration());
    let lost = client
        .renew_publications(&mut state)
        .err()
        .expect("expired");
    assert!(matches!(
        &lost,
        backend_client::ClientError::Protocol(message) if message.contains("unknown subscription lease")
    ));
    assert_eq!(wire.ledger().released.count(ReleaseReason::Expired), 1);
}

#[test]
fn a_resume_abandoned_mid_reset_leaves_the_admitted_root_untouched_and_the_owner_reclaims() {
    let clock = ManualClock::new();
    let base = certified_root(0, 0);
    let cursor = Cursor::for_view_root_at(&base, 0);
    let target = certified_root(256, 1);
    let wire = Wire::start(&clock, SubscriptionLeaseLimits::default(), |source| {
        source.set_owner_cursor(cursor);
        source.replies.push_back(Ok(SubscriptionReply::Accepted {
            credit: PUBLICATION_CREDIT,
        }));
        // The resume is answered with a four-page reset.
        source.replies.push_back(Ok(reset_to(&target, 1)));
    });
    let mut client = wire.client();
    let mut state = client
        .acquire_publications(Arc::clone(&base), cursor, &|| false)
        .expect("a quiet lease");
    // Withdrawn after the second page: the owner has served two of four.
    let polls = AtomicUsize::new(0);
    let error = client
        .resume_publications(&mut state, &|| polls.fetch_add(1, Ordering::SeqCst) >= 3)
        .err()
        .expect("withdrawn mid-reset");
    assert_eq!(error, protocol_error("publication observation withdrawn"));
    assert_eq!(state.cursor(), cursor, "no partial reset moved the cursor");
    assert!(
        Arc::ptr_eq(&state.root(), &base),
        "no partial reset replaced the complete root"
    );
    wire.wait_until("the owner to hold the half-served reset", |ledger| {
        ledger.active == 1
    });
    clock.advance(PUBLICATION_LEASE.duration());
    let reclaimed = wire.wait_until("the owner to reclaim it", |ledger| ledger.active == 0);
    assert_eq!(reclaimed.released.count(ReleaseReason::Expired), 1);
}
