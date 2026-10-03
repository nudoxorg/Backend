//! Exact address/factory regressions using immutable producer fixtures and the
//! actual store admission path. These schedules do not prove native pixels or
//! acceptance by a live Cargo owner.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, PackageId, VersionedRoot, admit_resource};
use crate::model::ServiceMode;
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{CargoSourcePage, GapReason, Known, LineSpan, PageValue, SourceOrigin, SourceText, SourceView};
use crate::navigation::{
    CargoBrowseContext, CargoSourcePath, CargoSourceRoute, PackageLane, PackageRoute,
};
use crate::runtime::owner::OwnerState;
use gpui::TestAppContext;
use std::path::Path;

fn root() -> VersionedRoot {
    VersionedRoot::synthetic(
        backend_library::view_state_root(&[("cargo-context".into(), "same-root".into())]),
        27,
    )
}

/// No subprocess or filesystem observation: the stable metadata witness is
/// an existing producer fixture, with an explicit requested member binding.
pub(crate) fn fixture(project: &LocalProjectId) -> (TreeModel, PackageRef, CargoBrowseContext) {
    use backend_advisory::{AdvisoryAuthority, normalize_package};
    use backend_library::browse::{
        ProjectTreeRequestBindingV1, build_tree, metadata_input_with_stable_source_witness,
    };
    const METADATA: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"
    ));
    let input =
        metadata_input_with_stable_source_witness(METADATA, "aarch64-apple-darwin", None, [7; 32])
            .expect("producer fixture");
    let authority = AdvisoryAuthority::new(1);
    let mut tree = build_tree(&input, &|name, version| {
        authority.observe(
            &normalize_package("cargo", name).expect("identity"),
            version,
            false,
            false,
            0,
            false,
        )
    });
    let binding = ProjectTreeRequestBindingV1::for_paths(
        Path::new(project.service_coordinate().expect("requested member")),
        &tree.root,
    )
    .expect("fixture binding");
    tree.request_binding = Some(binding);
    assert!(tree.has_admissible_shape());
    let package = PackageRef::from_reference(
        tree.package("serde", "1.0.219")
            .and_then(|row| row.source_qualified_reference())
            .expect("exact observed package"),
    );
    let context =
        CargoBrowseContext::from_binding_address(project.clone(), binding).expect("address only");
    (
        crate::runtime::browse_reads::tree_model(&tree),
        package,
        context,
    )
}

fn package_route(package: &PackageRef, context: CargoBrowseContext) -> Route {
    Route::Package(PackageRoute {
        cargo: Some(context),
        project: None,
        package: PackageId::new(package.as_str()).expect("package"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    })
}

fn at(route: Route) -> AppSnapshot {
    let snapshot = AppSnapshot::empty(root());
    let mut session = snapshot.session().clone();
    session.route = route;
    snapshot.with_session(session)
}

fn land(store: &mut DataStore, key: &PageKey, value: PageValue) {
    let generation = store
        .pages
        .begin(key, store.snapshot().key())
        .expect("fresh selected read");
    assert_eq!(
        store.pages.land(key, generation, Ok(value)),
        Landing::Applied
    );
}

pub(crate) fn force_land(store: &mut DataStore, key: &PageKey, value: PageValue) {
    let generation = store
        .pages
        .begin_forced(key, store.snapshot().key())
        .expect("new selected fixture read");
    assert_eq!(
        store.pages.land(key, generation, Ok(value)),
        Landing::Applied
    );
}

#[test]
fn current_tree_admits_exact_package_without_semantic_dossier_or_history() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    assert_ne!(
        tree.root.as_ref(),
        requested.service_coordinate().expect("coordinate")
    );
    let route = package_route(&package, context.clone());
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested.clone()));
    let dossier_key = PageKey::Package(package.clone());
    assert_eq!(
        plan.keys(),
        &[
            dossier_key.clone(),
            tree_key.clone(),
            PageKey::Browse(BrowseKey::CargoReadme(
                crate::model::browse::CargoReadmeKey {
                    context: context.clone(),
                    package: package.clone()
                }
            ))
        ]
    );
    let mut store = DataStore::new(Arc::new(at(route)), None);
    assert!(store.snapshot().session().back.is_empty());
    assert!(plan.current_cargo_package(&store, &package).is_none());
    land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))),
    );
    let generation = store
        .pages
        .begin(&dossier_key, root())
        .expect("dossier read");
    store.pages.land(
        &dossier_key,
        generation,
        Err(ReadFailure::Unavailable(
            UnavailableReason::OutOfScope,
            "exact package is not semantically indexed".into(),
        )),
    );
    assert!(
        !admit_resource(&store.package(&package), root(), store.owner_serving()).allows_actions()
    );
    assert!(
        !plan.content_loaded(&store),
        "the independent selected README has not answered yet"
    );
    let readme_key = PageKey::Browse(BrowseKey::CargoReadme(
        crate::model::browse::CargoReadmeKey {
            context: context.clone(),
            package: package.clone(),
        },
    ));
    land(
        &mut store,
        &readme_key,
        PageValue::Browse(BrowseValue::CargoReadme(Arc::new(
            crate::model::browse::CargoReadmeModel {
                package: package.clone(),
                request_binding: context.request_binding(),
                source_revision: [7; 32],
                state: crate::model::browse::CargoReadmeState::Absent(
                    backend_library::CargoPackageReadmeAbsenceV1::NoCargoDefault,
                ),
            },
        ))),
    );
    assert!(
        plan.content_loaded(&store),
        "optional semantic absence cannot delay current Tree and README observations"
    );
    assert_eq!(
        plan.content_phase(&store),
        crate::core::ReadPhase::Ready,
        "the shared destination phase excludes optional header faults"
    );
    let receipt = plan
        .current_cargo_package(&store, &package)
        .expect("current exact observation");
    assert_eq!(receipt.context(), &context);
    assert_eq!(receipt.package(), &package);
    assert_eq!(
        receipt.native_dependency(),
        (
            tree_key,
            store.stamp(&PageKey::Browse(BrowseKey::Tree(requested.clone())))
        )
    );

    let sibling = PackageRef::parse(&format!(
        "pkg:cargo/serde@1.0.219?cargo-authority={}",
        "b".repeat(64)
    ))
    .expect("other exact receipt");
    assert!(plan.current_cargo_package(&store, &sibling).is_none());
    let another_observed = tree
        .source_packages
        .iter()
        .find(|other| *other != &package)
        .expect("another exact observed Cargo package");
    assert!(
        plan.current_cargo_package(&store, another_observed)
            .is_none(),
        "a Package-route factory is scoped to its exact viewed package"
    );
    let changed = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
        Path::new(requested.service_coordinate().expect("coordinate")),
        "/replacement/workspace",
    )
    .expect("other effective root");
    let changed = CargoBrowseContext::from_binding_address(requested, changed)
        .expect("different saved address");
    assert!(
        RouteDependencies::new(&package_route(&package, changed), None)
            .current_cargo_package(&store, &package)
            .is_none(),
        "one requested Tree cannot supply another effective binding"
    );
    let alias = LocalProjectId::new("/workspace/backend/other-member").expect("other request");
    let alias_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
        Path::new(alias.service_coordinate().expect("coordinate")),
        &tree.root,
    )
    .expect("alias binding");
    let alias_context =
        CargoBrowseContext::from_binding_address(alias, alias_binding).expect("alias address");
    assert!(
        RouteDependencies::new(&package_route(&package, alias_context), None)
            .current_cargo_package(&store, &package)
            .is_none(),
        "resident unrelated Trees and equal package digests are not a selected observation"
    );
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
    changed.request_binding = Some(
        backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            Path::new(requested.service_coordinate().expect("coordinate")),
            "/replacement/workspace",
        )
        .expect("changed binding"),
    );
    land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(changed))),
    );
    assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Terminal);
    assert!(!plan.content_loaded(&store));
    assert!(plan.current_cargo_package(&store, &package).is_none());
}

#[test]
fn source_zoom_out_preserves_address_and_evicted_tree_requires_fresh_observation() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    let source = CargoSourceRoute::new(
        context.clone(),
        PackageId::new(package.as_str()).expect("package"),
        CargoSourcePath::new("src/lib.rs").expect("file"),
        Some(7),
    )
    .expect("bound source");
    let plan = RouteDependencies::new(&Route::CargoSource(source.clone()), None);
    let pair = plan.cargo().expect("paired reads");
    let source_key = PageKey::CargoSource(pair.file.clone());
    let mut store = DataStore::new(Arc::new(at(Route::CargoSource(source.clone()))), None);
    land(
        &mut store,
        &source_key,
        PageValue::CargoSource(CargoSourcePage {
            package: package.clone(),
            request_binding: context.request_binding(),
            target: source.target.clone(),
            source: SourceText::new(
                "pub fn observed() {}\n".into(),
                1,
                SourceOrigin::LocalFile,
                true,
            )
            .expect("unindexed bytes"),
            content_digest: [7; 32],
            source_revision: [9; 32],
        }),
    );
    assert!(plan.content_loaded(&store), "inventory may arrive later");
    assert_eq!(
        plan.current_cargo_package(&store, &package)
            .expect("current file projection")
            .context(),
        &context
    );
    let parent = Route::Package(source.package_route());
    assert_eq!(source.package_route().cargo.as_ref(), Some(&context));
    let parent_plan = RouteDependencies::new(&parent, None);
    assert!(
        parent_plan
            .current_cargo_package(&store, &package)
            .is_none(),
        "source history is never searched as a package-page authority"
    );
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested));
    land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))),
    );
    assert!(
        parent_plan
            .current_cargo_package(&store, &package)
            .is_some()
    );
    // The real bounded Browse store evicts the selected Tree. Other current
    // Trees may contain the same exact package, but cannot lend their binding.
    for i in 0..4 {
        let project = LocalProjectId::new(&format!("/workspace/backend/alias-{i}")).expect("alias");
        let mut other = tree.clone();
        other.request_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            Path::new(project.service_coordinate().expect("coordinate")),
            &other.root,
        );
        land(
            &mut store,
            &PageKey::Browse(BrowseKey::Tree(project)),
            PageValue::Browse(BrowseValue::Tree(Arc::new(other))),
        );
    }
    assert!(
        parent_plan
            .current_cargo_package(&store, &package)
            .is_none()
    );
    assert!(!parent_plan.content_loaded(&store));
}

#[gpui::test]
fn coalesced_same_root_owner_replacement_revokes_captured_and_newly_projected_receipts(
    cx: &mut TestAppContext,
) {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, package, context) = fixture(&requested);
    let route = package_route(&package, context);
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(requested));
    let gate = OwnerGate::ready(root(), ServiceMode::Embedded);
    let store = cx.update(|cx| {
        cx.new(|cx| {
            // No focused keys or workers: this exercises lazy admission revocation,
            // not an eager reread and not live-owner acceptance.
            let mut store = DataStore::new(Arc::new(at(route)), None);
            store.owner = OwnerLink::behind(gate.clone());
            store.owner_ready(cx);
            land(
                &mut store,
                &tree_key,
                PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))),
            );
            store
        })
    });
    let (attachment, receipt) = store.read_with(cx, |store, _| {
        (
            store.current_owner_attachment().expect("first attachment"),
            plan.current_cargo_package(store, &package)
                .expect("current receipt")
                .native_dependency(),
        )
    });
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready {
        key: root(),
        mode: ServiceMode::Embedded,
    });
    store.read_with(cx, |store, _| {
        assert!(!store.admits_owner_attachment(&attachment));
        assert!(
            plan.current_cargo_package(store, &package).is_none(),
            "the UI need not observe Starting to deny the replaced attachment"
        );
    });
    store.update(cx, |store, cx| store.owner_ready(cx));
    store.read_with(cx, |store, _| {
        assert!(
            store
                .pages()
                .browse(match &tree_key {
                    PageKey::Browse(key) => key,
                    _ => unreachable!(),
                })
                .loaded_value()
                .is_some()
        );
        assert!(
            plan.current_cargo_package(store, &package).is_none(),
            "newly painted offers cannot re-admit retained Tree bytes through the new token"
        );
        assert!(!plan.admits_native_stamp(store, root(), &receipt));
        assert_eq!(
            store.stats().submitted,
            0,
            "nonvisible predecessor reads stay lazy"
        );
    });
    store.update(cx, |store, _| {
        land(
            store,
            &tree_key,
            PageValue::Browse(BrowseValue::Tree(Arc::new(tree))),
        )
    });
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
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let key = match request {
                ReadRequest::Symbol(symbol) => PageKey::Symbol(symbol.clone()),
                ReadRequest::Source(symbol) => PageKey::Source(symbol.clone()),
                ReadRequest::Package(package) => PageKey::Package(package.clone()),
                _ => {
                    return Err(ReadFailure::Unavailable(
                        UnavailableReason::Unsupported,
                        "header fixture".into(),
                    ));
                }
            };
            let count = {
                let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
                let count = counts.entry(key.clone()).or_default();
                *count += 1;
                *count
            };
            if matches!(key, PageKey::Package(_)) && count == 1 {
                return Err(ReadFailure::Unavailable(
                    UnavailableReason::OutOfScope,
                    "optional semantic dossier absent".into(),
                ));
            }
            crate::shell::tests::Fixture.read(request, context)
        }
    }
    cx.executor().allow_parking();
    let route = crate::shell::kit::symbol_view_route(
        crate::shell::tests::PACKAGE,
        &crate::shell::tests::symbol("RelationLabel"),
        View::Code,
        None,
    )
    .expect("code route");
    let plan = RouteDependencies::new(&route, None);
    let package = route_package(&route).expect("visible titlebar package");
    let header = PageKey::Package(package.clone());
    assert!(plan.keys().contains(&header));
    assert_eq!(
        RouteDependencies::chrome_keys(&route),
        vec![
            PageKey::Symbol(route_symbol(&route).expect("symbol")),
            header.clone()
        ]
    );
    let counts = Counts::default();
    let workers = Arc::clone(&counts);
    let pool =
        ReadPool::start(2, move |_| HeaderReader(Arc::clone(&workers))).expect("fixture pool");
    let gate = OwnerGate::ready(root(), ServiceMode::Embedded);
    let store = cx.update(|cx| {
        DataStore::install_with_owner(
            cx,
            Arc::new(at(route)),
            Some(pool),
            Some(gate.clone()),
            None,
        )
    });
    crate::runtime::wait::until(
        "current code and explicitly absent titlebar dossier",
        || {
            cx.run_until_parked();
            store.read_with(cx, |store, _| {
                plan.content_loaded(store)
                    && matches!(
                        store.package(&package).terminal(),
                        crate::core::ResourceTerminal::Unavailable(_)
                    )
            })
        },
    );
    assert_eq!(
        counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&header),
        Some(&1)
    );
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready {
        key: root(),
        mode: ServiceMode::Embedded,
    });
    store.update(cx, |store, cx| store.owner_ready(cx));
    crate::runtime::wait::until("all visible reads renew through the unified plan", || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| {
            plan.content_loaded(store)
                && admit_resource(&store.package(&package), root(), store.owner_serving())
                    .allows_actions()
        })
    });
    let counts = counts.lock().unwrap_or_else(PoisonError::into_inner);
    for key in plan.keys() {
        assert_eq!(
            counts.get(key),
            Some(&2),
            "visible key {key:?} renewed lazily through the route plan"
        );
    }
}

#[test]
fn required_tree_refusal_is_visible_for_every_optional_dossier_phase() {
    for optional in 0..4 {
        let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
        let (mut tree, package, context) = fixture(&requested);
        let route = package_route(&package, context);
        let plan = RouteDependencies::new(&route, None);
        let tree_key = PageKey::Browse(BrowseKey::Tree(requested.clone()));
        let dossier_key = PageKey::Package(package.clone());
        let mut store = DataStore::new(Arc::new(at(route)), None);
        tree.request_binding = Some(
            backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
                Path::new(requested.service_coordinate().expect("coordinate")),
                "/replacement/workspace",
            )
            .expect("changed binding"),
        );
        land(
            &mut store,
            &tree_key,
            PageValue::Browse(BrowseValue::Tree(Arc::new(tree))),
        );
        let generation = store
            .pages
            .begin(&dossier_key, root())
            .expect("optional read");
        match optional {
            0 => {} // Pending optional metadata must not hide a finished refusal.
            1 => {
                store.pages.land(
                    &dossier_key,
                    generation,
                    Err(ReadFailure::Fault(ErrorValue::new(
                        FaultCode::Missing,
                        "optional dossier missing",
                    ))),
                );
            }
            2 => {
                store.pages.land(
                    &dossier_key,
                    generation,
                    Err(ReadFailure::Unavailable(
                        UnavailableReason::OutOfScope,
                        "optional semantics unavailable".into(),
                    )),
                );
            }
            _ => {
                store.pages.land(
                    &dossier_key,
                    generation,
                    Ok(PageValue::Package(crate::shell::tests::registry_dossier(
                        &package,
                    ))),
                );
            }
        }
        let pages = crate::shell::bodies::Pages::gather(&store, &plan);
        let (key, failure) = pages
            .content_failure()
            .expect("required Tree refusal is painted");
        assert_eq!(
            key, &tree_key,
            "optional dossier phase {optional} selected the wrong refusal"
        );
        assert!(
            matches!(failure, ContentFailure::Fault(error) if error.code() == FaultCode::Protocol && error.message().contains("saved Cargo source binding"))
        );
        assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Terminal);
    }
}

#[test]
fn required_symbol_fault_wins_over_pending_source_and_optional_package_fault() {
    let symbol = crate::shell::tests::symbol("RelationLabel");
    let route = crate::shell::kit::symbol_view_route(
        crate::shell::tests::PACKAGE,
        &symbol,
        crate::navigation::View::Code,
        None,
    )
    .expect("code address");
    let mut store = DataStore::new(Arc::new(at(route.clone())), None);
    let key = PageKey::Symbol(symbol.clone());
    let generation = store.pages.begin(&key, root()).expect("symbol request");
    store.pages.land(
        &key,
        generation,
        Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Missing,
            "declaration revoked",
        ))),
    );
    let optional = PageKey::Package(route_package(&route).expect("package"));
    let generation = store
        .pages
        .begin(&optional, root())
        .expect("optional dossier request");
    store.pages.land(
        &optional,
        generation,
        Err(ReadFailure::Fault(ErrorValue::new(
            FaultCode::Transport,
            "optional dossier offline",
        ))),
    );
    let pages = crate::shell::bodies::Pages::gather(&store, &RouteDependencies::new(&route, None));
    let (selected, failure) = pages
        .content_failure()
        .expect("required fault before source arrives");
    assert_eq!(selected, &key);
    assert!(
        matches!(failure, ContentFailure::Fault(error) if error.message() == "declaration revoked")
    );
}

#[test]
fn current_code_display_does_not_wait_for_a_failed_symbol_pane() {
    let symbol = crate::shell::tests::symbol("RelationLabel");
    let route = crate::shell::tests::view_route("RelationLabel", crate::navigation::View::Code);
    let plan = RouteDependencies::new(&route, None);
    let mut store = DataStore::new(Arc::new(at(route)), None);
    let page = crate::shell::tests::page("RelationLabel");
    let source = SourceView {
        symbol: page.identity,
        file: Known::Known(Arc::from("relation.rs")),
        editor_path: Known::unknown(GapReason::NotServed, "no editor path"),
        text: Known::Known(SourceText::new(
            Arc::from("pub enum RelationLabel { Typed }\n"), 1, SourceOrigin::LocalFile, true,
        ).expect("source text")),
        declaration: Known::Known(LineSpan { first: 1, last: 1 }),
        identifiers: Known::Known(Arc::from([])),
        uses: Known::Known(Arc::from([])),
        uses_elsewhere: Arc::from([]),
    };
    land(&mut store, &PageKey::Source(symbol.clone()), PageValue::Source(source));
    let key = PageKey::Symbol(symbol);
    let generation = store.pages.begin(&key, root()).expect("symbol request");
    store.pages.land(&key, generation, Err(ReadFailure::Fault(ErrorValue::new(
        FaultCode::Missing, "symbol unavailable",
    ))));
    assert_eq!(plan.display_phase(&store), crate::core::ReadPhase::Ready);
    assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Terminal);
}

#[test]
fn current_dossier_display_does_not_wait_for_a_failed_cargo_sibling() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (_, package, context) = fixture(&requested);
    let route = package_route(&package, context.clone());
    let plan = RouteDependencies::new(&route, None);
    let mut store = DataStore::new(Arc::new(at(route)), None);
    land(&mut store, &PageKey::Package(package.clone()), PageValue::Package(
        crate::shell::tests::registry_dossier(&package),
    ));
    let key = PageKey::Browse(BrowseKey::Tree(requested));
    let generation = store.pages.begin(&key, root()).expect("Tree request");
    store.pages.land(&key, generation, Err(ReadFailure::Fault(ErrorValue::new(
        FaultCode::Transport, "Tree unavailable",
    ))));
    assert_eq!(plan.display_phase(&store), crate::core::ReadPhase::Ready);
    assert_eq!(plan.content_phase(&store), crate::core::ReadPhase::Terminal);
    assert!(plan.current_cargo_package(&store, &package).is_none());
}

#[test]
fn local_library_and_settings_survive_failed_optional_catalog_reads() {
    let route = Route::Orbit(crate::navigation::OrbitRoute::Home);
    let mut store = DataStore::new(Arc::new(at(route.clone())), None);
    for key in [PageKey::Orbit, PageKey::Health] {
        let generation = store
            .pages
            .begin(&key, root())
            .expect("optional owner read");
        store.pages.land(
            &key,
            generation,
            Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Transport,
                "owner unavailable",
            ))),
        );
    }
    for overlay in [
        None,
        Some(crate::navigation::Overlay::Settings(
            crate::navigation::SettingsPage::Appearance,
        )),
        Some(crate::navigation::Overlay::Inbox),
    ] {
        let plan = RouteDependencies::new(&route, overlay);
        assert_eq!(
            plan.content_phase(&store),
            crate::core::ReadPhase::Ready,
            "local content cannot wait for the optional index catalog"
        );
        assert!(
            crate::shell::bodies::Pages::gather(&store, &plan)
                .content_failure()
                .is_none(),
            "remote failure replaced the local shell"
        );
    }
}

#[test]
fn owner_readme_is_independent_of_semantic_dossier_but_requires_exact_current_local_tree() {
    use crate::runtime::cargo_readme_reads::tests::{fixture as readme_fixture, fixture_value};
    let (raw_tree, readme, _) = readme_fixture();
    let (selected, value) = fixture_value();
    assert_eq!(selected, readme);
    let tree = crate::runtime::browse_reads::tree_model(&raw_tree);
    let route = package_route(&readme.package, readme.context.clone());
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(readme.context.requested_project().clone()));
    let readme_key = PageKey::Browse(BrowseKey::CargoReadme(readme.clone()));
    let mut store = DataStore::new(Arc::new(at(route)), None);
    land(&mut store, &readme_key, value.clone());
    assert!(
        plan.current_cargo_readme(&store).is_none(),
        "README bytes alone cannot borrow an unobserved local Tree"
    );
    land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))),
    );
    let current = plan
        .current_cargo_readme(&store)
        .expect("exact independent local README receipt");
    assert_eq!(current.native_dependency().0, readme_key);
    assert!(store.package(&readme.package).loaded_value().is_none());
    assert!(
        plan.content_loaded(&store),
        "current local Tree and owner README require no remote/global semantic dossier"
    );
    let dossier = PageKey::Package(readme.package.clone());
    let generation = store
        .pages
        .begin(&dossier, root())
        .expect("optional semantic read");
    store.pages.land(
        &dossier,
        generation,
        Err(ReadFailure::Unavailable(
            UnavailableReason::OutOfScope,
            "not semantically indexed".into(),
        )),
    );
    assert!(plan.current_cargo_readme(&store).is_some());
    assert!(plan.content_loaded(&store));

    let receipt = current.native_dependency();
    let before = store.stamp(&readme_key);
    let mut other = tree.clone();
    let mut binding = readme.context.request_binding();
    binding.effective_workspace_root_digest = [3; 32];
    other.request_binding = Some(binding);
    force_land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(other))),
    );
    assert_eq!(
        store.stamp(&readme_key),
        before,
        "only the selected Tree changed"
    );
    assert!(plan.current_cargo_readme(&store).is_none());
    assert!(
        !plan.admits_native_stamp(&store, root(), &receipt),
        "a current README slot cannot override its wrong-bound selected Tree at activation or queue flush"
    );

    force_land(
        &mut store,
        &tree_key,
        PageValue::Browse(BrowseValue::Tree(Arc::new(tree))),
    );
    let PageValue::Browse(BrowseValue::CargoReadme(model)) = value else {
        panic!("README fixture");
    };
    let mut bad = (*model).clone();
    let crate::model::browse::CargoReadmeState::Read(document) = &bad.state else {
        panic!("Markdown");
    };
    let mut document = (**document).clone();
    document.origin.request_binding.requested_root_digest = [4; 32];
    bad.state = crate::model::browse::CargoReadmeState::Read(Arc::new(document));
    force_land(
        &mut store,
        &readme_key,
        PageValue::Browse(BrowseValue::CargoReadme(Arc::new(bad))),
    );
    assert!(
        plan.current_cargo_readme(&store).is_none(),
        "a typed projection with a different origin cannot lend the model's outer package/binding to its links"
    );
}

#[gpui::test]
fn readme_owner_replacement_retains_inert_bytes_and_fences_late_digest_then_admits_only_new_success(
    cx: &mut TestAppContext,
) {
    use crate::runtime::cargo_readme_reads::tests::{
        fixture as readme_fixture, fixture_value, fixture_with_contents,
    };
    let (raw_tree, selected, _) = readme_fixture();
    let tree = crate::runtime::browse_reads::tree_model(&raw_tree);
    let (_, old) = fixture_value();
    let route = package_route(&selected.package, selected.context.clone());
    let plan = RouteDependencies::new(&route, None);
    let tree_key = PageKey::Browse(BrowseKey::Tree(
        selected.context.requested_project().clone(),
    ));
    let key = PageKey::Browse(BrowseKey::CargoReadme(selected));
    let gate = OwnerGate::ready(root(), ServiceMode::Embedded);
    let store = cx.update(|cx| {
        cx.new(|cx| {
            let mut store = DataStore::new(Arc::new(at(route)), None);
            store.owner = OwnerLink::behind(gate.clone());
            store.owner_ready(cx);
            land(
                &mut store,
                &tree_key,
                PageValue::Browse(BrowseValue::Tree(Arc::new(tree.clone()))),
            );
            land(&mut store, &key, old.clone());
            store
        })
    });
    let (attachment, lease, digest) = store.read_with(cx, |store, _| {
        let current = plan
            .current_cargo_readme(store)
            .expect("current owner-backed README");
        let crate::model::browse::CargoReadmeState::Read(document) = &current.model().state else {
            panic!("read");
        };
        (
            store
                .current_owner_attachment()
                .expect("serving attachment"),
            RouteReadLease::capture(store, current.native_dependency())
                .expect("rendered action lease"),
            document.origin.content_digest,
        )
    });
    let late = store.update(cx, |store, _| {
        store
            .pages
            .begin_forced(&key, root())
            .expect("old owner's replacement read")
    });
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready {
        key: root().observed_at(99),
        mode: ServiceMode::Embedded,
    });
    store.read_with(cx, |store, _| {
        assert!(!store.admits_owner_attachment(&attachment));
        assert!(!lease.admits(store));
        assert!(
            plan.current_cargo_readme(store).is_none(),
            "direct attachment rejection does not wait for the UI watcher"
        );
    });
    store.update(cx, |store, cx| {
        store.owner_ready(cx);
        assert!(store.pages.browse(match &key { PageKey::Browse(key) => key, _ => unreachable!() }).loaded_value().is_some());
        assert!(store.pages.is_owner_read_revoked(&key));
        assert!(plan.current_cargo_readme(store).is_none(), "newly painted controls cannot re-admit retained bytes under the new owner token");
        assert_eq!(store.pages.land(&key, late, Ok(old)), Landing::Superseded, "late successful bytes/digest from the prior read cannot restore admission");
        land(store, &tree_key, PageValue::Browse(BrowseValue::Tree(Arc::new(tree))));
        assert!(plan.current_cargo_readme(store).is_none(), "fresh Tree alone does not complete the revoked README");
        let (_, fresh) = fixture_with_contents("# Changed\n\n[Current file](../src/current.rs#L2)\n");
        land(store, &key, fresh);
        let current = plan.current_cargo_readme(store).expect("new typed README success");
        let crate::model::browse::CargoReadmeState::Read(document) = &current.model().state else { panic!("read"); };
        assert_ne!(document.origin.content_digest, digest);
        assert!(RouteReadLease::capture(store, current.native_dependency()).is_some());
        assert!(!lease.admits(store), "a fresh current read at the same root never revives the predecessor's action lease");
        assert_eq!(store.stats().submitted, 0, "nonvisible owner-backed observations remain lazy and no global indexing task was required");
    });
}
