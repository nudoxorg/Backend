//! The Library page end to end: a real owner (configured with the real
//! RUSTSEC-2025-0141) reads a real two-member Cargo workspace through
//! `cargo metadata`, the desktop's read pool lowers the reply into the
//! product's words, and the shell draws them. Assertions are the sentences
//! on screen, never counts alone.
//!
//! The workspace is `apps/desktop/tests/fixtures/browse_tree`: `app` pins
//! toml 0.8 and bincode 1.3.3; `tool` depends on trybuild, which asks for
//! toml 1.x. Its `Cargo.lock` is committed, and every crate it names is in
//! the local cargo registry cache, so the read is offline and fixed.

#![allow(clippy::expect_used, clippy::panic)]

use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::browse::{ApiItem, BrowseKey, BrowseValue, CompareModel, FindModel, PackageApi};
use crate::model::pages::{DeclRef, Gap, GapReason, Known, MatchReason, PackageRef, SearchPage, SearchQuery, SearchRow, SignatureText};
use crate::navigation::{BrowseRoute, CompareSet, OrbitRoute, Overlay, Route, SettingsPage};
use crate::model::ServiceMode;
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::shell::tests::{Fixture, RootOnly, rig, rig_with_engine_gate, rig_with_reads};
use backend_client::Session;
use backend_library::{DeclarationKind, SurfaceCommand, SurfaceReply};
use gpui::{SharedString, TestAppContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn retained_tree_names_require_the_exact_requested_and_effective_roots() {
    let requested = LocalProjectId::new("/workspace/backend/member").expect("member");
    let (tree, _, _) = crate::runtime::store::cargo_context_tests::fixture(&requested);
    let route = BrowseRoute::Tree(requested);
    assert!(super::retained_tree_matches_route(&route, &tree));
    let other = BrowseRoute::Tree(LocalProjectId::new("/workspace/other/member").expect("other member"));
    assert!(!super::retained_tree_matches_route(&other, &tree));
    let mut wrong_root = tree.clone();
    wrong_root.root = "/replacement/workspace".into();
    assert!(!super::retained_tree_matches_route(&route, &wrong_root));
    let mut no_binding = tree;
    no_binding.request_binding = None;
    assert!(!super::retained_tree_matches_route(&route, &no_binding));
}

#[test]
fn library_release_actions_keep_exact_source_and_row_keys_survive_reorder() {
    use crate::model::browse::{TreeDestination, TreeReleaseLink, TreeRoleLinks, TreeRowLinks};
    use crate::model::pages::PackageRef;
    use backend_library::browse::RoleId;
    use facet::browse::library::ReleaseHandle;

    let first = PackageRef::parse("pkg:cargo/shared@1.0.0").expect("crates.io release");
    let alternate = PackageRef::parse(
        "pkg:cargo/shared@1.0.0?repository_url=https%3A%2F%2Fregistry.example%2Findex",
    )
    .expect("alternate registry release");
    let make = |packages: &[PackageRef]| TreeRoleLinks {
        role: RoleId::Formats,
        rows: vec![TreeRowLinks {
            name: "shared".into(),
            key: "shared-exact-row".into(),
            releases: packages
                .iter()
                .cloned()
                .map(|package| TreeReleaseLink {
                    version: "1.0.0".into(),
                    key: package.as_str().into(),
                    destination: TreeDestination::Open(package),
                    source_detail: None,
                })
                .collect::<Vec<_>>()
                .into(),
        }]
        .into(),
    };
    let original = [make(&[first.clone(), alternate.clone()])];
    let reversed = [make(&[alternate.clone(), first.clone()])];
    let old_first =
        ReleaseHandle::identified(0, 0, 0, "formats", "shared-exact-row", first.as_str());
    let new_first =
        ReleaseHandle::identified(0, 0, 1, "formats", "shared-exact-row", first.as_str());
    assert_eq!(
        super::typed_library_release(&original, &old_first),
        Some(&first)
    );
    assert_eq!(
        super::typed_library_release(&reversed, &old_first),
        None,
        "an old position cannot navigate to another source after reorder"
    );
    assert_eq!(
        super::typed_library_release(&reversed, &new_first),
        Some(&first)
    );
    let alternate_at_zero =
        ReleaseHandle::identified(0, 0, 0, "formats", "shared-exact-row", alternate.as_str());
    assert_eq!(
        super::typed_library_release(&reversed, &alternate_at_zero),
        Some(&alternate)
    );

    let row = backend_present::RowReading {
        name: "shared".to_owned(),
        at_rest: None,
        evidence: String::new(),
        description: None,
        versions: vec!["1.0.0".to_owned(), "1.0.0".to_owned()].into_boxed_slice(),
        sources: vec![
            Some(first.reference().clone()),
            Some(alternate.reference().clone()),
        ]
        .into_boxed_slice(),
        origins: vec![backend_library::browse::PackageOrigin::Unresolved { source: None }; 2]
            .into_boxed_slice(),
    };
    let original_key = crate::runtime::browse_reads::row_key(&row, original[0].rows.first());
    let reversed_key = crate::runtime::browse_reads::row_key(&row, reversed[0].rows.first());
    assert_eq!(
        original_key, reversed_key,
        "reordering two distinct same-version sources preserves disclosure identity"
    );
    assert_ne!(
        original_key,
        crate::runtime::browse_reads::row_key(&row, make(&[first]).rows.first()),
        "removing a source changes the row identity"
    );
}

#[test]
fn inventory_actions_refuse_stale_positions_and_unverified_sources() {
    use crate::model::browse::{TreeDestination, TreeInventoryLink};
    use crate::model::pages::PackageRef;
    use facet::browse::library::InventoryHandle;

    let first = PackageRef::parse("pkg:cargo/shared@1.0.0?cargo-authority=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .expect("first exact source");
    let second = PackageRef::parse("pkg:cargo/shared@1.0.0?cargo-authority=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .expect("second exact source");
    let row = |key: &str, destination| TreeInventoryLink {
        key: key.into(),
        name: "shared".into(),
        version: "1.0.0".into(),
        destination,
    };
    let rows = [
        row("registry-a", TreeDestination::Open(first.clone())),
        row("registry-b", TreeDestination::Open(second.clone())),
        row(
            "unverified",
            TreeDestination::Unavailable("no receipt".into()),
        ),
    ];
    let a = InventoryHandle::new(0, "registry-a");
    let b = InventoryHandle::new(1, "registry-b");
    assert_eq!(super::typed_library_inventory(&rows, &a), Some(&first));
    assert_eq!(super::typed_library_inventory(&rows, &b), Some(&second));
    assert_eq!(
        super::typed_library_inventory(&rows, &InventoryHandle::new(2, "unverified")),
        None
    );
    let reordered = [rows[1].clone(), rows[0].clone(), rows[2].clone()];
    assert_eq!(super::typed_library_inventory(&reordered, &a), None);
    assert_eq!(
        super::typed_library_inventory(&reordered, &InventoryHandle::new(1, "registry-a")),
        Some(&first)
    );
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository")
}

/// An owner on a private workspace, its RustSec source the one real advisory.
fn owner() -> (
    backend_local_service::EmbeddedLocalService,
    PathBuf,
    PathBuf,
) {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            % 100_000
    );
    // A short Unix socket base, or this user's temp directory on Windows.
    let state = crate::host::scratch_base().join(format!("nx-browse-{nonce}"));
    let endpoint = crate::host::scratch_base().join(format!("nx-browse-{nonce}.sock"));
    // The owner refuses a state directory anyone else could enter: 0700, not the umask's 0755.
    crate::host::private_dir(&state.join("data")).expect("workspace");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(repository()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .expect("workspace paths");
    paths.initialize().expect("initialize");
    let advisory =
        repository().join("crates/advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md");
    let arguments = [
        "--endpoint",
        paths.endpoint().to_str().expect("utf-8"),
        "--workspace",
        paths.data().to_str().expect("utf-8"),
        "--authority-secret-file",
        paths.authority_secret().to_str().expect("utf-8"),
        "--profile",
        "builtin",
        "--advisory-rustsec",
        advisory.to_str().expect("utf-8"),
    ]
    .map(ToOwned::to_owned);
    let config = backend_local_service::ProcessConfig::parse(arguments).expect("owner arguments");
    let service =
        backend_local_service::EmbeddedLocalService::start(config).expect("embedded owner");
    (service, endpoint, state)
}

fn tree_route(root: &Path) -> Route {
    Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(
        LocalProjectId::from_path(root).expect("project"),
    )))
}

#[gpui::test]
fn the_library_page_shows_a_real_tree_read_by_a_real_owner(cx: &mut TestAppContext) {
    let (_service, endpoint, state) = owner();
    let mut session = Session::connect(&endpoint).expect("session");
    let SurfaceReply::AdvisoryRefreshed(sources) = session
        .surface(SurfaceCommand::AdvisoryRefresh)
        .expect("refresh")
    else {
        panic!("advisory-refresh answered another reply");
    };
    assert_eq!(sources.len(), 1);
    assert_eq!(
        (
            sources[0].source.as_str(),
            sources[0].advisories,
            sources[0].complete
        ),
        ("rustsec", 1, false)
    );

    let fixture = repository().join("apps/desktop/tests/fixtures/browse_tree");
    let reader_endpoint = endpoint.clone();
    let pool =
        ReadPool::start(2, move |_| SessionReader::connect(&reader_endpoint)).expect("read pool");
    let mut rig = rig_with_reads(cx, None, 1440.0, 900.0, pool);
    // A real owner reads the tree through `cargo metadata`: real seconds,
    // more under load, which `settle` waits for in real time.
    rig.patience = Duration::from_secs(120);
    rig.go(crate::navigation::Intent::Navigate(tree_route(&fixture)));
    let lede = "Your 2 packages lean on 3 others directly, and 31 in all.";
    let started = Instant::now();
    let mut said = rig.said();
    while !said.iter().any(|line| line == lede) {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the tree never landed: {said:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
        rig.settle();
        said = rig.said();
    }
    for expected in [
        "browse_tree",
        lede,
        "bincode 1.3.3 is unmaintained",
        "4 crates are here twice",
        "advisories from a partial source, not a full check of 31",
        "checks our work",
        "trybuild",
        "speaks formats",
        "toml",
        "twice · 0.8.23 · 1.1.6",
        "bincode",
        "Here twice",
        "4 crates appear at more than one version",
    ] {
        assert!(
            said.iter().any(|line| line == expected),
            "{expected:?} is not on screen: {said:#?}"
        );
    }
    // bincode speaks a format; its "network-programming" category loses the vote.
    let formats = said
        .iter()
        .position(|line| line == "speaks formats")
        .expect("formats");
    let bincode = said
        .iter()
        .position(|line| line == "bincode")
        .expect("bincode row");
    assert!(
        bincode > formats,
        "bincode is listed under speaks formats: {said:#?}"
    );
    let address = rig.shell.read_with(rig.cx, |_, cx| {
        let snapshot = rig.graph.store.read(cx).snapshot().clone();
        crate::shell::jump::address_parts(&snapshot).full()
    });
    assert_eq!(address, "nudox://browse_tree/tree");

    // The same owner reads a second pinned project without serving the
    // first one's cached tree. Neither assertion depends on this repo's
    // live Cargo.lock or directory name.
    let mut reader = SessionReader::connect(&endpoint);
    let tree = |root: &Path, reader: &mut SessionReader| {
        let key = crate::model::browse::BrowseKey::Tree(
            LocalProjectId::from_path(root).expect("project"),
        );
        let cancel = crate::runtime::CancellationToken::new();
        let outlines = crate::runtime::reads::OutlineCache::default();
        let context = crate::runtime::reads::ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
            progress: None,
        };
        match crate::runtime::reads::PageReader::read(
            reader,
            &crate::runtime::reads::ReadRequest::Browse(key),
            &context,
        ) {
            Ok(crate::model::pages::PageValue::Browse(value)) => {
                value.tree().expect("a tree").reading.clone()
            }
            other => panic!("the tree read failed: {other:?}"),
        }
    };
    let other_fixture = repository().join("frontends/rust/fixtures/toml_pin");
    let other_tree = tree(&other_fixture, &mut reader);
    assert_eq!(other_tree.name, "toml_pin");
    let again = tree(&fixture, &mut reader);
    assert_eq!(again.name, "browse_tree");
    assert_eq!(again.lede, lede);
    let toml = again
        .twice
        .iter()
        .find(|twice| twice.name == "toml")
        .expect("toml twice");
    assert_eq!(
        toml.paths.as_ref(),
        [
            "app → toml 0.8.23".to_owned(),
            "tool → trybuild 1.0.121 → toml 1.1.6".to_owned()
        ]
    );
    assert_eq!(toml.verdict, "Moving yours to 1.1.6 drops a copy.");
    assert_eq!(again.alerts[0].why, "app → bincode 1.3.3");
    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn find_invalid_input_is_distinct_from_blank_and_missing_package_is_unavailable() {
    use facet::browse::find::{QueryInput, Routability};
    assert_eq!(super::query_input("  "), QueryInput::Blank);
    assert_eq!(super::query_input("from_str"), QueryInput::Valid);
    assert!(matches!(super::query_input("bad\0query"), QueryInput::Invalid(_)));
    let unresolved = super::symbol_routability(&"unqualified::Thing".into());
    assert!(matches!(unresolved, Routability::Unavailable(reason) if reason.contains("no addressable package")));
    assert_eq!(super::symbol_routability(&"/fixture/app::app.rs:1::main".into()), Routability::Available);
}

fn find_fixture(query: &str, includes_answer: bool) -> BrowseValue {
    let coordinate = "/fixture/present::glyph.rs:138::RelationLabel";
    let rows = if includes_answer {
        vec![SearchRow {
            rank: 0,
            decl: DeclRef::from_label(coordinate, None, Some(DeclarationKind::Struct), Some(("glyph.rs", 138))).expect("fixture declaration"),
            package: Some(Arc::from("/fixture/present")),
            score: Known::Unknown(Gap::new(GapReason::NotRecorded, "")),
            signature: Known::Known(SignatureText {
                text: Arc::from("pub struct RelationLabel"), tokens: Arc::from([]),
                name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
            }),
            snippet: None,
            reason: MatchReason::ExactName,
        }]
    } else { vec![] };
    let answers = Known::Known(SearchPage {
        query: Arc::from(query), rows: rows.into(),
        coverage: backend_present::CoverageLine::new(&[], Some(u64::from(includes_answer))),
        next: None,
    });
    let package_coverage = Known::Known(());
    let prepared = Arc::new(crate::runtime::browse_views::prepare_find(query, &answers, &[], &package_coverage));
    BrowseValue::Find(Arc::new(FindModel { answers, packages: Arc::from([]), package_coverage, prepared }))
}

fn find_callback(rig: &mut crate::shell::tests::Rig, route: &BrowseRoute) -> super::FindActionSource {
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    super::FindActionSource {
        links: crate::shell::region::Links {
            root: rig.graph.root.downgrade(), store: rig.graph.store.clone(), shell: rig.shell.downgrade(), reader: Default::default(),
        },
        route: route.clone(), key: BrowseKey::from(route), root,
        visit: browse_visit(rig),
    }
}

fn land_find(rig: &mut crate::shell::tests::Rig, route: &BrowseRoute, query: &str, includes_answer: bool) {
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(BrowseKey::from(route), find_fixture(query, includes_answer), cx));
}

#[gpui::test]
fn find_source_callback_opens_a_member_of_the_unchanged_live_reading(cx: &mut TestAppContext) {
    let query = SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).expect("query");
    let browse = BrowseRoute::Find(query);
    let current = Route::Orbit(OrbitRoute::Browse(browse.clone()));
    let mut rig = rig(cx, Some(current), 1200.0, 800.0);
    land_find(&mut rig, &browse, "RelationLabel", true);
    let source = find_callback(&mut rig, &browse);
    let open = super::find_symbol_action(source, false);
    rig.cx.update(|window, cx| open(SharedString::from("/fixture/present::glyph.rs:138::RelationLabel"), window, cx));
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(route) if route.id.as_str() == "/fixture/present::glyph.rs:138::RelationLabel"));
}

#[gpui::test]
fn find_opens_an_observed_package_root_result_as_its_package(cx: &mut TestAppContext) {
    let root = "/private/tmp/nudox-gui-user-audit-20261003/real-source-base16ct-1.0.0";
    let query = SearchQuery::new("base16ct", SearchQuery::DEFAULT_LIMIT).expect("query");
    let browse = BrowseRoute::Find(query.clone());
    let mut rig = rig(cx, Some(Route::Orbit(OrbitRoute::Browse(browse.clone()))), 1200.0, 800.0);
    let producer = backend_library::Row::new(
        backend_library::RowId::Package(backend_library::package_key(root)),
        backend_library::Basis::new(backend_library::view_state_root(&[]), backend_library::object_version(b"root-result")),
        root,
    );
    let page = crate::runtime::page_mapping::search_rows(
        query.text.as_ref(), &[producer], &[backend_library::Coverage::Complete], None, 0,
    );
    let answers = Known::Known(page);
    let package_coverage = Known::Known(());
    let prepared = Arc::new(crate::runtime::browse_views::prepare_find(
        query.text.as_ref(), &answers, &[], &package_coverage,
    ));
    let value = BrowseValue::Find(Arc::new(FindModel {
        answers, packages: Arc::from([]), package_coverage, prepared,
    }));
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(BrowseKey::from(&browse), value, cx));
    let source = find_callback(&mut rig, &browse);
    let open = super::find_symbol_action(source, false);
    rig.cx.update(|window, cx| open(root.into(), window, cx));
    rig.settle();
    assert!(matches!(rig.route(), Route::Package(route) if route.package.as_str() == root));
}

#[gpui::test]
fn find_source_callback_rechecks_membership_query_overlay_owner_and_root(cx: &mut TestAppContext) {
    let query = SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).expect("query");
    let browse = BrowseRoute::Find(query);
    let current = Route::Orbit(OrbitRoute::Browse(browse.clone()));
    let mut rig = rig(cx, Some(current.clone()), 1200.0, 800.0);
    land_find(&mut rig, &browse, "RelationLabel", true);
    let source = find_callback(&mut rig, &browse);
    let open = super::find_symbol_action(source.clone(), false);
    let open_package = super::find_package_action(source);
    let key = SharedString::from("/fixture/present::glyph.rs:138::RelationLabel");
    let invoke = |rig: &mut crate::shell::tests::Rig, expected: &Route| {
        rig.cx.update(|window, cx| open(key.clone(), window, cx));
        rig.cx.update(|window, cx| open_package(SharedString::from("/fixture/present"), window, cx));
        rig.cx.run_until_parked();
        assert_eq!(&rig.route(), expected, "an obsolete callback must leave the route alone");
    };

    land_find(&mut rig, &browse, "RelationLabel", false);
    invoke(&mut rig, &current);
    land_find(&mut rig, &browse, "different query", true);
    invoke(&mut rig, &current);
    land_find(&mut rig, &browse, "RelationLabel", true);

    let original = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let mut covered = original.session().clone();
    covered.overlay = Some(Overlay::Settings(SettingsPage::Appearance));
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_session(covered)), cx));
    invoke(&mut rig, &current);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::clone(&original), cx));

    let mut elsewhere = original.session().clone();
    elsewhere.route = Route::Orbit(OrbitRoute::Home);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_session(elsewhere.clone())), cx));
    invoke(&mut rig, &elsewhere.route);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::clone(&original), cx));
    rig.cx.run_until_parked();
    land_find(&mut rig, &browse, "RelationLabel", true);

    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    invoke(&mut rig, &current);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));

    let newer = VersionedRoot::synthetic(backend_library::view_state_root(&[("find".into(), "new root".into())]), 9);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_key(newer, None)), cx));
    invoke(&mut rig, &current);
}

#[gpui::test]
fn find_callback_never_revives_on_a_new_same_route_visit_or_owner_attachment(cx: &mut TestAppContext) {
    let browse = BrowseRoute::Find(SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).expect("query"));
    let route = Route::Orbit(OrbitRoute::Browse(browse.clone()));
    let mut rig = rig(cx, Some(route.clone()), 1200.0, 800.0);
    land_find(&mut rig, &browse, "RelationLabel", true);
    let old_visit = find_callback(&mut rig, &browse);
    let old_open = super::find_symbol_action(old_visit.clone(), false);
    assert!(rig.cx.update(|_, cx| old_visit.current(cx, |_| Some(())).is_ok()));

    rig.go(crate::navigation::Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.settle();
    rig.go(crate::navigation::Intent::Navigate(route.clone()));
    rig.settle();
    land_find(&mut rig, &browse, "RelationLabel", true);
    assert!(rig.cx.update(|_, cx| old_visit.current(cx, |_| Some(())).is_err()), "a renewed identical route revived its old row");
    rig.cx.update(|window, cx| old_open("/fixture/present::glyph.rs:138::RelationLabel".into(), window, cx));
    assert_eq!(rig.route(), route, "the painted old visit opened a symbol on the new visit");
    let old_attachment = find_callback(&mut rig, &browse);
    let old_attachment_open = super::find_symbol_action(old_attachment.clone(), false);
    assert!(rig.cx.update(|_, cx| old_attachment.current(cx, |_| Some(())).is_ok()));
    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));
    land_find(&mut rig, &browse, "RelationLabel", true);
    assert!(rig.cx.update(|_, cx| old_attachment.current(cx, |_| Some(())).is_err()),
        "same route/root/content under a new owner attachment revived its old row");
    rig.cx.update(|window, cx| old_attachment_open("/fixture/present::glyph.rs:138::RelationLabel".into(), window, cx));
    assert_eq!(rig.route(), route);
}

/// The first Find paint defers its focus claim. Every input or ownership
/// change between that paint and the effect callback must reject the old
/// claim before it can move native focus.
#[gpui::test]
fn stale_first_find_focus_claim_is_denied_before_focus(cx: &mut TestAppContext) {
    let browse = BrowseRoute::Find(SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).expect("query"));
    let route = Route::Orbit(OrbitRoute::Browse(browse.clone()));
    for change in ["route", "add", "ask", "tab", "key", "attachment", "root"] {
        let mut rig = rig(cx, Some(route.clone()), 1200.0, 800.0);
        rig.settle();
        let source = find_callback(&mut rig, &browse);
        let claim = rig.cx.update(|_, cx| super::FindQueryClaim::new(&source, cx));
        let (query, epoch) = rig.cx.update(|window, cx| (window.focused(cx).expect("Find query"), window.focus_epoch()));
        assert!(rig.cx.update(|window, cx| claim.admits(&query, epoch, window, cx)),
            "the current mounted Find query should be claimable before {change}");
        match change {
            "route" => rig.go(crate::navigation::Intent::Navigate(Route::Orbit(OrbitRoute::Home))),
            "add" => rig.keys("secondary-o"),
            "ask" => rig.keys("secondary-k"),
            "tab" => {
                // This deferred admission is queued before the focus move,
                // just as a Find paint can queue its claim before the next
                // input in the same GPUI effect cycle.
                let observed = std::rc::Rc::new(std::cell::Cell::new(None));
                let result = std::rc::Rc::clone(&observed);
                let deferred = claim.clone();
                let target = query.clone();
                rig.cx.update(|window, cx| {
                    window.defer(cx, move |window, cx| result.set(Some(deferred.admits(&target, epoch, window, cx))));
                    window.focus_next(cx);
                });
                assert_eq!(observed.get(), Some(false), "a focus move outran the queued first claim");
            }
            "key" => rig.cx.simulate_keystrokes("left"),
            "attachment" => {
                rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
                rig.cx.run_until_parked();
                rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));
                rig.cx.run_until_parked();
            }
            "root" => {
                let original = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
                let next = VersionedRoot::synthetic(
                    backend_library::view_state_root(&[("find".into(), "new claim root".into())]), 9,
                );
                rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_key(next, None)), cx));
            }
            _ => unreachable!(),
        }
        assert!(!rig.cx.update(|window, cx| claim.admits(&query, epoch, window, cx)),
            "an old Find focus claim survived {change}");
    }
}

#[gpui::test]
fn failed_owner_still_admits_the_local_find_query_claim(cx: &mut TestAppContext) {
    let browse = BrowseRoute::Find(SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).expect("query"));
    let route = Route::Orbit(OrbitRoute::Browse(browse.clone()));
    let mut rig = rig(cx, Some(route), 1200.0, 800.0);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(
        &crate::runtime::owner::OwnerFault::Lost("owner lost".into()), cx,
    ));
    rig.settle();
    assert!(!rig.graph.store.read_with(rig.cx, |store, _| store.owner_serving()));
    let source = find_callback(&mut rig, &browse);
    let claim = rig.cx.update(|_, cx| super::FindQueryClaim::new(&source, cx));
    let (query, epoch) = rig.cx.update(|window, cx| (window.focused(cx).expect("Find query"), window.focus_epoch()));
    assert!(rig.cx.update(|window, cx| claim.admits(&query, epoch, window, cx)),
        "Find's local editor cannot depend on owner readiness");
}

fn compare_selection() -> CompareSet {
    CompareSet::new(["/fixture/present", "/fixture/second", "/fixture/third"]
        .into_iter().map(|key| PackageRef::parse(key).expect("fixture package"))).expect("three packages")
}

fn compare_fixture(selection: &CompareSet, answer: bool, source_available: bool) -> BrowseValue {
    let dossiers = selection.packages().iter().map(|package| {
        let mut dossier = crate::shell::tests::dossier();
        dossier.package = package.clone();
        if let Known::Known(record) = &mut dossier.record {
            record.package = package.clone();
            record.name = Arc::from(package.display_name());
        }
        dossier.outline = Known::Unknown(Gap::new(GapReason::NotRecorded, "synthetic comparison outline"));
        dossier
    }).collect::<Vec<_>>();
    let apis = selection.packages().iter().enumerate().map(|(at, package)| Known::Known(PackageApi {
        package: package.clone(), complete: true,
        items: if at == 0 && answer { Arc::from([ApiItem {
            decl: DeclRef::from_label(
                &format!("{}::glyph.rs:138::RelationLabel", package.as_str()),
                None, Some(DeclarationKind::Struct), source_available.then_some(("glyph.rs", 138)),
            ).expect("fixture declaration"),
            signature: Known::Known(SignatureText {
                text: Arc::from("pub struct RelationLabel"), tokens: Arc::from([]),
                name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
            }),
            summary: None,
        }]) } else { Arc::from([]) },
    })).collect::<Vec<_>>();
    let prepared = Arc::new(crate::runtime::browse_views::prepare_compare(&dossiers, &apis));
    BrowseValue::Compare(Arc::new(CompareModel {
        packages: dossiers.into(),
        apis: apis.into(), prepared,
    }))
}

fn compare_callback(rig: &mut crate::shell::tests::Rig, selection: &CompareSet) -> super::CompareActionSource {
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    super::CompareActionSource {
        links: crate::shell::region::Links {
            root: rig.graph.root.downgrade(), store: rig.graph.store.clone(), shell: rig.shell.downgrade(), reader: Default::default(),
        },
        selection: selection.clone(), root,
        visit: browse_visit(rig),
    }
}

#[gpui::test]
fn compare_callback_never_revives_on_a_new_same_route_visit(cx: &mut TestAppContext) {
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(route.clone()), 1200.0, 800.0);
    land_compare(&mut rig, &selection, true, true);
    let old = compare_callback(&mut rig, &selection);
    let old_open = super::compare_package_action(old.clone());
    assert!(rig.cx.update(|_, cx| old.current(cx, |_| Some(())).is_ok()));
    rig.go(crate::navigation::Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.settle();
    rig.go(crate::navigation::Intent::Navigate(route.clone()));
    rig.settle();
    land_compare(&mut rig, &selection, true, true);
    assert!(rig.cx.update(|_, cx| old.current(cx, |_| Some(())).is_err()));
    rig.cx.update(|window, cx| old_open("/fixture/second".into(), window, cx));
    assert_eq!(rig.route(), route, "the painted old Compare choice opened after a new visit");
    let current = compare_callback(&mut rig, &selection);
    assert!(rig.cx.update(|_, cx| current.current(cx, |_| Some(())).is_ok()));
}

/// A Compare choice painted under one owner attachment cannot act after the
/// same root is re-attached (Starting, then Ready again), even though the
/// route, the selection and the root revision all still match.
#[gpui::test]
fn compare_painted_callback_cannot_cross_same_root_owner_attachment(cx: &mut TestAppContext) {
    let root = VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4,
    );
    let gate = OwnerGate::ready(root, ServiceMode::Attached);
    let pool = ReadPool::start(2, |_| Fixture).expect("fixture pool");
    let mut rig = rig_with_engine_gate(cx, None, 1200.0, 800.0, pool, RootOnly, Some(gate.clone()));
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    rig.go(crate::navigation::Intent::Navigate(route.clone()));
    land_compare(&mut rig, &selection, true, true);
    let painted = compare_callback(&mut rig, &selection);
    let open = super::compare_package_action(painted.clone());
    assert!(rig.cx.update(|_, cx| painted.current(cx, |_| Some(())).is_ok()),
        "the Compare choice must first be live under its own attachment");
    gate.publish(OwnerState::Starting);
    rig.cx.run_until_parked();
    gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
    rig.cx.run_until_parked();
    assert!(rig.cx.update(|_, cx| painted.current(cx, |_| Some(())).is_err()),
        "the old attachment's Compare lease survived a same-root re-attachment");
    rig.cx.update(|window, cx| open("/fixture/second".into(), window, cx));
    assert_eq!(rig.route(), route,
        "a painted Compare choice from the old owner attachment opened on the renewed same root");
}

fn browse_visit(rig: &mut crate::shell::tests::Rig) -> super::CurrentBrowseVisit {
    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let place = reader.read_with(rig.cx, |reader, _| reader.current_place_key().expect("mounted browse visit"));
    let (route, root, lease) = rig.graph.store.read_with(rig.cx, |store, _| {
        let snapshot = store.snapshot();
        let route = snapshot.route().clone();
        let root = snapshot.key();
        let lease = crate::shell::reader::NativeActionLease::Resource {
            attachment: store.current_owner_attachment(),
            stamp: crate::runtime::store::RouteDependencies::new(&route, None).native_stamp(store, false),
        };
        (route, root, lease)
    });
    super::CurrentBrowseVisit { reader: reader.downgrade(), place, route, root, lease }
}

fn land_compare(rig: &mut crate::shell::tests::Rig, selection: &CompareSet, answer: bool, source_available: bool) {
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(
        BrowseKey::Compare(selection.clone()), compare_fixture(selection, answer, source_available), cx,
    ));
}

/// The old native Compare return selected `from_str` instead of the clicked
/// `advance_signal`. Keep both names in immutable, exact package evidence so
/// that default-selection fallback cannot make the history test pass.
fn compare_history_fixture(selection: &CompareSet, insert_before: bool, omit_selected: bool) -> BrowseValue {
    let BrowseValue::Compare(base) = compare_fixture(selection, true, true) else { unreachable!() };
    let apis = selection.packages().iter().enumerate().map(|(column, package)| {
        let mut names = match column {
            0 => (0..26).map(|at| format!("function_{at:02}")).collect::<Vec<_>>(),
            1 => vec!["from_str".to_owned()],
            _ => Vec::new(),
        };
        if column == 0 {
            if !omit_selected { names.insert(0, "advance_signal".to_owned()); }
            if insert_before { names.insert(0, "aa_added".to_owned()); }
        }
        let items = names.into_iter().enumerate().map(|(at, name)| {
            let line = at as u32 + 1;
            ApiItem {
                decl: DeclRef::from_label(
                    &format!("{}::src/cadence.rs:{line}::{name}", package.as_str()),
                    None, Some(DeclarationKind::Function), Some(("src/cadence.rs", line)),
                ).expect("exact comparison declaration"),
                signature: Known::Known(SignatureText {
                    text: Arc::from(format!("pub fn {name}()")), tokens: Arc::from([]),
                    name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
                }),
                summary: None,
            }
        }).collect::<Vec<_>>();
        Known::Known(PackageApi { package: package.clone(), complete: true, items: items.into() })
    }).collect::<Vec<_>>();
    let prepared = Arc::new(crate::runtime::browse_views::prepare_compare(&base.packages, &apis));
    BrowseValue::Compare(Arc::new(CompareModel {
        packages: Arc::clone(&base.packages), apis: apis.into(), prepared,
    }))
}

fn land_history_compare(rig: &mut crate::shell::tests::Rig, selection: &CompareSet, insert_before: bool, omit_selected: bool) {
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(
        BrowseKey::Compare(selection.clone()), compare_history_fixture(selection, insert_before, omit_selected), cx,
    ));
}

/// `advance_signal` starts in slot 17 of the first 18-name page. An extra
/// exact name before it moves that same identity to slot 18 and the next
/// page; `from_str` remains a tempting but incorrect fallback detail.
fn compare_boundary_fixture(selection: &CompareSet, insert_before: bool, omit_selected: bool) -> BrowseValue {
    let BrowseValue::Compare(base) = compare_history_fixture(selection, false, false) else { unreachable!() };
    let mut apis = base.apis.to_vec();
    let Known::Known(first) = &mut apis[0] else { unreachable!() };
    let mut names = (0..17).map(|at| format!("aa_{at:02}")).collect::<Vec<_>>();
    if insert_before { names.push("aa_17".to_owned()); }
    if !omit_selected { names.push("advance_signal".to_owned()); }
    first.items = names.into_iter().enumerate().map(|(at, name)| {
        let line = at as u32 + 1;
        ApiItem {
            decl: DeclRef::from_label(
                &format!("{}::src/cadence.rs:{line}::{name}", first.package.as_str()),
                None, Some(DeclarationKind::Function), Some(("src/cadence.rs", line)),
            ).expect("boundary declaration"),
            signature: Known::Known(SignatureText {
                text: Arc::from(format!("pub fn {name}()")), tokens: Arc::from([]),
                name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
            }),
            summary: None,
        }
    }).collect::<Vec<_>>().into();
    let prepared = Arc::new(crate::runtime::browse_views::prepare_compare(&base.packages, &apis));
    BrowseValue::Compare(Arc::new(CompareModel { packages: Arc::clone(&base.packages), apis: apis.into(), prepared }))
}

fn land_boundary_compare(rig: &mut crate::shell::tests::Rig, selection: &CompareSet, insert_before: bool, omit_selected: bool) {
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(
        BrowseKey::Compare(selection.clone()), compare_boundary_fixture(selection, insert_before, omit_selected), cx,
    ));
}

#[gpui::test]
fn compare_source_callbacks_open_members_and_remove_only_from_the_current_reading(cx: &mut TestAppContext) {
    let selection = compare_selection();
    let current = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(current.clone()), 1200.0, 800.0);
    land_compare(&mut rig, &selection, true, true);
    let source = compare_callback(&mut rig, &selection);
    let open = super::compare_symbol_action(source.clone(), false);
    rig.cx.update(|window, cx| open("/fixture/present::glyph.rs:138::RelationLabel".into(), window, cx));
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(route) if route.id.as_str() == "/fixture/present::glyph.rs:138::RelationLabel"));

    rig.go(crate::navigation::Intent::Navigate(current.clone()));
    land_compare(&mut rig, &selection, true, true);
    let source = compare_callback(&mut rig, &selection);
    let open = super::compare_package_action(source);
    rig.cx.update(|window, cx| open("/fixture/second".into(), window, cx));
    rig.settle();
    assert!(matches!(rig.route(), Route::Package(route) if route.package.as_str() == "/fixture/second"));

    rig.go(crate::navigation::Intent::Navigate(current));
    land_compare(&mut rig, &selection, true, true);
    let source = compare_callback(&mut rig, &selection);
    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let remove = super::compare_remove_action(source, reader.downgrade());
    rig.cx.update(|window, cx| remove("/fixture/third".into(), window, cx));
    rig.settle();
    let expected = CompareSet::new(selection.packages()[..2].iter().cloned()).expect("two remaining packages");
    assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(expected))));
}

#[gpui::test]
fn compare_source_callbacks_deny_replaced_membership_selection_overlay_owner_and_root(cx: &mut TestAppContext) {
    let selection = compare_selection();
    let current = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(current.clone()), 1200.0, 800.0);
    land_compare(&mut rig, &selection, true, true);
    let source = compare_callback(&mut rig, &selection);
    let symbol = super::compare_symbol_action(source.clone(), false);
    let code = super::compare_symbol_action(source.clone(), true);
    let package = super::compare_package_action(source.clone());
    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let remove = super::compare_remove_action(source.clone(), reader.downgrade());
    let member = SharedString::from("/fixture/present::glyph.rs:138::RelationLabel");
    let invoke = |rig: &mut crate::shell::tests::Rig, expected: &Route| {
        rig.cx.update(|window, cx| symbol(member.clone(), window, cx));
        rig.cx.update(|window, cx| code(member.clone(), window, cx));
        rig.cx.update(|window, cx| package("/fixture/second".into(), window, cx));
        rig.cx.update(|window, cx| remove("/fixture/third".into(), window, cx));
        rig.cx.run_until_parked();
        assert_eq!(&rig.route(), expected, "an obsolete Compare callback cannot navigate");
    };

    land_compare(&mut rig, &selection, false, false);
    rig.cx.update(|window, cx| symbol(member.clone(), window, cx));
    rig.cx.update(|window, cx| code(member.clone(), window, cx));
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), current, "removed symbol membership cannot open Page or Code");
    land_compare(&mut rig, &selection, true, false);
    rig.cx.update(|window, cx| code(member.clone(), window, cx));
    rig.cx.run_until_parked();
    assert_eq!(rig.route(), current, "a signature without indexed source cannot open Code");

    let another = CompareSet::new(["/fixture/present", "/fixture/other", "/fixture/third"]
        .into_iter().map(|key| PackageRef::parse(key).expect("package"))).expect("other selection");
    rig.graph.store.update(rig.cx, |store, cx| store.test_land_browse(
        BrowseKey::Compare(selection.clone()), compare_fixture(&another, true, true), cx,
    ));
    invoke(&mut rig, &current);
    land_compare(&mut rig, &selection, true, true);

    let original = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let mut covered = original.session().clone();
    covered.overlay = Some(Overlay::Settings(SettingsPage::Appearance));
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_session(covered)), cx));
    invoke(&mut rig, &current);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::clone(&original), cx));

    let mut elsewhere = original.session().clone();
    elsewhere.route = Route::Orbit(OrbitRoute::Home);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_session(elsewhere.clone())), cx));
    invoke(&mut rig, &elsewhere.route);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::clone(&original), cx));
    rig.cx.run_until_parked();
    land_compare(&mut rig, &selection, true, true);

    rig.graph.store.update(rig.cx, |store, cx| store.owner_starting(cx));
    invoke(&mut rig, &current);
    rig.graph.store.update(rig.cx, |store, cx| store.owner_ready(cx));

    let newer = VersionedRoot::synthetic(backend_library::view_state_root(&[("compare".into(), "new root".into())]), 9);
    rig.graph.store.update(rig.cx, |store, cx| store.admit_snapshot(Arc::new(original.with_key(newer, None)), cx));
    invoke(&mut rig, &current);
}

#[gpui::test]
fn mounted_compare_native_choices_survive_back_forward_without_reusing_visit_actions(cx: &mut TestAppContext) {
    use crate::navigation::{Intent, presentation::ReadingPresentation};
    use gpui::{Modifiers, point, px};
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(route.clone()), 1440.0, 1400.0);
    rig.cx.update(|window, cx| { window.set_a11y_forced(true); facet::probe::enable(cx); });
    land_compare(&mut rig, &selection, true, true);
    rig.settle();
    for suffix in ["scope-1", "facts-toggle"] {
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        let target = ledger.targets.iter().find(|target| target.key.ends_with(suffix))
            .unwrap_or_else(|| panic!("native Compare control {suffix} absent: {:?}", ledger.targets));
        let at = point(px(target.bounds.x + target.bounds.width / 2.0), px(target.bounds.y + target.bounds.height / 2.0));
        rig.cx.simulate_click(at, Modifiers::none());
        rig.settle();
    }
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &before.presentation else { panic!("Compare visit") };
    assert_eq!(comparison.scope, Some(facet::browse::compare::Scope::Shared));
    assert!(comparison.facts, "actual native facts toggle changes visit intent");
    assert!(rig.said().iter().any(|words| words.contains("Source")), "expanded facts paint");
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.keys("secondary-[");
    assert_eq!(rig.route(), route);
    let restored = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    assert_eq!(restored.id, before.id);
    assert_eq!(restored.presentation, before.presentation);
    assert!(rig.said().iter().any(|words| words == "Hide package facts"), "Back remounts expanded native facts panel");
    rig.keys("secondary-]");
    assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Home));
    rig.keys("secondary-[");
    assert!(rig.said().iter().any(|words| words == "Hide package facts"));
}

#[gpui::test]
fn mounted_compare_distinct_selected_code_back_restores_the_exact_reading_visit(cx: &mut TestAppContext) {
    use crate::navigation::{Intent, View, presentation::ReadingPresentation};
    use facet::icons::Kind;
    use gpui::Modifiers;
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(route.clone()), 1440.0, 900.0);
    land_history_compare(&mut rig, &selection, false, false);
    rig.settle();
    let different = crate::shell::tests::native_bounds(&mut rig, "Button", "Different names", true)
        .expect("mounted Compare filter");
    rig.cx.simulate_click(different.center(), Modifiers::none());
    rig.settle();
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let selected_row = ledger.texts.iter().find(|text| text.content == "advance_signal" && text.key.contains("row-"))
        .expect("the exact aligned row is painted under Different names");
    let at = gpui::point(gpui::px(selected_row.bounds.x + selected_row.bounds.width / 2.0),
        gpui::px(selected_row.bounds.y + selected_row.bounds.height / 2.0));
    rig.cx.simulate_click(at, Modifiers::none());
    rig.settle();
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &before.presentation else { panic!("Compare visit") };
    assert_eq!(comparison.scope, Some(facet::browse::compare::Scope::Distinct));
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)));
    let code = crate::shell::tests::native_bounds(&mut rig, "Button", "Code", true)
        .expect("the selected exact declaration has its own native Code action");
    rig.cx.simulate_click(code.center(), Modifiers::none());
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(ref symbol)
        if symbol.view == View::Code && symbol.id.as_str().ends_with("::advance_signal")));
    rig.keys("secondary-[");
    assert_eq!(rig.route(), route);
    let returned = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    assert_eq!(returned, before, "Back restores the selected alignment and filter on the original VisitId");
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(ledger.texts.iter().any(|text| text.key.contains("focused-name") && text.content == "advance_signal"),
        "the detail must paint advance_signal, not the default from_str fallback");

    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.go(Intent::Navigate(route));
    land_history_compare(&mut rig, &selection, false, false);
    rig.settle();
    let fresh = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &fresh.presentation else { panic!("fresh Compare visit") };
    assert_ne!(fresh.id, before.id, "new navigation cannot reuse the returned visit's identity");
    assert_eq!(*comparison, facet::browse::compare::Presentation::default(),
        "a new Compare visit starts without another visit's selection or filter");
}

#[gpui::test]
fn compare_selected_identity_and_scroll_survive_reflow_and_back(cx: &mut TestAppContext) {
    use crate::navigation::{Intent, View, presentation::ReadingPresentation};
    use facet::icons::Kind;
    use gpui::{Modifiers, point, px};
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(route.clone()), 1440.0, 900.0);
    land_history_compare(&mut rig, &selection, false, false);
    rig.settle();
    let different = crate::shell::tests::native_bounds(&mut rig, "Button", "Different names", true).expect("filter");
    rig.cx.simulate_click(different.center(), Modifiers::none());
    rig.settle();
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let row = ledger.texts.iter().find(|text| text.content == "advance_signal" && text.key.contains("row-"))
        .expect("alignment row");
    rig.cx.simulate_click(point(px(row.bounds.x + row.bounds.width / 2.0),
        px(row.bounds.y + row.bounds.height / 2.0)), Modifiers::none());
    rig.settle();
    let visit = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id);
    land_history_compare(&mut rig, &selection, true, false);
    rig.settle();
    let after_insert = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &after_insert.presentation else { panic!("Compare") };
    assert_eq!(after_insert.id, visit);
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)),
        "a new immutable alignment order cannot turn a selected name into a row index");
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    assert!(rig.cx.update(|_, cx| facet::probe::take(cx)).texts.iter()
        .any(|text| text.key.contains("focused-name") && text.content == "advance_signal"));

    land_history_compare(&mut rig, &selection, true, true);
    rig.settle();
    let without = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &without.presentation else { panic!("Compare") };
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)),
        "a temporarily absent alignment must not erase the visit's semantic choice");
    land_history_compare(&mut rig, &selection, true, false);
    rig.settle();
    crate::shell::tests::wheel(rig.cx, point(px(1200.0), px(700.0)), point(px(0.0), px(-96.0)));
    rig.repaint();
    rig.settle();
    let saved = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let y = saved.presentation.controls().offset.pixels().1;
    assert!(y < -1.0, "native wheel moves the actual overflowing Reader viewport");
    let source = compare_callback(&mut rig, &selection);
    let code = super::compare_symbol_action(source, true);
    rig.cx.update(|window, cx| code("/fixture/present::src/cadence.rs:2::advance_signal".into(), window, cx));
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(ref symbol) if symbol.view == View::Code));
    rig.keys("secondary-[");
    rig.settle();
    assert_eq!(rig.route(), route);
    let restored = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    assert_eq!(restored.id, visit);
    assert_eq!(restored.presentation, saved.presentation,
        "Back restores filter, selected identity, and recorded scroll together");
    let actual = rig.shell.read_with(rig.cx, |shell, cx| f32::from(shell.source_reader_scroll_offset(cx).y));
    assert!((actual - y).abs() <= 2.0, "the mounted Reader must restore its measured viewport: {y} → {actual}");
}

#[gpui::test]
fn compare_follows_a_selected_name_across_the_page_boundary_without_overriding_explicit_pagination(cx: &mut TestAppContext) {
    use crate::navigation::presentation::ReadingPresentation;
    use facet::icons::Kind;
    use gpui::{Modifiers, point, px};
    let selection = compare_selection();
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection.clone())));
    let mut rig = rig(cx, Some(route), 1440.0, 2000.0);
    land_boundary_compare(&mut rig, &selection, false, false);
    rig.settle();
    let different = crate::shell::tests::native_bounds(&mut rig, "Button", "Different names", true).expect("native filter");
    rig.cx.simulate_click(different.center(), Modifiers::none());
    rig.settle();
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let row = ledger.texts.iter().find(|text| text.content == "advance_signal" && text.key.contains("row-"))
        .expect("selected row is actually rendered at slot 17 on page one");
    assert!(ledger.texts.iter().any(|text| text.key.contains("page-reading") && text.content == "1–18 of 19 recorded names"));
    rig.cx.simulate_click(point(px(row.bounds.x + row.bounds.width / 2.0),
        px(row.bounds.y + row.bounds.height / 2.0)), Modifiers::none());
    rig.settle();
    let original = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &original.presentation else { panic!("Compare") };
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)));
    assert!(comparison.follow_selected);
    assert_eq!(comparison.scope, Some(facet::browse::compare::Scope::Distinct));

    land_boundary_compare(&mut rig, &selection, true, false);
    rig.settle();
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let shifted = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(shifted.texts.iter().any(|text| text.key.contains("page-reading") && text.content == "19–20 of 20 recorded names"),
        "insertion before slot 17 must move the selected name to the second actual page");
    assert!(shifted.texts.iter().any(|text| text.key.contains("row-") && text.content == "advance_signal"));
    assert!(shifted.texts.iter().any(|text| text.key.contains("focused-name") && text.content == "advance_signal"),
        "the detail follows the typed name and kind, never its previous numeric row");
    let shifted_visit = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    assert_eq!(shifted_visit.id, original.id, "immutable data reflow is not navigation");
    let ReadingPresentation::Compare { comparison, .. } = &shifted_visit.presentation else { panic!("Compare") };
    assert_eq!(comparison.scope, Some(facet::browse::compare::Scope::Distinct));
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)));

    land_boundary_compare(&mut rig, &selection, true, true);
    rig.settle();
    let absent = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &absent.presentation else { panic!("Compare") };
    assert_eq!(comparison.selected, Some(("advance_signal".into(), Kind::Function)),
        "temporary absence cannot silently rewrite the visit's semantic selection");
    land_boundary_compare(&mut rig, &selection, true, false);
    rig.settle();
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let returned = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(returned.texts.iter().any(|text| text.key.contains("focused-name") && text.content == "advance_signal"));
    assert!(returned.texts.iter().any(|text| text.key.contains("page-reading") && text.content == "19–20 of 20 recorded names"));

    let previous = crate::shell::tests::native_bounds(&mut rig, "Button", "Previous names", true)
        .expect("a real native paginator on the followed second page");
    rig.cx.simulate_click(previous.center(), Modifiers::none());
    rig.settle();
    let explicit = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    let ReadingPresentation::Compare { comparison, .. } = &explicit.presentation else { panic!("Compare") };
    assert_eq!(explicit.id, original.id);
    assert_eq!(comparison.page, 0);
    assert!(!comparison.follow_selected, "Previous names is a deliberate page choice, not a request to follow the old row");
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let first_page = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(first_page.texts.iter().any(|text| text.key.contains("page-reading") && text.content == "1–18 of 20 recorded names"));
    assert!(!first_page.texts.iter().any(|text| text.key.contains("focused-name") && text.content == "advance_signal"),
        "explicit pagination must not be undone by the previous selection");
}
