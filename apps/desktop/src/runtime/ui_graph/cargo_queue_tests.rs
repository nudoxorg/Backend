//! Deferred input schedules through the actual root/store admission paths.
//! Producer fixtures and the no-I/O actor do not prove native/live-owner acceptance.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{PageKey, PageValue};
use crate::model::{PersistedRoute, ServiceMode};
use crate::navigation::{CargoBrowseContext, CargoSourcePath, CargoSourceRoute, OrbitRoute};
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

struct ReadmeSchedule {
    base: Schedule,
    place: Route,
    target: Route,
    key: PageKey,
}

impl ReadmeSchedule {
    fn new(cx: &mut TestAppContext) -> Self {
        let base = Schedule::new(cx);
        let (selected, value) = crate::runtime::cargo_readme_reads::tests::fixture_value();
        assert_eq!(selected.context, base.context);
        assert_eq!(selected.package.as_str(), base.source.package.as_str());
        let PageValue::Browse(BrowseValue::CargoReadme(model)) = &value else { panic!("README fixture"); };
        let crate::model::browse::CargoReadmeState::Read(document) = &model.state else { panic!("Markdown"); };
        let link = crate::navigation::CargoReadmeLinkAddress::new(document.origin.clone(), "../src/lib.rs#L7").expect("exact relative source address");
        let target = Route::CargoSource(CargoSourceRoute::readme_link(selected.context.clone(), base.source.package.clone(), link, Some(7)).expect("scope-preserving target"));
        let place = Route::Package(crate::navigation::PackageRoute { cargo: Some(selected.context.clone()), project: None,
            package: base.source.package.clone(), lane: crate::navigation::PackageLane::Overview, selected: None, at: None });
        let key = PageKey::Browse(BrowseKey::CargoReadme(selected));
        cx.update(|cx| {
            base.root.update(cx, |root, cx| root.dispatch(Intent::Navigate(place.clone()), cx));
            base.store.update(cx, |store, _| force_land(store, &key, value));
            assert!(RouteDependencies::new(&place, None).current_cargo_readme(base.store.read(cx)).is_some());
        });
        Self { base, place, target, key }
    }

    fn queue(&self, cx: &mut gpui::App) {
        let dependency = RouteDependencies::new(&self.place, None).current_cargo_readme(self.base.store.read(cx)).expect("selected current README").native_dependency();
        self.base.root.update(cx, |root, cx| root.queue_read(Intent::Navigate(self.target.clone()), dependency, cx));
    }
}

#[gpui::test]
fn readme_queued_source_refuses_replaced_attachment_or_digest_even_when_new_reads_are_current(cx: &mut TestAppContext) {
    for replace_owner in [false, true] {
        let schedule = ReadmeSchedule::new(cx);
        cx.update(|cx| {
            schedule.queue(cx);
            let reduced = schedule.base.root.read(cx).reduced();
            if replace_owner {
                schedule.base.gate.publish(OwnerState::Starting);
                schedule.base.gate.publish(OwnerState::Ready { key: authority(), mode: ServiceMode::Embedded });
                schedule.base.store.update(cx, |store, cx| {
                    store.owner_ready(cx);
                    force_land(store, &schedule.base.key, PageValue::Browse(BrowseValue::Tree(Arc::new(schedule.base.tree.clone()))));
                });
            }
            let (_, fresh) = crate::runtime::cargo_readme_reads::tests::fixture_with_contents("# New origin\n\n[Current file](../src/current.rs#L2)\n");
            schedule.base.store.update(cx, |store, _| force_land(store, &schedule.key, fresh));
            assert!(RouteDependencies::new(&schedule.place, None).current_cargo_readme(schedule.base.store.read(cx)).is_some());
            schedule.base.flush(cx);
            let root = schedule.base.root.read(cx);
            assert_eq!(root.snapshot().route(), &schedule.place);
            assert_eq!(root.reduced(), reduced, "fresh README bytes cannot lend the predecessor's queued source address their stamp or attachment");
            assert!(matches!(PersistentState::project(&root.snapshot()).route, PersistedRoute::Package { cargo: Some(_), .. }));
        });
    }
}

#[gpui::test]
fn readme_queue_rechecks_selected_tree_and_observation_only_bumps_keep_exact_scope(cx: &mut TestAppContext) {
    let changed = ReadmeSchedule::new(cx);
    cx.update(|cx| {
        changed.queue(cx);
        let stamp = changed.base.store.read(cx).stamp(&changed.key);
        let mut tree = changed.base.tree.clone(); tree.source_packages = Arc::new(BTreeSet::new());
        changed.base.store.update(cx, |store, _| force_land(store, &changed.base.key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree)))));
        assert_eq!(changed.base.store.read(cx).stamp(&changed.key), stamp);
        changed.base.flush(cx);
        assert_eq!(changed.base.root.read(cx).snapshot().route(), &changed.place, "README stamp alone is not the local Tree receipt used by the queued action");
    });
    let current = ReadmeSchedule::new(cx);
    cx.update(|cx| {
        current.queue(cx);
        current.base.root.update(cx, |root, cx| root.dispatch_runtime(Intent::OwnerReady { key: authority().observed_at(77), mode: ServiceMode::Embedded }, cx));
        current.base.flush(cx);
        let root = current.base.root.read(cx);
        assert_eq!(root.snapshot().route(), &current.target);
        let PersistedRoute::CargoReadmeLink { browse, origin, href, line, .. } = PersistentState::project(&root.snapshot()).route else { panic!("scoped durable source target"); };
        assert_eq!(browse.request_binding, current.base.context.request_binding());
        assert_eq!(origin.root_scope, backend_library::CargoPackageReadmeRootScopeV1::EffectiveWorkspace);
        assert_eq!(href, "../src/lib.rs#L7"); assert_eq!(line, Some(7));
    });
}
