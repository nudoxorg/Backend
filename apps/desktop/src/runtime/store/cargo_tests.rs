//! Controlled read schedules prove dependency notifications and owner fences.
//! These are fixture scheduling tests, not native or live Cargo-owner acceptance.

#![allow(clippy::expect_used)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::ServiceMode;
use crate::model::browse::{BrowseKey, BrowseValue, CargoSourceInventoryModel};
use crate::model::pages::{CargoSourcePage, PageValue, SourceOrigin, SourceText};
use crate::navigation::{CargoSourcePath, CargoSourceRoute, Overlay, SettingsPage};
use crate::runtime::owner::OwnerState;
use crate::runtime::reads::{PageReader, ReadContext};
use gpui::{Subscription, TestAppContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

type InventoryGate = Arc<(Mutex<bool>, Condvar)>;

struct ScheduledReader {
    inventory_gate: InventoryGate,
    file_reads: Arc<AtomicUsize>,
}

impl PageReader for ScheduledReader {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        match request {
            ReadRequest::CargoSource(key) => {
                let read = self.file_reads.fetch_add(1, Ordering::SeqCst) + 1;
                let contents = format!("pub fn checked_{read}() {{}}\n");
                let content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
                Ok(PageValue::CargoSource(CargoSourcePage {
                    package: key.package.clone(),
                    file: key.file.clone(),
                    source: SourceText::new(contents.into(), 1, SourceOrigin::LocalFile, true)
                        .expect("fixture bytes"),
                    content_digest,
                    source_revision: [9; 32],
                }))
            }
            ReadRequest::Browse(BrowseKey::CargoSourceInventory(key)) => {
                let (lock, released) = &*self.inventory_gate;
                let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !*open {
                    if context.cancel.is_cancelled() {
                        return Err(ReadFailure::Cancelled);
                    }
                    open = released
                        .wait_timeout(open, Duration::from_millis(5))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                Ok(PageValue::Browse(BrowseValue::CargoSourceInventory(
                    Arc::new(CargoSourceInventoryModel {
                        package: key.package.clone(),
                        paths: Arc::from([
                            CargoSourcePath::new("src/lib.rs").expect("file address")
                        ]),
                        coverage: backend_library::CargoPackageSourceInventoryCoverageV1::Complete,
                        source_revision: [9; 32],
                    }),
                )))
            }
            _ => Err(ReadFailure::Unavailable(
                UnavailableReason::Unsupported,
                "scheduled fixture".into(),
            )),
        }
    }
}

struct CargoRig {
    store: Entity<DataStore>,
    inventory_gate: InventoryGate,
    file_reads: Arc<AtomicUsize>,
    notifications: Rc<RefCell<Vec<PageKey>>>,
    dependencies: RouteDependencies,
    _events: Subscription,
}

impl Drop for CargoRig {
    fn drop(&mut self) {
        self.release_inventory();
    }
}

fn root() -> VersionedRoot {
    VersionedRoot::synthetic(
        backend_library::view_state_root(&[("cargo-schedule".to_owned(), "same-root".to_owned())]),
        7,
    )
}

fn route() -> Route {
    Route::CargoSource(
        CargoSourceRoute::new(
            LocalProjectId::new("/fixture/cargo-schedule").expect("project address"),
            PackageId::new(&format!(
                "pkg:cargo/scheduled@1.0.0?cargo-authority={}",
                "a".repeat(64)
            ))
            .expect("qualified address"),
            CargoSourcePath::new("src/lib.rs").expect("file address"),
            None,
        )
        .expect("Cargo address"),
    )
}

fn rig(cx: &mut TestAppContext, gate: Option<OwnerGate>, inventory_ready: bool) -> CargoRig {
    cx.executor().allow_parking();
    let inventory_gate = Arc::new((Mutex::new(inventory_ready), Condvar::new()));
    let file_reads = Arc::new(AtomicUsize::new(0));
    let workers_gate = Arc::clone(&inventory_gate);
    let workers_reads = Arc::clone(&file_reads);
    let pool = ReadPool::start(2, move |_| ScheduledReader {
        inventory_gate: Arc::clone(&workers_gate),
        file_reads: Arc::clone(&workers_reads),
    })
    .expect("read pool");
    let mut snapshot = AppSnapshot::empty(root());
    let mut session = snapshot.session().clone();
    session.route = route();
    snapshot = snapshot.with_session(session);
    let dependencies = RouteDependencies::new(snapshot.route(), snapshot.overlay());
    let store = cx
        .update(|cx| DataStore::install_with_owner(cx, Arc::new(snapshot), Some(pool), gate, None));
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&notifications);
    let keys = dependencies.keys().to_vec();
    let events = cx.update(|cx| {
        let mut watch = Watch::new(store.read(cx), keys, &[]);
        cx.subscribe(&store, move |store, event, cx| {
            if watch.changed(store.read(cx), event)
                && let StoreEvent::Resource(key) = event
            {
                sink.borrow_mut().push(key.clone());
            }
        })
    });
    CargoRig {
        store,
        inventory_gate,
        file_reads,
        notifications,
        dependencies,
        _events: events,
    }
}

impl CargoRig {
    fn release_inventory(&self) {
        let (lock, released) = &*self.inventory_gate;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        released.notify_all();
    }

    fn until(&self, cx: &mut TestAppContext, done: impl Fn(&DataStore) -> bool) {
        crate::runtime::wait::until("the Cargo read schedule reached its next step", || {
            cx.run_until_parked();
            self.store.read_with(cx, |store, _| done(store))
        });
    }

    fn file_key(&self) -> PageKey {
        PageKey::CargoSource(self.dependencies.cargo().expect("Cargo pair").file.clone())
    }

    fn inventory_key(&self) -> PageKey {
        PageKey::Browse(BrowseKey::CargoSourceInventory(
            self.dependencies
                .cargo()
                .expect("Cargo pair")
                .inventory
                .clone(),
        ))
    }

    fn file_current(&self, store: &DataStore) -> bool {
        let file = &self.dependencies.cargo().expect("Cargo pair").file;
        store.cargo_read_admission(&self.file_key(), &store.cargo_source(file))
            == CargoReadAdmission::Current
    }

    fn both_current(&self, store: &DataStore) -> bool {
        let PageKey::Browse(inventory) = self.inventory_key() else {
            return false;
        };
        self.file_current(store)
            && store.cargo_read_admission(&self.inventory_key(), &store.pages.browse(&inventory))
                == CargoReadAdmission::Current
    }
}

#[gpui::test]
fn inventory_arriving_after_idle_file_wakes_the_same_reading_plan_and_stays_resident(
    cx: &mut TestAppContext,
) {
    let rig = rig(cx, None, false);
    rig.until(cx, |store| {
        rig.file_current(store) && store.is_loading(&rig.inventory_key())
    });
    rig.notifications.borrow_mut().clear();
    rig.release_inventory();
    rig.until(cx, |store| rig.both_current(store));
    assert!(
        rig.notifications.borrow().contains(&rig.inventory_key()),
        "the late auxiliary reply wakes the route's watch"
    );
    rig.store.read_with(cx, |store, _| {
        assert_eq!(
            store.focused(),
            &BTreeSet::from_iter(rig.dependencies.keys().iter().cloned())
        );
        let pages = crate::shell::bodies::Pages::gather(store, &rig.dependencies);
        let PageKey::Browse(inventory) = rig.inventory_key() else {
            return;
        };
        assert!(
            pages.browse(&inventory).is_loaded(),
            "gathering keeps the same independently arriving resource"
        );
    });
    for at in 0..8 {
        let key = PageKey::Browse(BrowseKey::Tree(
            LocalProjectId::new(&format!("/fixture/unrelated-{at}")).expect("tree"),
        ));
        rig.store.update(cx, |store, cx| {
            store.ensure(key, cx);
        });
        cx.run_until_parked();
    }
    assert!(
        rig.store.read_with(cx, |store, _| rig.both_current(store)),
        "unrelated browse reads cannot evict the visible inventory"
    );
}

#[gpui::test]
fn idle_source_and_inventory_revocations_notify_without_an_inflight_read(cx: &mut TestAppContext) {
    let gate = OwnerGate::ready(root(), ServiceMode::Attached);
    let rig = rig(cx, Some(gate.clone()), true);
    rig.until(cx, |store| rig.both_current(store));
    rig.notifications.borrow_mut().clear();
    gate.publish(OwnerState::Starting);
    rig.store.update(cx, DataStore::owner_starting);
    cx.run_until_parked();
    for key in rig.dependencies.keys() {
        assert!(
            rig.notifications.borrow().contains(key),
            "idle revocation publishes {key}"
        );
    }
    rig.store.read_with(cx, |store, _| {
        let file = &rig.dependencies.cargo().expect("Cargo pair").file;
        assert!(store.cargo_source(file).loaded_value().is_none());
        assert_eq!(
            store.cargo_read_admission(&rig.file_key(), &store.cargo_source(file)),
            CargoReadAdmission::Checking
        );
    });
    rig.notifications.borrow_mut().clear();
    rig.store.update(cx, DataStore::owner_starting);
    cx.run_until_parked();
    assert!(
        rig.notifications.borrow().is_empty(),
        "unchanged pending resources do not notify again"
    );
}

#[gpui::test]
fn same_root_reattachment_rechecks_completed_bytes_even_if_starting_is_coalesced(
    cx: &mut TestAppContext,
) {
    let gate = OwnerGate::ready(root(), ServiceMode::Attached);
    let rig = rig(cx, Some(gate.clone()), true);
    rig.until(cx, |store| rig.both_current(store));
    let old_attachment = rig.store.read_with(cx, |store, _| {
        store
            .current_owner_attachment()
            .expect("serving first owner")
    });
    let old_digest = rig.store.read_with(cx, |store, _| {
        store
            .cargo_source(&rig.dependencies.cargo().expect("Cargo pair").file)
            .loaded_value()
            .expect("first reply")
            .content_digest
    });
    let attachment = gate.attached_ready_epoch().expect("attached owner");
    assert!(gate.attached_lost_at(attachment, "fixture socket closed".into()));
    rig.store.read_with(cx, |store, _| {
        assert!(!store.admits_owner_attachment(&old_attachment));
        let resource = store.cargo_source(&rig.dependencies.cargo().expect("Cargo pair").file);
        assert!(
            resource.loaded_value().is_some(),
            "loss precedes the UI watcher"
        );
        assert!(
            matches!(
                store.cargo_read_admission(&rig.file_key(), &resource),
                CargoReadAdmission::Fault(_)
            ),
            "the old loaded slot cannot paint through endpoint loss"
        );
    });
    assert!(gate.restart());
    gate.publish(OwnerState::Ready {
        key: root(),
        mode: ServiceMode::Attached,
    });
    rig.store.read_with(cx, |store, _| {
        assert!(!store.admits_owner_attachment(&old_attachment));
        let resource = store.cargo_source(&rig.dependencies.cargo().expect("Cargo pair").file);
        assert_eq!(
            store.cargo_read_admission(&rig.file_key(), &resource),
            CargoReadAdmission::Checking,
            "same-root replacement cannot inherit prior serving admission"
        );
    });
    rig.store.update(cx, DataStore::owner_ready);
    rig.until(cx, |store| rig.both_current(store));
    assert!(!rig.store.read_with(cx, |store, _| {
        store.admits_owner_attachment(&old_attachment)
    }));
    assert_eq!(
        rig.file_reads.load(Ordering::SeqCst),
        2,
        "a completed old file was read again"
    );
    rig.store.read_with(cx, |store, _| {
        let resource = store.cargo_source(&rig.dependencies.cargo().expect("Cargo pair").file);
        assert_ne!(
            resource.loaded_value().expect("new reply").content_digest,
            old_digest
        );
    });
}

#[gpui::test]
fn diagnostic_observation_bumps_do_not_revoke_current_cargo_admission(cx: &mut TestAppContext) {
    let gate = OwnerGate::ready(root(), ServiceMode::Attached);
    let rig = rig(cx, Some(gate), true);
    rig.until(cx, |store| rig.both_current(store));
    let captured = rig.store.read_with(cx, |store, _| {
        store
            .current_owner_attachment()
            .expect("fixture attached owner")
    });
    rig.store.update(cx, |store, cx| {
        let next = store.snapshot().with_key(root().observed_at(99), None);
        store.admit_snapshot(Arc::new(next), cx);
    });
    assert!(rig.store.read_with(cx, |store, _| rig.both_current(store)));
    assert!(
        rig.store
            .read_with(cx, |store, _| store.admits_owner_attachment(&captured))
    );
    assert_eq!(rig.file_reads.load(Ordering::SeqCst), 1);
}

#[test]
fn an_unserved_placeholder_cannot_admit_loaded_cargo_bytes() {
    let dependencies = RouteDependencies::new(&route(), None);
    let store = DataStore::new(
        Arc::new(AppSnapshot::empty(VersionedRoot::unserved())),
        None,
    );
    assert_eq!(
        store.cargo_read_admission(
            &dependencies.keys()[0],
            &Resource::loaded_at(7_u32, VersionedRoot::unserved())
        ),
        CargoReadAdmission::Checking
    );
}

#[gpui::test]
fn closing_settings_reenters_the_same_source_pair_with_a_fresh_owner_read(cx: &mut TestAppContext) {
    let rig = rig(cx, None, true);
    rig.until(cx, |store| rig.both_current(store));
    rig.store.update(cx, |store, cx| {
        let snapshot = store.snapshot();
        let mut session = snapshot.session().clone();
        session.overlay = Some(Overlay::Settings(SettingsPage::Appearance));
        store.admit_snapshot(Arc::new(snapshot.with_session(session)), cx);
    });
    assert_eq!(
        rig.store.read_with(cx, |store, _| store.focused().clone()),
        BTreeSet::from([PageKey::Health])
    );
    rig.store.update(cx, |store, cx| {
        let snapshot = store.snapshot();
        let mut session = snapshot.session().clone();
        session.overlay = None;
        store.admit_snapshot(Arc::new(snapshot.with_session(session)), cx);
    });
    rig.until(cx, |store| rig.both_current(store));
    assert_eq!(rig.file_reads.load(Ordering::SeqCst), 2);
}

#[test]
fn settings_and_inbox_replace_the_visible_cargo_dependency_pair() {
    let route = route();
    assert_eq!(
        RouteDependencies::new(&route, Some(Overlay::Settings(SettingsPage::Appearance))).keys(),
        &[PageKey::Health]
    );
    assert!(
        RouteDependencies::new(&route, Some(Overlay::Inbox))
            .keys()
            .is_empty()
    );
    assert!(
        RouteDependencies::new(&route, Some(Overlay::Inbox))
            .cargo()
            .is_none()
    );
}

#[test]
fn world_keeps_its_resident_orbit_read_without_delaying_graph_content() {
    let dependencies = RouteDependencies::new(&Route::World, None);
    let store = DataStore::new(Arc::new(AppSnapshot::empty(root())), None);
    assert_eq!(dependencies.keys(), &[PageKey::Orbit]);
    assert!(dependencies.content_loaded(&store));
}

#[test]
fn code_source_and_symbol_dependencies_are_kept_once_each() {
    let route = crate::shell::tests::view_route("RelationLabel", crate::navigation::View::Code);
    let keys = RouteDependencies::new(&route, None).into_keys();
    assert!(matches!(
        keys.as_slice(),
        [PageKey::Source(_), PageKey::Symbol(_)]
    ));
    let kept = crate::runtime::snapshot::kept_keys(&route);
    assert_eq!(
        kept.len(),
        BTreeSet::from_iter(kept.iter().cloned()).len(),
        "launch snapshots cannot contain duplicate resource claims"
    );
}
