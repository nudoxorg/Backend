/// Native GPU evidence for the same held-read fixture as the state regression.
/// Session owns an invisible window, real text system and renderer; these are
/// actual pixels and AccessKit trees, not a drawing of the trace rectangles.
#[test]
#[ignore = "native offscreen films: NUDOX_TRANSIT_FILM=DIR NUDOX_TRANSIT_REVISION=SHA"]
fn native_pending_reversal_and_resize_film() {
    run_native_transit_film(false);
}

/// Apply this test file alone to the 6f38 product for the negative control.
/// It keeps that product's original seven-frame held-read route sequence and
/// reaches the first resize without assuming the new pending-read policy.
/// All actual native runs and pixels are saved before the duplicate assertion.
#[test]
#[ignore = "native compositor negative control: same film env, test-only file atop unfixed product"]
fn native_first_resize_compositor_control() {
    run_native_transit_film(true);
}

fn run_native_transit_film(control: bool) {
    use crate::navigation::Intent;
    use crate::runtime::{DesktopRuntime, EngineActor, UiEntityGraph};
    use gpui::AppContext as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use backend_gui_harness::{Session, SessionOptions, Quiet, Viewport};

    let out = std::path::PathBuf::from(std::env::var_os("NUDOX_TRANSIT_FILM")
        .expect("native film output directory"));
    let revision = std::env::var("NUDOX_TRANSIT_REVISION").expect("exact compiled source revision");
    std::fs::create_dir_all(&out).expect("native film directory");
    let gate = crate::runtime::owner::OwnerGate::starting();
    let held = gate.clone();
    let (entered, received) = std::sync::mpsc::channel();
    let pool = crate::runtime::reads::ReadPool::start(2, move |_| HeldDestination {
        gate: held.clone(), entered: entered.clone(),
    }).expect("real native film read workers");
    let handles = Rc::new(RefCell::new(None));
    let mounted = handles.clone();
    let mut session = Session::open(Viewport::new(1440, 900, 1).expect("viewport"),
        SessionOptions {
            asset_source: Arc::new(facet::icons::Assets),
            frame_ms: 16,
            capture_native_accessibility: true,
        }, move |window, cx| {
            gpui_component::init(cx);
            facet::fonts::install(cx).expect("actual bundled fonts");
            cx.bind_keys(crate::shell::keys::bindings());
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
            cx.set_global(InkTrace::default());
            let key = crate::core::VersionedRoot::synthetic(
                backend_library::view_state_root(&[("transit".to_owned(), "native film".to_owned())]), 4);
            let mut state = crate::model::SessionState::default();
            state.route = crate::shell::tests::page_route("RelationLabel");
            let mut snapshot = crate::model::AppSnapshot::empty(key).with_session(state);
            let directory = std::env::temp_dir().join(format!("nudox-transit-native-{}", std::process::id()));
            std::fs::create_dir_all(&directory).expect("private fixture project directory");
            let mut workspace = snapshot.workspace().clone();
            workspace.host = crate::core::LocalProjectId::from_path(&directory).ok();
            snapshot = snapshot.with_workspace(workspace);
            let actor = EngineActor::start(crate::shell::tests::RootOnly, 8).expect("real actor");
            let graph = UiEntityGraph::install_with_reads(cx, DesktopRuntime::new(snapshot, actor), None, Some(pool));
            let shell = crate::shell::open_shell(&graph, window, cx);
            let closing = crate::host::close::GracefulClose::install(&graph, cx);
            crate::host::close::GracefulClose::attach(&closing, window, cx);
            let view = cx.new(|cx| crate::host::close::CloseView::new(shell.clone(), closing, cx));
            *mounted.borrow_mut() = Some((graph, shell));
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        }).expect("native renderer-backed private window");
    let (graph, shell) = handles.borrow_mut().take().expect("actual mounted shell");
    let store = graph.store.clone();
    let root = graph.root.clone();
    session.set_quiet(Some(Quiet {
        check: Box::new(move |cx| store.read(cx).pool_activity().is_idle()
            && store.read(cx).symbol(&crate::shell::tests::symbol("RelationLabel")).is_loaded()
            && !root.read(cx).has_pending_work()),
        deadline: Duration::from_secs(20),
    }));
    session.quiesce().expect("real initial read settled");
    let key = session.update(|_, cx| graph.store.read(cx).snapshot().key()).expect("owner key");
    let release = ReleaseDestination(gate, key);
    let capture = |session: &mut backend_gui_harness::Session, name: &str| {
        let name = if control { format!("control-{name}") } else { name.to_owned() };
        capture_native_transit(session, &shell, &out, &revision, &name);
    };
    session.update(|_, cx| {
        let display = shell.read(cx).display_key();
        graph.root.update(cx, |root, cx| root.queue(Intent::ZoomTo { display, percent: 200 }, cx));
    }).expect("real text-size intent");
    session.advance_to(1000);
    capture(&mut session, "rest-200");
    session.set_quiet(None);
    session.update(|_, cx| graph.root.update(cx, |root, cx| root.queue(
        Intent::Navigate(crate::shell::tests::page_route("TransitHeldDestination")), cx)))
        .expect("native held destination navigation");
    capture(&mut session, "pending-000");
    received.recv_timeout(Duration::from_secs(1)).expect("real read entered before film");
    session.advance_to(1112);
    capture(&mut session, "pending-112");
    session.apply(&backend_gui_harness::Act::Key { chord: "secondary-[".to_owned() },
        &mut |_, _, _| {}).expect("native Back keystroke");
    session.advance_to(1128);
    capture(&mut session, "reverse-128");
    session.resize(1000, 700).expect("native private window resize");
    capture(&mut session, "resize-first-128");
    session.advance_to(1144);
    capture(&mut session, "resize-144");
    drop(release);
    session.advance_to(2000);
    capture(&mut session, "back-settled");

    if control { return; }

    // Cancellation acceptance cannot stand in for an actual moving reversal.
    // Re-open through the real now-released worker, then interrupt its plate
    // and resize its live parent on the first same-clock frame at 200%.
    session.resize(1440, 900).expect("ready-film full viewport");
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-rest-200");
    session.update(|_, cx| graph.root.update(cx, |root, cx| root.queue(
        Intent::Navigate(crate::shell::tests::page_route("TransitHeldDestination")), cx)))
        .expect("real ready-destination visit");
    let ready_store = graph.store.clone();
    let ready_root = graph.root.clone();
    session.set_quiet(Some(Quiet {
        check: Box::new(move |cx| ready_store.read(cx).symbol(
            &crate::shell::tests::symbol("TransitHeldDestination")).is_loaded()
            && !ready_root.read(cx).has_pending_work()),
        deadline: Duration::from_secs(20),
    }));
    session.quiesce().expect("the real destination answers before its opening frame");
    session.set_quiet(None);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-open-000");
    let p0 = session.update(|_, cx| shell.read(cx).reader_entity().read(cx).transit
        .as_ref().map(|transit| transit.carry.value(facet::motion::now(cx))))
        .expect("native ready driver").expect("actual Ready starts a plate");
    assert!(p0 < 0.1, "the real opening begins on its first frame, p={p0}");
    session.advance_to(2112);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-open-112");
    let before = session.update(|_, cx| shell.read(cx).reader_entity().read(cx).transit
        .as_ref().expect("actual opening still moves").carry.value(facet::motion::now(cx)))
        .expect("painted native opening value");
    session.apply(&backend_gui_harness::Act::Key { chord: "secondary-[".to_owned() },
        &mut |_, _, _| {}).expect("native Back interrupts a real opening");
    session.advance_to(2128);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-reverse-128");
    let after = session.update(|_, cx| shell.read(cx).reader_entity().read(cx).transit
        .as_ref().expect("actual reversal owns the same carry").carry.value(facet::motion::now(cx)))
        .expect("native reversing value");
    assert!((after - before).abs() < 0.12, "native reversal preserves its painted carry: {before} -> {after}");
    session.resize(1000, 700).expect("actual first-frame resize during Ready reversal");
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-resize-first-128");
    session.advance_to(2144);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-resize-144");
    session.advance_to(3000);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-back-settled");
    session.update(|window, cx| assert_eq!(window.simulate_next_frame(cx), 0,
        "the fully settled native film leaves no motion frame requested"))
        .expect("native settled wake proof");
}

fn capture_native_transit(session: &mut backend_gui_harness::Session,
    shell: &gpui::Entity<crate::shell::Shell>, out: &std::path::Path,
    revision: &str, name: &str)
{
    session.update(|_, cx| {
        cx.global_mut::<InkTrace>().0.clear();
        facet::probe::take(cx);
    }).expect("capture ledger reset");
    let (drawn, image) = session.frame(true).expect("native GPU frame");
    let image = image.expect("actual PNG pixels");
    image.save(out.join(format!("{name}.png"))).expect("native PNG");
    let (frame, native, ink, all_ink, ledger) = session.update(|window, cx| {
        let frame = shell.read(cx).reader_entity().read(cx).frame.get().expect("actual Reader frame");
        let native = window.debug_a11y_tree_json().expect("native frame tree");
        if matches!(name, "pending-000" | "pending-112") {
            let reader = shell.read(cx).reader_entity();
            assert!(reader.read(cx).transit.is_none(), "pending is prepared without an empty plate");
            assert!(reader.read(cx).arrival.is_some(), "the route still owns its prepared real arrival");
            assert!(native.contains("Opening page"), "the pending status is actual native accessibility");
        }
        let ink: Vec<_> = cx.global::<InkTrace>().0.iter().map(|(owner, text)| serde_json::json!({
            "page": owner, "text": text.text.as_ref(), "alpha": text.alpha,
            "bounds": [f32::from(text.bounds.left()), f32::from(text.bounds.top()),
                f32::from(text.bounds.right()), f32::from(text.bounds.bottom())],
        })).collect();
        let all_ink: Vec<_> = window.painted_texts().iter().map(|text| serde_json::json!({
            "text": text.text.as_ref(), "alpha": text.alpha,
            "bounds": [f32::from(text.bounds.left()), f32::from(text.bounds.top()),
                f32::from(text.bounds.right()), f32::from(text.bounds.bottom())],
        })).collect();
        (frame, native, ink, all_ink, format!("{:#?}", facet::probe::take(cx)))
    }).expect("native capture evidence");
    let x = f32::from(frame.left()).max(0.0).floor() as u32;
    let y = f32::from(frame.top()).max(0.0).floor() as u32;
    let width = (f32::from(frame.right()).ceil() as u32).min(image.width()) - x;
    let height = (f32::from(frame.bottom()).ceil() as u32).min(image.height()) - y;
    assert!(width > 0 && height > 0, "the real Reader crop is nonempty");
    image::imageops::crop_imm(&image, x, y, width, height).to_image()
        .save(out.join(format!("{name}-reader.png"))).expect("actual Reader pixels crop");
    let evidence = serde_json::json!({ "compiled_revision": revision, "frame": name,
        "at_ms": drawn.at_ms, "viewport": [drawn.viewport.width, drawn.viewport.height],
        "reader_crop": [x, y, width, height], "owned_native_ink": ink,
        "all_native_ink": all_ink,
        "accesskit": serde_json::from_str::<serde_json::Value>(&native).expect("native AX JSON"),
        "motion": ledger,
    });
    std::fs::write(out.join(format!("{name}.json")), serde_json::to_vec_pretty(&evidence).expect("film JSON"))
        .expect("source-linked native film evidence");
    if matches!(name, "reverse-128" | "resize-first-128" | "resize-144"
        | "ready-reverse-128" | "ready-resize-first-128" | "ready-resize-144"
        | "control-resize-first-128") {
        for unique in ["The readable label of RelationLabel.", "It names one relation group.",
            "one of 2", "Exactly one of these at a time"] {
            let runs: Vec<_> = evidence["all_native_ink"].as_array().expect("actual native ledger")
                .iter().filter(|run| run["text"].as_str() == Some(unique)).collect();
            assert_eq!(runs.len(), 1,
                "{name}: the original prose/caption has one actual native run, including deferred paint: {runs:#?}");
        }
    }
}
