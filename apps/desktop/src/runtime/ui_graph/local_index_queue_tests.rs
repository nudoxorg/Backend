//! Local admissions wait for an exact live owner before becoming mutations.
#![allow(clippy::expect_used)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::{CatalogState, ObjectId, PackageSummary, ProjectPhase, ServiceMode};
use crate::navigation::OrbitRoute;
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
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
        let actor = EngineActor::start(NoIo, 4).expect("actor");
        let attached = store.clone();
        let root = cx.update(|cx| {
            cx.new(|_| {
                let mut root = UiRootEntity::new(DesktopRuntime::new(snapshot, actor), None);
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
            assert!(root.snapshot().workspace().projects[0].request.is_some());
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
        schedule.gate.publish(OwnerState::Starting);
        schedule.gate.publish(OwnerState::Ready {
            key: authority(),
            mode: ServiceMode::Attached,
        });
        schedule.store.update(cx, |store, cx| store.owner_ready(cx));
        schedule.root.update(cx, |root, cx| root.flush_pending(cx));
        schedule.assert_unsent(cx);
        schedule.root.update(cx, |root, cx| {
            root.schedule_pending_indexes(cx);
            root.flush_pending(cx);
            assert!(
                root.snapshot().workspace().projects[0].request.is_some(),
                "fresh attachment admits fresh scheduling"
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
            persistence
                .save(&PersistentState::project(&root.snapshot()))
                .expect("save queued admission");
            assert_eq!(
                persistence.load().expect("queued state").shelf[0].phase,
                crate::model::PersistedProjectPhase::Queued
            );
            root.persistence = Some(persistence.clone());
            root.schedule_pending_indexes(cx);
            root.flush_pending(cx);
            assert!(root.snapshot().workspace().projects[0].request.is_some());
            assert_eq!(
                persistence.load().expect("submitted state").shelf[0].phase,
                crate::model::PersistedProjectPhase::Indexing
            );
        });
    });
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
            assert!(!root.has_pending_work());
            let snapshot = root.snapshot();
            assert_eq!(snapshot.workspace().projects[0].phase, ProjectPhase::Failed);
            assert_eq!(snapshot.workspace().projects[0].request, None);
            assert!(
                snapshot.workspace().projects[0]
                    .error
                    .as_deref()
                    .expect("local admission failure")
                    .contains("could not be saved")
            );
            assert!(
                root.pending.is_empty(),
                "local failure does not reschedule the same unsent operation"
            );
        });
    });
    std::fs::remove_dir_all(directory).expect("remove persistence directory");
}
