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

use crate::core::LocalProjectId;
use crate::model::pages::PackageRef;
use crate::navigation::{BrowseRoute, OrbitRoute, Route};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::shell::tests::rig_with_reads;
use backend_client::Session;
use backend_library::{
    MAX_PACKAGE_GRAPH_PAGE_ROWS, PackageGraphDirection, PackageGraphPageRequest,
    PackageGraphPageTerminal, SurfaceCommand, SurfaceReply,
};
use gpui::TestAppContext;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    // `/tmp`, not `temp_dir()`: a long socket path exceeds `sockaddr_un`.
    let state = PathBuf::from("/tmp").join(format!("nx-browse-{nonce}"));
    let endpoint = PathBuf::from("/tmp").join(format!("nx-browse-{nonce}.sock"));
    std::fs::create_dir_all(state.join("data")).expect("workspace");
    let service = start_owner(&endpoint, &state);
    (service, endpoint, state)
}

fn start_owner(endpoint: &Path, state: &Path) -> backend_local_service::EmbeddedLocalService {
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(repository()),
        Some(state.join("data")),
        Some(endpoint.to_path_buf()),
    )
    .expect("workspace paths");
    paths.initialize().expect("initialize");
    let advisory =
        repository().join("crates/advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md");
    let arguments = [
        "--endpoint",
        endpoint.to_str().expect("utf-8"),
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
    backend_local_service::EmbeddedLocalService::start(config).expect("embedded owner")
}

fn tree_route(root: &Path) -> Route {
    Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(
        LocalProjectId::from_path(root).expect("project"),
    )))
}

fn package_graph_route(
    package: PackageRef,
    direction: backend_library::PackageGraphDirection,
) -> Route {
    Route::Orbit(OrbitRoute::Browse(BrowseRoute::PackageGraph {
        package,
        direction,
        authority: None,
        cursor: None,
    }))
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
    let mut rig = rig_with_reads(cx, Some(tree_route(&fixture)), 1440.0, 900.0, pool);
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

#[gpui::test]
fn a_registry_package_graph_keeps_exact_evidence_through_keyboard_and_resize(
    cx: &mut TestAppContext,
) {
    let (service, endpoint, state) = owner();
    let fixture = PackageRef::parse("pkg:cargo/toml@0.8.23").expect("exact registry release");
    let graph_package = backend_library::PackageReference::parse(fixture.as_str())
        .expect("exact graph package reference");
    let mut session = Session::connect(&endpoint).expect("session");
    let request = PackageGraphPageRequest::new(
        graph_package,
        PackageGraphDirection::Dependencies,
        None,
        MAX_PACKAGE_GRAPH_PAGE_ROWS,
    )
    .expect("bounded graph request");
    let SurfaceReply::PackageGraphPage(before_reopen) = session
        .surface(SurfaceCommand::PackageGraphPage {
            request: request.clone(),
        })
        .expect("live Turso graph read")
    else {
        panic!("package graph answered another reply");
    };
    assert!(
        !before_reopen.rows.is_empty(),
        "the pinned toml release has recorded graph edges"
    );
    assert!(
        before_reopen.source.is_some(),
        "the registry authority is exact"
    );
    assert_eq!(before_reopen.terminal, PackageGraphPageTerminal::Complete);
    drop(session);
    drop(service);
    // The owner reopens the same Turso database. The view root and facts
    // witness select the committed graph snapshot that the desktop reads.
    let _reopened = start_owner(&endpoint, &state);
    let pool = ReadPool::start(2, move |_| SessionReader::connect(&endpoint)).expect("read pool");
    let mut rig = rig_with_reads(
        cx,
        Some(package_graph_route(
            fixture,
            backend_library::PackageGraphDirection::Dependencies,
        )),
        1440.0,
        900.0,
        pool,
    );
    let started = Instant::now();
    let mut said = rig.said();
    while !said
        .iter()
        .any(|line| line.starts_with("Graph snapshot · root "))
    {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the package graph never landed: {said:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
        rig.settle();
        said = rig.said();
    }
    for expected in [
        "toml",
        "pkg:cargo/toml@0.8.23",
        "Dependencies",
        "Ecosystem · cargo",
        "Release · 0.8.23",
    ] {
        assert!(
            said.iter()
                .any(|line| line == expected || line.starts_with(expected)),
            "{expected:?} is not on screen: {said:#?}"
        );
    }
    assert!(
        said.iter()
            .any(|line| line.starts_with("Selected exact source authority · ")),
        "the authority stays explicit: {said:#?}"
    );
    let address = rig.graph.store.read_with(rig.cx, |store, _| {
        crate::shell::jump::address_parts(&store.snapshot()).full()
    });
    assert!(
        address.contains("pkg%3Acargo%2Ftoml%400.8.23"),
        "exact release is in the address: {address}"
    );

    // The page owns its keyboard focus: right and left move between graph
    // directions while retaining this exact package coordinate.
    rig.keys("x");
    rig.keys("right");
    assert!(matches!(
        rig.route(),
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::PackageGraph {
            direction: backend_library::PackageGraphDirection::Dependents,
            ..
        }))
    ));
    rig.keys("left");
    assert!(matches!(
        rig.route(),
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::PackageGraph {
            direction: backend_library::PackageGraphDirection::Dependencies,
            ..
        }))
    ));

    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.cx
        .simulate_resize(gpui::size(gpui::px(480.0), gpui::px(900.0)));
    rig.go(crate::navigation::Intent::ZoomTo {
        display,
        percent: 125,
    });
    let said = rig.said();
    assert!(
        said.iter().any(|line| line == "toml"),
        "package header survives narrow text layout: {said:#?}"
    );
    assert!(
        said.iter()
            .any(|line| line.starts_with("Graph snapshot · root ")),
        "snapshot evidence survives narrow layout: {said:#?}"
    );
    drop(rig);
    drop(_reopened);
    let _ = std::fs::remove_dir_all(state);
}
