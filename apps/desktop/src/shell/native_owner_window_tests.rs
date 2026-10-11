//! A controlled owner behind an actual Linux GPUI window. Pixels are captured
//! externally from that window; TestPlatform and offscreen readback are absent.
//!
//! After separate resource admission, run this ignored test under an isolated
//! X11 display with NUDOX_OWNER_WINDOW_DIRECTORY and NUDOX_OWNER_WINDOW_SOURCE.
//! Atomically publish command.json: {"seq":1,"command":"fail"}. Commands are
//! fail, replace, ready, advance, quit. Retry and Back use real native pointer
//! input from the external driver, never a command that dispatches their intent.
//! evidence.json describes the last committed frame; it never requests a draw.

use crate::core::VersionedRoot;
use crate::model::{AppSnapshot, MotionPreference, ServiceMode};
use crate::navigation::{Intent, Route};
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::owner::{OwnerGate, OwnerState, PublicationAdmission};
use crate::runtime::reads::ReadPool;
use crate::runtime::{DesktopRuntime, EngineActor, UiEntityGraph};
use gpui::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct StableRoot;

impl EngineClient for StableRoot {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Root { request, basis, .. } = request else {
            return Err(EngineFault::Cancelled);
        };
        Ok(EngineDto::Root {
            request: *request,
            basis: *basis,
            key: *basis,
            revision: basis.revision(),
            delta: None,
            project: None,
            catalog: None,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    seq: u64,
    command: String,
}

fn read_command(path: &Path) -> Result<Option<Command>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 4096 => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Ok(_) => Err("native fixture command exceeds 4096 bytes".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

#[test]
#[ignore = "actual Linux window: admitted display, private evidence directory and exact source receipt required"]
fn owner_publications_mount_a_native_linux_window() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("NUDOX_OWNER_WINDOW_DIRECTORY")
            .expect("owned native-window evidence directory"),
    );
    assert!(directory.is_absolute() && directory.is_dir());
    let source = std::env::var("NUDOX_OWNER_WINDOW_SOURCE").expect("launcher exact-source receipt");
    assert_eq!(source.len(), 40, "source must be the full candidate commit");
    facet::fonts::verify().expect("bundled font identities");
    let failed = Rc::new(RefCell::new(None));
    let failure = Rc::clone(&failed);
    gpui_platform::application().with_assets(facet::icons::Assets).run(move |cx| {
        gpui_component::init(cx);
        facet::fonts::install(cx).expect("actual platform font installation");
        cx.bind_keys(super::keys::bindings());
        cx.set_global(gpui::TextTrace);
        let view = crate::runtime::owner::publication_tests::view();
        let mut cursor = backend_library::Cursor::for_view_root_at(&view, 0);
        let key = VersionedRoot::from_revision(4, cursor, 0);
        let gate = OwnerGate::ready(key, ServiceMode::Attached);
        let attachment = gate.ready_epoch().expect("initial owner attachment");
        assert_eq!(gate.publish_view(attachment, Arc::clone(&view), cursor), PublicationAdmission::Admitted);
        let snapshot = AppSnapshot::empty(key);
        let mut settings = snapshot.settings().clone();
        settings.motion = MotionPreference::Reduced;
        settings.reduced_motion = true;
        settings.service_mode = ServiceMode::Attached;
        let snapshot = snapshot.with_settings(settings);
        let actor = EngineActor::start(StableRoot, 8).expect("controlled owner actor");
        let pool = ReadPool::start(2, |_| super::tests::Fixture).expect("controlled native page pool");
        let graph = UiEntityGraph::install_with_owner(cx, DesktopRuntime::new(snapshot, actor),
            None, Some(pool), Some(gate.clone()), None);
        let mounted = Rc::new(RefCell::new(None));
        let built = Rc::clone(&mounted);
        let window_graph = UiEntityGraph { root: graph.root.clone(), store: graph.store.clone() };
        let closing = crate::host::close::GracefulClose::install(&graph, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))))),
            titlebar: Some(gpui::TitlebarOptions { title: Some("Nudox controlled native owner".into()), ..Default::default() }),
            ..Default::default()
        };
        let window = cx.open_window(options, move |window, cx| {
            window.set_a11y_forced(true);
            let shell = super::open_shell(&window_graph, window, cx);
            crate::host::close::GracefulClose::attach(&closing, window, cx);
            let view = cx.new(|cx| crate::host::close::CloseView::new(shell.clone(), closing, cx));
            *built.borrow_mut() = Some(shell);
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        }).expect("real Linux platform window");
        let shell = mounted.borrow_mut().take().expect("mounted production shell");
        graph.root.update(cx, |root, cx| root.dispatch(Intent::Navigate(Route::World), cx));
        let quit_gate = gate.clone();
        cx.on_app_quit(move |_| { quit_gate.close(); async {} }).detach();
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&events);
        let subscription = cx.subscribe(&graph.store, move |_, event, _| {
            let mut events = observed.borrow_mut();
            if events.len() == 256 { events.remove(0); }
            events.push(format!("{event:?}"));
        });
        cx.activate(true);
        let timer = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            let _subscription = subscription;
            let deadline = Instant::now() + Duration::from_secs(300);
            let mut seq = 0;
            let mut saved = None;
            loop {
                timer.timer(Duration::from_millis(50)).await;
                let result = cx.update(|cx| -> Result<bool, String> {
                    if Instant::now() > deadline { return Err("native fixture exceeded its five-minute lifetime".into()); }
                    if let Some(command) = read_command(&directory.join("command.json"))? {
                        if command.seq > seq {
                            if command.seq != seq + 1 { return Err("native fixture command sequence is not contiguous".into()); }
                            match command.command.as_str() {
                                "fail" => gate.publish(OwnerState::Failed("controlled identical owner failure".into())),
                                "replace" => {
                                    if !graph.store.read(cx).owner_serving() { return Err("replace requires an admitted serving owner".into()); }
                                    let key = graph.store.read(cx).snapshot().key();
                                    gate.publish(OwnerState::Starting);
                                    gate.publish(OwnerState::Ready { key, mode: ServiceMode::Attached });
                                }
                                "ready" => {
                                    if !matches!(gate.state(), OwnerState::Starting) { return Err("ready requires an actual native Retry to start the owner".into()); }
                                    gate.publish(OwnerState::Ready { key: VersionedRoot::from_revision(4, cursor, 0), mode: ServiceMode::Attached });
                                    let attachment = gate.ready_epoch().ok_or("responding owner absent")?;
                                    if gate.publish_view(attachment, Arc::clone(&view), cursor) != PublicationAdmission::Admitted {
                                        return Err("certified fixture recovery was not admitted".into());
                                    }
                                }
                                "advance" => {
                                    cursor = backend_library::Cursor::for_view_root_at(&view, cursor.sequence().saturating_add(1));
                                    let attachment = gate.ready_epoch().ok_or("advance requires serving owner")?;
                                    if gate.publish_view(attachment, Arc::clone(&view), cursor) != PublicationAdmission::Admitted {
                                        return Err("fixture authority advance was not admitted".into());
                                    }
                                }
                                "quit" => { cx.quit(); return Ok(false); }
                                _ => return Err("unknown native fixture command".into()),
                            }
                            seq = command.seq;
                        }
                    }
                    window.update(cx, |_, window, cx| -> Result<(), String> {
                        let frame = window.a11y_frame_number();
                        if saved == Some((seq, frame)) { return Ok(()); }
                        let store = graph.store.read(cx);
                        let snapshot = store.snapshot();
                        let counts = shell.read(cx).render_counts(cx);
                        let Some(tree) = window.debug_a11y_tree_json() else { return Ok(()); };
                        let tree: serde_json::Value = serde_json::from_str(&tree)
                            .map_err(|error| error.to_string())?;
                        let evidence = serde_json::json!({
                            "schema": "nudox-native-owner-window-fixture.v1", "source_claim": source,
                            "controlled_owner": true, "platform": "Linux native GPUI", "pixel_source": "external window capture",
                            "command_seq": seq, "frame": frame, "titlebar_renders": counts.titlebar,
                            "route": format!("{:?}", snapshot.route()), "root": format!("{:?}", snapshot.key()),
                            "back": format!("{:?}", snapshot.session().back), "owner_state": format!("{:?}", gate.state()),
                            "owner_serving": store.owner_serving(), "retry_current": store.current_owner_retry().is_some(),
                            "store_events": events.borrow().clone(), "native_tree": tree,
                        });
                        let bytes = serde_json::to_vec_pretty(&evidence).map_err(|error| error.to_string())?;
                        std::fs::write(directory.join("evidence.next.json"), bytes).map_err(|error| error.to_string())?;
                        std::fs::rename(directory.join("evidence.next.json"), directory.join("evidence.json")).map_err(|error| error.to_string())?;
                        saved = Some((seq, frame));
                        Ok(())
                    }).map_err(|error| error.to_string())??;
                    Ok(true)
                });
                match result {
                    Ok(true) => {}
                    Ok(false) => break,
                    result => {
                        *failure.borrow_mut() = Some(format!("native fixture stopped: {result:?}"));
                        let _ = cx.update(|cx| cx.quit());
                        break;
                    }
                }
            }
        }).detach();
    });
    assert!(
        failed.borrow().is_none(),
        "native fixture failed: {:?}",
        failed.borrow()
    );
}
