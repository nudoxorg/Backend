//! Exact address/factory regressions using immutable producer fixtures and the
//! actual store admission path. These schedules do not prove native pixels or
//! acceptance by a live Cargo owner.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot, admit_resource};
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{CargoSourcePage, PageValue, SourceOrigin, SourceText};
use crate::model::ServiceMode;
use crate::navigation::{CargoBrowseContext, CargoSourcePath, CargoSourceRoute, PackageLane, PackageRoute};
use crate::runtime::owner::OwnerState;
use gpui::TestAppContext;
use std::path::Path;

fn root() -> VersionedRoot {
    VersionedRoot::synthetic(backend_library::view_state_root(&[("cargo-context".into(), "same-root".into())]), 27)
}

/// No subprocess or filesystem observation: the stable metadata witness is
/// an existing producer fixture, with an explicit requested member binding.
pub(crate) fn fixture(project: &LocalProjectId) -> (TreeModel, PackageRef, CargoBrowseContext) {
    use backend_advisory::{AdvisoryAuthority, normalize_package};
    use backend_library::browse::{ProjectTreeRequestBindingV1, build_tree, metadata_input_with_stable_source_witness};
    const METADATA: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"));
    let input = metadata_input_with_stable_source_witness(METADATA, "aarch64-apple-darwin", None, [7; 32]).expect("producer fixture");
    let authority = AdvisoryAuthority::new(1);
    let mut tree = build_tree(&input, &|name: &str, version: &str| {
        authority.observe(&normalize_package("cargo", name).expect("identity"), version, false, false, 0, false)
    });
    let binding = ProjectTreeRequestBindingV1::for_paths(Path::new(project.service_coordinate().expect("requested member")), &tree.root).expect("fixture binding");
    tree.request_binding = Some(binding);
    assert!(tree.has_admissible_shape());
    let package = PackageRef::from_reference(tree.package("serde", "1.0.219").and_then(|row| row.source_qualified_reference()).expect("exact observed package"));
    let context = CargoBrowseContext::from_binding_address(project.clone(), binding).expect("address only");
    (crate::runtime::browse_reads::tree_model(&tree), package, context)
}

fn package_route(package: &PackageRef, context: CargoBrowseContext) -> Route {
    Route::Package(PackageRoute { cargo: Some(context), project: None, package: PackageId::new(package.as_str()).expect("package"), lane: PackageLane::Overview, selected: None, at: None })
}

fn at(route: Route) -> AppSnapshot {
    let snapshot = AppSnapshot::empty(root());
    let mut session = snapshot.session().clone();
    session.route = route;
    snapshot.with_session(session)
}

fn land(store: &mut DataStore, key: &PageKey, value: PageValue) {
    let generation = store.pages.begin(key, store.snapshot().key()).expect("fresh selected read");
    assert_eq!(store.pages.land(key, generation, Ok(value)), Landing::Applied);
}

pub(crate) fn force_land(store: &mut DataStore, key: &PageKey, value: PageValue) {
    let generation = store.pages.begin_forced(key, store.snapshot().key()).expect("new selected fixture read");
    assert_eq!(store.pages.land(key, generation, Ok(value)), Landing::Applied);
}

#[test]
fn current_tree_admits_exact_package_without_semantic_dossier_or_history() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    assert_ne!(tree.root.as_ref(), requested.service_coordinate().expect("coordinate"));
    let route = package_route(&package, context.clone());
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested.clone()));
    let dossier_key = PageKey::Package(package.clone());
    assert_eq!(plan.keys(), &[dossier_key.clone(), tree_key.clone()]);
    let mut store = DataStore::new(Arc::new(at(route)), None);
    assert!(store.snapshot().session().back.is_empty());
    assert!(plan.current_cargo_package(&store, &package).is_none());
    land(&mut store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))));
    let generation = store.pages.begin(&dossier_key, root()).expect("dossier read");
    store.pages.land(&dossier_key, generation, Err(ReadFailure::Unavailable(UnavailableReason::OutOfScope, "exact package is not semantically indexed".into())));
    assert!(!admit_resource(&store.package(&package), root(), store.owner_serving()).allows_actions());
    assert!(plan.content_loaded(&store), "optional semantic absence cannot delay the Cargo page");
    assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Ready,
        "the shared destination phase excludes optional header faults");
    let receipt = plan.current_cargo_package(&store, &package).expect("current exact observation");
    assert_eq!(receipt.context(), &context);
    assert_eq!(receipt.package(), &package);
    assert_eq!(receipt.native_dependency(), (tree_key, store.stamp(&PageKey::Browse(BrowseKey::Tree(requested.clone())))));

    let sibling = PackageRef::parse(&format!("pkg:cargo/serde@1.0.219?cargo-authority={}", "b".repeat(64))).expect("other exact receipt");
    assert!(plan.current_cargo_package(&store, &sibling).is_none());
    let another_observed = tree.source_packages.iter().find(|other| *other != &package).expect("another exact observed Cargo package");
    assert!(plan.current_cargo_package(&store, another_observed).is_none(), "a Package-route factory is scoped to its exact viewed package");
    let changed = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(Path::new(requested.service_coordinate().expect("coordinate")), "/replacement/workspace").expect("other effective root");
    let changed = CargoBrowseContext::from_binding_address(requested, changed).expect("different saved address");
    assert!(RouteDependencies::new(&package_route(&package, changed), None).current_cargo_package(&store, &package).is_none(), "one requested Tree cannot supply another effective binding");
    let alias = LocalProjectId::new("/workspace/backend/other-member").expect("other request");
    let alias_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(Path::new(alias.service_coordinate().expect("coordinate")), &tree.root).expect("alias binding");
    let alias_context = CargoBrowseContext::from_binding_address(alias, alias_binding).expect("alias address");
    assert!(RouteDependencies::new(&package_route(&package, alias_context), None).current_cargo_package(&store, &package).is_none(), "resident unrelated Trees and equal package digests are not a selected observation");
}

#[test]
fn completed_tree_for_another_binding_is_terminal_instead_of_an_infinite_wait() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    let route = package_route(&package, context);
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested.clone()));
    let mut store = DataStore::new(Arc::new(at(route)), None);
    let mut changed = tree;
    changed.request_binding = Some(backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
        Path::new(requested.service_coordinate().expect("coordinate")), "/replacement/workspace",
    ).expect("changed binding"));
    land(&mut store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(changed))));
    assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Terminal);
    assert!(!plan.content_loaded(&store));
    assert!(plan.current_cargo_package(&store, &package).is_none());
}

#[test]
fn source_zoom_out_preserves_address_and_evicted_tree_requires_fresh_observation() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    let source = CargoSourceRoute::new(context.clone(), PackageId::new(package.as_str()).expect("package"), CargoSourcePath::new("src/lib.rs").expect("file"), Some(7)).expect("bound source");
    let plan = RouteDependencies::new(&Route::CargoSource(source.clone()), None);
    let pair = plan.cargo().expect("paired reads");
    let source_key = PageKey::CargoSource(pair.file.clone());
    let mut store = DataStore::new(Arc::new(at(Route::CargoSource(source.clone()))), None);
    land(&mut store, &source_key, PageValue::CargoSource(CargoSourcePage {
        package: package.clone(), request_binding: context.request_binding(), file: source.file.clone(),
        source: SourceText::new("pub fn observed() {}\n".into(), 1, SourceOrigin::LocalFile, true).expect("unindexed bytes"),
        content_digest: [7; 32], source_revision: [9; 32],
    }));
    assert!(plan.content_loaded(&store), "inventory may arrive later");
    assert_eq!(plan.current_cargo_package(&store, &package).expect("current file projection").context(), &context);
    let parent = Route::Package(source.package_route());
    assert_eq!(source.package_route().cargo.as_ref(), Some(&context));
    let parent_plan = RouteDependencies::new(&parent, None);
    assert!(parent_plan.current_cargo_package(&store, &package).is_none(), "source history is never searched as a package-page authority");
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested));
    land(&mut store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))));
    assert!(parent_plan.current_cargo_package(&store, &package).is_some());
    // The real bounded Browse store evicts the selected Tree. Other current
    // Trees may contain the same exact package, but cannot lend their binding.
    for i in 0..4 {
        let project = LocalProjectId::new(&format!("/workspace/backend/alias-{i}")).expect("alias");
        let mut other = tree.clone();
        other.request_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(Path::new(project.service_coordinate().expect("coordinate")), &other.root);
        land(&mut store, &PageKey::Browse(BrowseKey::Tree(project)), PageValue::Browse(BrowseValue::Tree(Arc::new(other))));
    }
    assert!(parent_plan.current_cargo_package(&store, &package).is_none());
    assert!(!parent_plan.content_loaded(&store));
}

#[gpui::test]
fn coalesced_same_root_owner_replacement_revokes_captured_and_newly_projected_receipts(cx: &mut TestAppContext) {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    let route = package_route(&package, context);
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested));
    let gate = OwnerGate::ready(root(), ServiceMode::Embedded);
    let store = cx.update(|cx| cx.new(|cx| {
        // No focused keys or workers: this exercises lazy admission revocation,
        // not an eager reread and not live-owner acceptance.
        let mut store = DataStore::new(Arc::new(at(route)), None);
        store.owner = OwnerLink::behind(gate.clone());
        store.owner_ready(cx);
        land(&mut store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))));
        store
    }));
    let (attachment, receipt) = store.read_with(cx, |store, _| {
        (store.current_owner_attachment().expect("first attachment"), plan.current_cargo_package(store, &package).expect("current receipt").native_dependency())
    });
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready { key: root(), mode: ServiceMode::Embedded });
    store.read_with(cx, |store, _| {
        assert!(!store.admits_owner_attachment(&attachment));
        assert!(plan.current_cargo_package(store, &package).is_none(), "the UI need not observe Starting to deny the replaced attachment");
    });
    store.update(cx, |store, cx| store.owner_ready(cx));
    store.read_with(cx, |store, _| {
        assert!(store.pages().browse(match &tree_key { PageKey::Browse(key) => key, _ => unreachable!() }).loaded_value().is_some());
        assert!(plan.current_cargo_package(store, &package).is_none(), "newly painted offers cannot re-admit retained Tree bytes through the new token");
        assert!(!plan.admits_native_stamp(store, root(), &receipt));
        assert_eq!(store.stats().submitted, 0, "nonvisible predecessor reads stay lazy");
    });
    store.update(cx, |store, _| land(store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree)))));
    store.read_with(cx, |store, _| {
        assert!(plan.current_cargo_package(store, &package).is_some());
        assert!(!store.admits_owner_attachment(&attachment));
        assert!(!plan.admits_native_stamp(store, root(), &receipt));
    });
}

#[gpui::test]
fn visible_symbol_titlebar_package_renews_without_becoming_page_readiness(cx: &mut TestAppContext) {
    use crate::navigation::View;
    use crate::runtime::reads::{PageReader, ReadContext};
    use std::sync::{Mutex, PoisonError};
    type Counts = Arc<Mutex<BTreeMap<PageKey, usize>>>;
    struct HeaderReader(Counts);
    impl PageReader for HeaderReader {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            let key = match request {
                ReadRequest::Symbol(symbol) => PageKey::Symbol(symbol.clone()),
                ReadRequest::Source(symbol) => PageKey::Source(symbol.clone()),
                ReadRequest::Package(package) => PageKey::Package(package.clone()),
                _ => return Err(ReadFailure::Unavailable(UnavailableReason::Unsupported, "header fixture".into())),
            };
            let count = { let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner); let count = counts.entry(key.clone()).or_default(); *count += 1; *count };
            if matches!(key, PageKey::Package(_)) && count == 1 {
                return Err(ReadFailure::Unavailable(UnavailableReason::OutOfScope, "optional semantic dossier absent".into()));
            }
            crate::shell::tests::Fixture.read(request, context)
        }
    }
    cx.executor().allow_parking();
    let route = crate::shell::kit::symbol_view_route(crate::shell::tests::PACKAGE, &crate::shell::tests::symbol("RelationLabel"), View::Code, None).expect("code route");
    let plan = RouteDependencies::new(&route, None);
    let package = route_package(&route).expect("visible titlebar package");
    let header = PageKey::Package(package.clone());
    assert!(plan.keys().contains(&header));
    assert_eq!(RouteDependencies::chrome_keys(&route), vec![PageKey::Symbol(route_symbol(&route).expect("symbol")), header.clone()]);
    let counts = Counts::default();
    let workers = Arc::clone(&counts);
    let pool = ReadPool::start(2, move |_| HeaderReader(Arc::clone(&workers))).expect("fixture pool");
    let gate = OwnerGate::ready(root(), ServiceMode::Embedded);
    let store = cx.update(|cx| DataStore::install_with_owner(cx, Arc::new(at(route)), Some(pool), Some(gate.clone()), None));
    crate::runtime::wait::until("current code and explicitly absent titlebar dossier", || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| plan.content_loaded(store) && matches!(store.package(&package).terminal(), crate::core::ResourceTerminal::Unavailable(_)))
    });
    assert_eq!(counts.lock().unwrap_or_else(PoisonError::into_inner).get(&header), Some(&1));
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready { key: root(), mode: ServiceMode::Embedded });
    store.update(cx, |store, cx| store.owner_ready(cx));
    crate::runtime::wait::until("all visible reads renew through the unified plan", || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| plan.content_loaded(store) && admit_resource(&store.package(&package), root(), store.owner_serving()).allows_actions())
    });
    let counts = counts.lock().unwrap_or_else(PoisonError::into_inner);
    for key in plan.keys() { assert_eq!(counts.get(key), Some(&2), "visible key {key:?} renewed lazily through the route plan"); }
}
