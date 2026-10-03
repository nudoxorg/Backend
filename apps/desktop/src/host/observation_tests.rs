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
    LocaldService, OwnerService, ProtocolError, RunReport, UnixListenerService,
};
use backend_replication::{
    LocalControlExchangeError, LocalControlExchangePhase, LocalControlExchangeProgress,
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionResponse,
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

impl OwnerService for BlockingSubscriptionOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        Ok(body.to_vec())
    }
    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let EngineRequest::Subscription(request) = request else {
            return Ok(EngineStatus::Rejected("wrong fixture operation".to_owned()));
        };
        let response = match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => LocalSubscriptionResponse::Opened {
                request_id,
                lease: self.lease,
                cursor: self.cursor.clone(),
                credit,
                lease_ms,
            },
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } if lease == self.lease && cursor == self.cursor => {
                self.resumes.fetch_add(1, Ordering::AcqRel);
                self.entered.send(request_id).expect("test holds receiver");
                let (lock, wake) = &*self.release;
                let mut released = lock.lock().expect("release lock");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
                LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms,
                }
            }
            LocalSubscriptionOperation::Renew {
                lease,
                cursor,
                credit,
                ..
            } if lease == self.lease && cursor == self.cursor => {
                LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms: 10_000,
                }
            }
            LocalSubscriptionOperation::Cancel { lease } if lease == self.lease => {
                LocalSubscriptionResponse::Cancelled { request_id, lease }
            }
            _ => return Ok(EngineStatus::Rejected("misbound fixture lease".to_owned())),
        };
        Ok(EngineStatus::Subscription(response))
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

impl OwnerService for RejectFirstResume {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        Ok(body.to_vec())
    }

    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let EngineRequest::Subscription(request) = request else {
            return Ok(EngineStatus::Rejected("wrong fixture operation".to_owned()));
        };
        let first = LocalSubscriptionId::from_bytes([3; 16]);
        let second = LocalSubscriptionId::from_bytes([4; 16]);
        let response = match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => {
                let number = self.opens.fetch_add(1, Ordering::AcqRel) + 1;
                LocalSubscriptionResponse::Opened {
                    request_id,
                    lease: if number == 1 { first } else { second },
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms,
                }
            }
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } if cursor == self.cursor => {
                self.resumes.fetch_add(1, Ordering::AcqRel);
                if lease == first {
                    return Ok(EngineStatus::Rejected(
                        "lease refused by producer".to_owned(),
                    ));
                }
                if lease != second {
                    return Ok(EngineStatus::Rejected("unknown fixture lease".to_owned()));
                }
                LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: self.cursor.clone(),
                    credit,
                    lease_ms,
                }
            }
            LocalSubscriptionOperation::Cancel { lease } => {
                LocalSubscriptionResponse::Cancelled { request_id, lease }
            }
            _ => {
                return Ok(EngineStatus::Rejected(
                    "misbound fixture operation".to_owned(),
                ));
            }
        };
        Ok(EngineStatus::Subscription(response))
    }

    fn serve_one(&mut self) -> bool {
        false
    }

    fn close(&mut self) {}
}

fn socket_path() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    PathBuf::from("/tmp").join(format!("nudox-observe-{}-{nonce}.sock", std::process::id()))
}

fn start_blocked_case() -> BlockedCase {
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
