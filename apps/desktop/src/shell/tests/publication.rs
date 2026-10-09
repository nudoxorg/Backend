//! Mounted regions renew their own visible observations at unchanged authority.

use super::{Fixture, PACKAGE, Rig, package, page, registry_dossier, rig_with_reads};
use crate::model::pages::{
    DeclRef, DocFragment, GapReason, IndexedPackage, Known, OutlinePosition, PackageRef, PageKey,
    PageValue, ReadFailure, Readiness, SignatureText, SymbolRef,
};
use crate::navigation::{Coordinate, Intent, PackageLane, PackageRoute, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use backend_library::DeclarationKind;
use gpui::{AppContext as _, TestAppContext};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ERROR_TYPE: &str = "/fixture/present::errors.go:1::ErrorHandling";
const CONTINUE: &str = "/fixture/present::errors.go:2::ContinueOnError";

#[derive(Default)]
pub(crate) struct Observations {
    pub(crate) published: AtomicBool,
    cancel_next_companion: AtomicBool,
    fail_next_companion: AtomicBool,
    reads: Mutex<BTreeMap<PageKey, usize>>,
}

impl Observations {
    fn count(&self, key: &PageKey) -> usize {
        self.reads.lock().expect("read counts").get(key).copied().unwrap_or(0)
    }

    pub(crate) fn capture_read_counts(&self) -> BTreeMap<String, usize> {
        self.reads.lock().expect("capture read counts").iter()
            .map(|(key, count)| (format!("{key:?}"), *count)).collect()
    }
}

struct PublicationFixture(Arc<Observations>);

pub(crate) fn capture_pool() -> Result<(ReadPool, Arc<Observations>), String> {
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    ReadPool::start(2, move |_| PublicationFixture(Arc::clone(&reads)))
        .map(|pool| (pool, observed)).map_err(|error| error.to_string())
}

pub(crate) fn capture_route() -> Route { go_route() }

impl PageReader for PublicationFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let key = match request {
            ReadRequest::Orbit => PageKey::Orbit,
            ReadRequest::Package(package) => PageKey::Package(package.clone()),
            ReadRequest::Symbol(symbol) => PageKey::Symbol(symbol.clone()),
            _ => return Fixture.read(request, context),
        };
        *self.0.reads.lock().expect("read counts").entry(key).or_default() += 1;
        if matches!(request, ReadRequest::Symbol(symbol) if symbol.as_str() == CONTINUE) {
            if self.0.cancel_next_companion.swap(false, Ordering::AcqRel) {
                return Err(ReadFailure::Cancelled);
            }
            if self.0.fail_next_companion.swap(false, Ordering::AcqRel) {
                return Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol, "controlled companion failure")));
            }
        }
        let published = self.0.published.load(Ordering::Acquire);
        match request {
            ReadRequest::Orbit => {
                let PageValue::Orbit(mut model) = Fixture.read(request, context)? else {
                    unreachable!("fixture Orbit family")
                };
                model.indexed = Known::Known(Arc::from([IndexedPackage {
                    package: package(),
                    name: Arc::from(if published { "Published library" } else { "Earlier library" }),
                    readiness: Readiness::Ready, verified_registry_release: None,
                }]));
                Ok(PageValue::Orbit(model))
            }
            ReadRequest::Package(package) if !package.is_local() => {
                Ok(PageValue::Package(registry_dossier(package)))
            }
            ReadRequest::Symbol(symbol) if [ERROR_TYPE, CONTINUE].contains(&symbol.as_str()) => {
                let is_type = symbol.as_str() == ERROR_TYPE;
                let kind = if is_type { DeclarationKind::Type } else { DeclarationKind::Constant };
                let mut value = page(symbol.identity().name());
                value.identity = DeclRef::from_label(symbol.as_str(), None, Some(kind), None).expect("Go declaration");
                value.members = Known::unknown(GapReason::NoSemanticPublication, "companion fixture");
                value.signature = Known::Known(SignatureText {
                    text: Arc::from(if is_type { "type ErrorHandling int" } else { "const ContinueOnError ErrorHandling = iota" }),
                    tokens: Arc::from([]),
                    name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
                });
                value.docs = Arc::from([DocFragment::Text(Arc::from(if is_type {
                    "Controls error handling."
                } else if published {
                    "After publication."
                } else {
                    "Before publication."
                }))]);
                if is_type {
                    value.outline = Known::Known(OutlinePosition {
                        ancestors: Arc::from([]),
                        siblings: Arc::from([
                            value.identity.clone(),
                            DeclRef::from_label(CONTINUE, None, Some(DeclarationKind::Constant), None).expect("Go constant"),
                        ]),
                        index: Some(0),
                    });
                }
                Ok(PageValue::Symbol(value))
            }
            _ => Fixture.read(request, context),
        }
    }
}

fn open(cx: &mut TestAppContext, route: Route) -> (Rig, Arc<Observations>) {
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    let pool = ReadPool::start(2, move |_| PublicationFixture(Arc::clone(&reads))).expect("publication pool");
    (rig_with_reads(cx, Some(route), 1440.0, 900.0, pool), observed)
}

fn package_route() -> Route {
    Route::Package(PackageRoute {
        cargo: None,
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package route"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    })
}

fn go_route() -> Route {
    Route::Symbol(SymbolRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("Go package"),
        id: Coordinate::new(ERROR_TYPE).expect("Go coordinate"),
        at: None,
        view: View::Page,
        line: None,
        selected: None,
    })
}

// Companion summaries are wrapped text painted by the anatomy component;
// the Reader's `said` list only records the host's own synchronous words.
fn companion_is_painted(rig: &mut Rig, words: &str) -> bool {
    // Read the already committed frame. In particular, publication must wake
    // the mounted reader itself; a forced repaint here would hide that defect.
    let painted = rig.cx.update(|window, _| {
        window.painted_texts().iter().any(|text| text.text.as_ref() == words)
            && window.debug_a11y_tree_json().is_some_and(|tree| {
                let tree: serde_json::Value = serde_json::from_str(&tree).expect("native companion JSON");
                tree["nodes"].as_object().expect("native companion nodes").values()
                    .any(|node| node["aria"]["role"] == "Label" && node["aria"]["label"] == words)
            })
    });
    if !painted {
        super::native_boundary_evidence(rig, "go-companion-native-frame");
        let companion = SymbolRef::new(CONTINUE).expect("companion address");
        rig.graph.store.read_with(rig.cx, |store, _| {
            let resource = store.symbol(&companion);
            eprintln!("nudox-native-boundary companion-resource={resource:?}");
        });
    }
    painted
}

#[gpui::test]
fn mounted_shelf_refreshes_its_unfocused_orbit_and_leaves_hidden_packages_lazy(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, package_route());
    let hidden = PackageRef::parse("pkg:cargo/hidden@1.0.0").expect("hidden release");
    let hidden_key = PageKey::Package(hidden.clone());
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(hidden_key.clone(), cx); });
    rig.settle();
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&PageKey::Orbit));
        assert!(!store.focused().contains(&hidden_key));
        store.snapshot()
    });
    let shelf_renders = rig.counts().shelf;
    let orbit_reads = observed.count(&PageKey::Orbit);
    let hidden_reads = observed.count(&hidden_key);
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([hidden]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&PageKey::Orbit), orbit_reads + 1, "the mounted Shelf renews its own key once");
    assert_eq!(observed.count(&hidden_key), hidden_reads, "resident hidden pages cost no eager read");
    assert!(rig.counts().shelf > shelf_renders, "the real Shelf observed the renewed data");
    rig.graph.store.read_with(rig.cx, |store, _| {
        let now = store.snapshot();
        assert!(before.key().same_authority(now.key()));
        assert_eq!(before.route(), now.route());
        assert_eq!(before.session().back, now.session().back);
        assert_eq!(before.session().forward, now.session().forward);
        assert!(store.observation_revoked(&hidden_key));
        assert!(!store.observation_revoked(&PageKey::Orbit));
        assert_eq!(store.orbit().loaded_value().expect("fresh Orbit").indexed.known()
            .expect("indexed packages").first().expect("indexed package").name.as_ref(), "Published library");
    });
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(PageKey::Orbit, cx); });
    rig.settle();
    assert_eq!(observed.count(&PageKey::Orbit), orbit_reads + 1, "the repaired observation remains current");
}

#[gpui::test]
fn mounted_go_companion_refreshes_without_navigation_and_stays_lazy_after_leaving(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, go_route());
    // Establish native observation once before the controlled publication.
    // The subsequent publication check pumps only executor/timer events.
    rig.cx.update(|window, cx| { cx.set_global(gpui::TextTrace); window.set_a11y_forced(true); });
    rig.repaint();
    rig.settle();
    let companion = PageKey::Symbol(SymbolRef::new(CONTINUE).expect("companion key"));
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&companion));
        store.snapshot()
    });
    let companion_reads = observed.count(&companion);
    assert!(companion_reads > 0, "the mounted Go page requested its actual companion");
    assert!(companion_is_painted(&mut rig, "before publication"));
    let frame_before = rig.cx.update(|window, _| window.a11y_frame_number());
    let renders_before = rig.counts().reader;
    rig.cx.run_until_parked();
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame_before,
        "the idle observer does not force a native frame");
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    // App::flush_effects draws only invalidated windows in test-support. Do
    // not call Rig::settle/draw/refresh: those render an otherwise clean root.
    let deadline = Instant::now() + rig.patience;
    loop {
        rig.cx.run_until_parked();
        if rig.graph.store.read_with(rig.cx, |store, _| store.pool_activity().is_idle()) {
            break;
        }
        assert!(Instant::now() < deadline, "the publication companion read never completed");
        rig.cx.executor().advance_clock(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(observed.count(&companion), companion_reads + 1, "the companion renews once at the same route and authority");
    assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame_before,
        "publication invalidation committed a native frame without a test draw");
    assert!(rig.counts().reader > renders_before,
        "the mounted Reader observed publication without a test draw");
    assert!(companion_is_painted(&mut rig, "after publication"), "fresh companion words reach the mounted Reader");
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(before.key().same_authority(store.snapshot().key()));
        assert_eq!(before.route(), store.snapshot().route());
        assert!(!store.observation_revoked(&companion));
    });
    rig.go(Intent::Navigate(package_route()));
    let settled_reads = observed.count(&companion);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&companion), settled_reads, "an old subscription cannot eagerly renew a hidden companion");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.observation_revoked(&companion)));
    rig.go(Intent::Navigate(go_route()));
    assert_eq!(observed.count(&companion), settled_reads + 1, "visiting the same companion list renews its revoked observations");
}


/// Ordinary executor/producer work only: no test draw, refresh, or notify.
fn passive_work(rig: &mut Rig) {
    let deadline = Instant::now() + rig.patience;
    loop {
        rig.cx.run_until_parked();
        let idle = rig.graph.store.read_with(rig.cx, |store, _| store.pool_activity().is_idle())
            && rig.graph.root.read_with(rig.cx, |root, _| !root.has_pending_work());
        if idle { return; }
        assert!(Instant::now() < deadline, "native auxiliary read never became idle");
        rig.cx.executor().advance_clock(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[gpui::test]
fn mounted_go_companion_renews_at_new_root_and_reopens_without_hidden_reads(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, go_route());
    rig.cx.update(|window, cx| {
        facet::probe::disable(cx); cx.set_global(gpui::TextTrace); window.set_a11y_forced(true);
    });
    rig.repaint();
    rig.settle();
    let symbol = SymbolRef::new(CONTINUE).expect("companion key");
    let key = PageKey::Symbol(symbol.clone());
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&key)); store.snapshot()
    });
    assert!(companion_is_painted(&mut rig, "before publication"));
    let reads = observed.count(&key);
    let renders = rig.counts().reader;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    rig.cx.run_until_parked();
    assert_eq!(rig.counts().reader, renders, "idle does not render the Reader");
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame,
        "idle does not draw a native frame");
    observed.published.store(true, Ordering::Release);
    rig.graph.root.update(rig.cx, |root, cx| root.refresh_root(cx));
    passive_work(&mut rig);
    let current = rig.graph.store.read_with(rig.cx, |store, _| {
        let root = store.snapshot().key();
        assert!(!root.same_authority(before.key()), "the real root reply advances authority");
        assert_eq!(store.snapshot().route(), before.route());
        assert_eq!(store.snapshot().session().reading.current.id, before.session().reading.current.id);
        assert!(store.symbol(&symbol).value_root().is_some_and(|value| value.same_authority(root)),
            "the companion was admitted at the current producer basis");
        root
    });
    assert_eq!(observed.count(&key), reads + 1, "the visible companion renews exactly once");
    assert!(rig.counts().reader > renders);
    assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame);
    assert!(companion_is_painted(&mut rig, "after publication"),
        "current companion words reach native paint and accessibility without a test draw");
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), reads + 1, "accepted current bytes remain idempotent");
    rig.go(Intent::Navigate(package_route()));
    let hidden = observed.count(&key);
    observed.published.store(false, Ordering::Release);
    rig.graph.root.update(rig.cx, |root, cx| root.refresh_root(cx));
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), hidden, "a hidden companion does not renew on root change");
    assert!(!rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()).same_authority(current));
    rig.go(Intent::Navigate(go_route()));
    assert_eq!(observed.count(&key), hidden + 1, "reopening renews the exact current dependency once");
    assert!(companion_is_painted(&mut rig, "before publication"));
    eprintln!("native Go root/reopen: initial_reads={reads} reopened_reads={}", observed.count(&key));
}

#[gpui::test]
fn mounted_go_cancelled_companion_renews_but_terminal_failure_waits_for_retry(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, go_route());
    rig.cx.update(|window, cx| {
        facet::probe::disable(cx); cx.set_global(gpui::TextTrace); window.set_a11y_forced(true);
    });
    rig.repaint();
    rig.settle();
    let symbol = SymbolRef::new(CONTINUE).expect("companion key");
    let key = PageKey::Symbol(symbol.clone());
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    let route = rig.route();
    let reads = observed.count(&key);
    let renders = rig.counts().reader;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    rig.cx.run_until_parked();
    assert_eq!(rig.counts().reader, renders);
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
    observed.cancel_next_companion.store(true, Ordering::Release);
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| store.retry(key.clone(), cx));
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), reads + 2,
        "one actual cancellation gets one replacement for the still-visible dependency");
    assert!(companion_is_painted(&mut rig, "after publication"));
    assert!(rig.counts().reader > renders);
    assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame);
    observed.fail_next_companion.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| store.retry(key.clone(), cx));
    passive_work(&mut rig);
    let failed = observed.count(&key);
    assert_eq!(failed, reads + 3, "the controlled terminal failure executes once");
    assert!(rig.graph.store.read_with(rig.cx, |store, _|
        matches!(store.symbol(&symbol).terminal(), crate::core::ResourceTerminal::Fault(_))));
    let stamp = rig.graph.store.read_with(rig.cx, |store, _| store.stamp(&key));
    for _ in 0..3 { passive_work(&mut rig); }
    assert_eq!(observed.count(&key), failed, "passive observation does not loop a terminal failure");
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stamp(&key)), stamp);
    observed.published.store(false, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| store.retry(key.clone(), cx));
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), failed + 1, "explicit Retry admits one fresh generation");
    assert!(companion_is_painted(&mut rig, "before publication"));
    assert_eq!(rig.route(), route);
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
    eprintln!("native Go cancellation/failure: initial_reads={reads} final_reads={}", observed.count(&key));
}


#[gpui::test]
fn mounted_go_covered_by_native_ask_keeps_auxiliary_reads_lazy_until_escape(cx: &mut TestAppContext) {
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    let pool = ReadPool::start(2, move |_| PublicationFixture(Arc::clone(&reads))).expect("publication pool");
    // 720px at normal text retains a readable preview; 640px is a sheet.
    let mut rig = rig_with_reads(cx, Some(go_route()), 640.0, 900.0, pool);
    rig.cx.update(|window, cx| {
        facet::probe::disable(cx); cx.set_global(gpui::TextTrace); window.set_a11y_forced(true);
    });
    rig.repaint();
    rig.settle();
    assert!(companion_is_painted(&mut rig, "before publication"));
    let symbol = SymbolRef::new(CONTINUE).expect("companion key");
    let key = PageKey::Symbol(symbol.clone());
    let route = rig.route();
    rig.keys("secondary-k");
    rig.cx.simulate_input("RelationLabel");
    rig.settle();
    let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
    assert!(rig.cx.update(|window, cx| ask.read(cx).owns_focus(window, cx)),
        "the mounted native query owns keyboard focus");
    assert!(!rig.cx.update(|window, _| window.painted_texts().iter()
        .any(|text| text.text.as_ref() == "before publication")),
        "the real narrow Ask sheet fully covers the Go page");
    let hidden = observed.count(&key);
    observed.published.store(true, Ordering::Release);
    rig.graph.root.update(rig.cx, |root, cx| root.refresh_root(cx));
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), hidden, "a fully covered auxiliary read stays lazy");
    assert_eq!(rig.route(), route);
    rig.keys("escape");
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), hidden + 1, "native Escape reveals and renews the dependency once");
    assert!(companion_is_painted(&mut rig, "after publication"));
    assert!(!rig.cx.update(|window, cx| ask.read(cx).owns_focus(window, cx)),
        "the covered editor releases native keyboard focus");
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.symbol(&symbol).value_root().is_some_and(|value|
            value.same_authority(store.snapshot().key())), "revealed bytes use the current basis");
    });
    eprintln!("native Go Ask cover: hidden_reads={hidden} revealed_reads={}", observed.count(&key));
}

#[gpui::test]
fn mounted_go_readable_native_ask_preview_renews_its_auxiliary_read(cx: &mut TestAppContext) {
    // Ask owns native accessibility and input while the painted preview is inert.
    let preview_has_words = |rig: &mut Rig, words: &str| rig.cx.update(|window, _|
        window.painted_texts().iter().any(|text| text.text.as_ref() == words));
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    let pool = ReadPool::start(2, move |_| PublicationFixture(Arc::clone(&reads))).expect("publication pool");
    let mut rig = rig_with_reads(cx, Some(go_route()), 720.0, 900.0, pool);
    rig.cx.update(|window, cx| {
        facet::probe::disable(cx); cx.set_global(gpui::TextTrace); window.set_a11y_forced(true);
    });
    rig.repaint();
    rig.settle();
    assert!(companion_is_painted(&mut rig, "before publication"));
    rig.keys("secondary-k");
    rig.cx.simulate_input("RelationLabel");
    rig.settle();
    let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
    assert!(rig.cx.update(|window, cx| ask.read(cx).owns_focus(window, cx)));
    assert!(preview_has_words(&mut rig, "before publication"), "720px leaves a real readable Go preview");
    let symbol = SymbolRef::new(CONTINUE).expect("companion key");
    let key = PageKey::Symbol(symbol.clone());
    let reads = observed.count(&key);
    let route = rig.route();
    let renders = rig.counts().reader;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    rig.cx.run_until_parked();
    assert_eq!(rig.counts().reader, renders, "idle does not render the preview");
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame, "idle does not draw a preview frame");
    observed.published.store(true, Ordering::Release);
    rig.graph.root.update(rig.cx, |root, cx| root.refresh_root(cx));
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), reads + 1, "the readable preview renews its exact companion once");
    assert!(rig.counts().reader > renders);
    assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame);
    assert!(preview_has_words(&mut rig, "after publication"), "the current preview reaches native paint without a test draw");
    assert!(rig.cx.update(|window, cx| ask.read(cx).owns_focus(window, cx)), "publication keeps native query focus");
    assert_eq!(rig.route(), route);
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.symbol(&symbol).value_root().is_some_and(|value|
            value.same_authority(store.snapshot().key())), "preview bytes use the current root");
    });
    rig.keys("escape");
    passive_work(&mut rig);
    assert_eq!(observed.count(&key), reads + 1, "Escape reuses the already current readable preview");
    assert!(companion_is_painted(&mut rig, "after publication"), "Escape restores current Go native accessibility and paint");
    assert!(!rig.cx.update(|window, cx| ask.read(cx).owns_focus(window, cx)));
    eprintln!("native Go Ask preview: before_reads={reads} current_reads={}", observed.count(&key));
}
