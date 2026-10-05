//! Local admissions wait for an exact live owner before becoming mutations.
#![allow(clippy::expect_used)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::{CatalogState, ObjectId, PackageSummary, ProjectPhase, ServiceMode};
use crate::navigation::OrbitRoute;
use crate::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::owner::{OwnerGate, OwnerState};
use gpui::TestAppContext;

struct NoIo;
impl EngineClient for NoIo {
    fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
        Err(EngineFault::Cancelled)
    }
}

fn authority() -> VersionedRoot {
    VersionedRoot::synthetic(
        backend_library::view_state_root(&[("local-index-queue".into(), "owner".into())]),
        7,
    )
}

struct Schedule {
    root: Entity<UiRootEntity>,
    store: Entity<DataStore>,
    gate: OwnerGate,
    project: LocalProjectId,
}
impl Schedule {
    fn new(cx: &mut TestAppContext, gate: OwnerGate) -> Self {
        // This oracle races the real filesystem writer and engine actor.
        cx.executor().allow_parking();
        let project = LocalProjectId::new("/fixture/local-project").expect("project");
        let snapshot = crate::navigation::reduce(
            &AppSnapshot::empty(authority()),
            Intent::AddProject {
                project: project.clone(),
            },
        )
        .snapshot;
        let store = cx.update(|cx| {
            DataStore::install_with_owner(
                cx,
                Arc::new(snapshot.clone()),
                None,
                Some(gate.clone()),
                None,
            )
        });
        static NEXT_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let directory = std::env::temp_dir().join(format!("nudox-operation-preflight-{}-{}", std::process::id(),
            NEXT_STATE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        crate::host::private_dir(&directory).expect("private operation fixture state");
        let persistence = PersistentState::at(directory.join("desktop.json"));
        let actor = EngineActor::start(NoIo, 4).expect("actor");
        let attached = store.clone();
        let root = cx.update(|cx| {
            cx.new(|_| {
                let mut root = UiRootEntity::new(DesktopRuntime::new(snapshot, actor), Some(persistence));
                root.pending.clear();
                root.store = Some(attached);
                root.published = Some(root.snapshot());
                root
            })
        });
        Self {
            root,
            store,
            gate,
            project,
        }
    }

    fn assert_unsent(&self, cx: &gpui::App) {
        let root = self.root.read(cx);
        let snapshot = root.snapshot();
        let project = &snapshot.workspace().projects[0];
        assert_eq!(project.phase, ProjectPhase::Indexing);
        assert_eq!(
            project.request, None,
            "a local admission is not an owner operation"
        );
        assert!(
            !root.has_pending_work(),
            "no engine mutation is submitted while unavailable"
        );
    }
}

#[gpui::test]
fn failed_or_starting_owner_keeps_local_admissions_unsent_and_ready_resumes_once(
    cx: &mut TestAppContext,
) {
    let schedule = Schedule::new(cx, OwnerGate::starting());
    cx.update(|cx| {
        schedule
            .root
            .update(cx, |root, cx| root.schedule_pending_indexes(cx));
        schedule.assert_unsent(cx);
        schedule
            .gate
            .publish(OwnerState::Failed("owner unavailable".into()));
        schedule
            .root
            .update(cx, |root, cx| root.schedule_pending_indexes(cx));
        schedule.assert_unsent(cx);
        assert!(schedule.gate.restart());
        schedule.gate.publish(OwnerState::Ready {
            key: authority(),
            mode: ServiceMode::Embedded,
        });
        schedule.store.update(cx, |store, cx| store.owner_ready(cx));
        schedule.root.update(cx, |root, cx| {
            root.schedule_pending_indexes(cx);
            root.schedule_pending_indexes(cx);
            assert_eq!(
                root.pending.len(),
                1,
                "repeated scheduling coalesces the exact project"
            );
            root.flush_pending(cx);
            assert_eq!(root.index_preflights.len(), 1, "the exact request waits for a durable acknowledgment");
        });
    });
}

#[gpui::test]
fn queued_index_cannot_cross_a_same_root_owner_replacement(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx, OwnerGate::ready(authority(), ServiceMode::Attached));
    cx.update(|cx| {
        schedule.root.update(cx, |root, cx| {
            root.schedule_index(schedule.project.clone(), cx)
        });
        let stale = schedule.root.read(cx).pending[0].intent().clone();
        schedule.gate.publish(OwnerState::Starting);
        schedule.gate.publish(OwnerState::Ready {
            key: authority(),
            mode: ServiceMode::Attached,
        });
        schedule.store.update(cx, |store, cx| store.owner_ready(cx));
        schedule.root.update(cx, |root, cx| root.flush_pending(cx));
        schedule.assert_unsent(cx);
        schedule.root.update(cx, |root, cx| {
            assert_eq!(root.pending.len(), 1, "stale callback removal schedules the current owner without another event");
            let (Intent::IndexProject { request: old, operation: old_claim, .. },
                Intent::IndexProject { request: new, operation: new_claim, .. }) = (&stale, root.pending[0].intent()) else {
                assert!(matches!((&stale, root.pending[0].intent()),
                    (Intent::IndexProject { .. }, Intent::IndexProject { .. })), "only the unsent index lane is queued");
                return;
            };
            assert_ne!(old, new);
            assert_ne!(old_claim.key, new_claim.key);
            root.flush_pending(cx);
            assert!(
                root.index_preflights.contains_key(&schedule.project),
                "fresh attachment admits a new durable barrier"
            );
        });
    });
}

#[gpui::test]
fn catalog_publication_keeps_home_and_its_navigation_history(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx, OwnerGate::starting());
    cx.update(|cx| {
        schedule.root.update(cx, |root, cx| {
            let before = root.snapshot().session().clone();
            let row = PackageSummary {
                coordinate: PackageId::new("pkg:cargo/serde@1.0.0").expect("package"),
                name: "serde".into(),
                version: "1.0.0".into(),
                ecosystem: "Cargo".into(),
                bytes: 0,
                standing: "current".into(),
                downloads: "unknown".into(),
                advisory: "unknown".into(),
                object: ObjectId::test(7),
            };
            let publication = root.snapshot().with_catalog(
                CatalogState {
                    packages: Arc::from([row]),
                },
                authority(),
            );
            root.apply_events(
                vec![RuntimeEvent::SnapshotChanged(Arc::new(publication))],
                cx,
            );
            root.flush_pending(cx);
            let after = root.snapshot();
            assert_eq!(after.route(), &Route::Orbit(OrbitRoute::Home));
            assert_eq!(after.session().back, before.back);
            assert_eq!(after.session().forward, before.forward);
            assert_eq!(
                root.reduced(),
                0,
                "a data publication is not user navigation"
            );
        });
    });
}

#[gpui::test]
fn a_queued_request_is_saved_as_submitted_before_entering_the_actor(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx, OwnerGate::ready(authority(), ServiceMode::Embedded));
    let directory =
        std::env::temp_dir().join(format!("nudox-index-preflight-{}", std::process::id()));
    crate::host::private_dir(&directory).expect("private persistence directory");
    let persistence = PersistentState::at(directory.join("desktop.json"));
    cx.update(|cx| {
        schedule.root.update(cx, |root, cx| {
            persistence.save(&PersistentState::project(&root.snapshot())).expect("save queued admission");
            assert_eq!(persistence.load().expect("queued state").shelf[0].phase, crate::model::PersistedProjectPhase::Queued);
            root.persistence = Some(persistence.clone());
            root.schedule_pending_indexes(cx);
            root.flush_pending(cx);
            assert_eq!(root.index_preflights.len(), 1);
            assert!(root.snapshot().workspace().projects[0].request.is_none(), "saving asynchronously is not transport admission");
        });
    });
    crate::runtime::wait::until("the asynchronous durable barrier completed", || {
        cx.run_until_parked();
        schedule.root.read_with(cx, |root, _| root.index_preflights.is_empty())
    });
    let written = persistence.load().expect("saved exact claim");
    assert!(written.shelf[0].operation.is_some());
    schedule.root.read_with(cx, |root, _| assert_eq!(written.shelf[0].operation.as_ref().map(|claim| claim.key),
        root.snapshot().workspace().projects[0].operation.as_ref().map(|claim| claim.key)));
    let finished = schedule.root.update(cx, |root, cx| root.finish_persistence(cx)).expect("writer finish");
    crate::runtime::wait::until_some("state writer drained", || finished.try_recv().ok()).expect("synchronized state");
    std::fs::remove_dir_all(directory).expect("remove persistence directory");
}

#[gpui::test]
fn failed_durable_admission_never_submits_an_index_or_requeues_it_forever(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx, OwnerGate::ready(authority(), ServiceMode::Embedded));
    let directory = std::env::temp_dir().join(format!(
        "nudox-index-preflight-failure-{}",
        std::process::id()
    ));
    crate::host::private_dir(&directory).expect("private persistence directory");
    let path = directory.join("desktop.json");
    std::fs::create_dir(&path).expect("a directory cannot be replaced by a state file");
    cx.update(|cx| {
        schedule.root.update(cx, |root, cx| {
            root.persistence = Some(PersistentState::at(path.clone()));
            root.schedule_pending_indexes(cx);
            root.flush_pending(cx);
            assert_eq!(root.index_preflights.len(), 1);
        });
    });
    crate::runtime::wait::until("failed durable barrier completed", || {
        cx.run_until_parked();
        schedule.root.read_with(cx, |root, _| root.index_preflights.is_empty())
    });
    schedule.root.read_with(cx, |root, _| {
        assert!(!root.has_pending_work());
        let snapshot = root.snapshot();
        assert_eq!(snapshot.workspace().projects[0].phase, ProjectPhase::Failed);
        assert_eq!(snapshot.workspace().projects[0].request, None);
        assert!(snapshot.workspace().projects[0].error.as_deref().expect("local admission failure").contains("could not be saved"));
        assert!(root.pending.is_empty(), "failure does not reschedule the unsent operation");
    });
    let finished = schedule.root.update(cx, |root, cx| root.finish_persistence(cx)).expect("writer finish");
    assert!(crate::runtime::wait::until_some("failed writer drained", || finished.try_recv().ok()).is_err());
    std::fs::remove_dir_all(directory).expect("remove persistence directory");
}

#[gpui::test]
fn absent_persistence_fails_closed_without_allocating_a_sent_claim(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx, OwnerGate::ready(authority(), ServiceMode::Attached));
    cx.update(|cx| schedule.root.update(cx, |root, cx| {
        root.persistence = None;
        root.schedule_pending_indexes(cx);
        root.flush_pending(cx);
        let snapshot = root.snapshot();
        let row = &snapshot.workspace().projects[0];
        assert_eq!(row.phase, ProjectPhase::Failed);
        assert_eq!(row.request, None);
        assert_eq!(row.operation, None);
        assert!(!root.has_pending_work());
        assert!(row.error.as_deref().expect("actionable reason").contains("cannot be saved"));
    }));
}
