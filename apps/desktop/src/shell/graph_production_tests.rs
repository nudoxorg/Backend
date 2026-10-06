//! Local native presentation over bytes admitted by the actual indexed owner.
#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use crate::core::{LocalProjectId, VersionedRoot};
use crate::host::registry;
use crate::model::ServiceMode;
use crate::navigation::{Intent, Overlay, Route, View};
use crate::runtime::client::LocalEngineClient;
use crate::runtime::indexed_world::{self, Origin, State, TestProjectionGate};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::shell::tests::{Rig, native_bounds, rig_with_production_owner, wheel};
use backend_client::Session;
use backend_local_service::{EmbeddedLocalService, ProcessConfig};
use backend_runtime::WorkspacePaths;
use gpui::{AppContext as _, Focusable as _, Modifiers, TestAppContext, point, px};
use gpui_component::WindowExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const CHILD: &str = "NUDOX_TEST_RETAINED_PRODUCTION_CHILD";
const NAME: &str = "shell::graph_tests::production::production_composition_loss_preserves_local_native_graph_without_resource_authority";
const RECEIPT: &str = "RETAINED-PRODUCTION: IndexedOwner revision, withdrawal, local native Graph/Source, stale reply and reconnect";

#[test]
fn production_composition_loss_preserves_local_native_graph_without_resource_authority() {
    // The serving registry is process-global. An isolated test executable
    // exercises its ordinary production path without a fixture projection.
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1").output().expect("isolated production test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "production native test failed: {stdout}\n{stderr}");
        assert!(stdout.contains(RECEIPT), "exact production test receipt: {stdout}\n{stderr}");
        return;
    }
    let mut fixture = OwnerFixture::start();
    exercise(&fixture);
    fixture.service.close().expect("close actual private service");
    std::fs::remove_dir_all(&fixture.scratch).expect("remove owned fixture");
    println!("{RECEIPT}");
}

struct OwnerFixture {
    service: EmbeddedLocalService,
    paths: WorkspacePaths,
    project: PathBuf,
    scratch: PathBuf,
    root: VersionedRoot,
}

impl OwnerFixture {
    fn start() -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .expect("clock").as_nanos();
        let scratch = crate::host::scratch_base().join(format!("nx-retain-{}-{nonce}", std::process::id()));
        crate::host::private_dir(&scratch).expect("owned private workspace");
        let project = scratch.join("source");
        std::fs::create_dir_all(project.join("src")).expect("owned project");
        std::fs::write(project.join("Cargo.toml"),
            "[package]\nname = \"local-native-keep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("manifest");
        std::fs::write(project.join("src/lib.rs"),
            "pub struct LocalKeep;\npub fn keep_local(value: u32) -> u32 {\n    value + 1\n}\n").expect("source");
        crate::host::private_dir(&scratch.join("data")).expect("owner state");
        let paths = WorkspacePaths::discover(Some(project.clone()), Some(scratch.join("data")),
            Some(scratch.with_extension("sock"))).expect("owner paths");
        paths.initialize().expect("authenticated owner paths");
        let mut config = ProcessConfig::parse([
            "--endpoint", paths.endpoint().to_str().expect("endpoint"),
            "--workspace", paths.data().to_str().expect("workspace"),
            "--authority-secret-file", paths.authority_secret().to_str().expect("secret path"),
            "--profile", "builtin", "--registry-offline", "--registry-discovery-offline",
            "--advisory-offline", "--forge-offline",
        ].map(ToOwned::to_owned)).expect("ordinary offline service configuration");
        config.compiler_environment = Some(crate::host::toolchain::prepared_by_the_process()
            .expect("captured actual compiler environment"));
        let service = EmbeddedLocalService::start(config).expect("real embedded service");
        let mut proof = Session::connect(paths.endpoint()).expect("actual authenticated session");
        proof.index(project.to_str().expect("source path")).expect("index actual owned source");
        let revision = proof.revision().expect("certified indexed revision");
        let root = VersionedRoot::from_revision(1, revision.cursor(), 0);
        Self { service, paths, project, scratch, root }
    }
}

fn projection(rig: &mut Rig) -> (indexed_world::Key, Origin) {
    rig.shell.read_with(rig.cx, |shell, cx| shell.reader_entity().read(cx)
        .graph_projection_evidence(cx)).expect("painted production projection")
}

fn settings_round_trip(rig: &mut Rig) {
    rig.keys("secondary-,");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| matches!(
        store.snapshot().page_overlay(), Some(Overlay::Settings(_)))), "Settings actually mounted before dismissal");
    assert!(native_bounds(rig, "Heading", "Appearance", false).is_some(), "actual settled Settings body");
    rig.keys("escape");
}

fn current_input(rig: &mut Rig) -> gpui::Entity<gpui_component::input::InputState> {
    rig.cx.update(|window, cx| {
        assert!(window.has_focused_input(cx), "currently mounted native editor owns typing");
        window.focused_input(cx).and_then(|input| input.as_input().cloned()).expect("native input engine")
    })
}

fn exercise(fixture: &OwnerFixture) {
    let mut lease = Some(registry::publish_serving(fixture.paths.endpoint(), fixture.paths.data(), None, None));
    let gate = OwnerGate::ready(fixture.root, ServiceMode::Attached);
    let endpoint = fixture.paths.endpoint().to_path_buf();
    let read_gate = gate.clone();
    let pool = ReadPool::start(2, move |_| SessionReader::gated(&endpoint, read_gate.clone())).expect("real read pool");
    let engine = LocalEngineClient::gated(fixture.paths.endpoint(),
        LocalProjectId::from_path(&fixture.project).expect("actual project id"), gate.clone());
    let mut cx = TestAppContext::single();
    let mut rig = rig_with_production_owner(&mut cx, pool, engine, gate.clone(), fixture.root);
    rig.go(Intent::Navigate(Route::World));
    let (old_key, origin) = projection(&mut rig);
    assert_eq!(origin, Origin::IndexedOwner, "no TestProjection installation");
    assert!(old_key.at_authority(fixture.root), "the actual service revision owns these bytes");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key().same_authority(fixture.root)),
        "ordinary shell observations retain the actual certified producer revision");
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("actual indexed graph");
    assert!(graph.read_with(rig.cx, |graph, _| graph.world().nodes.iter().any(|node| node.name.as_ref() == "keep_local")),
        "the real compiled declaration was painted");
    let fitted = graph.read_with(rig.cx, |graph, _| graph.camera().expect("laid out camera"));
    let painted_canvas = native_bounds(&mut rig, "Group", "Graph", false).expect("actual indexed native canvas");
    // The provenance panel occupies the right half. Use native Group bounds
    // to choose the lower-left canvas, away from Find and diagnostic chrome.
    let viewport_point = point(painted_canvas.origin.x + painted_canvas.size.width * 0.30,
        painted_canvas.origin.y + painted_canvas.size.height * 0.75);
    wheel(rig.cx, viewport_point, point(px(30.0), px(15.0)));
    rig.settle();
    let camera = graph.read_with(rig.cx, |graph, _| graph.camera().expect("actual resource-enabled wheel camera"));
    assert_ne!(camera, fitted, "positive native wheel at the exact later stale-frame point");

    // Delay an actual successful projection, after real rows were assembled.
    // This gate supplies no root, facts, Ready state, or composition receipt.
    let delivery = Arc::new(TestProjectionGate::default());
    rig.cx.update(|_, cx| {
        indexed_world::install_test_production_delivery(&old_key, delivery.clone(), cx);
        indexed_world::invalidate_publication(fixture.root.authority(), cx);
    });
    let requester = rig.cx.new(|_| ());
    let held_key = requester.update(rig.cx, |_, cx| {
        let key = indexed_world::key(fixture.root, old_key.preferred().cloned(), cx).expect("real serving key");
        assert!(matches!(indexed_world::get(&key, cx), State::Reading));
        key
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    while !delivery.entered() {
        rig.cx.run_until_parked();
        assert!(Instant::now() < deadline, "actual production projection must reach its delivery gate");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(delivery.production_rows().is_some_and(|rows| rows > 0), "held value contains actual indexed rows");

    let attachment = gate.attached_ready_epoch().expect("actual attached epoch");
    assert!(gate.attached_lost_at(attachment, "attached service unavailable in private fixture".into()));
    assert!(registry::serving_composed().is_some(), "a real endpoint registration can outlive its attached read capability");
    // The actual old canvas is still painted here. Withdrawal cannot let a
    // resource-enabled old frame adopt the later local-only admission.
    wheel(rig.cx, viewport_point, point(px(80.0), px(35.0)));
    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.camera()), Some(camera), "stale resource-enabled native wheel is refused before repaint");
    rig.settle();
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("attachment-loss retained scene").entity_id(), graph.entity_id());
    assert!(native_bounds(&mut rig, "Status", "Earlier indexed graph · local exploration available; the index connection is unavailable.", false).is_some(),
        "a retained real scene does not claim current coverage merely because the endpoint remains registered");
    let mut renewal = Session::connect(fixture.paths.endpoint()).expect("actual still-registered service");
    let revision = renewal.revision().expect("actual service recertifies attachment renewal");
    let renewed_root = VersionedRoot::from_revision(1, revision.cursor(), 0);
    assert!(renewed_root.same_authority(fixture.root));
    gate.publish(OwnerState::Ready { key: renewed_root, mode: ServiceMode::Attached });
    rig.settle();
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("renewed same-generation scene").entity_id(), graph.entity_id());
    assert!(native_bounds(&mut rig, "Button", "Declarations", true).is_some(), "a certified attachment renewal re-admits resource controls");
    assert!(native_bounds(&mut rig, "Status", "Earlier indexed graph · local exploration available; the index connection is unavailable.", false).is_none(),
        "same-generation certified renewal clears the earlier notice");
    assert!(gate.attached_lost_at(gate.attached_ready_epoch().expect("renewed attachment"), "actual renewed attachment withdrawn".into()));
    rig.settle();
    drop(lease.take());
    assert!(registry::serving_composed().is_none(), "actual registry generation withdrawn");
    assert!(rig.cx.update(|_, cx| old_key.serving_owner(cx).is_none()), "resource capability retired immediately");
    rig.settle();
    let retained = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("already painted immutable graph survives");
    assert_eq!(retained.entity_id(), graph.entity_id(), "no fabricated replacement scene");
    assert_eq!(retained.read_with(rig.cx, |graph, _| graph.camera()), Some(camera));
    assert_eq!(projection(&mut rig).1, Origin::IndexedOwner);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment().is_none()));

    rig.cx.update(|window, cx| retained.focus_handle(cx).focus(window, cx));
    rig.settle();
    settings_round_trip(&mut rig);
    assert!(rig.cx.update(|window, cx| retained.focus_handle(cx).is_focused(window)), "exact local Graph-root origin returns without a serving owner");
    rig.keys("secondary-,");
    assert!(native_bounds(&mut rig, "Heading", "Appearance", false).is_some());
    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
    rig.cx.update(|_, cx| links.dispatch(Intent::DismissOverlay, cx));
    // The reducer/subscribers have completed. This is an ordinary later
    // platform blur before the destination's next frame, not an event
    // interleaved into synchronous overlay reduction.
    rig.cx.update(|window, _| window.blur());
    rig.settle();
    assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()), "local Graph return cannot undo a later real blur");
    rig.cx.update(|window, cx| retained.focus_handle(cx).focus(window, cx));
    rig.settle();
    rig.keys("/");
    let find = current_input(&mut rig);
    rig.cx.simulate_input("keep_local");
    rig.settle();
    assert_eq!(find.read_with(rig.cx, |input, _| input.value().to_string()), "keep_local");
    rig.keys("enter");
    assert_eq!(rig.route(), Route::World, "Find Return cannot open a resource while its owner is absent");
    assert!(retained.read_with(rig.cx, |graph, _| graph.focused().is_none()), "no semantic selection through missing authority");
    rig.keys("escape");
    let canvas = native_bounds(&mut rig, "Group", "Graph", false).expect("actual retained native canvas");
    let from = point(canvas.origin.x + canvas.size.width * 0.30, canvas.origin.y + canvas.size.height * 0.75);
    let to = from + point(px(90.0), px(45.0));
    eprintln!("before wheel at {from:?} within {canvas:?}: {}", rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    wheel(rig.cx, from, point(px(40.0), px(20.0)));
    rig.settle();
    let wheeled = retained.read_with(rig.cx, |graph, _| graph.camera().expect("current local wheel camera"));
    assert_ne!(wheeled, camera, "current local-only native wheel remains available: {}",
        rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    rig.cx.simulate_event(gpui::PinchEvent { position: from, delta: 0.2,
        modifiers: Modifiers::none(), phase: gpui::TouchPhase::Moved });
    rig.settle();
    let zoomed = retained.read_with(rig.cx, |graph, _| graph.camera().expect("current local pinch camera"));
    assert!(zoomed.w < wheeled.w, "native pinch zoom survives keyboard modality over the retained local scene");
    eprintln!("before drag: {}", rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    rig.cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::none());
    eprintln!("after Down: {}", rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    rig.cx.simulate_mouse_move(to, Some(gpui::MouseButton::Left), Modifiers::none());
    eprintln!("after Move: {}", rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    rig.cx.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::none());
    eprintln!("after Up: {}", rig.cx.update(|window, cx| retained.read(cx).native_focus_diagnostic(window, cx)));
    rig.settle();
    let dragged = retained.read_with(rig.cx, |graph, _| graph.camera().expect("retained local camera"));
    assert_ne!(dragged, zoomed, "native drag pans the actual retained scene");
    let panned = dragged;
    assert_eq!(rig.route(), Route::World);
    for inactive in [false, true] {
        let canvas = native_bounds(&mut rig, "Group", "Graph", false).expect("current local canvas");
        let from = point(canvas.origin.x + canvas.size.width * 0.30, canvas.origin.y + canvas.size.height * 0.75);
        let to = from + point(px(110.0), px(60.0));
        rig.cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::none());
        if inactive { rig.cx.deactivate_window(); }
        rig.cx.update(|window, _| window.blur());
        rig.cx.simulate_mouse_move(to, Some(gpui::MouseButton::Left), Modifiers::none());
        rig.cx.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::none());
        rig.settle();
        assert_eq!(retained.read_with(rig.cx, |graph, _| graph.camera()), Some(panned), "later blur/inactive window cancels the local Down lease");
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()));
        rig.cx.update(|window, _| window.activate_window());
    }

    delivery.release();
    let deadline = Instant::now() + Duration::from_secs(2);
    while delivery.reads_returned() != delivery.reads_entered() {
        rig.cx.run_until_parked();
        assert!(Instant::now() < deadline, "held real successful projection actually returns after retirement");
        std::thread::sleep(Duration::from_millis(2));
    }
    rig.cx.run_until_parked();
    assert!(!rig.cx.update(|_, cx| indexed_world::read_completed(&held_key, cx)), "held real reply cannot republish into the retired owner");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("retained scene").entity_id(), graph.entity_id());
    lease = Some(registry::publish_serving(fixture.paths.endpoint(), fixture.paths.data(), None, None));
    let mut proof = Session::connect(fixture.paths.endpoint()).expect("real reconnect");
    let revision = proof.revision().expect("actual unchanged service revision");
    let resumed_root = VersionedRoot::from_revision(1, revision.cursor(), 0);
    assert!(resumed_root.same_authority(fixture.root));
    gate.publish(OwnerState::Ready { key: resumed_root, mode: ServiceMode::Attached });
    rig.settle();
    let resumed = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("new composition projection");
    assert_ne!(resumed.entity_id(), retained.entity_id(), "new serving generation replaces once");
    assert_eq!(projection(&mut rig).1, Origin::IndexedOwner);
    assert_eq!(resumed.read_with(rig.cx, |graph, _| graph.camera()), Some(panned), "exact scene geometry retains local camera");
    rig.settle();
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("stable new scene").entity_id(), resumed.entity_id());
    rig.cx.update(|window, cx| resumed.focus_handle(cx).focus(window, cx));
    rig.settle();
    rig.keys("/");
    assert_eq!(current_input(&mut rig).read_with(rig.cx, |input, _| input.value().to_string()), "keep_local", "same visit retains the native Find draft through reconnect");
    // Obtain the exact source identity from the admitted production projection.
    let new_key = projection(&mut rig).0;
    let source = requester.update(rig.cx, |_, cx| match indexed_world::get(&new_key, cx) {
        State::Ready(projection) => {
            assert_eq!(projection.origin, Origin::IndexedOwner);
            let node = projection.world.nodes.iter().position(|node| node.name.as_ref() == "keep_local").expect("actual declaration");
            projection.identities.exact_node(node as u32).expect("exact compiler source coordinate")
        }
        _ => panic!("completed production projection"),
    });
    let code = crate::shell::kit::symbol_view_route(source.package.as_str(), &source.symbol,
        View::Code, source.line).expect("exact source route");
    rig.keys("enter");
    assert!(resumed.read_with(rig.cx, |graph, _| graph.focused().is_some()),
        "the reconnected owner admits the real Find result");
    rig.keys("enter");
    let page = crate::shell::kit::symbol_view_route(source.package.as_str(), &source.symbol,
        View::Page, source.line).expect("exact page route");
    assert_eq!(rig.route(), page, "native Graph Return opens the exact real compiler coordinate");
    let code_button = native_bounds(&mut rig, "Button", "Show Code view", true).expect("actual indexed Page code action");
    rig.cx.simulate_click(code_button.center(), Modifiers::none());
    rig.settle();
    assert_eq!(rig.route(), code, "native Page action opens current real source bytes");
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    rig.cx.update(|window, cx| assert!(targets.focus_native("source-jump-field", window, cx)));
    rig.draw_frame();
    let _input = current_input(&mut rig);
    rig.cx.simulate_input("2");
    rig.settle();
    drop(lease.take());
    assert!(gate.attached_lost_at(gate.attached_ready_epoch().expect("resumed attached epoch"), "second production withdrawal".into()));
    rig.settle();
    let input = current_input(&mut rig);
    rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all("3", window, cx)));
    rig.settle();
    assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "3", "retained actual source editor remains locally writable");
    let before_return = rig.said();
    rig.keys("enter");
    assert_eq!(rig.route(), code, "unavailable source Return cannot perform a semantic jump");
    assert_eq!(rig.said(), before_return, "a valid local source line cannot change range or show a jump error through retired authority");
    assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(), Some("source-jump-field"));
    settings_round_trip(&mut rig);
    let returned = current_input(&mut rig);
    rig.cx.simulate_input("4");
    rig.settle();
    assert_eq!(returned.read_with(rig.cx, |input, _| input.value().to_string()), "34", "Settings returns typing to the current retained source editor");
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.current_owner_attachment().is_none());
        assert_eq!(store.snapshot().session().reading.current.presentation.controls().source_line_draft.as_str(), "34");
    });
    drop(requester);
    drop(rig);
    gate.close();
}
