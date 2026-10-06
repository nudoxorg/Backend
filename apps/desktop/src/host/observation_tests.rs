//! Real local listener and typed observer regressions.

use super::*;
use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use backend_library::{
    AuthorityScopeClaim, Basis, CoverageCapability, Cursor, Frontier, ProducerObservationClaims,
    ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, ViewRoot,
    admit_complete_scope, admit_producer_observation, object_version, view_key, view_state_root,
};
use backend_local_service::{
    EngineRequest, EngineStatus, FrameLimits, ListenerConfig, ListenerError, ListenerShutdown,
    LocaldService, OwnerService, ProtocolError, ResponseFrame, RunReport, UnixListenerService,
    decode_response, encode_response,
};
use backend_replication::{
    LocalControlExchangeError, LocalControlExchangePhase, LocalControlExchangeProgress,
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResponse,
};
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;

struct RunningFixture {
    gate: OwnerGate,
    path: PathBuf,
    release: Arc<(Mutex<bool>, Condvar)>,
    shutdown: ListenerShutdown,
    observer: Option<thread::JoinHandle<bool>>,
    server: Option<thread::JoinHandle<Result<RunReport, ListenerError>>>,
}

impl RunningFixture {
    fn close(&mut self) {
        let (lock, wake) = &*self.release;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        wake.notify_all();
        self.gate.close();
        self.shutdown.request();
    }
}

impl Drop for RunningFixture {
    fn drop(&mut self) {
        self.close();
        if let Some(observer) = self.observer.take() {
            let _ = observer.join();
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

struct BlockedCase {
    gate: OwnerGate,
    old: Epoch,
    entered: mpsc::Receiver<u64>,
    resumes: Arc<AtomicUsize>,
    ticks: Arc<AtomicUsize>,
    running: RunningFixture,
}

struct FixtureVerifier;
impl ProducerObservationVerifier for FixtureVerifier {
    type Error = &'static str;
    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.evidence() != b"publication-fixture" {
            return Err("wrong fixture evidence");
        }
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

fn root() -> Arc<ViewRoot> {
    let basis = Basis::new(view_state_root(&[]), object_version(b"publication-source"));
    let scope = ScopeRoot::from_bytes(basis.object.to_bytes());
    let observation = admit_producer_observation(
        UntrustedProducerObservation::new([7; 32], scope, [9; 32], b"publication-fixture".to_vec()),
        &FixtureVerifier,
    )
    .expect("fixture producer proof");
    let coverage = CoverageCapability::from_authorized_with_evidence(
        admit_complete_scope(
            AuthorityScopeClaim::from_object_version(basis.object),
            observation,
        )
        .expect("complete scope"),
        b"publication-fixture".to_vec(),
    )
    .expect("complete coverage");
    Arc::new(
        ViewRoot::empty_checked(
            view_key(b"publication-view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            coverage,
        )
        .expect("complete root"),
    )
}

struct BlockingSubscriptionOwner {
    cursor: Box<[u8]>,
    lease: LocalSubscriptionId,
    entered: mpsc::Sender<u64>,
    release: Arc<(Mutex<bool>, Condvar)>,
    resumes: Arc<AtomicUsize>,
}

// The fixture retains the typed status for legacy direct callers, but both
// entry points must prepare the bounded outer reply before committing any
// counters or entering the artificial producer-stall gate.
fn prepare_fixture_status(
    request_id: u64,
    status: EngineStatus,
    limits: FrameLimits,
) -> Result<(EngineStatus, Vec<u8>), ProtocolError> {
    let bytes = encode_response(
        &ResponseFrame::Engine {
            request_id,
            status: status.clone(),
        },
        limits,
    )?;
    Ok((status, bytes))
}

impl BlockingSubscriptionOwner {
    fn prepare(
        &mut self,
        request_id: u64,
        request: EngineRequest,
        limits: FrameLimits,
    ) -> Result<(EngineStatus, Vec<u8>), ProtocolError> {
        let EngineRequest::Subscription(request) = request else {
            return prepare_fixture_status(
                request_id,
                EngineStatus::Rejected("wrong fixture operation".to_owned()),
                limits,
            );
        };
        let (response, resume) = match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => (
                LocalSubscriptionResponse::Opened {
                    request_id,
                    lease: self.lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms,
                },
                false,
            ),
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } if lease == self.lease && cursor == self.cursor => (
                LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms,
                },
                true,
            ),
            LocalSubscriptionOperation::Renew {
                lease,
                cursor,
                credit,
                ..
            } if lease == self.lease && cursor == self.cursor => (
                LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms: 10_000,
                },
                false,
            ),
            LocalSubscriptionOperation::Cancel { lease } if lease == self.lease => (
                LocalSubscriptionResponse::Cancelled { request_id, lease },
                false,
            ),
            _ => {
                return prepare_fixture_status(
                    request_id,
                    EngineStatus::Rejected("misbound fixture lease".to_owned()),
                    limits,
                );
            }
        };
        let prepared =
            prepare_fixture_status(request_id, EngineStatus::Subscription(response), limits)?;
        if resume {
            self.resumes.fetch_add(1, Ordering::AcqRel);
            self.entered.send(request_id).expect("test holds receiver");
            // Withhold the already encoded reply until the same real-socket
            // stall gate opens, preserving the observer's timing scenario.
            let (lock, wake) = &*self.release;
            let mut released = lock.lock().expect("release lock");
            while !*released {
                released = wake.wait(released).expect("release wait");
            }
        }
        Ok(prepared)
    }
}

impl OwnerService for BlockingSubscriptionOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        Ok(body.to_vec())
    }
    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        self.prepare(request_id, request, FrameLimits::default())
            .map(|(status, _)| status)
    }
    fn engine_prepared(
        &mut self,
        request_id: u64,
        request: EngineRequest,
        limits: FrameLimits,
    ) -> Result<Vec<u8>, ProtocolError> {
        self.prepare(request_id, request, limits)
            .map(|(_, bytes)| bytes)
    }
    fn serve_one(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
}

struct RejectFirstResume {
    cursor: Box<[u8]>,
    opens: Arc<AtomicUsize>,
    resumes: Arc<AtomicUsize>,
}

impl RejectFirstResume {
    fn prepare(
        &mut self,
        request_id: u64,
        request: EngineRequest,
        limits: FrameLimits,
    ) -> Result<(EngineStatus, Vec<u8>), ProtocolError> {
        let EngineRequest::Subscription(request) = request else {
            return prepare_fixture_status(
                request_id,
                EngineStatus::Rejected("wrong fixture operation".to_owned()),
                limits,
            );
        };
        let first = LocalSubscriptionId::from_bytes([3; 16]);
        let second = LocalSubscriptionId::from_bytes([4; 16]);
        let (status, opened, resumed) = match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => {
                // Only this fixture's single owner loop advances the counter.
                // A failed encoding must not consume the prospective lease.
                let lease = if self.opens.load(Ordering::Acquire) == 0 {
                    first
                } else {
                    second
                };
                (
                    EngineStatus::Subscription(LocalSubscriptionResponse::Opened {
                        request_id,
                        lease,
                        cursor: self.cursor.clone(),
                        credit,
                        lease_ms,
                    }),
                    true,
                    false,
                )
            }
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => {
                let status = if lease == first {
                    EngineStatus::Rejected("lease refused by producer".to_owned())
                } else if lease != second {
                    EngineStatus::Rejected("unknown fixture lease".to_owned())
                } else {
                    EngineStatus::Subscription(LocalSubscriptionResponse::Resumed {
                        request_id,
                        lease,
                        cursor: self.cursor.clone(),
                        credit,
                        lease_ms,
                    })
                };
                (status, false, true)
            }
            LocalSubscriptionOperation::Cancel { lease } => (
                EngineStatus::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id,
                    lease,
                }),
                false,
                false,
            ),
            _ => (
                EngineStatus::Rejected("misbound fixture operation".to_owned()),
                false,
                false,
            ),
        };
        let prepared = prepare_fixture_status(request_id, status, limits)?;
        if opened {
            self.opens.fetch_add(1, Ordering::AcqRel);
        }
        if resumed {
            self.resumes.fetch_add(1, Ordering::AcqRel);
        }
        Ok(prepared)
    }
}

impl OwnerService for RejectFirstResume {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        Ok(body.to_vec())
    }
    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        self.prepare(request_id, request, FrameLimits::default())
            .map(|(status, _)| status)
    }
    fn engine_prepared(
        &mut self,
        request_id: u64,
        request: EngineRequest,
        limits: FrameLimits,
    ) -> Result<Vec<u8>, ProtocolError> {
        self.prepare(request_id, request, limits)
            .map(|(_, bytes)| bytes)
    }
    fn serve_one(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
}

fn fixture_subscription(request_id: u64, operation: LocalSubscriptionOperation) -> EngineRequest {
    EngineRequest::Subscription(LocalSubscriptionRequest {
        request_id,
        operation,
    })
}

fn too_small_fixture_reply_limits() -> FrameLimits {
    let mut limits = FrameLimits::default();
    limits.max_frame = 1;
    limits.max_cursor = 1;
    limits.transport.max_frame = 1;
    limits.transport.max_chunk = 1;
    limits
        .validate()
        .expect("valid limits, but no subscription response can fit");
    limits
}

#[test]
fn blocked_resume_fixture_encodes_before_advancing_its_counter_or_gate() {
    let cursor = Cursor::for_view_root_at(&root(), 0).encode_control();
    let lease = LocalSubscriptionId::from_bytes([3; 16]);
    let (entered, observed) = mpsc::channel();
    let resumes = Arc::new(AtomicUsize::new(0));
    // Open only this test's artificial gate to make an incorrect
    // engine-then-encode implementation fail promptly instead of hanging.
    let release = Arc::new((Mutex::new(true), Condvar::new()));
    let mut owner = BlockingSubscriptionOwner {
        cursor: cursor.clone(),
        lease,
        entered,
        release,
        resumes: Arc::clone(&resumes),
    };
    let operation = LocalSubscriptionOperation::Resume {
        lease,
        cursor: cursor.clone(),
        credit: 1,
        lease_ms: 10_000,
    };
    assert!(
        owner
            .engine_prepared(
                1,
                fixture_subscription(1, operation.clone()),
                too_small_fixture_reply_limits()
            )
            .is_err()
    );
    assert_eq!(resumes.load(Ordering::Acquire), 0);
    assert!(
        matches!(observed.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a rejected outer reply cannot enter the stall gate"
    );
    let bytes = owner
        .engine_prepared(
            2,
            fixture_subscription(2, operation),
            FrameLimits::default(),
        )
        .expect("prepared Resume");
    assert_eq!(resumes.load(Ordering::Acquire), 1);
    assert_eq!(
        observed
            .recv_timeout(Duration::from_secs(1))
            .expect("only successful preparation enters"),
        2
    );
    assert_eq!(
        decode_response(&bytes, FrameLimits::default()).expect("bounded reply"),
        ResponseFrame::Engine {
            request_id: 2,
            status: EngineStatus::Subscription(LocalSubscriptionResponse::Resumed {
                request_id: 2,
                lease,
                cursor,
                credit: 1,
                lease_ms: 10_000
            }),
        }
    );
}

#[test]
fn resume_refusal_fixture_consumes_counters_only_after_exact_response_encoding() {
    let cursor = Cursor::for_view_root_at(&root(), 0).encode_control();
    let opens = Arc::new(AtomicUsize::new(0));
    let resumes = Arc::new(AtomicUsize::new(0));
    let mut owner = RejectFirstResume {
        cursor: cursor.clone(),
        opens: Arc::clone(&opens),
        resumes: Arc::clone(&resumes),
    };
    let open = LocalSubscriptionOperation::Open {
        cursor: cursor.clone(),
        credit: 1,
        lease_ms: 10_000,
    };
    assert!(
        owner
            .engine_prepared(
                1,
                fixture_subscription(1, open.clone()),
                too_small_fixture_reply_limits()
            )
            .is_err()
    );
    assert_eq!(
        opens.load(Ordering::Acquire),
        0,
        "failed outer encoding cannot consume the first lease"
    );
    for (request_id, lease) in [
        (2, LocalSubscriptionId::from_bytes([3; 16])),
        (3, LocalSubscriptionId::from_bytes([4; 16])),
    ] {
        let bytes = owner
            .engine_prepared(
                request_id,
                fixture_subscription(request_id, open.clone()),
                FrameLimits::default(),
            )
            .expect("prepared Open");
        assert_eq!(
            decode_response(&bytes, FrameLimits::default()).expect("bounded Open"),
            ResponseFrame::Engine {
                request_id,
                status: EngineStatus::Subscription(LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: cursor.clone(),
                    credit: 1,
                    lease_ms: 10_000
                }),
            }
        );
    }
    assert_eq!(opens.load(Ordering::Acquire), 2);
    let rejected_resume = LocalSubscriptionOperation::Resume {
        lease: LocalSubscriptionId::from_bytes([3; 16]),
        cursor: cursor.clone(),
        credit: 1,
        lease_ms: 10_000,
    };
    assert!(
        owner
            .engine_prepared(
                4,
                fixture_subscription(4, rejected_resume.clone()),
                too_small_fixture_reply_limits()
            )
            .is_err()
    );
    assert_eq!(
        resumes.load(Ordering::Acquire),
        0,
        "even the explicit refusal counter requires a prepared outer reply"
    );
    let refusal = owner
        .engine_prepared(
            5,
            fixture_subscription(5, rejected_resume),
            FrameLimits::default(),
        )
        .expect("prepared explicit refusal");
    assert_eq!(
        decode_response(&refusal, FrameLimits::default()).expect("bounded refusal"),
        ResponseFrame::Engine {
            request_id: 5,
            status: EngineStatus::Rejected("lease refused by producer".to_owned())
        }
    );
    assert_eq!(resumes.load(Ordering::Acquire), 1);
    let second = LocalSubscriptionId::from_bytes([4; 16]);
    let bytes = owner
        .engine_prepared(
            6,
            fixture_subscription(
                6,
                LocalSubscriptionOperation::Resume {
                    lease: second,
                    cursor: cursor.clone(),
                    credit: 1,
                    lease_ms: 10_000,
                },
            ),
            FrameLimits::default(),
        )
        .expect("prepared reacquired Resume");
    assert_eq!(
        decode_response(&bytes, FrameLimits::default()).expect("bounded second Resume"),
        ResponseFrame::Engine {
            request_id: 6,
            status: EngineStatus::Subscription(LocalSubscriptionResponse::Resumed {
                request_id: 6,
                lease: second,
                cursor,
                credit: 1,
                lease_ms: 10_000
            }),
        }
    );
    assert_eq!(resumes.load(Ordering::Acquire), 2);
}

fn socket_path() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    PathBuf::from("/tmp").join(format!("nudox-observe-{}-{nonce}.sock", std::process::id()))
}

fn start_blocked_case() -> BlockedCase {
    start_blocked_case_with_timing(ObservationTiming {
        poll: Duration::from_millis(60),
        io_tick: Duration::from_millis(20),
        dial: Duration::from_millis(300),
        freshness: Duration::from_millis(100),
        ordinary_recovery: Duration::from_secs(3),
        first_root: Duration::from_secs(2),
        renew: Duration::from_secs(10),
        max_reconnects: 2,
    })
}

fn start_blocked_case_with_timing(timing: ObservationTiming) -> BlockedCase {
    let root = root();
    let cursor = Cursor::for_view_root_at(&root, 0);
    let gate = OwnerGate::ready(
        VersionedRoot::from_revision(1, cursor, 0),
        ServiceMode::Attached,
    );
    let old = gate.ready_epoch().expect("serving attachment");
    assert_eq!(
        gate.publish_view(old, root, cursor),
        PublicationAdmission::Admitted
    );
    let path = socket_path();
    let (entered_sender, entered) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let resumes = Arc::new(AtomicUsize::new(0));
    let owner = BlockingSubscriptionOwner {
        cursor: cursor.encode_control(),
        lease: LocalSubscriptionId::from_bytes([3; 16]),
        entered: entered_sender,
        release: Arc::clone(&release),
        resumes: Arc::clone(&resumes),
    };
    let mut config = ListenerConfig::new(&path);
    config.idle_timeout = None;
    config.poll_interval = Duration::from_millis(1);
    let service = LocaldService::new(owner, FrameLimits::default()).expect("fixture service");
    let mut listener = UnixListenerService::bind(service, config).expect("private listener");
    let shutdown = listener.shutdown_handle();
    let server = thread::spawn(move || listener.run());
    let observer_gate = gate.clone();
    let endpoint = path.clone();
    let ticks = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&ticks);
    let observer = thread::spawn(move || {
        serve_with_timing(&observer_gate, &endpoint, timing, move |progress| {
            if progress.exchange.request_id == 2 {
                counting.fetch_add(1, Ordering::AcqRel);
            }
        })
    });
    let running = RunningFixture {
        gate: gate.clone(),
        path,
        release,
        shutdown,
        observer: Some(observer),
        server: Some(server),
    };
    BlockedCase {
        gate,
        old,
        entered,
        resumes,
        ticks,
        running,
    }
}

fn await_suspension(case: &BlockedCase) -> u64 {
    let held_request = case
        .entered
        .recv_timeout(Duration::from_secs(3))
        .expect("Resume reached owner");
    let deadline = Instant::now() + Duration::from_secs(2);
    while case.gate.ready_epoch().is_some() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        case.gate.ready_epoch(),
        None,
        "the one blocked reply fenced stale reads"
    );
    assert_eq!(
        case.resumes.load(Ordering::Acquire),
        1,
        "socket ticks did not duplicate Resume"
    );
    assert!(
        case.ticks.load(Ordering::Acquire) >= 3,
        "one Resume crossed multiple socket ticks"
    );
    held_request
}

#[test]
fn typed_exchange_failure_distinguishes_closed_peer_from_a_stall() {
    let progress = LocalControlExchangeProgress {
        request_id: 7,
        phase: LocalControlExchangePhase::ReadingHeader,
        write_offset: 11,
        header_offset: 2,
        body_len: None,
        body_offset: 0,
        deadline: Instant::now() + Duration::from_secs(1),
        elapsed: Duration::from_millis(150),
    };
    let closed = PublicationExchangeError::Exchange(LocalControlExchangeError {
        failure: LocalControlExchangeFailure::Closed,
        progress,
    });
    assert!(recoverable(&closed));
    assert_eq!(
        classify(closed),
        ObservationFailure::PeerClosed {
            phase: progress.phase
        }
    );
    let stalled = PublicationExchangeError::Exchange(LocalControlExchangeError {
        failure: LocalControlExchangeFailure::Stalled,
        progress,
    });
    assert!(
        !recoverable(&stalled),
        "a fixed deadline never replays the request"
    );
    assert_eq!(
        classify(stalled),
        ObservationFailure::ResponseStalled {
            phase: progress.phase,
            elapsed: progress.elapsed,
        }
    );
    let refused = PublicationExchangeError::ProducerRejected {
        operation: PublicationOperation::Resume,
        request_id: 7,
        message: "unknown lease".to_owned(),
    };
    assert!(
        !recoverable(&refused),
        "only the explicit Resume branch may fresh-Open"
    );
}

#[test]
fn blocked_owner_resume_survives_multiple_ticks_and_reopens_exact_attachment() {
    let mut case = start_blocked_case();
    let held_request = await_suspension(&case);
    let (lock, wake) = &*case.running.release;
    *lock.lock().expect("release lock") = true;
    wake.notify_all();
    let deadline = Instant::now() + Duration::from_secs(2);
    while case.gate.ready_epoch().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    let reopened = case
        .gate
        .ready_epoch()
        .expect("certified same-root Resume reopened reads");
    assert_ne!(reopened, case.old);
    assert_eq!(
        held_request, 2,
        "Open and blocked Resume retain distinct exact request IDs"
    );
    case.running.close();
    case.running
        .observer
        .take()
        .expect("observer handle")
        .join()
        .expect("observer joined");
    let report = case
        .running
        .server
        .take()
        .expect("listener handle")
        .join()
        .expect("listener joined")
        .expect("listener run");
    assert_eq!(
        report.connections, 1,
        "socket ticks stayed on the original worker connection"
    );
}

#[test]
fn close_interrupts_the_blocked_exchange_before_the_owner_releases_it() {
    let mut case = start_blocked_case();
    let held_request = await_suspension(&case);
    case.gate.close();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !case
        .running
        .observer
        .as_ref()
        .is_some_and(thread::JoinHandle::is_finished)
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        case.running
            .observer
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished),
        "the exact socket interrupt joined the observer while owner work was still blocked"
    );
    assert_eq!(case.resumes.load(Ordering::Acquire), 1);
    assert_eq!(held_request, 2);
    assert_eq!(case.gate.ready_epoch(), None);
    // The fixture's Drop releases owner work and joins the listener.
}

#[test]
fn only_explicit_resume_refusal_reacquires_one_new_lease_and_certifies_same_root() {
    let root = root();
    let cursor = Cursor::for_view_root_at(&root, 0);
    let gate = OwnerGate::ready(
        VersionedRoot::from_revision(1, cursor, 0),
        ServiceMode::Attached,
    );
    let old = gate.ready_epoch().expect("initial attachment");
    assert_eq!(
        gate.publish_view(old, Arc::clone(&root), cursor),
        PublicationAdmission::Admitted
    );
    let path = socket_path();
    let opens = Arc::new(AtomicUsize::new(0));
    let resumes = Arc::new(AtomicUsize::new(0));
    let owner = RejectFirstResume {
        cursor: cursor.encode_control(),
        opens: Arc::clone(&opens),
        resumes: Arc::clone(&resumes),
    };
    let mut config = ListenerConfig::new(&path);
    config.idle_timeout = None;
    config.poll_interval = Duration::from_millis(1);
    let service = LocaldService::new(owner, FrameLimits::default()).expect("fixture service");
    let mut listener = UnixListenerService::bind(service, config).expect("private listener");
    let shutdown = listener.shutdown_handle();
    let server = thread::spawn(move || listener.run());
    let observer_gate = gate.clone();
    let endpoint = path.clone();
    let timing = ObservationTiming {
        poll: Duration::from_millis(60),
        io_tick: Duration::from_millis(20),
        dial: Duration::from_millis(300),
        freshness: Duration::from_millis(100),
        ordinary_recovery: Duration::from_secs(3),
        first_root: Duration::from_secs(2),
        renew: Duration::from_secs(10),
        max_reconnects: 2,
    };
    let observer =
        thread::spawn(move || serve_with_timing(&observer_gate, &endpoint, timing, |_| {}));
    let release = Arc::new((Mutex::new(true), Condvar::new()));
    let mut running = RunningFixture {
        gate: gate.clone(),
        path,
        release,
        shutdown,
        observer: Some(observer),
        server: Some(server),
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    let new = loop {
        if let Some(epoch) = gate.ready_epoch()
            && epoch != old
        {
            break epoch;
        }
        assert!(
            Instant::now() < deadline,
            "fresh Open did not certify a new attachment"
        );
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(
        opens.load(Ordering::Acquire),
        2,
        "one original and one fresh Open"
    );
    assert!(
        resumes.load(Ordering::Acquire) >= 1,
        "the producer rejected Resume first"
    );
    let (retained, retained_cursor) = gate.publication(new).expect("reacquired root");
    assert!(Arc::ptr_eq(&retained, &root));
    assert_eq!(retained_cursor, cursor);
    running.close();
}

#[test]
fn provisional_timeout_recovers_on_a_new_socket_inside_the_original_window() {
    let mut case = start_blocked_case_with_timing(ObservationTiming {
        ordinary_recovery: Duration::from_secs(30),
        freshness: Duration::from_millis(100),
        io_tick: Duration::from_millis(50),
        poll: Duration::from_millis(20),
        renew: Duration::from_secs(60),
        ..ObservationTiming::CANDIDATE
    });
    assert_eq!(await_suspension(&case), 2);
    // The shared reset contract fixes this first response bound at ten
    // seconds. Waiting on a quiet channel makes the held duration
    // explicit and bounded; owner work remains blocked throughout it.
    let (_sender, receiver) = mpsc::channel::<()>();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(11)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(case.gate.ready_epoch(), None, "no proof has reopened reads");
    assert!(
        matches!(
            case.gate.state(),
            crate::runtime::owner::OwnerState::Ready { .. }
        ),
        "one provisional timeout must not turn a healthy owner into terminal failure"
    );
    let (lock, wake) = &*case.running.release;
    *lock.lock().expect("release owner") = true;
    wake.notify_all();
    let deadline = Instant::now() + Duration::from_secs(4);
    while case.gate.ready_epoch().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        case.gate.ready_epoch().is_some(),
        "exact-cursor Resume certified recovery"
    );
    assert!(
        case.resumes.load(Ordering::Acquire) >= 2,
        "recovery must certify a Resume on the replacement socket"
    );
    case.running.close();
    case.running
        .observer
        .take()
        .expect("observer")
        .join()
        .expect("observer joined");
    let report = case
        .running
        .server
        .take()
        .expect("server")
        .join()
        .expect("server joined")
        .expect("listener run");
    assert_eq!(report.connections, 2);
}
