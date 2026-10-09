//! Renderer-backed controlled fixture evidence, never a live owner claim.

use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::pages::{
    DeclRef, Gap, GapReason, Known, MatchReason, PageValue, ReadFailure, SearchPage, SearchRow,
};
use crate::model::{AppSnapshot, ProjectPhase, SessionState, WorkspaceProject};
use crate::navigation::{BrowseRoute, OrbitRoute, Route};
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::{DesktopRuntime, EngineActor, UiEntityGraph};
use crate::harness::capture_evidence::{CaptureImage, QuietCommits, SavedArtifact, source_labels};
use backend_gui_harness::{Act, Quiet, Session, SessionOptions, Viewport};
use gpui::{AppContext as _, Entity, Focusable as _};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn words(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct CaptureReads {
    no_place: bool,
    requests: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl PageReader for CaptureReads {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        *self.requests.lock().map_err(|_| ReadFailure::Fault(crate::core::ErrorValue::new(
            crate::core::FaultCode::Protocol, "capture request counter poisoned",
        )))?.entry(format!("{request:?}")).or_default() += 1;
        if let ReadRequest::Browse(crate::model::browse::BrowseKey::Compare(selection)) = request {
            use crate::model::browse::{BrowseValue, CompareModel, PackageApi};
            let dossiers = selection.packages().iter().map(|package| {
                let mut dossier = super::tests::dossier();
                dossier.package = package.clone();
                if let Known::Known(record) = &mut dossier.record {
                    record.package = package.clone();
                    record.name = Arc::from(package.display_name());
                }
                dossier
            }).collect::<Vec<_>>();
            let apis = selection.packages().iter().map(|package| Known::Known(PackageApi {
                package: package.clone(), complete: true, items: Arc::from([]),
            })).collect::<Vec<_>>();
            let prepared = Arc::new(crate::runtime::browse_views::prepare_compare(&dossiers, &apis));
            return Ok(PageValue::Browse(BrowseValue::Compare(Arc::new(CompareModel {
                packages: dossiers.into(), apis: apis.into(), prepared,
            }))));
        }
        if self.no_place
            && let ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } = request
        {
            let decl = DeclRef::from_label(
                "::Mystery",
                None,
                Some(backend_library::DeclarationKind::Struct),
                None,
            )
            .ok_or_else(|| {
                ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    "invalid controlled no-destination declaration",
                ))
            })?;
            return Ok(PageValue::Search(SearchPage {
                query: Arc::clone(&query.text),
                rows: Arc::from([SearchRow {
                    rank: 0,
                    decl,
                    package: None,
                    score: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                    signature: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                    snippet: None,
                    reason: MatchReason::ExactName,
                }]),
                coverage: backend_present::CoverageLine::new(&[], None),
                next: None,
            }));
        }
        super::tests::Fixture.read(request, context)
    }
}

struct Mounted {
    graph: UiEntityGraph,
    shell: Entity<super::Shell>,
    // Release retained entity handles before Session closes its GPUI app.
    session: Session,
    requests: Arc<Mutex<BTreeMap<String, usize>>>,
    capture_image: CaptureImage,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // Release the capture's entity closures before the owning App closes.
        self.session.set_quiet(None);
    }
}

fn open_fixture(project: Option<WorkspaceProject>, no_place: bool) -> Result<Mounted, String> {
    open_fixture_with(project, no_place, None, None)
}

fn open_fixture_with(project: Option<WorkspaceProject>, no_place: bool,
    route: Option<Route>, pool: Option<ReadPool>) -> Result<Mounted, String> {
    open_fixture_with_authority(project, no_place, route, pool, None)
}

fn open_fixture_with_authority(
    project: Option<WorkspaceProject>,
    no_place: bool,
    route: Option<Route>,
    pool: Option<ReadPool>,
    authority: Option<(VersionedRoot, EngineActor, OwnerGate)>,
) -> Result<Mounted, String> {
    fn main_image_anchor() { std::hint::black_box("lifecycle capture main image"); }
    let capture_image = CaptureImage::read(main_image_anchor).map_err(words)?;
    facet::fonts::verify().map_err(words)?;
    let (root, actor, gate) = match authority {
        Some((root, actor, gate)) => (root, actor, Some(gate)),
        None => {
            let root = if no_place {
                VersionedRoot::synthetic(
                    backend_library::view_state_root(&[(
                        "ask".to_owned(),
                        "native fixture".to_owned(),
                    )]),
                    4,
                )
            } else {
                VersionedRoot::unserved()
            };
            let actor = EngineActor::start(super::tests::RootOnly, 8).map_err(words)?;
            let gate = (!no_place).then(|| {
                let gate = OwnerGate::starting();
                gate.publish(OwnerState::Failed(OwnerFault::Host(Arc::from(
                    "controlled fixture owner unavailable",
                ))));
                gate
            });
            (root, actor, gate)
        }
    };
    let mut snapshot = AppSnapshot::empty(root);
    let mut state = SessionState::default();
    if no_place {
        state.route = super::tests::page_route("RelationLabel");
    }
    if let Some(route) = route {
        state.route = route;
    }
    snapshot = snapshot.with_session(state);
    let mut workspace = snapshot.workspace().clone();
    if let Some(project) = project {
        workspace.active = Some(project.id.clone());
        workspace.projects = Arc::from([project]);
    }
    snapshot = snapshot.with_workspace(workspace);
    let requests = Arc::new(Mutex::new(BTreeMap::new()));
    let pool = match pool {
        Some(pool) => pool,
        None => {
            let requests = Arc::clone(&requests);
            ReadPool::start(2, move |_| CaptureReads {
                no_place,
                requests: Arc::clone(&requests),
            })
            .map_err(words)?
        }
    };
    let mounted = Rc::new(RefCell::new(None));
    let built = Rc::clone(&mounted);
    let failure = Rc::new(RefCell::new(None));
    let font_failure = Rc::clone(&failure);
    let session = Session::open(
        Viewport::new(1440, 900, 1).map_err(words)?,
        SessionOptions {
            asset_source: Arc::new(facet::icons::Assets),
            frame_ms: 16,
            capture_native_accessibility: true,
        },
        move |window, cx| {
            gpui_component::init(cx);
            if let Err(error) = facet::fonts::install(cx) {
                *font_failure.borrow_mut() = Some(words(error));
            }
            cx.bind_keys(super::keys::bindings());
            cx.set_global(gpui::TextTrace);
            let graph = UiEntityGraph::install_with_owner(
                cx,
                DesktopRuntime::new(snapshot, actor),
                None,
                Some(pool),
                gate,
                None,
            );
            let shell = super::open_shell(&graph, window, cx);
            let closing = crate::host::close::GracefulClose::install(&graph, cx);
            crate::host::close::GracefulClose::attach(&closing, window, cx);
            let view = cx.new(|cx| crate::host::close::CloseView::new(shell.clone(), closing, cx));
            *built.borrow_mut() = Some((graph, shell));
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        },
    )
    .map_err(words)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(format!("native font installation: {error}"));
    }
    let (graph, shell) = mounted
        .borrow_mut()
        .take()
        .ok_or("native shell was not mounted")?;
    let mut mounted = Mounted {
        session,
        graph,
        shell,
        requests,
        capture_image,
    };
    let quiet_root = mounted.graph.root.clone();
    let quiet_store = mounted.graph.store.clone();
    mounted.session.set_quiet(Some(Quiet {
        check: Box::new(move |cx| {
            quiet_root.update(cx, |root, cx| root.drain_now(cx));
            quiet_store.update(cx, |store, cx| {
                store.drain(cx);
            });
            let store = quiet_store.read(cx);
            store.pool_activity().is_idle()
                && store.focused().iter().all(|key| !store.is_loading(key))
                && !quiet_root.read(cx).has_pending_work()
        }),
        deadline: std::time::Duration::from_secs(5),
    }));
    advance(&mut mounted.session, 400)?;
    if no_place {
        mounted
            .session
            .update(|_, cx| {
                let root = mounted.graph.store.read(cx).snapshot().key();
                super::bodies::graph::install_test_fixture(root, cx);
            })
            .map_err(words)?;
    }
    Ok(mounted)
}

fn advance(session: &mut Session, duration: u64) -> Result<(), String> {
    let until = session
        .now_ms()
        .checked_add(duration)
        .ok_or("capture clock overflow")?;
    while session.now_ms() < until {
        session.advance_to(session.now_ms().saturating_add(16).min(until));
        session.frame(false).map_err(words)?;
    }
    session.quiesce().map_err(words)?;
    Ok(())
}

fn key(session: &mut Session, chord: &str) -> Result<(), String> {
    dispatch_key(session, chord)?;
    advance(session, 240)
}

fn dispatch_key(session: &mut Session, chord: &str) -> Result<(), String> {
    session
        .apply(
            &Act::Key {
                chord: chord.to_owned(),
            },
            &mut |_, _, _| {},
        )
        .map_err(words)?;
    let keystroke = gpui::Keystroke::parse(chord).map_err(words)?;
    session
        .update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
                cx,
            );
        })
        .map_err(words)?;
    Ok(())
}

fn focus_reader(mounted: &mut Mounted, target: &str) -> Result<(), String> {
    let focused = mounted
        .session
        .update(|window, cx| {
            mounted
                .shell
                .read(cx)
                .reader_targets(cx)
                .focus_native(target, window, cx)
        })
        .map_err(words)?;
    if !focused {
        return Err(format!("native target is not mounted: {target}"));
    }
    advance(&mut mounted.session, 16)?;
    let label = if target.starts_with("owner-words-") {
        "Profiles needing attention"
    } else if target.starts_with("orbit-project-") {
        "mixed-lifecycle"
    } else {
        return Err(format!("unrecognized native fixture control: {target}"));
    };
    native_control(mounted, label)?;
    Ok(())
}

fn native_control(mounted: &mut Mounted, label: &str) -> Result<(f32, f32), String> {
    mounted
        .session
        .update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let raw = window
                .debug_a11y_tree_json()
                .ok_or("native control tree missing")?;
            let tree: serde_json::Value = serde_json::from_str(&raw).map_err(words)?;
            let nodes = tree["nodes"]
                .as_object()
                .ok_or("native control nodes missing")?
                .values()
                .filter(|node| node["aria"]["label"].as_str() == Some(label)
                    && node["aria"]["disabled"] != true
                    && node["aria"]["on_action"].as_array().is_some_and(|actions|
                        actions.iter().any(|action| action == "Click")
                            && actions.iter().any(|action| action == "Focus")))
                .collect::<Vec<_>>();
            assert_eq!(nodes.len(), 1, "one exact enabled native action target: {label}");
            let node = nodes[0];
            let actions = node["aria"]["on_action"]
                .as_array()
                .ok_or("native actions missing")?;
            assert!(
                actions.iter().any(|action| action == "Click"),
                "native pointer activation: {node}"
            );
            assert!(
                actions.iter().any(|action| action == "Focus"),
                "native keyboard control: {node}"
            );
            let bounds = &node["bounds"];
            let x = bounds["x"].as_f64().ok_or("native x missing")?;
            let y = bounds["y"].as_f64().ok_or("native y missing")?;
            let width = bounds["width"].as_f64().ok_or("native width missing")?;
            let height = bounds["height"].as_f64().ok_or("native height missing")?;
            assert!(width > 0.0 && height > 0.0, "native painted hitbox: {node}");
            Ok::<_, String>(((x + width / 2.0) as f32, (y + height / 2.0) as f32))
        })
        .map_err(words)?
}

fn click_control(mounted: &mut Mounted, label: &str) -> Result<(), String> {
    let (x, y) = native_control(mounted, label)?;
    mounted
        .session
        .apply(
            &Act::Click {
                x,
                y,
                button: backend_gui_harness::Button::Left,
            },
            &mut |_, _, _| {},
        )
        .map_err(words)?;
    advance(&mut mounted.session, 240)
}

fn capture(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    settle_capture(mounted)?;
    capture_sample(mounted, out, revision, name)
}

/// Sample the current frame without claiming that any outer plate is settled.
/// Transition diagnostics use this path; completed layout captures use `capture`.
fn capture_sample(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    let virtual_ms = mounted.session.now_ms();
    let viewport = mounted.session.viewport();
    // PNG, AccessKit and typed route/state are read inside the same App update.
    let (image, native, mut meta) = mounted.session.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        let image = window.render_to_image().map_err(words)?;
        let native = window.debug_a11y_tree_json().ok_or("forced native AccessKit tree missing")?;
        let native_tree: serde_json::Value = serde_json::from_str(&native).map_err(words)?;
        backend_gui_harness::NativeAccessibilityFrame::new(
            name, virtual_ms, viewport, window.a11y_frame_number(), &native, &image,
        )?;
        let first_result_bounds = native_tree["nodes"].as_object().and_then(|nodes| nodes.values().find(|node|
            node["aria"]["label"].as_str().is_some_and(|label| label.starts_with("Result 1: "))))
            .map(|node| node["bounds"].clone());
        let runtime = mounted.graph.root.read(cx).snapshot();
        let live = mounted.graph.store.read(cx);
        let store = live.snapshot();
        let reader = mounted.shell.read(cx).reader_entity();
        let reader = reader.read(cx);
        let renders = mounted.shell.read(cx).render_counts(cx);
        let (ask_scene, ask_moving, columns_moving, drawer_departing, background_input) =
            mounted.shell.read(cx).diagnostic_cover_motion(cx);
        let (drawer_open, drawer_scene, drawer_raw_live) = mounted.shell.read(cx).diagnostic_drawer_motion(cx);
        let browse = if let Route::Orbit(OrbitRoute::Browse(route)) = runtime.route() {
            let key = crate::model::browse::BrowseKey::from(route);
            let resource = live.pages().browse(&key);
            let admission = crate::core::admit_resource(&resource, store.key(), live.owner_serving());
            Some(serde_json::json!({
                "phase": format!("{:?}", admission.phase()),
                "allows_actions": admission.allows_actions(),
                "loading": live.is_loading(&crate::model::pages::PageKey::Browse(key.clone())),
                "observation_revoked": live.observation_revoked(&crate::model::pages::PageKey::Browse(key)),
            }))
        } else { None };
        let mut meta = serde_json::json!({
            "source_provenance": source_labels(Some(revision), None),
            "capture_image": mounted.capture_image,
            "scope": "controlled admitted fixture; no live producer acceptance",
            "renderer": "actual platform offscreen renderer", "frame": name,
            "root_key": format!("{:?}", store.key()),
            "text_scale_percent": store.settings().zoom.percent(&super::system::display_key(window, cx)),
            "ask_owns_native_focus": mounted.shell.read(cx).ask_entity().read(cx).owns_focus(window, cx),
            "ask_first_result_bounds": first_result_bounds,
            "route": format!("{:?}", runtime.route()), "overlay": format!("{:?}", runtime.overlay()), "runtime_store_workspace_equal": runtime.workspace() == store.workspace(),
            "projects": runtime.workspace().projects.iter().map(|row| serde_json::json!({
                "id": row.id.as_str(), "phase": format!("{:?}", row.phase), "operation": row.operation,
            })).collect::<Vec<_>>(),
            "virtual_ms": virtual_ms,
            "native_motion_settled": reader.native_motion_settled(),
            "native_input_allowed": reader.native_input_allowed(),
            "background_presentation": format!("{:?}", reader.diagnostic_background_presentation()),
            "ask_scene_phase": ask_scene.and_then(|scene| scene.phase).map(|phase| format!("{phase:?}")),
            "ask_scene_plate_width": ask_scene.and_then(|scene| scene.geometry).map(|geometry| f32::from(geometry.plate.size.width)),
            "ask_scene_preview_left": ask_scene.and_then(|scene| scene.geometry).and_then(|geometry| geometry.preview_left).map(f32::from),
            "ask_scene_veil": ask_scene.map(|scene| scene.veil),
            "ask_motion_live": ask_moving, "columns_motion_live": columns_moving,
            "drawer_departing": drawer_departing, "shell_background_input_allowed": background_input,
            "drawer_open": drawer_open, "drawer_motion_live": drawer_raw_live,
            "drawer_visible": drawer_scene.map(|scene| scene.visible),
            "drawer_offset": drawer_scene.map(|scene| f32::from(scene.offset)),
            "drawer_moving": drawer_scene.map(|scene| scene.moving),
            "owner_serving": live.owner_serving(),
            "owner_attachment": format!("{:?}", live.current_owner_attachment()),
            "pool_activity": format!("{:?}", live.pool_activity()),
            "render_counts": {"shell": renders.shell, "titlebar": renders.titlebar,
                "shelf": renders.shelf, "reader": renders.reader, "status": renders.status,
                "pins": renders.pins, "ask": renders.ask},
            "browse_admission": browse,
            "fixture_request_counts": *mounted.requests.lock().map_err(words)?,
            "painted": window.painted_texts().iter().map(|text| text.text.to_string()).collect::<Vec<_>>(),
            "width": image.width(), "height": image.height(),
        });
        meta["native_frame"] = window.a11y_frame_number().into();
        meta["drawer_contains_native_focus"] = mounted.shell.read(cx)
            .drawer_contains_native_focus(window, cx).into();
        meta["ask_editor_owns_native_focus"] = mounted.shell.read(cx).ask_entity().read(cx)
            .input().read(cx).focus_handle(cx).is_focused(window).into();
        Ok::<_, String>((image, native, meta))
    }).map_err(words)??;
    let png = SavedArtifact::png(out, &format!("{name}.png"), &image)?;
    let top = image::imageops::crop_imm(&image, 0, 0, image.width(), image.height().min(520)).to_image();
    let top_png = SavedArtifact::png(out, &format!("{name}-top.png"), &top)?;
    let accesskit = SavedArtifact::write(out, &format!("{name}.accesskit.json"), native.as_bytes()).map_err(words)?;
    meta["artifacts"] = serde_json::json!({
        "native_frame": meta["native_frame"], "virtual_ms": virtual_ms,
        "png": png, "top_png": top_png, "accesskit": accesskit,
        "sampling": "PNG, raw AccessKit tree and route sampled in the same App update after one draw; top PNG is a crop of that image",
    });
    std::fs::write(
        out.join(format!("{name}.route.json")),
        serde_json::to_vec_pretty(&meta).map_err(words)?,
    )
    .map_err(words)?;
    Ok(meta)
}

/// A final fixture frame includes reads started by the last painted body and
/// the completed plate. A fixed delay before quiescence can finish I/O without
/// ever painting the body that requests its companions, or capture Back while
/// its controls are correctly blocked by the still-moving arrival.
fn settle_capture(mounted: &mut Mounted) -> Result<(), String> {
    let deadline = mounted.session.now_ms().checked_add(5_000).ok_or("capture clock overflow")?;
    let previous = mounted.session.update(|window, _| window.a11y_frame_number()).map_err(words)?;
    let mut quiet_frames = QuietCommits::after(previous);
    loop {
        mounted.session.quiesce().map_err(words)?;
        mounted.session.advance_to(mounted.session.now_ms().saturating_add(16));
        mounted.session.frame(false).map_err(words)?;
        let (frame, native, quiet) = mounted.session.update(|window, cx| {
            let reader = mounted.shell.read(cx).reader_entity();
            let quiet = reader.read(cx).native_motion_settled()
                && mounted.graph.store.read(cx).pool_activity().is_idle()
                && !mounted.graph.root.read(cx).has_pending_work();
            (window.a11y_frame_number(), window.debug_a11y_tree_json(), quiet)
        }).map_err(words)?;
        let native = native.ok_or("settlement has no committed native tree")?;
        let tree = serde_json::from_str(&native).map_err(words)?;
        if quiet_frames.observe(frame, &tree, quiet)? { return Ok(()); }
        if mounted.session.now_ms() >= deadline {
            return Err("native capture never reached two settled frames".to_owned());
        }
    }
}

fn navigate(mounted: &mut Mounted, route: Route) -> Result<(), String> {
    mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| {
        root.dispatch(crate::navigation::Intent::Navigate(route), cx);
    })).map_err(words)?;
    advance(&mut mounted.session, 400)
}

/// Current-source pixels from controlled page readers and an explicit failed
/// owner, not a live service or a proof of naturally scheduled repaint.
#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_owner_navigation_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR")
        .ok_or("capture directory missing")?);
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    let mut failed = open_fixture(None, false)?;
    capture(&mut failed, &out, &revision, "failed-owner-library")?;
    click_control(&mut failed, "Inbox")?;
    let inbox = capture(&mut failed, &out, &revision, "failed-owner-inbox")?;
    assert!(inbox["painted"].as_array().is_some_and(|words| words.iter()
        .any(|word| word.as_str().is_some_and(|word| word.starts_with("Nothing followed yet.")))));
    key(&mut failed.session, "escape")?;
    key(&mut failed.session, "secondary-,")?;
    let settings = capture(&mut failed, &out, &revision, "failed-owner-settings")?;
    assert!(settings["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "Theme")));
    key(&mut failed.session, "escape")?;
    let query = crate::model::pages::SearchQuery::new("needle", crate::model::pages::SearchQuery::DEFAULT_LIMIT).map_err(words)?;
    navigate(&mut failed, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))))?;
    capture(&mut failed, &out, &revision, "failed-owner-find-local-query")?;
    drop(failed);

    let selection = crate::navigation::CompareSet::new([
        "/fixture/present", "/fixture/second", "/fixture/third",
    ].into_iter().map(crate::model::pages::PackageRef::parse).collect::<Result<Vec<_>, _>>().map_err(words)?)
        .map_err(|error| format!("controlled Compare selection: {error:?}"))?;
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection)));
    let mut compare = open_fixture_with(None, true, Some(route.clone()), None)?;
    let current = capture(&mut compare, &out, &revision, "compare-current")?;
    click_control(&mut compare, "Compare package facts")?;
    let facts = capture(&mut compare, &out, &revision, "compare-expanded-facts")?;
    assert!(facts["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "License as declared")));
    navigate(&mut compare, Route::Orbit(OrbitRoute::Home))?;
    key(&mut compare.session, "secondary-[")?;
    let back = capture(&mut compare, &out, &revision, "compare-back")?;
    assert_eq!(compare.session.update(|_, cx| compare.graph.root.read(cx).snapshot().route().clone()).map_err(words)?, route);
    assert!(back["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "Hide package facts")));
    assert_eq!(back["native_motion_settled"], true, "Back captured a settled plate");
    assert_eq!(back["native_input_allowed"], true, "the settled Compare owns native input");
    assert_eq!(back["browse_admission"]["allows_actions"], true, "Back kept a current Compare read");
    let compare_reads = |frame: &serde_json::Value| frame["fixture_request_counts"]
        .as_object().expect("fixture request counters").iter()
        .filter(|(request, _)| request.starts_with("Browse(Compare("))
        .map(|(_, count)| count.as_u64().expect("fixture request count")).sum::<u64>();
    assert!(compare_reads(&current) > 0, "the mounted Compare requested its model");
    assert_eq!(compare_reads(&back), compare_reads(&current), "Back reuses the resident Compare read");
    for package in ["/fixture/present", "/fixture/second", "/fixture/third"] {
        native_control(&mut compare, &format!("Explore Local source · {package}"))?;
        native_control(&mut compare, &format!("Remove Local source · {package} from comparison"))?;
    }
    drop(compare);

    let (pool, published) = super::tests::publication::capture_pool()?;
    let mut go = open_fixture_with(None, true, Some(super::tests::publication::capture_route()), Some(pool))?;
    let before = capture(&mut go, &out, &revision, "go-before-publication")?;
    let read_state = go.session.update(|_, cx| {
        let store = go.graph.store.read(cx);
        let companion = crate::model::pages::SymbolRef::new(
            "/fixture/present::errors.go:2::ContinueOnError").map_err(words)?;
        let key = crate::model::pages::PageKey::Symbol(companion.clone());
        Ok::<_, String>(serde_json::json!({
            "source": revision,
            "scope": "controlled fixture diagnostics; no live producer acceptance",
            "request_counts": published.capture_read_counts(),
            "companion_resource": format!("{:?}", store.symbol(&companion)),
            "companion_loading": store.is_loading(&key),
            "companion_revoked": store.observation_revoked(&key),
            "focused_keys": format!("{:?}", store.focused()),
        }))
    }).map_err(words)??;
    std::fs::write(out.join("go-before-publication-read-state.json"),
        serde_json::to_vec_pretty(&read_state).map_err(words)?).map_err(words)?;
    assert!(before["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "before publication")));
    published.published.store(true, std::sync::atomic::Ordering::Release);
    let published_package = crate::model::pages::PackageRef::parse(super::tests::PACKAGE).map_err(words)?;
    go.session.update(|_, cx| go.graph.store.update(cx, |store, cx| {
        store.packages_published(&std::collections::BTreeSet::from([published_package]), cx);
    })).map_err(words)?;
    advance(&mut go.session, 400)?;
    let after = capture(&mut go, &out, &revision, "go-after-publication")?;
    assert!(after["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "after publication")));
    Ok(())
}

#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_partial_and_ask_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(
        std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?,
    );
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    let project = LocalProjectId::new("/fixture/mixed-lifecycle").map_err(words)?;
    let mut row = WorkspaceProject::indexing_with_id(project.clone());
    let mut operation = crate::model::index_operation::tests::claim(&project, 0x85);
    operation.observation = Some(crate::model::index_operation::tests::partially_published(
        &operation,
    )?);
    row.phase = ProjectPhase::Ready;
    row.operation = Some(operation);
    let mut partial = open_fixture(Some(row.clone()), false)?;
    capture(&mut partial, &out, &revision, "partial-headline")?;
    focus_reader(&mut partial, &format!("owner-words-{}", project.as_str()))?;
    key(&mut partial.session, "enter")?;
    let refusal = capture(&mut partial, &out, &revision, "partial-refusal")?;
    assert!(refusal["painted"].as_array().is_some_and(|words| {
        words
            .iter()
            .any(|word| word == "typescript: the compiler is unavailable.")
    }));
    click_control(&mut partial, "Hide profiles needing attention")?;
    let closed = capture(
        &mut partial,
        &out,
        &revision,
        "partial-refusal-pointer-closed",
    )?;
    assert!(!closed["painted"].as_array().is_some_and(|words| {
        words
            .iter()
            .any(|word| word == "typescript: the compiler is unavailable.")
    }));
    focus_reader(&mut partial, &format!("owner-words-{}", project.as_str()))?;
    key(&mut partial.session, "enter")?;
    let reopened = capture(
        &mut partial,
        &out,
        &revision,
        "partial-refusal-keyboard-restored",
    )?;
    assert!(reopened["painted"].as_array().is_some_and(|words| {
        words
            .iter()
            .any(|word| word == "typescript: the compiler is unavailable.")
    }));
    let targets = partial.session.update(|_, cx| partial.shell.read(cx).reader_targets(cx)).map_err(words)?;
    assert!(!targets.native_keys().iter().any(|id| id.starts_with("orbit-tree-")),
        "an unserved owner and no selected Package capability cannot admit a Cargo tree");
    focus_reader(&mut partial, &format!("orbit-project-{}", project.as_str()))?;
    key(&mut partial.session, "enter")?;
    capture(&mut partial, &out, &revision, "partial-project")?;
    assert_eq!(
        partial
            .session
            .update(|_, cx| partial.graph.root.read(cx).snapshot().route().clone())
            .map_err(words)?,
        super::kit::package_route(&crate::model::pages::PackageRef::parse(project.as_str()).map_err(words)?)
            .ok_or("exact local Package route missing")?
    );
    key(&mut partial.session, "secondary-[")?;
    let back = capture(&mut partial, &out, &revision, "partial-back")?;
    assert_eq!(back["runtime_store_workspace_equal"], true);
    assert_eq!(
        partial
            .session
            .update(|_, cx| partial
                .graph
                .root
                .read(cx)
                .snapshot()
                .workspace()
                .projects
                .to_vec())
            .map_err(words)?,
        vec![row]
    );
    drop(partial);

    for percent in [100, 200] {
        let mut ask = open_fixture(None, true)?;
        if percent == 200 {
            key(&mut ask.session, "secondary-,")?;
            click_control(&mut ask, "200%")?;
            key(&mut ask.session, "escape")?;
        }
        key(&mut ask.session, "secondary-k")?;
        ask.session.apply(&Act::Type { text: "mystery".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
        advance(&mut ask.session, 400)?;
        for width in [1440, 720, 1080] {
            ask.session.apply(&Act::Resize { width, height: 900 }, &mut |_, _, _| {}).map_err(words)?;
            key(&mut ask.session, "backspace")?;
            ask.session.apply(&Act::Type { text: "y".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
            advance(&mut ask.session, 400)?;
            let before = capture(&mut ask, &out, &revision, &format!("ask-{percent}-{width}-before-enter"))?;
            assert!(before["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "Mystery")),
                "the admitted no-destination row is painted before Enter: {before}");
            assert!(!before["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "Mystery has no page yet")),
                "each geometry control starts without a submission refusal");
            assert_eq!(before["text_scale_percent"], percent);
            assert!(!before["ask_first_result_bounds"].is_null(), "actual selected native row bounds");
            key(&mut ask.session, "enter")?;
            let after = capture(&mut ask, &out, &revision, &format!("ask-{percent}-{width}-after-enter"))?;
            assert_eq!(after["ask_first_result_bounds"], before["ask_first_result_bounds"],
                "failed Enter does not move the selected row at {percent}%/{width}px");
            assert_eq!(after["route"], before["route"]);
            assert_eq!(after["overlay"], before["overlay"]);
            assert_eq!(after["ask_owns_native_focus"], true);
            assert_eq!(after["ask_scene_plate_width"], before["ask_scene_plate_width"]);
            assert_eq!(after["ask_scene_preview_left"], before["ask_scene_preview_left"]);
            assert_eq!(ask.session.update(|_, cx| ask.shell.read(cx).ask_entity().read(cx)
                .input().read(cx).value().to_string()).map_err(words)?, "mystery");
            assert!(after["painted"].as_array().is_some_and(|words| words.iter().any(|word| word == "Mystery has no page yet")),
                "the refusal is actually painted in the retained query: {after}");
        }
    }
    Ok(())
}

/// Exercise real native resize, text-size selection, Ask cover and Escape.
/// These frames document layout; passive wake is asserted in publication tests.
#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_resize_ask_publication_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR")
        .ok_or("capture directory missing")?);
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    let painted = |frame: &serde_json::Value, expected: &str| frame["painted"].as_array()
        .is_some_and(|words| words.iter().any(|word| word == expected));
    for percent in [100, 200] {
        let (pool, observed) = super::tests::publication::capture_pool()?;
        let route = super::tests::publication::capture_route();
        let mut mounted = open_fixture_with(None, true, Some(route.clone()), Some(pool))?;
        let initial = capture(&mut mounted, &out, &revision, &format!("text-{percent}-initial-at-100"))?;
        assert!(painted(&initial, "before publication"));
        if percent == 200 {
            key(&mut mounted.session, "secondary-,")?;
            click_control(&mut mounted, "200%")?;
            let settings = capture(&mut mounted, &out, &revision, "text-200-native-settings")?;
            assert_eq!(settings["text_scale_percent"], 200, "the native Settings choice changes this display");
            key(&mut mounted.session, "escape")?;
        }
        for (width, height, name) in [(1080, 760, "resized"), (720, 900, "narrow")] {
            mounted.session.apply(&Act::Resize { width, height }, &mut |_, _, _| {}).map_err(words)?;
            advance(&mut mounted.session, 400)?;
            let frame = capture(&mut mounted, &out, &revision, &format!("text-{percent}-{name}"))?;
            assert_eq!(frame["width"], width);
            assert_eq!(frame["height"], height);
            assert_eq!(frame["text_scale_percent"], percent);
            assert_eq!(frame["native_motion_settled"], true);
            assert_eq!(frame["native_input_allowed"], true);
            assert!(painted(&frame, "before publication"));
        }
        key(&mut mounted.session, "secondary-k")?;
        mounted.session.apply(&Act::Type { text: "RelationLabel".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
        advance(&mut mounted.session, 400)?;
        let companion_reads = || observed.capture_read_counts().iter()
            .filter(|(key, _)| key.contains("::ContinueOnError")).map(|(_, count)| count).sum::<usize>();
        if percent == 100 {
            let preview = capture(&mut mounted, &out, &revision, "text-100-ask-readable-preview-720")?;
            assert_eq!(preview["ask_owns_native_focus"], true);
            assert_eq!(preview["ask_scene_phase"], "Open");
            assert_eq!(preview["ask_motion_live"], false);
            assert!(painted(&preview, "before publication"), "720px keeps a readable native preview");
            let preview_reads = companion_reads();
            observed.published.store(true, std::sync::atomic::Ordering::Release);
            mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| root.refresh_root(cx))).map_err(words)?;
            advance(&mut mounted.session, 400)?;
            let renewed = capture(&mut mounted, &out, &revision, "text-100-ask-preview-new-root-720")?;
            assert_eq!(companion_reads(), preview_reads + 1, "the visible preview renews once");
            assert_eq!(renewed["ask_owns_native_focus"], true);
            assert_eq!(renewed["ask_scene_plate_width"], preview["ask_scene_plate_width"], "publication does not move a settled Ask plate");
            assert_eq!(renewed["ask_scene_preview_left"], preview["ask_scene_preview_left"], "publication does not move settled Reader clearance");
            assert!(painted(&renewed, "after publication"));
            observed.published.store(false, std::sync::atomic::Ordering::Release);
            mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| root.refresh_root(cx))).map_err(words)?;
            advance(&mut mounted.session, 400)?;
            let reset = capture(&mut mounted, &out, &revision, "text-100-ask-preview-restored-720")?;
            assert_eq!(companion_reads(), preview_reads + 2);
            assert_eq!(reset["ask_scene_plate_width"], preview["ask_scene_plate_width"]);
            assert_eq!(reset["ask_scene_preview_left"], preview["ask_scene_preview_left"]);
            assert!(painted(&reset, "before publication"));
            mounted.session.apply(&Act::Resize { width: 640, height: 900 }, &mut |_, _, _| {}).map_err(words)?;
            advance(&mut mounted.session, 400)?;
        }
        let cover = capture(&mut mounted, &out, &revision, &format!("text-{percent}-ask-covered"))?;
        assert_eq!(cover["ask_owns_native_focus"], true);
        assert!(!painted(&cover, "before publication"), "the narrow Ask sheet covers Go");
        let hidden_reads = companion_reads();
        observed.published.store(true, std::sync::atomic::Ordering::Release);
        mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| root.refresh_root(cx))).map_err(words)?;
        advance(&mut mounted.session, 400)?;
        let hidden = capture(&mut mounted, &out, &revision, &format!("text-{percent}-ask-new-root"))?;
        assert_ne!(hidden["root_key"], cover["root_key"], "the actual producer published another authority");
        assert_eq!(hidden["ask_owns_native_focus"], true);
        assert_eq!(companion_reads(), hidden_reads, "covered companions remain lazy");
        assert!(!painted(&hidden, "after publication"));
        key(&mut mounted.session, "escape")?;
        let reveal = capture(&mut mounted, &out, &revision, &format!("text-{percent}-escape-revealed"))?;
        assert_eq!(reveal["ask_owns_native_focus"], false);
        assert_eq!(reveal["native_motion_settled"], true);
        assert_eq!(reveal["native_input_allowed"], true);
        assert_eq!(companion_reads(), hidden_reads + 1, "Escape renews one current companion");
        assert!(painted(&reveal, "after publication"));
        mounted.session.apply(&Act::Resize { width: 1440, height: 900 }, &mut |_, _, _| {}).map_err(words)?;
        advance(&mut mounted.session, 400)?;
        let wide = capture(&mut mounted, &out, &revision, &format!("text-{percent}-wide-restored"))?;
        assert_eq!(wide["text_scale_percent"], percent);
        assert!(painted(&wide, "after publication"));
        let visible_reads = companion_reads();
        observed.published.store(false, std::sync::atomic::Ordering::Release);
        mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| root.refresh_root(cx))).map_err(words)?;
        advance(&mut mounted.session, 400)?;
        let renewed = capture(&mut mounted, &out, &revision, &format!("text-{percent}-visible-new-root"))?;
        assert_ne!(renewed["root_key"], wide["root_key"]);
        assert_eq!(companion_reads(), visible_reads + 1, "a visible new root renews one companion");
        assert!(painted(&renewed, "before publication"));
        mounted.session.update(|_, cx| {
            assert_eq!(mounted.graph.root.read(cx).snapshot().route(), &route);
            let store = mounted.graph.store.read(cx);
            let symbol = crate::model::pages::SymbolRef::new("/fixture/present::errors.go:2::ContinueOnError").map_err(words)?;
            assert!(store.symbol(&symbol).value_root().is_some_and(|value|
                value.same_authority(store.snapshot().key())), "the visible companion uses the current root");
            Ok::<_, String>(())
        }).map_err(words)??;
        std::fs::write(out.join(format!("text-{percent}-read-counts.json")),
            serde_json::to_vec_pretty(&observed.capture_read_counts()).map_err(words)?).map_err(words)?;
    }
    Ok(())
}

/// Record every real 16ms frame until the sampled scene admits the requested
/// state. PNGs at named milestones document motion; no settlement is forced.
fn trace_ask_transition(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    name: &str,
    opening: bool,
) -> Result<serde_json::Value, String> {
    let start = mounted.session.now_ms();
    let mut frames = Vec::new();
    loop {
        mounted.session.quiesce().map_err(words)?;
        mounted.session.frame(false).map_err(words)?;
        let elapsed = mounted.session.now_ms() - start;
        let state = mounted.session.update(|window, cx| {
            let shell = mounted.shell.read(cx);
            let reader = shell.reader_entity();
            let reader = reader.read(cx);
            let (scene, ask_moving, columns_moving, drawer_departing, background_input) = shell.diagnostic_cover_motion(cx);
            let native: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("native tree"))
                .expect("native JSON");
            let keyboard = shell.keyboard_diagnostic(window, cx);
            let ask_result_links = native["nodes"].as_object().map_or(0, |nodes| nodes.values().filter(|node|
                node["aria"]["role"] == "Link" && node["aria"]["disabled"] != true
                    && node["aria"]["label"].as_str().is_some_and(|label| label.starts_with("Result "))
                    && node["aria"]["on_action"].as_array().is_some_and(|actions|
                        actions.iter().any(|action| action == "Click") && actions.iter().any(|action| action == "Focus"))).count());
            let source_live = native["nodes"].as_object().is_some_and(|nodes| nodes.values().any(|node|
                node["aria"]["label"] == "glyph.rs:138" && node["aria"]["disabled"] != true
                    && node["aria"]["on_action"].as_array().is_some_and(|actions|
                        actions.iter().any(|action| action == "Click") && actions.iter().any(|action| action == "Focus"))));
            serde_json::json!({
                "elapsed_ms": elapsed, "ask_phase": scene.and_then(|scene| scene.phase).map(|phase| format!("{phase:?}")),
                "plate_width": scene.and_then(|scene| scene.geometry).map(|geometry| f32::from(geometry.plate.size.width)),
                "ask_results_mounted": scene.is_some_and(|scene| scene.live_results),
                "ask_native_result_links": ask_result_links,
                "ask_content_width": scene.map(|scene| f32::from(scene.content_width)),
                "backing_scale": window.scale_factor(),
                "native_input_handler_present": keyboard.input_handler_present,
                "native_focus_owner": keyboard.focus_owner,
                "veil": scene.map(|scene| scene.veil), "ask_motion_live": ask_moving, "columns_motion_live": columns_moving,
                "drawer_departing": drawer_departing, "shell_background_input_allowed": background_input,
                "background_presentation": format!("{:?}", reader.diagnostic_background_presentation()),
                "native_motion_settled": reader.native_motion_settled(), "native_input_allowed": reader.native_input_allowed(),
                "ask_owns_native_focus": shell.ask_entity().read(cx).owns_focus(window, cx), "source_link_live": source_live,
                "native_focus_mounted": window.focused(cx).is_some_and(|focus| window.is_focus_handle_mounted(&focus)),
            })
        }).map_err(words)?;
        let done = if opening { state["ask_phase"] == "Open" } else {
            state["ask_phase"].is_null() && state["native_input_allowed"] == true
        };
        if [0, 64, 128, 240, 400, 512].contains(&elapsed) || done {
            capture_sample(mounted, out, revision, &format!("{name}-{elapsed:04}ms"))?;
        }
        frames.push(state.clone());
        std::fs::write(out.join(format!("{name}-trace.json")), serde_json::to_vec_pretty(&frames).map_err(words)?).map_err(words)?;
        if state["background_presentation"] == "Moving" {
            assert_eq!(state["native_motion_settled"], false, "a moving shell cannot advertise a settled page");
            assert_eq!(state["native_input_allowed"], false);
        }
        if state["native_input_allowed"] == false { assert_eq!(state["source_link_live"], false, "covered or moving pages have no live native source action"); }
        if done {
            assert!(elapsed <= 1_500, "Ask exceeded the 1500ms native interaction budget: {state}");
            assert_eq!(state["ask_owns_native_focus"], opening, "the current modal owns focus through interruption");
            assert_eq!(state["native_focus_mounted"], true, "a completed transition retains a mounted native keyboard owner");
            if !opening { assert_eq!(state["source_link_live"], true, "settled Escape restores the actual native source hit target"); }
            return Ok(state);
        }
        assert!(elapsed < 1_500, "Ask exceeded the 1500ms native interaction budget: {state}");
        mounted.session.advance_to(mounted.session.now_ms().saturating_add(16));
    }
}

#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_ask_transition_fixture_frames() -> Result<(), String> {
    #[derive(Default)]
    struct SourceLaunch(RefCell<Vec<crate::host::editor::Command>>);
    impl crate::host::editor::Launch for SourceLaunch {
        fn run(&self, command: &crate::host::editor::Command) -> std::io::Result<()> {
            self.0.borrow_mut().push(command.clone());
            Ok(())
        }
    }
    let out = std::path::PathBuf::from(std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?);
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    let (pool, observed) = super::tests::publication::capture_pool()?;
    let route = super::tests::publication::capture_route();
    let mut mounted = open_fixture_with(None, true, Some(route.clone()), Some(pool))?;
    let source_launch = Rc::new(SourceLaunch::default());
    let launch = Rc::clone(&source_launch);
    mounted.session.update(|_, cx| crate::host::editor::install(launch, cx)).map_err(words)?;
    mounted.session.apply(&Act::Resize { width: 640, height: 900 }, &mut |_, _, _| {}).map_err(words)?;
    capture(&mut mounted, &out, &revision, "transition-initial")?;
    dispatch_key(&mut mounted.session, "secondary-k")?;
    mounted.session.apply(&Act::Type { text: "RelationLabel".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
    let opened = trace_ask_transition(&mut mounted, &out, &revision, "transition-open", true)?;
    let companion_reads = || observed.capture_read_counts().iter().filter(|(key, _)| key.contains("::ContinueOnError")).map(|(_, count)| count).sum::<usize>();
    let hidden_reads = companion_reads();
    observed.published.store(true, std::sync::atomic::Ordering::Release);
    mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx| root.refresh_root(cx))).map_err(words)?;
    mounted.session.quiesce().map_err(words)?;
    assert_eq!(companion_reads(), hidden_reads, "a settled full sheet keeps renewed companions lazy");
    dispatch_key(&mut mounted.session, "escape")?;
    let closed = trace_ask_transition(&mut mounted, &out, &revision, "transition-close", false)?;
    assert_eq!(companion_reads(), hidden_reads + 1);
    dispatch_key(&mut mounted.session, "secondary-k")?;
    mounted.session.apply(&Act::Type { text: "RelationLabel".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
    trace_ask_transition(&mut mounted, &out, &revision, "transition-reopen", true)?;
    dispatch_key(&mut mounted.session, "escape")?;
    advance(&mut mounted.session, 48)?;
    capture_sample(&mut mounted, &out, &revision, "transition-interrupted-close-0048ms")?;
    dispatch_key(&mut mounted.session, "secondary-k")?;
    mounted.session.apply(&Act::Type { text: "RelationLabel".to_owned() }, &mut |_, _, _| {}).map_err(words)?;
    advance(&mut mounted.session, 16)?;
    let reentered = capture_sample(&mut mounted, &out, &revision, "transition-immediate-reentry-0016ms")?;
    assert_eq!(reentered["ask_owns_native_focus"], true);
    dispatch_key(&mut mounted.session, "escape")?;
    let cancelled = trace_ask_transition(&mut mounted, &out, &revision, "transition-cancel", false)?;
    native_control(&mut mounted, "glyph.rs:138")?;
    click_control(&mut mounted, "glyph.rs:138")?;
    assert_eq!(mounted.session.update(|_, cx| mounted.graph.root.read(cx).snapshot().route().clone()).map_err(words)?, route,
        "the native source action requests an editor without navigating the page");
    assert_eq!(&*source_launch.0.borrow(), &crate::host::editor::commands(None, "/fixture/present/glyph.rs", 138)[..1],
        "the settled target dispatches exactly the recorded fixture source and line through a real native pointer click");
    std::fs::write(out.join("transition-settlement.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "opening": opened, "closing": closed, "cancelled": cancelled, "interaction_budget_ms": 1500,
    })).map_err(words)?).map_err(words)?;
    Ok(())
}

/// Successor pixels for the preserved native re-entry and FastAPI controls.
/// These are current mounted platform frames, never backfilled old evidence.
#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_drawer_and_fastapi_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?);
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    for percent in [100, 200] {
        let mut mounted = open_fixture(None, true)?;
        mounted.session.apply(&Act::Resize { width: 400, height: 900 }, &mut |_, _, _| {}).map_err(words)?;
        let display = mounted.session.update(|_, cx| mounted.shell.read(cx).display_key()).map_err(words)?;
        mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx|
            root.dispatch(crate::navigation::Intent::ZoomTo { display, percent }, cx))).map_err(words)?;
        capture(&mut mounted, &out, &revision, &format!("drawer-{percent}-closed"))?;
        dispatch_key(&mut mounted.session, "secondary-\\")?;
        advance(&mut mounted.session, 160)?;
        let opened = capture_sample(&mut mounted, &out, &revision, &format!("drawer-{percent}-open-0160ms"))?;
        assert_eq!(opened["drawer_open"], true);
        assert_eq!(opened["native_input_allowed"], false);
        dispatch_key(&mut mounted.session, "escape")?;
        capture_sample(&mut mounted, &out, &revision, &format!("drawer-{percent}-close-0000ms"))?;
        advance(&mut mounted.session, 64)?;
        let departing = capture_sample(&mut mounted, &out, &revision, &format!("drawer-{percent}-close-0064ms"))?;
        assert_eq!(departing["drawer_departing"], true);
        assert_eq!(departing["native_input_allowed"], false);
        dispatch_key(&mut mounted.session, "secondary-\\")?;
        let reopened = capture_sample(&mut mounted, &out, &revision, &format!("drawer-{percent}-reopen-0000ms"))?;
        assert_eq!(reopened["drawer_open"], true, "the actual native command reverses its own departure");
        assert_eq!(reopened["drawer_offset"], departing["drawer_offset"], "re-entry retains the mounted position");
        assert_eq!(reopened["native_input_allowed"], false);
        advance(&mut mounted.session, 16)?;
        capture_sample(&mut mounted, &out, &revision, &format!("drawer-{percent}-reopen-0016ms"))?;
        dispatch_key(&mut mounted.session, "escape")?;
        let closed = capture(&mut mounted, &out, &revision, &format!("drawer-{percent}-retired"))?;
        assert_eq!(closed["drawer_visible"], false);
        assert_eq!(closed["drawer_motion_live"], false);
        assert_eq!(closed["native_input_allowed"], true);
    }
    for name in ["read_items", "create_access_token"] {
        let (route, pool) = super::bodies::fastapi_unavailable_capture(name)?;
        let mut mounted = open_fixture_with(None, true, Some(route), Some(pool))?;
        for (width, percent) in [(1440, 100), (720, 200)] {
            mounted.session.apply(&Act::Resize { width, height: 2400 }, &mut |_, _, _| {}).map_err(words)?;
            let display = mounted.session.update(|_, cx| mounted.shell.read(cx).display_key()).map_err(words)?;
            mounted.session.update(|_, cx| mounted.graph.root.update(cx, |root, cx|
                root.dispatch(crate::navigation::Intent::ZoomTo { display, percent }, cx))).map_err(words)?;
            let frame = capture(&mut mounted, &out, &revision, &format!("fastapi-{name}-{percent}"))?;
            let painted = frame["painted"].as_array().ok_or("painted native words missing")?;
            assert!(painted.iter().any(|word| word == "Reference information is unavailable."));
            for false_claim in ["Nothing in your workspace names it", "None of them use it", "declares no types here"] {
                assert!(!painted.iter().any(|word| word.as_str().is_some_and(|word| word.contains(false_claim))));
            }
            for annotation in if name == "read_items" { ["SessionDep", "CurrentUser", "Any"] } else { ["str | Any", "timedelta", "str"] } {
                assert!(painted.iter().any(|word| word == annotation));
            }
        }
    }
    Ok(())
}

/// An open drawer is settled only after its own scene and raw animation
/// clocks agree for two consecutive ordinary platform frames.
fn settle_open_drawer_capture(mounted: &mut Mounted) -> Result<serde_json::Value, String> {
    let deadline = mounted
        .session
        .now_ms()
        .checked_add(5_000)
        .ok_or("capture clock overflow")?;
    let mut consecutive = Vec::new();
    loop {
        mounted.session.quiesce().map_err(words)?;
        mounted
            .session
            .advance_to(mounted.session.now_ms().saturating_add(16));
        mounted.session.frame(false).map_err(words)?;
        let observed = mounted.session.update(|window, cx| {
            let (open, scene, raw_live) = mounted.shell.read(cx).diagnostic_drawer_motion(cx);
            let quiet = open && scene.is_some_and(|scene| scene.visible && !scene.moving)
                && !raw_live && mounted.graph.store.read(cx).pool_activity().is_idle()
                && !mounted.graph.root.read(cx).has_pending_work();
            (quiet, serde_json::json!({
                "native_frame": window.a11y_frame_number(), "drawer_open": open,
                "scene_visible": scene.map(|scene| scene.visible),
                "scene_moving": scene.map(|scene| scene.moving), "raw_motion_live": raw_live,
            }))
        }).map_err(words)?;
        if observed.0 {
            consecutive.push(observed.1);
            if consecutive.len() == 2 {
                assert!(consecutive[1]["native_frame"].as_u64() > consecutive[0]["native_frame"].as_u64(),
                    "settled drawer samples require two distinct committed native frames");
                return Ok(serde_json::json!(consecutive));
            }
        } else {
            consecutive.clear();
        }
        if mounted.session.now_ms() >= deadline {
            return Err("open drawer never reached two settled platform frames".to_owned());
        }
    }
}

fn capture_focused_drawer_lens(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    width: u32,
    suffix: &str,
    label: &str,
) -> Result<(), String> {
    capture_focused_drawer_control(
        mounted,
        out,
        revision,
        width,
        suffix,
        "Tab",
        label,
        Some(true),
    )
}

fn capture_focused_drawer_control(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    width: u32,
    suffix: &str,
    role: &str,
    label: &str,
    selected: Option<bool>,
) -> Result<(), String> {
    let native = capture_sample(
        mounted,
        out,
        revision,
        &format!("settled-drawer-200-{width}-{suffix}"),
    )?;
    assert_eq!(native["drawer_open"], true);
    assert_eq!(native["drawer_contains_native_focus"], true);
    assert_eq!(native["drawer_moving"], false);
    assert_eq!(native["drawer_motion_live"], false);
    let ax: serde_json::Value = serde_json::from_slice(
        &std::fs::read(out.join(format!(
            "settled-drawer-200-{width}-{suffix}.accesskit.json"
        )))
        .map_err(words)?,
    )
    .map_err(words)?;
    let focus = ax["gpui_focus"]
        .as_str()
        .ok_or("drawer native focus is missing")?;
    let node = &ax["nodes"][focus];
    assert_eq!(node["aria"]["role"], role);
    assert_eq!(node["aria"]["label"], label);
    if let Some(selected) = selected {
        assert_eq!(node["aria"]["selected"], selected);
    }
    let bounds = &node["bounds"];
    let left = bounds["x"].as_f64().ok_or("drawer native left missing")?;
    let top = bounds["y"].as_f64().ok_or("drawer native top missing")?;
    let w = bounds["width"]
        .as_f64()
        .ok_or("drawer native width missing")?;
    let h = bounds["height"]
        .as_f64()
        .ok_or("drawer native height missing")?;
    assert!(
        w > 0.0
            && h > 0.0
            && left >= 0.0
            && top >= 0.0
            && left + w <= f64::from(width) + 0.5
            && top + h <= 900.5,
        "settled drawer native focus and pointer hitbox fit the viewport: {node}"
    );
    Ok(())
}

#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_settled_drawer_200_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(
        std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?,
    );
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    for width in [1440, 720, 400] {
        let mut mounted = open_fixture(None, true)?;
        key(&mut mounted.session, "secondary-,")?;
        click_control(&mut mounted, "200%")?;
        key(&mut mounted.session, "escape")?;
        mounted
            .session
            .apply(&Act::Resize { width, height: 900 }, &mut |_, _, _| {})
            .map_err(words)?;
        let before = capture(
            &mut mounted,
            &out,
            &revision,
            &format!("settled-drawer-200-{width}-closed"),
        )?;
        assert_eq!(before["text_scale_percent"], 200);
        dispatch_key(&mut mounted.session, "secondary-\\")?;
        let settled = settle_open_drawer_capture(&mut mounted)?;
        let opened = capture_sample(
            &mut mounted,
            &out,
            &revision,
            &format!("settled-drawer-200-{width}-open"),
        )?;
        assert_eq!(opened["drawer_open"], true);
        assert_eq!(opened["drawer_visible"], true);
        assert_eq!(opened["drawer_moving"], false);
        assert_eq!(opened["drawer_motion_live"], false);
        assert_eq!(opened["native_input_allowed"], false);
        let (x, y) = native_control(&mut mounted, "Rests on")?;
        mounted
            .session
            .apply(
                &Act::Click {
                    x,
                    y,
                    button: backend_gui_harness::Button::Left,
                },
                &mut |_, _, _| {},
            )
            .map_err(words)?;
        std::fs::write(
            out.join(format!("settled-drawer-200-{width}-two-frames.json")),
            serde_json::to_vec_pretty(&settled).map_err(words)?,
        ).map_err(words)?;
        capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
            "native-focus", "Rests on")?;
        // Real keys exercise the mounted compound control; no test-supplied focus.
        for (chord, label) in [("right", "Used by"), ("left", "Rests on"),
            ("up", "Versions"), ("down", "Rests on"), ("home", "Contents"),
            ("end", "Used by"), ("enter", "Used by"), ("space", "Used by")] {
            key(&mut mounted.session, chord)?;
            capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
                &format!("key-{chord}"), label)?;
        }
        let selected = mounted.session.update(|window, cx| window.focused(cx)).map_err(words)?;
        key(&mut mounted.session, "tab")?;
        capture_focused_drawer_control(&mut mounted, &out, &revision, width,
            "tab", "Button", "Library", None)?;
        assert!(mounted.session.update(|window, cx| {
            window.focused(cx) != selected && mounted.shell.read(cx).drawer_contains_native_focus(window, cx)
        }).map_err(words)?, "CE Tab moves to another mounted control inside the drawer trap");
        key(&mut mounted.session, "shift-tab")?;
        capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
            "shift-tab", "Used by")?;
        key(&mut mounted.session, "g")?;
        key(&mut mounted.session, "r")?;
        capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
            "lens-chord", "Rests on")?;
        key(&mut mounted.session, "shift-a")?;
        capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
            "shifted-narrow", "Rests on")?;
        assert_eq!(mounted.session.update(|_, cx| mounted.shell.read(cx)
            .diagnostic_drawer_narrow_value(cx)).map_err(words)?, "A",
            "shifted typing changes the actual shelf narrowing value");
        key(&mut mounted.session, "backspace")?;
        assert_eq!(mounted.session.update(|_, cx| mounted.shell.read(cx)
            .diagnostic_drawer_narrow_value(cx)).map_err(words)?, "",
            "Backspace clears the actual shelf narrowing value");
        capture_focused_drawer_lens(&mut mounted, &out, &revision, width,
            "narrow-cleared", "Rests on")?;
        key(&mut mounted.session, "tab")?;
        capture_focused_drawer_control(&mut mounted, &out, &revision, width,
            "library-back-focused", "Button", "Library", None)?;
        let former_back = mounted.session.update(|window, cx| {
            assert!(!mounted.shell.read(cx).diagnostic_drawer_is_library_scope(cx),
                "Library back starts in the package scope");
            window.focused(cx).ok_or("native Library back focus missing")
        }).map_err(words)??;
        key(&mut mounted.session, "enter")?;
        let back = capture_sample(&mut mounted, &out, &revision,
            &format!("settled-drawer-200-{width}-library-back-activated"))?;
        assert_eq!(back["drawer_open"], true);
        assert_eq!(back["route"], opened["route"], "Library back changes the shelf scope, retaining the reader route");
        let ax: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join(
            format!("settled-drawer-200-{width}-library-back-activated.accesskit.json"))).map_err(words)?).map_err(words)?;
        let nodes = ax["nodes"].as_object().ok_or("Library back nodes missing")?;
        assert!(nodes.values().any(|node| node["aria"]["role"] == "Heading"
            && node["aria"]["label"].as_str().is_some_and(|label| label.starts_with("Library, "))),
            "native Enter renders the actual Library scope heading");
        assert!(!nodes.values().any(|node| node["aria"]["role"] == "Button"
            && node["aria"]["label"] == "Library"), "the former package back control is retired");
        assert!(mounted.session.update(|window, cx| {
            mounted.shell.read(cx).diagnostic_drawer_is_library_scope(cx)
                && !window.is_focus_handle_mounted(&former_back)
                && mounted.shell.read(cx).drawer_contains_native_focus(window, cx)
        }).map_err(words)?,
            "native Enter leaves the package scope, retires its back handle, and contains current focus");
        key(&mut mounted.session, "escape")?;
        let closed = capture(
            &mut mounted,
            &out,
            &revision,
            &format!("settled-drawer-200-{width}-retired"),
        )?;
        assert_eq!(closed["drawer_open"], false);
        assert_eq!(closed["drawer_visible"], false);
        assert_eq!(closed["drawer_motion_live"], false);
    }
    Ok(())
}

fn scroll_copy_into_view(
    mounted: &mut Mounted,
    width: u32,
    out: &Path,
    revision: &str,
    percent: u16,
) -> Result<(), String> {
    let mut observations = Vec::new();
    for wheel in 0..=12 {
        let sample = mounted.session.update(|window, _| {
            let tree: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json()
                .ok_or("native Copy tree missing")?).map_err(words)?;
            let nodes = tree["nodes"].as_object().ok_or("native Copy nodes missing")?.values()
                .filter(|node| node["aria"]["role"] == "Button"
                    && node["aria"]["label"] == "Copy diagnostic"
                    && node["aria"]["disabled"] != true).collect::<Vec<_>>();
            assert!(nodes.len() <= 1, "one actual current Copy control");
            let bounds = nodes.first().map(|node| node["bounds"].clone());
            let visible = bounds.as_ref().is_some_and(|bounds| {
                let left = bounds["x"].as_f64().unwrap_or(-1.0);
                let top = bounds["y"].as_f64().unwrap_or(-1.0);
                let w = bounds["width"].as_f64().unwrap_or(0.0);
                let h = bounds["height"].as_f64().unwrap_or(0.0);
                left >= 0.0 && top >= 0.0 && w > 0.0 && h > 0.0
                    && left + w <= f64::from(width) + 0.5 && top + h <= 900.5
            });
            Ok::<_, String>((visible, serde_json::json!({"native_frame": window.a11y_frame_number(),
                "native_Copy_bounds": bounds, "within_900_viewport": visible, "wheel_actions": wheel})))
        }).map_err(words)??;
        observations.push(sample.1);
        if sample.0 {
            std::fs::write(
                out.join(format!(
                    "corrected-copy-{percent}-{width}-native-scroll.json"
                )),
                serde_json::to_vec_pretty(&serde_json::json!({"source": revision, "width": width,
                    "height": 900, "observations": observations, "forced_focus": false}))
                .map_err(words)?,
            )
            .map_err(words)?;
            return Ok(());
        }
        if wheel == 12 {
            break;
        }
        mounted
            .session
            .apply(
                &Act::Scroll {
                    x: width as f32 - 40.0,
                    y: 700.0,
                    dx: 0.0,
                    dy: -160.0,
                },
                &mut |_, _, _| {},
            )
            .map_err(words)?;
        advance(&mut mounted.session, 16)?;
    }
    Err(format!(
        "actual Copy is unreachable after 12 native wheel gestures at {width}x900/{percent}%: {observations:?}"
    ))
}

struct CaptureSameRoot;
impl crate::runtime::actor::EngineClient for CaptureSameRoot {
    fn execute(
        &mut self,
        request: &crate::runtime::actor::EngineRequest,
    ) -> Result<crate::runtime::actor::EngineDto, crate::runtime::actor::EngineFault> {
        match request {
            crate::runtime::actor::EngineRequest::Root { request, basis, .. } => {
                Ok(crate::runtime::actor::EngineDto::Root {
                    request: *request,
                    basis: *basis,
                    key: *basis,
                    revision: basis.revision(),
                    delta: None,
                    project: None,
                    catalog: None,
                })
            }
            _ => Err(crate::runtime::actor::EngineFault::Cancelled),
        }
    }
}

fn open_ready_capture(route: Route, pool: ReadPool) -> Result<(Mounted, OwnerGate), String> {
    let root = VersionedRoot::synthetic(
        backend_library::view_state_root(&[(
            "capture".to_owned(),
            "current corrected native paths".to_owned(),
        )]),
        4,
    );
    let gate = OwnerGate::ready(root, crate::model::ServiceMode::Attached);
    let actor = EngineActor::start(CaptureSameRoot, 8).map_err(words)?;
    let mounted = open_fixture_with_authority(
        None,
        true,
        Some(route),
        Some(pool),
        Some((root, actor, gate.clone())),
    )?;
    Ok((mounted, gate))
}

struct CaptureDiagnosticRead {
    reads: Arc<std::sync::atomic::AtomicUsize>,
    detail: Arc<str>,
}
impl PageReader for CaptureDiagnosticRead {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if matches!(
            request,
            ReadRequest::Browse(crate::model::browse::BrowseKey::Tree(_))
        ) {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Err(ReadFailure::Fault(
                crate::core::ErrorValue::with_diagnostic(
                    crate::core::FaultCode::Transport,
                    "The index could not start. ",
                    &self.detail,
                ),
            ));
        }
        super::tests::Fixture.read(request, context)
    }
}

#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_corrected_ask_copy_and_retained_reader_fixture_frames() -> Result<(), String> {
    use crate::model::pages::{PageKey, SearchQuery};
    use crate::runtime::store::DataStore;
    use gpui::InputEvent as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let out = std::path::PathBuf::from(
        std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?,
    );
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    for all in [false, true] {
        let requests = Arc::new(Mutex::new(BTreeMap::new()));
        let observed = Arc::clone(&requests);
        let pool = ReadPool::start(1, move |_| CaptureReads {
            no_place: false, requests: Arc::clone(&observed),
        }).map_err(words)?;
        let (mut mounted, gate) =
            open_ready_capture(super::tests::page_route("RelationDirection"), pool)?;
        mounted.requests = requests;
        key(&mut mounted.session, "secondary-k")?;
        mounted
            .session
            .apply(
                &Act::Type {
                    text: "RelationLabel".to_owned(),
                },
                &mut |_, _, _| {},
            )
            .map_err(words)?;
        advance(&mut mounted.session, 240)?;
        let prefix = if all {
            "corrected-ask-all"
        } else {
            "corrected-ask-row"
        };
        let before = capture(&mut mounted, &out, &revision, &format!("{prefix}-before"))?;
        assert_eq!(before["ask_editor_owns_native_focus"], true);
        let (label, route, destination, ask, reader) = mounted
            .session
            .update(|window, cx| {
                let tree: serde_json::Value = serde_json::from_str(
                    &window
                        .debug_a11y_tree_json()
                        .ok_or("native Ask tree missing")?,
                )
                .map_err(words)?;
                let label = if all {
                    "Open search results".to_owned()
                } else {
                    tree["nodes"]
                        .as_object()
                        .ok_or("native nodes missing")?
                        .values()
                        .find(|node| {
                            node["aria"]["role"] == "Link"
                                && node["aria"]["label"]
                                    .as_str()
                                    .is_some_and(|label| label.starts_with("Result 1: "))
                        })
                        .and_then(|node| node["aria"]["label"].as_str())
                        .ok_or("actual native row missing")?
                        .to_owned()
                };
                let shell = mounted.shell.read(cx);
                let ask = shell.ask_entity();
                let destination = if all {
                    ask.read(cx)
                        .all_results(cx)
                        .ok_or("native all-results route missing")?
                } else {
                    super::tests::page_route("RelationLabel")
                };
                Ok::<_, String>((
                    label,
                    mounted.graph.store.read(cx).snapshot().route().clone(),
                    destination,
                    ask,
                    shell.reader_entity(),
                ))
            })
            .map_err(words)??;
        assert_ne!(route, destination);
        let (x, y) = native_control(&mut mounted, &label)?;
        let store = mounted.graph.store.clone();
        let input = mounted
            .session
            .update(|_, cx| ask.read(cx).input().clone())
            .map_err(words)?;
        let query = SearchQuery::new("RelationLabel", SearchQuery::DEFAULT_LIMIT).map_err(words)?;
        let dispatch = mounted.session.update(|window, cx| {
            let root = store.read(cx).snapshot().key();
            let former = store.read(cx).current_owner_attachment().ok_or("current owner missing")?;
            let frame = window.a11y_frame_number();
            window.dispatch_event(gpui::MouseDownEvent {
                position: gpui::point(gpui::px(x), gpui::px(y)), modifiers: gpui::Modifiers::none(),
                button: gpui::MouseButton::Left, click_count: 1, first_mouse: false,
            }.to_platform_input(), cx);
            let fault = OwnerFault::Lost("pressed capture owner retired".into());
            gate.publish(OwnerState::Failed(fault.clone()));
            store.update(cx, |store, cx| store.owner_failed(&fault, cx));
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            store.update(cx, DataStore::owner_ready);
            store.update(cx, |store, cx| { store.ensure(PageKey::Search(query.clone()), cx); });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                store.update(cx, |store, cx| store.drain(cx));
                let current = store.read(cx);
                let resource = current.search(&query);
                if crate::core::admit_resource(&resource, current.snapshot().key(), current.owner_serving())
                    .current_value().is_some_and(|page| page.query.as_ref() == query.text.as_ref()) { break; }
                assert!(std::time::Instant::now() < deadline, "replacement capture search never landed");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(store.read(cx).snapshot().key(), root);
            assert!(!store.read(cx).admits_owner_attachment(&former));
            assert_eq!(window.a11y_frame_number(), frame, "replacement cannot paint over the pressed frame");
            window.dispatch_event(gpui::MouseUpEvent {
                position: gpui::point(gpui::px(x), gpui::px(y)), modifiers: gpui::Modifiers::none(),
                button: gpui::MouseButton::Left, click_count: 1,
            }.to_platform_input(), cx);
            assert_eq!(window.a11y_frame_number(), frame);
            assert_eq!(store.read(cx).snapshot().route(), &route);
            assert!(input.read(cx).focus_handle(cx).is_focused(window), "native refusal restores editor focus before repaint");
            Ok::<_, String>(serde_json::json!({"pressed_frame": frame, "released_frame": window.a11y_frame_number(),
                "root": format!("{root:?}"), "former_attachment_revoked": true, "native_editor_focus_before_repaint": true}))
        }).map_err(words)??;
        let refused = capture(&mut mounted, &out, &revision, &format!("{prefix}-refused"))?;
        assert_eq!(refused["ask_editor_owns_native_focus"], true);
        assert_eq!(refused["route"], before["route"]);
        let ax: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out.join(format!("{prefix}-refused.accesskit.json"))).map_err(words)?,
        )
        .map_err(words)?;
        assert!(
            ax["nodes"]
                .as_object()
                .ok_or("native nodes missing")?
                .values()
                .any(|node| node["aria"]["role"] == "Status"
                    && node["aria"]["label"].as_str().is_some_and(
                        |label| label.contains("no longer verified by the current index")
                    ))
        );
        assert_eq!(
            mounted
                .session
                .update(|_, cx| input.read(cx).value().to_string())
                .map_err(words)?,
            "RelationLabel"
        );
        std::fs::write(
            out.join(format!("{prefix}-dispatch.json")),
            serde_json::to_vec_pretty(&dispatch).map_err(words)?,
        )
        .map_err(words)?;
        key(
            &mut mounted.session,
            if all { "secondary-enter" } else { "enter" },
        )?;
        assert_eq!(
            mounted
                .session
                .update(|_, cx| mounted.graph.store.read(cx).snapshot().route().clone())
                .map_err(words)?,
            destination
        );
        capture(
            &mut mounted,
            &out,
            &revision,
            &format!("{prefix}-fresh-native-submission"),
        )?;
        assert_eq!(
            mounted
                .session
                .update(|_, cx| mounted.shell.read(cx).reader_entity())
                .map_err(words)?,
            reader
        );
        if !all {
            let symbol = super::tests::symbol("RelationLabel");
            let body = mounted
                .session
                .update(|_, cx| {
                    mounted
                        .graph
                        .store
                        .read(cx)
                        .symbol(&symbol)
                        .loaded_arc()
                        .cloned()
                        .ok_or("current Reader body missing")
                })
                .map_err(words)??;
            let current = capture(&mut mounted, &out, &revision, "retained-reader-current")?;
            let structured = ["WHAT IT IS", "Typed", "SemanticLinkKind",
                "WHAT YOU CAN DO WITH IT", "as_str", "is_typed", "IN YOUR WORKSPACE",
                "Reference information is unavailable."];
            for fact in structured {
                assert!(current["painted"].as_array().is_some_and(|words|
                    words.iter().any(|word| word == fact)), "current exact declaration paints {fact}");
            }
            let fault = OwnerFault::Lost("controlled retained Reader owner retired".into());
            gate.publish(OwnerState::Failed(fault.clone()));
            mounted
                .session
                .update(|_, cx| {
                    mounted
                        .graph
                        .store
                        .update(cx, |store, cx| store.owner_failed(&fault, cx))
                })
                .map_err(words)?;
            let held = capture(
                &mut mounted,
                &out,
                &revision,
                "retained-reader-owner-unavailable",
            )?;
            assert_eq!(held["owner_serving"], false);
            assert_eq!(held["fixture_request_counts"], current["fixture_request_counts"],
                "retained presentation starts no primary or companion reads");
            for fact in structured {
                assert!(held["painted"].as_array().is_some_and(|words|
                    words.iter().any(|word| word == fact)), "earlier exact declaration keeps {fact}");
            }
            assert!(held["painted"].as_array().is_some_and(|words|
                words.iter().any(|word| word == "Earlier reading · read-only")));
            let tree: serde_json::Value = serde_json::from_slice(&std::fs::read(
                out.join("retained-reader-owner-unavailable.accesskit.json")).map_err(words)?).map_err(words)?;
            let nodes = tree["nodes"].as_object().ok_or("retained native tree missing")?;
            assert!(nodes.values().any(|node| node["aria"]["role"] == "Heading"
                && node["aria"]["label"] == "RelationLabel"));
            assert!(nodes.values().any(|node| node["aria"]["role"] == "Status"
                && node["aria"]["label"].as_str().is_some_and(|label| label.contains("read-only"))));
            let source = nodes.values().find(|node| node["element_id"] == "Name(\"s6-block-source-place\")")
                .ok_or("retained structured source location missing")?;
            assert_eq!(source["aria"]["label"], "glyph.rs:138");
            assert!(!nodes.values().any(|node| node["aria"]["label"] == "glyph.rs:138"
                && node["aria"]["disabled"] != true
                && node["aria"]["on_action"].as_array().is_some_and(|actions|
                    actions.iter().any(|action| action == "Click"))));
            let x = source["bounds"]["x"].as_f64().ok_or("retained source x missing")?
                + source["bounds"]["width"].as_f64().ok_or("retained source width missing")? / 2.0;
            let y = source["bounds"]["y"].as_f64().ok_or("retained source y missing")?
                + source["bounds"]["height"].as_f64().ok_or("retained source height missing")? / 2.0;
            assert!((0.0..1440.0).contains(&x) && (0.0..900.0).contains(&y));
            mounted.session.apply(&Act::Click { x: x as f32, y: y as f32,
                button: backend_gui_harness::Button::Left }, &mut |_, _, _| {}).map_err(words)?;
            let inert = capture(&mut mounted, &out, &revision, "retained-reader-native-source-click")?;
            assert_eq!(inert["route"], held["route"]);
            assert_eq!(inert["fixture_request_counts"], held["fixture_request_counts"]);
            mounted
                .session
                .update(|_, cx| {
                    let store = mounted.graph.store.read(cx);
                    let resource = store.symbol(&symbol);
                    assert!(
                        resource
                            .loaded_arc()
                            .is_some_and(|value| Arc::ptr_eq(value, &body))
                    );
                    assert!(
                        !crate::core::admit_resource(
                            &resource,
                            store.snapshot().key(),
                            store.owner_serving()
                        )
                        .allows_actions()
                    );
                    assert_eq!(mounted.shell.read(cx).reader_entity(), reader);
                })
                .map_err(words)?;
            let root = mounted
                .session
                .update(|_, cx| mounted.graph.store.read(cx).snapshot().key())
                .map_err(words)?;
            gate.publish(OwnerState::Ready {
                key: root,
                mode: crate::model::ServiceMode::Attached,
            });
            mounted
                .session
                .update(|_, cx| mounted.graph.store.update(cx, DataStore::owner_ready))
                .map_err(words)?;
            let renewed = capture(
                &mut mounted,
                &out,
                &revision,
                "retained-reader-same-root-renewed",
            )?;
            assert_eq!(renewed["owner_serving"], true);
            mounted
                .session
                .update(|_, cx| {
                    let store = mounted.graph.store.read(cx);
                    let resource = store.symbol(&symbol);
                    assert!(
                        crate::core::admit_resource(
                            &resource,
                            store.snapshot().key(),
                            store.owner_serving()
                        )
                        .current_value()
                        .is_some()
                    );
                    assert_eq!(mounted.shell.read(cx).reader_entity(), reader);
                })
                .map_err(words)?;
        }
    }
    let terminal = "caused by: publication journal root cause 日本語 sentinel";
    let detail: Arc<str> = format!(
        "open compiler owner: {}\n{terminal}\0",
        "intermediate 原因\t".repeat(90)
    )
    .into();
    let expected = crate::core::ErrorValue::with_diagnostic(
        crate::core::FaultCode::Transport,
        "The index could not start. ",
        &detail,
    );
    assert!(
        expected
            .diagnostic_detail()
            .is_some_and(|detail| detail.len() > 512 && detail.trim_end().ends_with(terminal))
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    let pool = ReadPool::start(1, move |_| CaptureDiagnosticRead {
        reads: Arc::clone(&observed),
        detail: Arc::clone(&detail),
    })
    .map_err(words)?;
    let project = LocalProjectId::new("/fixture/current-owner-diagnostic").map_err(words)?;
    let (mut mounted, _) = open_ready_capture(
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project))),
        pool,
    )?;
    for (width, percent) in [(1440, 100), (720, 200)] {
        let display = mounted
            .session
            .update(|_, cx| mounted.shell.read(cx).display_key())
            .map_err(words)?;
        mounted
            .session
            .update(|_, cx| {
                mounted.graph.root.update(cx, |root, cx| {
                    root.dispatch(crate::navigation::Intent::ZoomTo { display, percent }, cx)
                })
            })
            .map_err(words)?;
        mounted
            .session
            .apply(&Act::Resize { width, height: 900 }, &mut |_, _, _| {})
            .map_err(words)?;
        capture(
            &mut mounted,
            &out,
            &revision,
            &format!("corrected-copy-{percent}-{width}-before"),
        )?;
        let before = reads.load(Ordering::SeqCst);
        scroll_copy_into_view(&mut mounted, width, &out, &revision, percent)?;
        capture(
            &mut mounted,
            &out,
            &revision,
            &format!("corrected-copy-{percent}-{width}-reachable"),
        )?;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            before,
            "native scrolling cannot renew the failed read"
        );
        for keyboard in [false, true] {
            mounted
                .session
                .update(|_, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                        "before-corrected-copy".into(),
                    ))
                })
                .map_err(words)?;
            if keyboard {
                key(&mut mounted.session, "space")?;
            } else {
                click_control(&mut mounted, "Copy diagnostic")?;
            }
            let clipboard = mounted
                .session
                .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
                .map_err(words)?;
            assert_eq!(clipboard, expected.diagnostic_detail().map(str::to_owned));
            assert_eq!(
                reads.load(Ordering::SeqCst),
                before,
                "Copy cannot renew the failed read"
            );
            let name = format!(
                "corrected-copy-{percent}-{width}-{}",
                if keyboard { "keyboard" } else { "pointer" }
            );
            capture(&mut mounted, &out, &revision, &name)?;
            let ax: serde_json::Value = serde_json::from_slice(
                &std::fs::read(out.join(format!("{name}.accesskit.json"))).map_err(words)?,
            )
            .map_err(words)?;
            let focus = ax["gpui_focus"]
                .as_str()
                .ok_or("Copy native focus missing")?;
            let node = &ax["nodes"][focus];
            assert_eq!(node["aria"]["role"], "Button");
            assert_eq!(node["aria"]["label"], "Copy diagnostic");
            let bounds = &node["bounds"];
            let left = bounds["x"].as_f64().ok_or("Copy native left missing")?;
            let top = bounds["y"].as_f64().ok_or("Copy native top missing")?;
            let w = bounds["width"]
                .as_f64()
                .ok_or("Copy native width missing")?;
            let h = bounds["height"]
                .as_f64()
                .ok_or("Copy native height missing")?;
            assert!(
                w > 0.0
                    && h > 0.0
                    && left >= 0.0
                    && top >= 0.0
                    && left + w <= f64::from(width) + 0.5
                    && top + h <= 900.5,
                "actual Copy native focus and pointer hitbox fit the viewport: {node}"
            );
            std::fs::write(out.join(format!("{name}-clipboard.json")), serde_json::to_vec_pretty(&serde_json::json!({
                "source": revision, "clipboard": clipboard, "reads_before": before,
                "reads_after": reads.load(Ordering::SeqCst), "complete_sanitized_detail_equal": true,
            })).map_err(words)?).map_err(words)?;
        }
    }
    Ok(())
}
