//! The lease protocol behind a real locald owner and a real listener.
//!
//! The scripted tests prove the rules; these prove the production wiring that
//! applies them: that the owner's idle poll is what reclaims, that the real
//! listener drives that poll with no client connected, that close releases
//! reset roots before it joins deferred workers.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::host::LeaseHost;
use super::state::{LeasePhase, ReleaseReason};
use super::tests::{SECOND, ScriptedSource, reset_reply, term};
use crate::builtin::{EmptyOwner, open_empty_owner};
use crate::listener::{ListenerConfig, UnixListenerService};
use crate::protocol::{EngineRequest, EngineStatus, FrameLimits};
use crate::service::{CommandOutcome, DeferredCommands, LocaldService, OwnerService};
use crate::test_support::socket_path;
use backend_client::lease_contract::PUBLICATION_LEASE;
use backend_client::monotonic::ManualClock;
use backend_engine::{
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResponse, SubscriptionReply, ViewRoot,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

const NANOSECOND: Duration = Duration::from_nanos(1);

fn real_owner(directory: &tempfile::TempDir, clock: &Arc<ManualClock>) -> EmptyOwner {
    open_empty_owner(directory.path())
        .expect("a real empty product owner")
        .with_lease_clock(clock.clone())
}

/// Installs a quiet lease in the real owner's own table. The daemon of an
/// empty workspace cannot yet serve a subscription (it has published no
/// complete view), so the open is answered by a scripted source; everything
/// after it, from the idle poll to Cancel, runs through the real owner.
fn open_on(owner: &mut EmptyOwner, clock: &Arc<ManualClock>) -> LocalSubscriptionId {
    let mut source = ScriptedSource::new(clock.clone());
    source
        .replies
        .push_back(Ok(SubscriptionReply::Accepted { credit: 64 }));
    let status = LeaseHost::new(&mut owner.leases, &mut source)
        .handle(
            1,
            LocalSubscriptionRequest {
                request_id: 1,
                operation: LocalSubscriptionOperation::Open {
                    cursor: Box::new([]),
                    credit: 64,
                    lease_ms: PUBLICATION_LEASE.get(),
                },
            },
        )
        .expect("scripted open");
    match status {
        EngineStatus::Subscription(LocalSubscriptionResponse::Opened { lease, .. }) => lease,
        other => panic!("a quiet open answers Opened: {other:?}"),
    }
}

#[test]
fn a_real_owner_reclaims_an_abandoned_lease_on_its_idle_poll_without_counting_it_as_work() {
    let directory = tempfile::tempdir().expect("workspace directory");
    let clock = ManualClock::new();
    let mut owner = real_owner(&directory, &clock);
    open_on(&mut owner, &clock);
    assert_eq!(owner.leases.len(), 1);

    clock.advance(term() - NANOSECOND);
    assert!(!owner.serve_one(), "an idle owner has no work");
    assert_eq!(owner.leases.len(), 1, "not yet due");
    clock.advance(NANOSECOND);
    assert!(
        !owner.serve_one(),
        "reclaiming an abandoned lease is housekeeping, not served work: it must not hold off idle retirement"
    );
    assert_eq!(owner.leases.len(), 0, "the idle poll alone released it");
    assert_eq!(owner.leases.released().count(ReleaseReason::Expired), 1);
}

#[test]
fn a_real_owner_that_is_not_polled_still_refuses_an_expired_lease_on_use() {
    // Reclamation may be delayed by blocking owner work; expiry must not be.
    let directory = tempfile::tempdir().expect("workspace directory");
    let clock = ManualClock::new();
    let mut owner = real_owner(&directory, &clock);
    let lease = open_on(&mut owner, &clock);
    clock.advance(term());
    let cancel = || {
        EngineRequest::Subscription(LocalSubscriptionRequest {
            request_id: 2,
            operation: LocalSubscriptionOperation::Cancel { lease },
        })
    };
    assert_eq!(
        owner.engine(2, cancel()),
        Err(ProtocolError::InvalidControl(
            "durable subscriptions require prepared response encoding"
        )),
        "the unprepared path refuses before it can touch a durable lease"
    );
    assert_eq!(
        owner.leases.len(),
        1,
        "rejection left the expired lease unswept"
    );
    assert_eq!(owner.leases.released().count(ReleaseReason::Expired), 0);
    assert_eq!(owner.leases.released().count(ReleaseReason::Cancelled), 0);

    assert_eq!(
        owner.engine_prepared(2, cancel(), FrameLimits::default()),
        Err(ProtocolError::InvalidControl("unknown subscription lease")),
        "the production prepared path sweeps expiry before dispatching Cancel"
    );
    assert_eq!(owner.leases.len(), 0);
    assert_eq!(owner.leases.released().count(ReleaseReason::Expired), 1);
    assert_eq!(owner.leases.released().count(ReleaseReason::Cancelled), 0);
}

#[test]
fn the_production_listener_drives_the_idle_poll_with_no_client_connected() {
    let directory = tempfile::tempdir().expect("workspace directory");
    let clock = ManualClock::new();
    let mut owner = real_owner(&directory, &clock);
    open_on(&mut owner, &clock);
    let socket = socket_path("lease-poll");
    let mut config = ListenerConfig::new(&socket);
    config.poll_interval = Duration::from_millis(1);
    config.idle_timeout = None;
    let service = LocaldService::new(owner, FrameLimits::default()).expect("service");
    let mut listener = UnixListenerService::bind(service, config).expect("bind listener");

    // No client ever connects. The listener's own loop is the only thing that
    // can reach the owner, and it must reach it on every iteration.
    listener.run_once().expect("iteration before the deadline");
    assert_eq!(listener.service_mut().owner_mut().leases.len(), 1);
    clock.advance(term());
    listener.run_once().expect("iteration at the deadline");
    assert_eq!(
        listener.service_mut().owner_mut().leases.len(),
        0,
        "the listener loop polled the owner and the abandoned lease was reclaimed"
    );
}

#[derive(Debug)]
struct JoinProbe {
    root: Weak<ViewRoot>,
    root_was_released_before_join: Arc<AtomicBool>,
    joined: Arc<AtomicBool>,
}

impl<M, V, A> DeferredCommands<M, V, A> for JoinProbe
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    fn command(
        &mut self,
        _daemon: &mut crate::Locald<M, V, A>,
        _body: &[u8],
        _ticket: u64,
    ) -> Result<CommandOutcome, String> {
        Err("the probe takes no commands".to_owned())
    }

    fn poll(
        &mut self,
        _daemon: &mut crate::Locald<M, V, A>,
    ) -> Vec<(u64, Result<Vec<u8>, String>)> {
        Vec::new()
    }

    /// Stands in for the join of a long deferred worker: by the time the owner
    /// reaches it, the reset root must already be gone.
    fn close(&mut self) {
        self.root_was_released_before_join
            .store(self.root.upgrade().is_none(), Ordering::Release);
        self.joined.store(true, Ordering::Release);
    }
}

#[test]
fn closing_the_owner_frees_reset_roots_before_it_joins_deferred_workers() {
    let directory = tempfile::tempdir().expect("workspace directory");
    let clock = ManualClock::new();
    // A hydrating lease, installed through the real protocol against the
    // owner's own table (the real daemon has no rows to page through).
    let mut owner = real_owner(&directory, &clock);
    let mut source = ScriptedSource::new(clock.clone());
    source.replies.push_back(Ok(reset_reply(&["a", "b", "c"])));
    let response = LeaseHost::new(&mut owner.leases, &mut source)
        .handle(
            1,
            LocalSubscriptionRequest {
                request_id: 1,
                operation: LocalSubscriptionOperation::Open {
                    cursor: Box::new([]),
                    credit: 1,
                    lease_ms: PUBLICATION_LEASE.get(),
                },
            },
        )
        .expect("scripted open");
    let EngineStatus::Subscription(LocalSubscriptionResponse::SnapshotPage { lease, .. }) =
        response
    else {
        panic!("a multi-row reset answers a page");
    };
    let root = match owner.leases.get(lease).expect("lease").phase() {
        LeasePhase::Hydrating(hydration) => Arc::downgrade(hydration.root()),
        LeasePhase::Live => panic!("the reset has pages left"),
    };
    let released_first = Arc::new(AtomicBool::new(false));
    let joined = Arc::new(AtomicBool::new(false));
    let mut owner = owner.with_deferred_commands(Box::new(JoinProbe {
        root,
        root_was_released_before_join: Arc::clone(&released_first),
        joined: Arc::clone(&joined),
    }));

    owner.close();

    assert!(
        joined.load(Ordering::Acquire),
        "close reached the deferred join"
    );
    assert!(
        released_first.load(Ordering::Acquire),
        "the reset root was still pinned when the deferred join began"
    );
    assert_eq!(owner.leases.len(), 0);
    assert_eq!(owner.leases.released().count(ReleaseReason::OwnerClosed), 1);
}

#[test]
fn a_hydrating_lease_on_a_real_owner_is_reclaimed_with_its_root_by_the_idle_poll() {
    let directory = tempfile::tempdir().expect("workspace directory");
    let clock = ManualClock::new();
    let mut owner = real_owner(&directory, &clock);
    let mut source = ScriptedSource::new(clock.clone());
    source.replies.push_back(Ok(reset_reply(&["a", "b"])));
    LeaseHost::new(&mut owner.leases, &mut source)
        .handle(
            1,
            LocalSubscriptionRequest {
                request_id: 1,
                operation: LocalSubscriptionOperation::Open {
                    cursor: Box::new([]),
                    credit: 1,
                    lease_ms: PUBLICATION_LEASE.get(),
                },
            },
        )
        .expect("scripted open");
    let lease = owner.leases.ids()[0];
    let root = match owner.leases.get(lease).expect("lease").phase() {
        LeasePhase::Hydrating(hydration) => Arc::downgrade(hydration.root()),
        LeasePhase::Live => panic!("two rows at one row a page hydrate"),
    };
    clock.advance(SECOND * 10);
    let _ = owner.serve_one();
    assert!(
        root.upgrade().is_none(),
        "the idle poll freed the abandoned reset root"
    );
}
