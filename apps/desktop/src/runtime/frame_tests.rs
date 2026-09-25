//! Frame-budget proofs for the event-driven wake, through the real window
//! root (the shell and its regions).
//!
//! The test platform never draws on its own, so these tests play the
//! platform's part: every simulated vsync they run the window's next-frame
//! callbacks (what `request_animation_frame` and the motion gate schedule)
//! and draw only when a frame was requested or a view was notified (what
//! dirties a real window). An idle window must do neither — even with an
//! index running in the background.

#![allow(clippy::expect_used, clippy::panic)]

use super::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use super::coordinator::DesktopRuntime;
use super::reads::ReadPool;
use super::ui_graph::UiEntityGraph;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::Intent;
use crate::shell::Shell;
use crate::shell::tests::{Fixture, page_route};
use gpui::{AnyWindowHandle, AppContext as _, Entity, Subscription, TestAppContext};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Answers the root at once and holds every index request until released.
struct HeldIndexClient {
    release: Arc<(Mutex<bool>, Condvar)>,
}

fn next_key(basis: VersionedRoot) -> (VersionedRoot, backend_library::Cursor) {
    let revision = backend_library::Cursor::at(basis.root(), basis.generation().saturating_add(1));
    (
        VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation()),
        revision,
    )
}

impl EngineClient for HeldIndexClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { request, basis, .. } => {
                let (key, revision) = next_key(*basis);
                Ok(EngineDto::Root {
                    request: *request,
                    basis: *basis,
                    key,
                    revision,
                    delta: None,
                    project: None,
                    catalog: None,
                })
            }
            EngineRequest::IndexProject {
                request,
                project,
                basis,
                ..
            } => {
                let (lock, released) = &*self.release;
                let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
                let deadline = Instant::now() + Duration::from_secs(30);
                while !*open {
                    assert!(Instant::now() < deadline, "the index was never released");
                    open = released
                        .wait_timeout(open, Duration::from_millis(10))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                let (key, revision) = next_key(*basis);
                Ok(EngineDto::Index {
                    request: *request,
                    basis: *basis,
                    key,
                    revision,
                    delta: None,
                    project: project.clone(),
                    project_state: None,
                    catalog: None,
                    files_indexed: Some(3),
                })
            }
            EngineRequest::Object { .. } | EngineRequest::Surface { .. } => Err(EngineFault::Cancelled),
        }
    }
}

struct Rig {
    window: AnyWindowHandle,
    graph: UiEntityGraph,
    shell: Entity<Shell>,
    notified: Rc<Cell<u64>>,
    seen: Rc<Cell<u64>>,
    release: Arc<(Mutex<bool>, Condvar)>,
    project: LocalProjectId,
    _views: Vec<Subscription>,
    _folder: std::path::PathBuf,
}

fn rig(cx: &mut TestAppContext) -> Rig {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
        cx.bind_keys(crate::shell::key_bindings());
    });
    let folder = std::env::temp_dir().join(format!("nudox-frames-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("project folder");
    let project = LocalProjectId::from_path(&folder).expect("project identity");
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let client = HeldIndexClient {
        release: Arc::clone(&release),
    };
    let snapshot = AppSnapshot::empty(VersionedRoot::synthetic(
        backend_library::view_state_root(&[("frames".to_owned(), "idle".to_owned())]),
        3,
    ));
    let actor = EngineActor::start(client, 8).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let pool = ReadPool::start(2, |_| Fixture).expect("pool");
    let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
    let window_graph = UiEntityGraph {
        root: graph.root.clone(),
        store: graph.store.clone(),
    };
    let handle = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                cx.new(|cx| Shell::new(&window_graph, window, cx))
            })
        })
        .expect("test window");
    let shell = handle.root(cx).expect("shell");
    let notified = Rc::new(Cell::new(0_u64));
    let views = cx.update(|cx| Shell::watch_views(&shell, cx, Rc::clone(&notified)));
    Rig {
        window: handle.into(),
        graph,
        shell,
        notified,
        seen: Rc::new(Cell::new(0)),
        release,
        project,
        _views: views,
        _folder: folder,
    }
}

/// Plays the platform for `ticks` vsyncs of 16 ms: runs next-frame
/// callbacks and draws when a frame was requested or a view was notified
/// since the last draw. Returns (frames drawn, frames requested).
fn vsync(cx: &mut TestAppContext, rig: &Rig, ticks: usize) -> (usize, usize) {
    let mut drawn = 0;
    let mut requested = 0;
    for _ in 0..ticks {
        cx.executor().advance_clock(Duration::from_millis(16));
        cx.run_until_parked();
        let callbacks = cx
            .update_window(rig.window, |_, window, cx| window.simulate_next_frame(cx))
            .expect("window");
        requested += callbacks;
        cx.run_until_parked();
        if callbacks > 0 || rig.notified.get() != rig.seen.get() {
            draw(cx, rig);
            drawn += 1;
        }
    }
    (drawn, requested)
}

fn draw(cx: &mut TestAppContext, rig: &Rig) {
    cx.update_window(rig.window, |_, window, cx| window.draw(cx).clear(cx))
        .expect("window");
    cx.run_until_parked();
    rig.seen.set(rig.notified.get());
}

/// Plays vsyncs until one virtual second passes with no frame.
fn settle(cx: &mut TestAppContext, rig: &Rig) {
    draw(cx, rig);
    for _ in 0..400 {
        let (drawn, _) = vsync(cx, rig, 60);
        let (queued, running) = rig.graph.store.read_with(cx, |store, _| store.pool_load());
        if drawn == 0 && queued == 0 && running == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the window never settled");
}

fn phase(cx: &mut TestAppContext, rig: &Rig) -> Option<ProjectPhase> {
    let project = rig.project.clone();
    rig.graph.root.read_with(cx, |root, _| {
        root.snapshot()
            .workspace()
            .projects
            .iter()
            .find(|item| item.id == project)
            .map(|item| item.phase)
    })
}

fn start_index(cx: &mut TestAppContext, rig: &Rig) {
    let project = rig.project.clone();
    rig.graph.root.update(cx, |root, cx| {
        root.dispatch(Intent::AddProject { project }, cx);
    });
    for _ in 0..500 {
        cx.run_until_parked();
        let requested = rig.graph.root.read_with(cx, |root, _| {
            root.snapshot()
                .workspace()
                .projects
                .iter()
                .any(|item| item.phase == ProjectPhase::Indexing && item.request.is_some())
        });
        if requested {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the index never started");
}

fn release(rig: &Rig) {
    let (lock, released) = &*rig.release;
    *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
    released.notify_all();
}

#[gpui::test]
fn an_idle_shell_with_a_running_index_requests_no_frames(cx: &mut TestAppContext) {
    let rig = rig(cx);
    settle(cx, &rig);
    start_index(cx, &rig);
    rig.graph
        .root
        .update(cx, |root, cx| root.queue(Intent::Navigate(page_route("RelationLabel")), cx));
    settle(cx, &rig);
    let renders_before = rig.shell.read_with(cx, |shell, cx| shell.render_counts(cx));

    // Ten virtual seconds with the index still running.
    let (drawn, requested) = vsync(cx, &rig, 625);
    assert_eq!(phase(cx, &rig), Some(ProjectPhase::Indexing), "the index is still running");
    assert_eq!(requested, 0, "no animation frame was requested while idle");
    assert_eq!(drawn, 0, "no frame was drawn while idle");
    assert_eq!(
        rig.shell.read_with(cx, |shell, cx| shell.render_counts(cx)),
        renders_before,
        "no region rendered while idle"
    );

    // The result wakes the window by itself: no frame loop carried it here.
    release(&rig);
    let mut woke = false;
    for _ in 0..2_000 {
        cx.run_until_parked();
        if phase(cx, &rig) == Some(ProjectPhase::Ready) {
            woke = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(woke, "the finished index reached the root through the wake task");
    settle(cx, &rig);
    let (drawn, requested) = vsync(cx, &rig, 120);
    assert_eq!((drawn, requested), (0, 0), "then the window is idle again");
}

/// Control: the same measurement sees the descent's frames — a route change
/// animates, so frames are requested while it plays and stop when it lands.
/// The zero above is a real zero.
#[gpui::test]
fn the_measurement_sees_a_descent_and_then_sees_it_stop(cx: &mut TestAppContext) {
    let rig = rig(cx);
    settle(cx, &rig);
    rig.graph
        .root
        .update(cx, |root, cx| root.queue(Intent::Navigate(page_route("KindGlyph")), cx));
    cx.run_until_parked();
    draw(cx, &rig);
    let (drawn, requested) = vsync(cx, &rig, 20);
    assert!(requested >= 10 && drawn >= 10, "the descent asked for frames: {requested} requested, {drawn} drawn");
    settle(cx, &rig);
    let (drawn, requested) = vsync(cx, &rig, 120);
    assert_eq!((drawn, requested), (0, 0), "and stopped once it landed");
}
