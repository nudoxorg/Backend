//! Deferred input schedules through the actual root/store admission paths.
//! Producer fixtures and the no-I/O actor do not prove native/live-owner acceptance.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{PageKey, PageValue};
use crate::model::{PersistedRoute, ServiceMode};
use crate::navigation::{CargoBrowseContext, CargoSourcePath, CargoSourceRoute};
use crate::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::store::cargo_context_tests::{fixture, force_land};
use gpui::TestAppContext;

struct NoIo;
impl EngineClient for NoIo {
    fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> { Err(EngineFault::Cancelled) }
}

fn authority() -> VersionedRoot {
    VersionedRoot::synthetic(backend_library::view_state_root(&[("cargo-queued-read".into(), "same-owner-root".into())]), 19)
}

struct Schedule {
    root: Entity<UiRootEntity>, store: Entity<DataStore>, gate: OwnerGate,
    source: CargoSourceRoute, context: CargoBrowseContext, tree: TreeModel, key: PageKey,
}

impl Schedule {
    fn new(cx: &mut TestAppContext) -> Self {
        let project = LocalProjectId::new("/fixture/workspace/member").expect("requested project");
        let (tree, package, context) = fixture(&project);
        let source = CargoSourceRoute::from_unbound_saved(project.clone(), PackageId::new(package.as_str()).expect("package"), CargoSourcePath::new("src/lib.rs").expect("file"), Some(7)).expect("legacy address");
        let mut snapshot = AppSnapshot::empty(authority());
        let mut session = snapshot.session().clone(); session.route = Route::CargoSource(source.clone());
        snapshot = snapshot.with_session(session);
        let gate = OwnerGate::ready(authority(), ServiceMode::Embedded);
        let store = cx.update(|cx| DataStore::install_with_owner(cx, Arc::new(snapshot.clone()), None, Some(gate.clone()), None));
        let key = PageKey::Browse(BrowseKey::Tree(project));
        store.update(cx, |store, _| force_land(store, &key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone())))));
        let actor = EngineActor::start(NoIo, 1).expect("idle actor");
        let attached = store.clone();
        let root = cx.update(|cx| cx.new(|_| {
            let mut root = UiRootEntity::new(DesktopRuntime::new(snapshot, actor), None);
            // No startup read: this schedule holds one already-rendered input
            // instant through queue and flush without an independent root task.
            root.pending.clear(); root.published = Some(root.snapshot()); root.store = Some(attached); root
        }));
        Self { root, store, gate, source, context, tree, key }
    }

    fn queue(&self, cx: &mut gpui::App) {
        self.root.update(cx, |root, cx| root.queue(Intent::ResolveCargoBrowse { expected: self.source.clone(), context: self.context.clone() }, cx));
    }
    fn flush(&self, cx: &mut gpui::App) { self.root.update(cx, |root, cx| root.flush_pending(cx)); }
}

#[gpui::test]
fn replacing_owner_after_queue_cannot_persist_old_bound_address_even_after_fresh_same_binding_lands(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx);
    cx.update(|cx| {
        schedule.queue(cx);
        schedule.gate.publish(OwnerState::Starting);
        schedule.gate.publish(OwnerState::Ready { key: authority(), mode: ServiceMode::Embedded });
        schedule.store.update(cx, |store, cx| {
            store.owner_ready(cx);
            force_land(store, &schedule.key, PageValue::Browse(BrowseValue::Tree(Arc::new(schedule.tree.clone()))));
        });
        assert!(RouteDependencies::new(&Route::CargoSource(schedule.source.clone()), None)
            .current_cargo_package(schedule.store.read(cx), &crate::model::pages::PackageRef::parse(schedule.source.package.as_str()).expect("package")).is_some());
        schedule.flush(cx);
        let root = schedule.root.read(cx);
        assert_eq!(root.snapshot().route(), &Route::CargoSource(schedule.source.clone()));
        assert_eq!(root.reduced(), 0, "a newly current same-root receipt cannot lend the old attachment its flush admission");
        assert!(matches!(PersistentState::project(&root.snapshot()).route, PersistedRoute::CargoSource { request_binding: None, .. }));
    });
}

#[gpui::test]
fn changed_selected_tree_or_competing_visit_with_same_address_cancels_deferred_resolution(cx: &mut TestAppContext) {
    let changed = Schedule::new(cx);
    cx.update(|cx| {
        changed.queue(cx);
        let mut next = changed.tree.clone(); next.source_packages = Arc::new(BTreeSet::new());
        changed.store.update(cx, |store, _| force_land(store, &changed.key, PageValue::Browse(BrowseValue::Tree(Arc::new(next)))));
        changed.flush(cx);
        assert_eq!(changed.root.read(cx).snapshot().route(), &Route::CargoSource(changed.source.clone()));
        assert_eq!(changed.root.read(cx).reduced(), 0);
    });
    let competing = Schedule::new(cx);
    cx.update(|cx| {
        competing.queue(cx);
        competing.root.update(cx, |root, cx| {
            root.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Home)), cx);
            root.dispatch(Intent::Navigate(Route::CargoSource(competing.source.clone())), cx);
        });
        competing.flush(cx);
        assert_eq!(competing.root.read(cx).snapshot().route(), &Route::CargoSource(competing.source.clone()));
        assert_eq!(competing.root.read(cx).reduced(), 2, "returning to the same address does not restore the queued visit");
    });
}

#[gpui::test]
fn observation_only_bump_preserves_current_selected_read_and_fresh_queue_can_bind(cx: &mut TestAppContext) {
    let schedule = Schedule::new(cx);
    cx.update(|cx| {
        schedule.queue(cx);
        schedule.root.update(cx, |root, cx| root.dispatch_runtime(Intent::OwnerReady { key: authority().observed_at(71), mode: ServiceMode::Embedded }, cx));
        schedule.flush(cx);
        let expected = schedule.source.resolve_context(schedule.context.clone()).expect("same requested address");
        assert_eq!(schedule.root.read(cx).snapshot().route(), &Route::CargoSource(expected));
        assert!(matches!(PersistentState::project(&schedule.root.read(cx).snapshot()).route, PersistedRoute::CargoSource { request_binding: Some(binding), .. } if binding == schedule.context.request_binding()));
    });
}
