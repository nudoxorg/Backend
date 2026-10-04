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

const ERROR_TYPE: &str = "/fixture/present::errors.go:1::ErrorHandling";
const CONTINUE: &str = "/fixture/present::errors.go:2::ContinueOnError";

#[derive(Default)]
struct Observations {
    published: AtomicBool,
    reads: Mutex<BTreeMap<PageKey, usize>>,
}

impl Observations {
    fn count(&self, key: &PageKey) -> usize {
        self.reads.lock().expect("read counts").get(key).copied().unwrap_or(0)
    }
}

struct PublicationFixture(Arc<Observations>);

impl PageReader for PublicationFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let key = match request {
            ReadRequest::Orbit => PageKey::Orbit,
            ReadRequest::Package(package) => PageKey::Package(package.clone()),
            ReadRequest::Symbol(symbol) => PageKey::Symbol(symbol.clone()),
            _ => return Fixture.read(request, context),
        };
        *self.0.reads.lock().expect("read counts").entry(key).or_default() += 1;
        let published = self.0.published.load(Ordering::Acquire);
        match request {
            ReadRequest::Orbit => {
                let PageValue::Orbit(mut model) = Fixture.read(request, context)? else {
                    unreachable!("fixture Orbit family")
                };
                model.indexed = Known::Known(Arc::from([IndexedPackage {
                    package: package(),
                    name: Arc::from(if published { "Published library" } else { "Earlier library" }),
                    readiness: Readiness::Ready,
                    verified_registry_release: None,
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
    let companion = PageKey::Symbol(SymbolRef::new(CONTINUE).expect("companion key"));
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&companion));
        store.snapshot()
    });
    let companion_reads = observed.count(&companion);
    assert!(companion_reads > 0, "the mounted Go page requested its actual companion");
    assert!(rig.said().iter().any(|text| text.contains("Before publication")));
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&companion), companion_reads + 1, "the companion renews once at the same route and authority");
    assert!(rig.said().iter().any(|text| text.contains("After publication")), "fresh companion words reach the mounted Reader");
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
