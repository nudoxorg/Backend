//! Fixture schedules exercise the actual owner and resource admission APIs.
//! They do not establish native pixels, keyboard delivery, or live-owner acceptance.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, ResourceAdmission, admit_resource};
use crate::model::ServiceMode;
use crate::model::browse::{BrowseKey, BrowseValue};
use crate::model::pages::PageValue;
use crate::navigation::{BrowseRoute, OrbitRoute, Overlay};
use crate::runtime::owner::OwnerState;
use crate::runtime::reads::{PageReader, ReadContext};
use gpui::{Subscription, TestAppContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

type ReadCounts = Arc<Mutex<BTreeMap<PageKey, usize>>>;
type ReadGate = Arc<(Mutex<bool>, Condvar)>;

struct ScheduledOwnerReader {
    reads: ReadCounts,
    renewals: ReadGate,
}

impl PageReader for ScheduledOwnerReader {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        let key = match request {
            ReadRequest::Orbit => PageKey::Orbit,
            ReadRequest::Health => PageKey::Health,
            ReadRequest::Package(package) => PageKey::Package(package.clone()),
            ReadRequest::Browse(key @ BrowseKey::Tree(_)) => PageKey::Browse(key.clone()),
            _ => {
                return Err(ReadFailure::Unavailable(
                    UnavailableReason::Unsupported,
                    "owner schedule fixture".into(),
                ));
            }
        };
        let renewal = {
            let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
            let count = reads.entry(key).or_default();
            *count += 1;
            *count > 1
        };
        if renewal {
            let (lock, released) = &*self.renewals;
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
        }
        if let ReadRequest::Browse(BrowseKey::Tree(project)) = request {
            // An empty synthetic lockfile observation; no source capability
            // or compiler coverage is supplied by this fixture.
            use backend_library::browse::{
                LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership,
                TreeInput, TreeSource, build_tree,
            };
            let mut tree = build_tree(
                &TreeInput {
                    root: project
                        .service_coordinate()
                        .expect("fixture root")
                        .to_owned(),
                    source: TreeSource::Lockfile {
                        reason: "fixture observation".into(),
                        coverage: LockfileGraphCoverage::Complete,
                        workspace_membership: LockfileWorkspaceMembership::Unknown,
                    },
                    packages: Vec::new(),
                    edges: Vec::new(),
                    locked_inactive: 0,
                    locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
                },
                &|_: &str, _: &str| panic!("empty fixture must not ask for advisories"),
            );
            let binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
                &project.path(), &tree.root,
            ).expect("exact display fixture request");
            tree.observation = Some(backend_library::browse::ProjectTreeObservationV1::DisplayOnly { binding });
            return Ok(PageValue::Browse(BrowseValue::Tree(Arc::new(
                crate::runtime::browse_reads::tree_model(&tree),
            ))));
        }
        // The same immutable typed value is returned by both owners. A
        // successful new read must still publish current admission.
        crate::shell::tests::Fixture.read(request, context)
    }
}

fn root() -> crate::core::VersionedRoot {
    crate::core::VersionedRoot::synthetic(
        backend_library::view_state_root(&[("owner-read".into(), "same-root".into())]),
        17,
    )
}

fn routes() -> Vec<Route> {
    vec![
        Route::Orbit(OrbitRoute::Home),
        crate::shell::kit::package_route(&crate::shell::tests::dossier().package)
            .expect("package route"),
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(
            LocalProjectId::new("/fixture/owner-tree").expect("tree root"),
        ))),
    ]
}

/// Reads through the actual shared admission rule used by native actions.
fn admission(store: &DataStore, key: &PageKey) -> (bool, bool) {
    fn at<T>(store: &DataStore, resource: Resource<T>) -> (bool, bool) {
        let admission = admit_resource(&resource, store.snapshot().key(), store.owner_serving());
        (
            admission.allows_actions(),
            admission.retained_value().is_some(),
        )
    }
    match key {
        PageKey::Orbit => at(store, store.orbit()),
        PageKey::Health => at(store, store.health()),
        PageKey::Package(package) => at(store, store.package(package)),
        PageKey::Browse(key @ BrowseKey::Tree(_)) => at(store, store.pages().browse(key)),
        _ => panic!("not a scheduled owner resource"),
    }
}

struct Rig {
    store: Entity<DataStore>,
    gate: OwnerGate,
    reads: ReadCounts,
    renewals: ReadGate,
    main: PageKey,
    notifications: Rc<RefCell<Vec<PageKey>>>,
    _events: Subscription,
}

impl Rig {
    fn new(cx: &mut TestAppContext, route: Route) -> Self {
        cx.executor().allow_parking();
        let reads = ReadCounts::default();
        let renewals = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_counts = Arc::clone(&reads);
        let reader_renewals = Arc::clone(&renewals);
        let pool = ReadPool::start(2, move |_| ScheduledOwnerReader {
            reads: Arc::clone(&reader_counts),
            renewals: Arc::clone(&reader_renewals),
        })
        .expect("read pool");
        let gate = OwnerGate::ready(root(), ServiceMode::Attached);
        let mut snapshot = AppSnapshot::empty(root());
        let mut session = snapshot.session().clone();
        session.route = route.clone();
        snapshot = snapshot.with_session(session);
        let main = RouteDependencies::new(&route, None).keys()[0].clone();
        let store = cx.update(|cx| {
            DataStore::install_with_owner(
                cx,
                Arc::new(snapshot),
                Some(pool),
                Some(gate.clone()),
                None,
            )
        });
        let notifications = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&notifications);
        let keys = routes()
            .iter()
            .flat_map(|route| RouteDependencies::new(route, None).into_keys())
            .collect::<Vec<_>>();
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
        Self {
            store,
            gate,
            reads,
            renewals,
            main,
            notifications,
            _events: events,
        }
    }

    fn until(&self, cx: &mut TestAppContext, done: impl Fn(&DataStore) -> bool) {
        crate::runtime::wait::until("the owner read schedule reached its next step", || {
            cx.run_until_parked();
            self.store.read_with(cx, |store, _| done(store))
        });
    }

    fn count(&self, key: &PageKey) -> usize {
        *self
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .unwrap_or(&0)
    }

    fn release(&self) {
        let (lock, released) = &*self.renewals;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        released.notify_all();
    }

    fn go(&self, cx: &mut TestAppContext, route: Route, overlay: Option<Overlay>) {
        self.store.update(cx, |store, cx| {
            let snapshot = store.snapshot();
            let mut session = snapshot.session().clone();
            session.route = route;
            session.overlay = overlay;
            store.admit_snapshot(Arc::new(snapshot.with_session(session)), cx);
        });
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.release();
    }
}

#[gpui::test]
fn same_root_owner_replacement_revokes_old_callbacks_and_newly_painted_owner_reads(
    cx: &mut TestAppContext,
) {
    for route in routes() {
        for observe_starting in [false, true] {
            let rig = Rig::new(cx, route.clone());
            let all = routes()
                .iter()
                .map(|route| RouteDependencies::new(route, None).keys()[0].clone())
                .collect::<Vec<_>>();
            for key in &all {
                rig.store.update(cx, |store, cx| {
                    store.ensure(key.clone(), cx);
                });
            }
            rig.until(cx, |store| {
                all.iter().all(|key| admission(store, key).0) && store.pool_activity().is_idle()
            });
            let captured = rig.store.read_with(cx, |store, _| {
                store.current_owner_attachment().expect("first attachment")
            });
            let stamp = rig.store.read_with(cx, |store, _| store.stamp(&rig.main));
            rig.notifications.borrow_mut().clear();
            let old = rig.gate.attached_ready_epoch().expect("attached owner");
            assert!(
                rig.gate
                    .attached_lost_at(old, "fixture endpoint closed".into())
            );
            assert!(rig.gate.restart());
            if observe_starting {
                rig.store.update(cx, DataStore::owner_starting);
                cx.run_until_parked();
                for key in &all {
                    assert!(rig.notifications.borrow().contains(key));
                }
                rig.store.read_with(cx, |store, _| {
                    assert_eq!(admission(store, &rig.main), (false, true))
                });
                let stamps = rig.store.read_with(cx, |store, _| {
                    all.iter().map(|key| store.stamp(key)).collect::<Vec<_>>()
                });
                rig.store.update(cx, DataStore::owner_starting);
                rig.store.read_with(cx, |store, _| {
                    assert_eq!(
                        all.iter().map(|key| store.stamp(key)).collect::<Vec<_>>(),
                        stamps
                    )
                });
            }
            rig.gate.publish(OwnerState::Ready {
                key: root(),
                mode: ServiceMode::Attached,
            });
            rig.store.read_with(cx, |store, _| {
                assert!(
                    !store.admits_owner_attachment(&captured),
                    "gate loss precedes the UI watcher"
                );
                assert!(!admission(store, &rig.main).0);
            });
            rig.store.update(cx, DataStore::owner_ready);
            cx.run_until_parked();
            rig.store.read_with(cx, |store, _| {
                assert!(
                    !store.admits_owner_attachment(&captured),
                    "a retained callback keeps its old attachment"
                );
                assert_ne!(store.stamp(&rig.main), stamp);
                let new = store
                    .current_owner_attachment()
                    .expect("replacement attachment");
                assert!(store.admits_owner_attachment(&new));
                for key in &all {
                    assert_eq!(
                        admission(store, key),
                        (false, true),
                        "new render cannot turn predecessor bytes into current actions"
                    );
                    assert!(store.pages().is_owner_read_revoked(key));
                    assert!(
                        rig.notifications.borrow().contains(key),
                        "every invalidated slot publishes Resource"
                    );
                }
            });
            for key in all.iter().filter(|key| **key != rig.main) {
                assert_eq!(rig.count(key), 1, "inactive reads stay lazy");
            }
            rig.release();
            rig.until(cx, |store| admission(store, &rig.main).0);
            assert_eq!(rig.count(&rig.main), 2);
            rig.store.read_with(cx, |store, _| {
                assert!(!store.admits_owner_attachment(&captured))
            });
            let other_route = routes()
                .into_iter()
                .find(|candidate| candidate != &route)
                .expect("other route");
            let other = RouteDependencies::new(&other_route, None).keys()[0].clone();
            assert_eq!(rig.count(&other), 1);
            rig.go(cx, other_route, None);
            rig.until(cx, |store| admission(store, &other).0);
            assert_eq!(
                rig.count(&other),
                2,
                "entering the route renews its revoked read"
            );
        }
    }
}

#[gpui::test]
fn cancelling_a_replacement_read_never_readmits_its_retained_predecessor(cx: &mut TestAppContext) {
    for route in routes() {
        let rig = Rig::new(cx, route.clone());
        rig.until(cx, |store| {
            admission(store, &rig.main).0 && store.pool_activity().is_idle()
        });
        let old = rig.gate.attached_ready_epoch().expect("attached owner");
        assert!(
            rig.gate
                .attached_lost_at(old, "fixture endpoint closed".into())
        );
        assert!(rig.gate.restart());
        rig.gate.publish(OwnerState::Ready {
            key: root(),
            mode: ServiceMode::Attached,
        });
        rig.store.update(cx, DataStore::owner_ready);
        rig.until(cx, |_| rig.count(&rig.main) == 2);
        rig.go(cx, route.clone(), Some(Overlay::Inbox));
        rig.store.read_with(cx, |store, _| {
            assert!(store.owner_serving());
            assert!(!store.is_loading(&rig.main));
            assert_eq!(
                store.pages().activity(&rig.main),
                crate::core::Activity::Waiting
            );
            assert_eq!(admission(store, &rig.main), (false, true));
            assert!(store.pages().is_owner_read_revoked(&rig.main));
        });
        rig.store.update(cx, |store, cx| {
            let before = store.stats().submitted;
            if store.focused().contains(&rig.main) {
                store.ensure(rig.main.clone(), cx);
            }
            assert_eq!(
                store.stats().submitted,
                before,
                "covered body cannot renew through retained chrome"
            );
            assert!(!store.is_loading(&rig.main));
        });
        rig.release();
        rig.until(cx, |store| store.pool_activity().is_idle());
        rig.store.read_with(cx, |store, _| {
            assert_eq!(admission(store, &rig.main), (false, true));
            assert!(store.pages().is_owner_read_revoked(&rig.main));
        });
        rig.go(cx, route, None);
        rig.until(cx, |store| admission(store, &rig.main).0);
        rig.store.read_with(cx, |store, _| {
            assert!(!store.pages().is_owner_read_revoked(&rig.main))
        });
    }
}

#[test]
fn even_an_equal_new_landing_is_required_to_restore_current_read_admission() {
    let package = crate::shell::tests::dossier().package;
    let key = PageKey::Package(package.clone());
    let mut pages = PageStore::default();
    let first = pages
        .begin(&key, root())
        .expect("page generation admission")
        .expect("first read");
    assert_eq!(
        pages.land(
            &key,
            first,
            Ok(PageValue::Package(crate::shell::tests::dossier()))
        ),
        Landing::Applied
    );
    let predecessor = pages
        .package(&package)
        .loaded_arc()
        .expect("first value")
        .clone();
    assert!(pages.revoke_owner_read(&key));
    let revoked = pages.package(&package);
    assert!(Arc::ptr_eq(
        revoked.loaded_arc().expect("retained allocation"),
        &predecessor
    ));
    assert!(matches!(
        admit_resource(&revoked, root(), true),
        ResourceAdmission::Retained { .. }
    ));
    let next = pages
        .begin(&key, root())
        .expect("page generation admission")
        .expect("same-root renewal");
    assert_eq!(
        pages.land(&key, next, Err(ReadFailure::Cancelled)),
        Landing::Applied
    );
    assert!(!admit_resource(&pages.package(&package), root(), true).allows_actions());
    let next = pages
        .begin(&key, root())
        .expect("page generation admission")
        .expect("renewal after cancellation");
    assert_eq!(
        pages.land(
            &key,
            next,
            Ok(PageValue::Package(crate::shell::tests::dossier()))
        ),
        Landing::Applied
    );
    assert!(admit_resource(&pages.package(&package), root(), true).allows_actions());
    assert!(!pages.is_owner_read_revoked(&key));
}
