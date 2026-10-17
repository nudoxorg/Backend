//! Real actor lanes keep an explicit cancellation independent of busy reads.
#![allow(clippy::expect_used, clippy::panic)]
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::Duration;

struct HeldRead {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl EngineClient for HeldRead {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        assert!(matches!(
            request,
            EngineRequest::IndexOperationStatus { .. }
        ));
        let _ = self.entered.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
        Err(EngineFault::Cancelled)
    }
}
struct Cancel {
    calls: Arc<AtomicUsize>,
}
impl EngineClient for Cancel {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::CancelIndexOperation {
            request,
            project,
            operation,
            basis,
            ..
        } = request
        else {
            panic!("the mutation lane received a non-cancellation request");
        };
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(EngineDto::IndexOperation {
            request: *request,
            basis: *basis,
            project: project.clone(),
            operation: operation.clone(),
            observation: crate::model::index_operation::tests::published(operation),
        })
    }
}
struct Main {
    observer: Mutex<Option<HeldRead>>,
    canceller: Mutex<Option<Cancel>>,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl EngineClient for Main {
    fn operation_observer(&self) -> Option<Box<dyn EngineClient>> {
        self.observer
            .lock()
            .expect("observer")
            .take()
            .map(|client| Box::new(client) as Box<dyn EngineClient>)
    }
    fn index_cancellation_client(&self) -> Option<Box<dyn EngineClient>> {
        self.canceller
            .lock()
            .expect("canceller")
            .take()
            .map(|client| Box::new(client) as Box<dyn EngineClient>)
    }
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        assert!(matches!(request, EngineRequest::Root { .. }));
        let _ = self.entered.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
        Err(EngineFault::Cancelled)
    }
}
fn cancellation(owner: Option<IndexMutationLease>, id: u64) -> EngineRequest {
    let project = LocalProjectId::new("/fixture/owned-cancel-lane").expect("project");
    EngineRequest::CancelIndexOperation {
        owner,
        operation: crate::model::index_operation::tests::claim(&project, 0x81),
        project,
        request: RequestId::new(id),
        basis: VersionedRoot::unserved(),
        cancel: CancellationToken::new(),
    }
}

#[test]
fn cancellation_has_a_one_shot_lane_while_main_and_status_reads_are_held() {
    let (main_entered, main_seen) = mpsc::channel();
    let (main_release, main_held) = mpsc::channel();
    let (read_entered, read_seen) = mpsc::channel();
    let (read_release, read_held) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let actor = EngineActor::start(
        Main {
            observer: Mutex::new(Some(HeldRead {
                entered: read_entered,
                release: read_held,
            })),
            canceller: Mutex::new(Some(Cancel {
                calls: calls.clone(),
            })),
            entered: main_entered,
            release: main_held,
        },
        4,
    )
    .expect("three independent actor clients");
    assert!(matches!(
        actor.try_submit(EngineRequest::Root {
            project: None,
            request: RequestId::new(1),
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new()
        }),
        PushResult::Enqueued
    ));
    main_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("main read entered");
    let project = LocalProjectId::new("/fixture/owned-cancel-lane").expect("project");
    assert!(matches!(
        actor.try_submit(EngineRequest::IndexOperationStatus {
            operation: crate::model::index_operation::tests::claim(&project, 0x81),
            project,
            request: RequestId::new(2),
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new()
        }),
        PushResult::Enqueued
    ));
    read_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("status read entered");
    let request = cancellation(IndexMutationLease::capture(None, None), 3);
    assert_eq!(
        request.coalesce_key(),
        None,
        "explicit mutation cannot coalesce with a read"
    );
    assert!(matches!(actor.try_submit(request), PushResult::Enqueued));
    let event =
        crate::runtime::wait::until_some("cancellation does not wait for held reads", || {
            actor
                .drain_events()
                .into_iter()
                .find(|event| event.request == RequestId::new(3))
        });
    // Release both real held clients before asserting their result or joining.
    drop((main_release, read_release));
    actor.shutdown();
    assert!(matches!(event.result, Ok(EngineDto::IndexOperation { .. })));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_without_a_current_mutation_lease_never_enters_the_client() {
    struct Factory(Arc<AtomicUsize>);
    impl EngineClient for Factory {
        fn index_cancellation_client(&self) -> Option<Box<dyn EngineClient>> {
            Some(Box::new(Cancel {
                calls: self.0.clone(),
            }))
        }
        fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
            panic!("cancellation must not run on the normal read client");
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let actor = EngineActor::start(Factory(calls.clone()), 4).expect("actor");
    let gate = super::super::owner::OwnerGate::ready(
        VersionedRoot::unserved(),
        crate::model::ServiceMode::Attached,
    );
    let lease = IndexMutationLease::capture(Some(&gate), gate.ready_epoch())
        .expect("actual gate generation");
    gate.publish(super::super::owner::OwnerState::Failed(
        "old owner stopped".into(),
    ));
    for (id, owner) in [(4, None), (5, Some(lease))] {
        assert!(matches!(
            actor.try_submit(cancellation(owner, id)),
            PushResult::Enqueued
        ));
        let event = crate::runtime::wait::until_some(
            "cancel refuses stale or absent mutation authority",
            || {
                actor
                    .drain_events()
                    .into_iter()
                    .find(|event| event.request == RequestId::new(id))
            },
        );
        assert!(matches!(
            event.result,
            Err(EngineFault::IndexUnconfirmed { .. })
        ));
    }
    actor.shutdown();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
