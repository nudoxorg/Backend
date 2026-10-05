//! Controlled real worker schedules for the independent operation observer.
//! Receipts are typed fixtures; these tests do not claim live compiler proof.
#![allow(clippy::expect_used, clippy::panic)]
use super::*;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{ProjectPhase, ServiceMode};
use crate::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::owner::{OwnerGate, OwnerState};
use gpui::TestAppContext;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Hold {
    open: Mutex<bool>,
    changed: Condvar,
}
impl Hold {
    fn release(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let start = Instant::now();
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            let left = Duration::from_secs(10).saturating_sub(start.elapsed());
            if left.is_zero() {
                panic!("controlled worker was not released within its finite deadline");
            }
            open = self
                .changed
                .wait_timeout(open, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}
struct Release(Arc<Hold>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct HeldRoot {
    hold: Arc<Hold>,
    entered: mpsc::Sender<()>,
    observations: Arc<AtomicUsize>,
    root_projects: Arc<Mutex<Vec<Option<LocalProjectId>>>>,
    terminal: Arc<AtomicBool>,
}
struct Status {
    observations: Arc<AtomicUsize>,
    terminal: Arc<AtomicBool>,
}
impl EngineClient for HeldRoot {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        assert!(
            !matches!(request, EngineRequest::IndexOperationStatus { .. }),
            "production-shaped observer owns all status reads"
        );
        if let EngineRequest::Root { project, .. } = request {
            self.root_projects
                .lock()
                .expect("root request contexts")
                .push(project.clone());
            let _ = self.entered.send(());
            self.hold.wait();
        }
        Err(EngineFault::Cancelled)
    }
    fn operation_observer(&self) -> Option<Box<dyn EngineClient>> {
        Some(Box::new(Status {
            observations: self.observations.clone(),
            terminal: self.terminal.clone(),
        }))
    }
}
impl EngineClient for Status {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::IndexOperationStatus {
            request,
            basis,
            project,
            operation,
            ..
        } = request
        else {
            panic!("observer cannot execute a mutation or hydration");
        };
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(EngineDto::IndexOperation {
            request: *request,
            basis: *basis,
            project: project.clone(),
            operation: operation.clone(),
            observation: if self.terminal.load(Ordering::SeqCst) {
                crate::model::index_operation::tests::published(operation)
            } else {
                crate::model::index_operation::tests::observation(
                    operation,
                    backend_library::IndexOperationState::Accepted,
                )
            },
        })
    }
}
fn basis() -> VersionedRoot {
    VersionedRoot::synthetic(
        backend_library::view_state_root(&[("operation".into(), "observer".into())]),
        7,
    )
}
fn active_snapshot(project: &LocalProjectId) -> AppSnapshot {
    let snapshot = crate::navigation::reduce(
        &AppSnapshot::empty(basis()),
        Intent::AddProject {
            project: project.clone(),
        },
    )
    .snapshot;
    let mut workspace = snapshot.workspace().clone();
    let mut rows = workspace.projects.to_vec();
    let mut claim = crate::model::index_operation::tests::claim(project, 0x31);
    claim.observation = Some(crate::model::index_operation::tests::observation(
        &claim,
        backend_library::IndexOperationState::Accepted,
    ));
    rows[0].operation = Some(claim);
    rows[0].request = None;
    workspace.projects = rows.into();
    snapshot.with_workspace(workspace)
}
fn client() -> (HeldRoot, mpsc::Receiver<()>, Arc<AtomicUsize>, Release) {
    let hold = Arc::new(Hold::default());
    let (entered, seen) = mpsc::channel();
    let observations = Arc::new(AtomicUsize::new(0));
    (
        HeldRoot {
            hold: hold.clone(),
            entered,
            observations: observations.clone(),
            root_projects: Default::default(),
            terminal: Arc::new(AtomicBool::new(true)),
        },
        seen,
        observations,
        Release(hold),
    )
}

#[gpui::test]
fn terminal_receipt_updates_live_root_and_store_before_held_hydration_returns(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-independent").expect("project");
    let (client, entered, observations, _release) = client();
    let contexts = client.root_projects.clone();
    let actor = EngineActor::start(client, 4).expect("all worker lanes");
    let graph = cx.update(|cx| {
        UiEntityGraph::install(
            cx,
            DesktopRuntime::new(active_snapshot(&project), actor),
            None,
        )
    });
    let _release = _release; // release worker before the graph can drop
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("real root hydration is held");
    graph
        .root
        .update(cx, |root, cx| root.schedule_operation_observation(cx));
    cx.executor().advance_clock(Duration::from_millis(500));
    crate::runtime::wait::until("receipt bypasses held hydration", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
    assert_eq!(
        graph
            .store
            .read_with(cx, |store, _| store.snapshot().workspace().projects[0]
                .phase),
        ProjectPhase::Ready
    );
    assert!(graph.root.read_with(cx, |root, _| {
        root.snapshot().workspace().projects[0]
            .operation
            .as_ref()
            .expect("exact claim")
            .has_terminal_observation()
    }));
    assert_eq!(contexts.lock().expect("captured root contexts")[0], None);
    _release.0.release();
    crate::runtime::wait::until("next root captures the admitted project", || {
        cx.run_until_parked();
        contexts
            .lock()
            .expect("contexts")
            .iter()
            .any(|context| context.as_ref() == Some(&project))
    });
    assert_eq!(
        contexts.lock().expect("contexts")[0],
        None,
        "the entered root retains its original context after another receipt lands"
    );
}

#[gpui::test]
fn owner_recovery_rearms_observation_after_store_renewal_without_a_hydration_reply(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-owner-recovery").expect("project");
    let gate = OwnerGate::starting();
    let (client, entered, observations, _release) = client();
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(
                active_snapshot(&project),
                EngineActor::start(client, 4).expect("lanes"),
            ),
            None,
            None,
            Some(gate.clone()),
            None,
        )
    });
    let _release = _release; // release worker before the graph can drop
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("startup hydration remains held");
    graph.root.update(cx, |root, _| {
        root.index_poll = None;
    });
    gate.publish(OwnerState::Ready {
        key: basis(),
        mode: ServiceMode::Embedded,
    });
    cx.run_until_parked();
    assert!(
        graph
            .root
            .read_with(cx, |root, _| root.index_poll.is_some()),
        "deferred scheduling runs after the store is serving"
    );
    cx.executor().advance_clock(Duration::from_millis(500));
    crate::runtime::wait::until("recovery observes exact receipt", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn stale_status_callback_rearms_a_new_read_for_the_same_root_replacement(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-stale-status").expect("project");
    let gate = OwnerGate::starting();
    gate.publish(OwnerState::Ready {
        key: basis(),
        mode: ServiceMode::Embedded,
    });
    let (client, entered, observations, _release) = client();
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(
                active_snapshot(&project),
                EngineActor::start(client, 4).expect("lanes"),
            ),
            None,
            None,
            Some(gate.clone()),
            None,
        )
    });
    let _release = _release; // release worker before the graph can drop
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("hydration held");
    // An entity update flushes deferred effects before returning. Keep the
    // replacement inside one outer App update so this is genuinely an old
    // queued callback, rather than an already entered status request.
    cx.update(|cx| {
        graph.root.update(cx, |root, cx| {
            root.index_poll = None;
            root.schedule_index_check(project.clone(), cx);
            assert_eq!(root.pending.len(), 1, "old status remains queued");
        });
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Ready {
            key: basis(),
            mode: ServiceMode::Embedded,
        });
        graph.store.update(cx, |store, cx| store.owner_ready(cx));
        graph.root.update(cx, |root, cx| root.flush_pending(cx));
    });
    assert_eq!(
        observations.load(Ordering::SeqCst),
        0,
        "old callback never sends under a replacement attachment"
    );
    assert!(
        graph
            .root
            .read_with(cx, |root, _| root.index_poll.is_some()),
        "dropping stale status preserves a future observation"
    );
    cx.executor().advance_clock(Duration::from_millis(500));
    crate::runtime::wait::until("new attachment receives one fresh status", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn a_nonterminal_status_already_queued_before_same_root_replacement_cannot_land(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-old-owner").expect("project");
    let gate = OwnerGate::starting();
    gate.publish(OwnerState::Ready {
        key: basis(),
        mode: ServiceMode::Embedded,
    });
    let (client, entered, observations, _release) = client();
    client.terminal.store(false, Ordering::SeqCst);
    let terminal = client.terminal.clone();
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(
                active_snapshot(&project),
                EngineActor::start(client, 4).expect("lanes"),
            ),
            None,
            None,
            Some(gate.clone()),
            None,
        )
    });
    let _release = _release;
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("hydration held");
    graph.root.update(cx, |root, cx| {
        root.index_poll = None;
        root.schedule_index_check(project.clone(), cx);
        root.flush_pending(cx);
    });
    // Leave the GUI wake pending until the producer receipt is delivered.
    crate::runtime::wait::until("old owner's real status executed", || {
        observations.load(Ordering::SeqCst) == 1
    });
    crate::runtime::wait::until("old status delivered into bounded event mailbox", || {
        graph
            .root
            .read_with(cx, |root, _| root.runtime.queued_results() > 0)
    });
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready {
        key: basis(),
        mode: ServiceMode::Embedded,
    });
    graph.store.update(cx, |store, cx| store.owner_ready(cx));
    graph.root.update(cx, |root, cx| root.drain_engine(cx));
    graph.root.read_with(cx, |root, _| {
        let snapshot = root.snapshot();
        let row = &snapshot.workspace().projects[0];
        assert_eq!(
            row.phase,
            ProjectPhase::Unconfirmed,
            "withdrawn nonterminal evidence does not speak for a replacement owner"
        );
        assert!(row.request.is_none());
        assert_eq!(
            row.operation.as_ref().expect("exact claim retained").key,
            crate::model::index_operation::tests::claim(&project, 0x31).key
        );
    });
    terminal.store(true, Ordering::SeqCst);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    crate::runtime::wait::until("new owner terminal receipt is admitted", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 2);
}

#[gpui::test]
fn uncertain_saved_key_is_checked_once_after_initial_store_owner_admission(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-unknown-startup").expect("project");
    let mut snapshot = active_snapshot(&project);
    let mut workspace = snapshot.workspace().clone();
    let mut rows = workspace.projects.to_vec();
    rows[0].phase = ProjectPhase::Unconfirmed;
    let operation = rows[0].operation.as_mut().expect("saved key");
    operation.observation = Some(backend_library::IndexOperationObservation::Unknown {
        operation_key: operation.key,
    });
    workspace.projects = rows.into();
    snapshot = snapshot.with_workspace(workspace);
    let gate = OwnerGate::starting();
    let (client, entered, observations, _release) = client();
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(snapshot, EngineActor::start(client, 4).expect("lanes")),
            None,
            None,
            Some(gate.clone()),
            None,
        )
    });
    let _release = _release;
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("hydration held");
    gate.publish(OwnerState::Ready {
        key: basis(),
        mode: ServiceMode::Embedded,
    });
    crate::runtime::wait::until("uncertain startup key checks exact new owner", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(
        observations.load(Ordering::SeqCst),
        1,
        "terminal evidence never becomes a frame or heartbeat poll"
    );
}

#[gpui::test]
fn a_removed_projects_late_terminal_receipt_cannot_retarget_root_context(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-removed").expect("project");
    let (client, entered, observations, _release) = client();
    let contexts = client.root_projects.clone();
    let graph = cx.update(|cx| {
        UiEntityGraph::install(
            cx,
            DesktopRuntime::new(
                active_snapshot(&project),
                EngineActor::start(client, 4).expect("lanes"),
            ),
            None,
        )
    });
    let _release = _release;
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("hydration held");
    graph.root.update(cx, |root, cx| {
        root.index_poll = None;
        root.schedule_index_check(project.clone(), cx);
        root.flush_pending(cx);
    });
    crate::runtime::wait::until("terminal receipt delivered before removal", || {
        observations.load(Ordering::SeqCst) == 1
            && graph
                .root
                .read_with(cx, |root, _| root.runtime.queued_results() > 0)
    });
    graph.root.update(cx, |root, cx| {
        root.dispatch(Intent::RemoveProject(project.clone()), cx);
        root.drain_engine(cx);
    });
    assert!(graph.root.read_with(cx, |root, _| {
        root.snapshot().workspace().projects.is_empty()
    }));
    graph.root.update(cx, |root, cx| root.refresh_root(cx));
    _release.0.release();
    crate::runtime::wait::until("new root request executes after removal", || {
        cx.run_until_parked();
        contexts.lock().expect("contexts").len() >= 2
    });
    assert!(
        contexts
            .lock()
            .expect("contexts")
            .iter()
            .all(Option::is_none),
        "a rejected receipt cannot retarget a subsequent root request"
    );
}

#[gpui::test]
fn unknown_saved_key_replaces_an_obsolete_queued_check_after_owner_recovery(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-stale-status").expect("project");
    let gate = OwnerGate::starting();
    let (client, entered, observations, _release) = client();
    let mut snapshot = active_snapshot(&project);
    let mut workspace = snapshot.workspace().clone();
    let mut rows = workspace.projects.to_vec();
    rows[0].phase = ProjectPhase::Unconfirmed;
    let claim = rows[0].operation.as_mut().expect("saved claim");
    claim.observation = Some(backend_library::IndexOperationObservation::Unknown {
        operation_key: claim.key,
    });
    workspace.projects = rows.into();
    snapshot = snapshot.with_workspace(workspace);
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(snapshot, EngineActor::start(client, 4).expect("lanes")),
            None,
            None,
            Some(gate.clone()),
            None,
        )
    });
    let _release = _release; // release worker before the graph can drop
    cx.run_until_parked();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("hydration held");
    // The initial owner is still starting: its automatic cold reconciliation
    // must not race this deliberately queued callback. Both admissions and
    // the replacement happen before the outer update flushes any callback.
    cx.update(|cx| {
        gate.publish(OwnerState::Ready {
            key: basis(),
            mode: ServiceMode::Embedded,
        });
        graph.store.update(cx, |store, cx| store.owner_ready(cx));
        graph.root.update(cx, |root, cx| {
            root.index_poll = None;
            root.schedule_index_check(project.clone(), cx);
            assert_eq!(root.pending.len(), 1, "unknown key is queued at old owner");
        });
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Ready {
            key: basis(),
            mode: ServiceMode::Embedded,
        });
        graph.store.update(cx, |store, cx| store.owner_ready(cx));
        graph.root.update(cx, |root, cx| {
            root.schedule_index_check(project.clone(), cx);
            assert_eq!(
                root.pending.len(),
                1,
                "new owner replaces the obsolete read"
            );
            root.flush_pending(cx);
        });
    });
    crate::runtime::wait::until("new attachment receives one fresh status", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn removing_an_admitted_project_retires_its_future_root_context(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let project = LocalProjectId::new("/fixture/operation-admitted-removal").expect("project");
    let (client, entered, observations, release) = client();
    let contexts = client.root_projects.clone();
    let graph = cx.update(|cx| {
        UiEntityGraph::install(
            cx,
            DesktopRuntime::new(
                active_snapshot(&project),
                EngineActor::start(client, 4).expect("lanes"),
            ),
            None,
        )
    });
    let release = release;
    cx.run_until_parked();
    entered.recv_timeout(Duration::from_secs(1)).expect("hydration held");
    graph.root.update(cx, |root, cx| root.schedule_operation_observation(cx));
    cx.executor().advance_clock(Duration::from_millis(500));
    crate::runtime::wait::until("receipt establishes project context", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot().workspace().projects[0].phase == ProjectPhase::Ready
        })
    });
    assert_eq!(observations.load(Ordering::SeqCst), 1);
    graph.root.update(cx, |root, cx| {
        root.dispatch(Intent::RemoveProject(project.clone()), cx);
        root.refresh_root(cx);
    });
    release.0.release();
    crate::runtime::wait::until("root read follows admitted project removal", || {
        cx.run_until_parked();
        contexts.lock().expect("contexts").len() >= 2
    });
    assert_eq!(contexts.lock().expect("contexts").last(), Some(&None));
    assert!(graph.root.read_with(cx, |root, _| root.snapshot().workspace().projects.is_empty()));
}
