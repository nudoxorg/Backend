//! Real bounded actor schedules; the independent observer executes no Start.
#![allow(clippy::expect_used, clippy::panic)]
use super::*;
use std::sync::mpsc;
use std::time::Duration;

struct Main {
    observer: Mutex<Option<Observer>>,
    roots: mpsc::Sender<()>,
    mutation: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
struct Observer {
    entered: mpsc::Sender<crate::navigation::RequestId>,
    held: Option<mpsc::Receiver<()>>,
    cancel_release: mpsc::Sender<()>,
    observed: mpsc::Sender<crate::navigation::RequestId>,
}
impl EngineClient for Main {
    fn operation_observer(&self) -> Option<Box<dyn EngineClient>> {
        self.observer
            .lock()
            .expect("observer factory")
            .take()
            .map(|observer| Box::new(observer) as Box<dyn EngineClient>)
    }
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        assert!(!matches!(
            request,
            EngineRequest::IndexOperationStatus { .. }
        ));
        if let (EngineRequest::IndexProject { project, .. }, Some((entered, release))) =
            (request, &self.mutation)
        {
            let _ = entered.send(());
            release
                .recv_timeout(Duration::from_secs(10))
                .expect("finite entered mutation release");
            return Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            });
        }
        let _ = self.roots.send(());
        Err(EngineFault::Cancelled)
    }
}
impl EngineClient for Observer {
    fn execute(&mut self, input: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::IndexOperationStatus {
            request,
            project,
            operation,
            basis,
            ..
        } = input
        else {
            panic!("read-only observer received a mutation");
        };
        if let Some(held) = self.held.take() {
            let release = self.cancel_release.clone();
            let _wake = input.cancellation().on_cancel(move || {
                let _ = release.send(());
            });
            let _ = self.entered.send(*request);
            held.recv_timeout(Duration::from_secs(10))
                .expect("finite controlled read release");
        }
        let _ = self.observed.send(*request);
        if input.cancelled() {
            return Err(EngineFault::Cancelled);
        }
        Ok(EngineDto::IndexOperation {
            request: *request,
            basis: *basis,
            project: project.clone(),
            operation: operation.clone(),
            observation: crate::model::index_operation::tests::published(operation),
        })
    }
}
fn actor(
    capacity: usize,
) -> (
    EngineActor,
    mpsc::Receiver<crate::navigation::RequestId>,
    mpsc::Sender<()>,
    mpsc::Receiver<crate::navigation::RequestId>,
    mpsc::Receiver<()>,
) {
    let (entered, seen) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (observed, completed) = mpsc::channel();
    let (roots, root_seen) = mpsc::channel();
    let client = Main {
        observer: Mutex::new(Some(Observer {
            entered,
            held: Some(held),
            cancel_release: release.clone(),
            observed,
        })),
        roots,
        mutation: None,
    };
    (
        EngineActor::start(client, capacity).expect("independent lanes"),
        seen,
        release,
        completed,
        root_seen,
    )
}
fn status(project: &str, id: u64) -> EngineRequest {
    let project = LocalProjectId::new(project).expect("project");
    EngineRequest::IndexOperationStatus {
        owner: None,
        request: crate::navigation::RequestId::new(id),
        operation: crate::model::index_operation::tests::claim(
            &project,
            project.as_str().as_bytes().last().copied().unwrap_or(1),
        ),
        project,
        basis: VersionedRoot::synthetic(backend_library::view_state_root(&[]), 1),
        cancel: CancellationToken::new(),
    }
}

#[test]
fn status_coalescing_preserves_bounded_capacity_and_other_project_fifo_fairness() {
    let (actor, entered, release, completed, roots) = actor(2);
    assert!(matches!(
        actor.try_submit(status("/fixture/a", 1)),
        PushResult::Enqueued
    ));
    assert_eq!(
        entered
            .recv_timeout(Duration::from_secs(1))
            .expect("held active status"),
        crate::navigation::RequestId::new(1)
    );
    let queued = status("/fixture/a", 2);
    let obsolete = queued.cancellation().clone();
    assert!(matches!(actor.try_submit(queued), PushResult::Enqueued));
    assert!(matches!(
        actor.try_submit(status("/fixture/b", 3)),
        PushResult::Enqueued
    ));
    assert!(matches!(
        actor.try_submit(status("/fixture/a", 4)),
        PushResult::Coalesced(_)
    ));
    assert!(obsolete.is_cancelled());
    assert!(matches!(
        actor.try_submit(status("/fixture/c", 5)),
        PushResult::Full(_)
    ));
    assert_eq!(
        actor
            .operation
            .as_ref()
            .expect("real observer lane")
            .mailbox
            .len(),
        2
    );
    assert!(matches!(
        actor.try_submit(EngineRequest::Root {
            request: crate::navigation::RequestId::new(6),
            basis: VersionedRoot::synthetic(backend_library::view_state_root(&[]), 1),
            cancel: CancellationToken::new()
        }),
        PushResult::Enqueued
    ));
    roots
        .recv_timeout(Duration::from_secs(1))
        .expect("root lane remains independent of a held status");
    release.send(()).expect("release status");
    let observed = (0..3)
        .map(|_| {
            completed
                .recv_timeout(Duration::from_secs(1))
                .expect("fair bounded status delivery")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        [
            crate::navigation::RequestId::new(1),
            crate::navigation::RequestId::new(4),
            crate::navigation::RequestId::new(3)
        ]
    );
    actor.stop();
}

#[gpui::test]
fn stop_revokes_observer_and_collects_its_join(cx: &mut gpui::TestAppContext) {
    let (mut actor, entered, _release, completed, _roots) = actor(2);
    let request = status("/fixture/close", 7);
    let cancel = request.cancellation().clone();
    assert!(matches!(actor.try_submit(request), PushResult::Enqueued));
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("observer entered");
    let finish = actor.take_finish();
    assert!(cancel.is_cancelled());
    assert!(actor.join.is_none() && actor.local_join.is_none());
    assert!(
        actor
            .operation
            .as_ref()
            .expect("observer lane")
            .join
            .is_none(),
        "graceful close transfers every worker"
    );
    assert!(matches!(
        actor.try_submit(status("/fixture/close", 8)),
        PushResult::Closed(_)
    ));
    completed
        .recv_timeout(Duration::from_secs(1))
        .expect("cancel wakes held observer");
    let executor = cx.executor();
    let waiting = executor.clone();
    let (done, stopped) = async_channel::bounded(1);
    let task = executor.spawn(async move {
        finish.wait(waiting).await;
        let _ = done.try_send(());
    });
    crate::runtime::wait::until("all three owned workers stop", || {
        cx.executor().advance_clock(Duration::from_millis(25));
        cx.run_until_parked();
        stopped.try_recv().is_ok()
    });
    drop(task);
    let (restarted, entered, release, completed, _) = self::actor(2);
    assert!(matches!(
        restarted.try_submit(status("/fixture/restarted", 9)),
        PushResult::Enqueued
    ));
    assert_eq!(
        entered
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh observer enters"),
        crate::navigation::RequestId::new(9)
    );
    release.send(()).expect("release fresh observer");
    assert_eq!(
        completed
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh observer replies"),
        crate::navigation::RequestId::new(9)
    );
    restarted.stop();
}

#[test]
fn cancelling_an_active_status_does_not_cancel_an_entered_start_on_the_main_lane() {
    let (status_entered, status_seen) = mpsc::channel();
    let (status_release, status_held) = mpsc::channel();
    let (observed, status_done) = mpsc::channel();
    let (roots, _roots) = mpsc::channel();
    let (started, start_seen) = mpsc::channel();
    let (finish_start, held_start) = mpsc::channel();
    let actor = EngineActor::start(
        Main {
            observer: Mutex::new(Some(Observer {
                entered: status_entered,
                held: Some(status_held),
                cancel_release: status_release,
                observed,
            })),
            roots,
            mutation: Some((started, held_start)),
        },
        2,
    )
    .expect("independent lanes");
    let project = LocalProjectId::new("/fixture/entered-start").expect("project");
    let mutation = CancellationToken::new();
    assert!(matches!(
        actor.try_submit(EngineRequest::IndexProject {
            owner: IndexMutationLease::capture(None, None),
            operation: crate::model::index_operation::tests::claim(&project, 0x41),
            project,
            request: crate::navigation::RequestId::new(20),
            basis: VersionedRoot::synthetic(backend_library::view_state_root(&[]), 1),
            cancel: mutation.clone(),
        }),
        PushResult::Enqueued
    ));
    start_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("actual main worker entered Start adapter");
    let observation = status("/fixture/entered-start", 21);
    let cancel_status = observation.cancellation().clone();
    assert!(matches!(
        actor.try_submit(observation),
        PushResult::Enqueued
    ));
    status_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("independent observer entered");
    cancel_status.cancel();
    status_done
        .recv_timeout(Duration::from_secs(1))
        .expect("status cancellation releases only observer");
    assert!(
        !mutation.is_cancelled(),
        "read cancellation cannot revoke an entered mutation"
    );
    finish_start
        .send(())
        .expect("finish entered mutation explicitly");
    actor.stop();
}
