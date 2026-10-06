/// Native GPU evidence for the same held-read fixture as the state regression.
/// Session owns an invisible window, real text system and renderer; these are
/// actual pixels and AccessKit trees, not a drawing of the trace rectangles.
#[test]
#[ignore = "native offscreen films: NUDOX_TRANSIT_FILM=DIR NUDOX_TRANSIT_REVISION=SHA"]
fn native_pending_reversal_and_resize_film() {
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
    session.update(|_, cx| {
        let display = shell.read(cx).display_key();
        graph.root.update(cx, |root, cx| root.queue(Intent::ZoomTo { display, percent: 200 }, cx));
    }).expect("real text-size intent");
    session.advance_to(1000);
    capture_native_transit(&mut session, &shell, &out, &revision, "rest-200");
    session.set_quiet(None);
    session.update(|_, cx| graph.root.update(cx, |root, cx| root.queue(
        Intent::Navigate(crate::shell::tests::page_route("TransitHeldDestination")), cx)))
        .expect("native held destination navigation");
    capture_native_transit(&mut session, &shell, &out, &revision, "pending-000");
    received.recv_timeout(Duration::from_secs(1)).expect("real read entered before film");
    session.advance_to(1112);
    capture_native_transit(&mut session, &shell, &out, &revision, "pending-112");
    session.apply(&backend_gui_harness::Act::Key { chord: "secondary-[".to_owned() },
        &mut |_, _, _| {}).expect("native Back keystroke");
    session.advance_to(1128);
    capture_native_transit(&mut session, &shell, &out, &revision, "reverse-128");
    session.resize(1000, 700).expect("native private window resize");
    capture_native_transit(&mut session, &shell, &out, &revision, "resize-first-128");
    session.advance_to(1144);
    capture_native_transit(&mut session, &shell, &out, &revision, "resize-144");
    drop(release);
    session.advance_to(2000);
    capture_native_transit(&mut session, &shell, &out, &revision, "back-settled");
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
    let (frame, native, ink, ledger) = session.update(|window, cx| {
        let frame = shell.read(cx).reader_entity().read(cx).frame.get().expect("actual Reader frame");
        let native = window.debug_a11y_tree_json().expect("native frame tree");
        let ink: Vec<_> = cx.global::<InkTrace>().0.iter().map(|(owner, text)| serde_json::json!({
            "page": owner, "text": text.text.as_ref(), "alpha": text.alpha,
            "bounds": [f32::from(text.bounds.left()), f32::from(text.bounds.top()),
                f32::from(text.bounds.right()), f32::from(text.bounds.bottom())],
        })).collect();
        (frame, native, ink, format!("{:#?}", facet::probe::take(cx)))
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
        "accesskit": serde_json::from_str::<serde_json::Value>(&native).expect("native AX JSON"),
        "motion": ledger,
    });
    std::fs::write(out.join(format!("{name}.json")), serde_json::to_vec_pretty(&evidence).expect("film JSON"))
        .expect("source-linked native film evidence");
}
