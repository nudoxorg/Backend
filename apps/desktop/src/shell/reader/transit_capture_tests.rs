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
    let mut lifecycle = Vec::new();
    let capture = |session: &mut backend_gui_harness::Session, name: &str| {
        let name = if control { format!("control-{name}") } else { name.to_owned() };
        capture_native_transit(session, &shell, &out, &revision, &name);
    };
    session.update(|_, cx| {
        let display = shell.read(cx).display_key();
        graph.root.update(cx, |root, cx| root.queue(Intent::ZoomTo { display, percent: 200 }, cx));
    }).expect("real text-size intent");
    advance_native_transit(&mut session, 1000, control, &mut lifecycle);
    capture(&mut session, "rest-200");
    session.set_quiet(None);
    session.update(|_, cx| graph.root.update(cx, |root, cx| root.queue(
        Intent::Navigate(crate::shell::tests::page_route("TransitHeldDestination")), cx)))
        .expect("native held destination navigation");
    capture(&mut session, "pending-000");
    received.recv_timeout(Duration::from_secs(1)).expect("real read entered before film");
    advance_native_transit(&mut session, 1112, control, &mut lifecycle);
    capture(&mut session, "pending-112");
    session.apply(&backend_gui_harness::Act::Key { chord: "secondary-[".to_owned() },
        &mut |_, _, _| {}).expect("native Back keystroke");
    advance_native_transit(&mut session, 1128, control, &mut lifecycle);
    capture(&mut session, "reverse-128");
    session.resize(1000, 700).expect("native private window resize");
    capture(&mut session, "resize-first-128");
    advance_native_transit(&mut session, 1144, control, &mut lifecycle);
    capture(&mut session, "resize-144");
    drop(release);
    advance_native_transit(&mut session, 2000, control, &mut lifecycle);
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
    advance_native_transit(&mut session, 2112, control, &mut lifecycle);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-open-112");
    session
        .update(|_, cx| {
            let store = graph.store.read(cx);
            let destination = crate::shell::tests::symbol("TransitHeldDestination");
            let resource = store.symbol(&destination);
            assert!(resource.is_loaded());
            assert_eq!(
                resource
                    .loaded_value()
                    .expect("real loaded destination")
                    .identity
                    .coordinate,
                destination
            );
            assert!(
                resource
                    .value_root()
                    .expect("real producer receipt")
                    .same_authority(store.snapshot().key())
            );
            assert!(
                !shell
                    .read(cx)
                    .reader_entity()
                    .read(cx)
                    .native_input_allowed(),
                "a moving printed page cannot activate unprinted controls"
            );
        })
        .expect("current real destination receipt and native input gate");
    let before = session.update(|_, cx| shell.read(cx).reader_entity().read(cx).transit
        .as_ref().expect("actual opening still moves").carry.value(facet::motion::now(cx)))
        .expect("painted native opening value");
    session.apply(&backend_gui_harness::Act::Key { chord: "secondary-[".to_owned() },
        &mut |_, _, _| {}).expect("native Back interrupts a real opening");
    advance_native_transit(&mut session, 2128, control, &mut lifecycle);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-reverse-128");
    let after = session.update(|_, cx| shell.read(cx).reader_entity().read(cx).transit
        .as_ref().expect("actual reversal owns the same carry").carry.value(facet::motion::now(cx)))
        .expect("native reversing value");
    assert!((after - before).abs() < 0.12, "native reversal preserves its painted carry: {before} -> {after}");
    session.resize(1000, 700).expect("actual first-frame resize during Ready reversal");
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-resize-first-128");
    advance_native_transit(&mut session, 2144, control, &mut lifecycle);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-resize-144");
    advance_native_transit(&mut session, 3000, control, &mut lifecycle);
    settle_native_transit(&mut session, &mut lifecycle);
    capture_native_transit(&mut session, &shell, &out, &revision, "ready-back-settled");
    session.update(|window, cx| assert_eq!(window.simulate_next_frame(cx), 0,
        "the fully settled native film leaves no motion frame requested"))
        .expect("native settled wake proof");
    let requests = session
        .update(|_, cx| facet::motion::frames_requested(cx))
        .expect("settled native request count");
    // Both cached same-clock reuse and the next actual platform tick stay quiet.
    for advance in [0, session.frame_ms()] {
        session.advance_to(session.now_ms() + advance);
        let (drawn, ledger) = sample_native_transit(&mut session, &mut lifecycle);
        assert_eq!(
            drawn.callbacks, 0,
            "a settled native platform frame has no pending callback"
        );
        assert!(
            !ledger.any_live(),
            "ordinary settled replay starts no new trajectory: {ledger:#?}"
        );
        assert_eq!(
            session
                .update(|_, cx| facet::motion::frames_requested(cx))
                .expect("native request counter"),
            requests,
            "ordinary settled replay requests no new animation frame"
        );
    }
    std::fs::write(
        out.join("lifecycle.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "compiled_revision": revision, "frames": lifecycle,
        }))
        .expect("native lifecycle evidence"),
    )
    .expect("native lifecycle receipt");
}


fn sample_native_transit(
    session: &mut backend_gui_harness::Session,
    lifecycle: &mut Vec<serde_json::Value>,
) -> (backend_gui_harness::Drawn, facet::probe::Ledger) {
    session
        .update(|_, cx| {
            facet::probe::take(cx);
        })
        .expect("native per-frame ledger reset");
    let (drawn, _) = session
        .frame(false)
        .expect("actual intervening native platform frame");
    let ledger = session
        .update(|_, cx| facet::probe::take(cx))
        .expect("native sampled tracks");
    lifecycle.push(
        serde_json::json!({ "at_ms": drawn.at_ms, "callbacks": drawn.callbacks,
        "requests": ledger.frames_requested, "tracks": ledger.tracks.iter().map(|track|
            serde_json::json!({ "key": track.key, "value": track.value, "target": track.target,
                "velocity": track.velocity, "live": track.live, "started_ms": track.started_ms,
                "budget_ms": track.budget_ms })).collect::<Vec<_>>() }),
    );
    (drawn, ledger)
}

fn advance_native_transit(
    session: &mut backend_gui_harness::Session,
    at: u64,
    control: bool,
    lifecycle: &mut Vec<serde_json::Value>,
) {
    if !control {
        let period = session.frame_ms();
        assert!(period > 0, "the native film has a platform frame cadence");
        let mut next = (session.now_ms() / period + 1) * period;
        while next < at {
            session.advance_to(next);
            sample_native_transit(session, lifecycle);
            next += period;
        }
    }
    session.advance_to(at);
}

fn settle_native_transit(
    session: &mut backend_gui_harness::Session,
    lifecycle: &mut Vec<serde_json::Value>,
) {
    let started = std::time::Instant::now();
    let (_, mut ledger) = sample_native_transit(session, lifecycle);
    while ledger.any_live() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the actual native trajectories must naturally converge: {ledger:#?}"
        );
        let end = ledger
            .tracks
            .iter()
            .filter(|track| track.live)
            .map(|track| track.started_ms + track.budget_ms)
            .fold(0.0_f64, f64::max)
            .ceil();
        assert!(
            end.is_finite() && end > session.now_ms() as f64,
            "a live native track must advertise a future bounded end: {ledger:#?}"
        );
        advance_native_transit(session, end as u64, false, lifecycle);
        (_, ledger) = sample_native_transit(session, lifecycle);
    }
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
        (frame, native, ink, all_ink, facet::probe::take(cx))
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
        "motion": format!("{ledger:#?}"), "callbacks": drawn.callbacks,
    });
    std::fs::write(out.join(format!("{name}.json")), serde_json::to_vec_pretty(&evidence).expect("film JSON"))
        .expect("source-linked native film evidence");
    if !name.starts_with("control-") {
        let header: Vec<_> = evidence["all_native_ink"]
            .as_array()
            .expect("actual native header ink")
            .iter()
            .filter(|run| {
                run["bounds"][1].as_f64().expect("native top") < f32::from(frame.top()) as f64
            })
            .collect();
        for (index, a) in header.iter().enumerate() {
            for b in header.iter().skip(index + 1) {
                let rect = |run: &serde_json::Value| {
                    run["bounds"]
                        .as_array()
                        .expect("actual native rectangle")
                        .iter()
                        .map(|p| p.as_f64().expect("coordinate"))
                        .collect::<Vec<_>>()
                };
                let a_rect = rect(a);
                let b_rect = rect(b);
                assert!(
                    a_rect[2] <= b_rect[0]
                        || b_rect[2] <= a_rect[0]
                        || a_rect[3] <= b_rect[1]
                        || b_rect[3] <= a_rect[1],
                    "{name}: header native ink stays disjoint on the actual first frame: {a:?} / {b:?}"
                );
            }
        }
    }
    if name == "ready-open-112" {
        let coordinate = |key| ledger.track(key).expect("actual native plate edge").value;
        let plate = [
            coordinate("reader.plate.left"),
            coordinate("reader.plate.top"),
            coordinate("reader.plate.right"),
            coordinate("reader.plate.bottom"),
        ];
        for unique in [
            "TransitHeldDestination",
            "The readable label of TransitHeldDestination.",
        ] {
            let runs: Vec<_> = evidence["all_native_ink"]
                .as_array()
                .expect("native destination ink")
                .iter()
                .filter(|run| {
                    run["text"].as_str() == Some(unique)
                        && run["bounds"][1].as_f64().expect("native top")
                            >= f32::from(frame.top()) as f64
                })
                .collect();
            assert_eq!(
                runs.len(),
                1,
                "the ready destination paints its own original name and prose: {unique}"
            );
            let bounds = runs[0]["bounds"]
                .as_array()
                .expect("native destination visible ink");
            let bounds: Vec<_> = bounds
                .iter()
                .map(|p| p.as_f64().expect("coordinate") as f32)
                .collect();
            assert!(
                bounds[0] >= plate[0]
                    && bounds[1] >= plate[1]
                    && bounds[2] <= plate[2]
                    && bounds[3] <= plate[3]
                    && bounds[2] > bounds[0]
                    && bounds[3] > bounds[1],
                "ready original destination ink is positive inside its actual native plate: {bounds:?} / {plate:?}"
            );
        }
        let nodes = evidence["accesskit"]["nodes"]
            .as_object()
            .expect("native destination AX nodes");
        let doc = nodes
            .values()
            .find(|node| {
                node["aria"]["value"].as_str()
                    == Some("The readable label of TransitHeldDestination.")
            })
            .expect("actual incoming prose AX");
        assert!(
            doc["bounds"]["width"].as_f64().expect("native width") > 0.0
                && doc["bounds"]["height"].as_f64().expect("native height") > 0.0,
            "incoming original prose has positive native AX geometry: {doc:?}"
        );
    }
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
