//! The lifecycle around first run: what a relaunch gives back, and what the
//! window says when it could not.
//!
//! A "relaunch" here is the product's own `prepare` on the data directory the
//! last run wrote (the same restore `main` does before its window exists).
//! Each assertion is on a value a person would see or feel: which projects,
//! which one is active, what phase, which theme, how dense, how large.

#![allow(clippy::expect_used, clippy::panic)]

use super::launch::prepare;
use crate::core::{LocalProjectId, PackageId, VersionedRoot};
use crate::model::pages::{PageKey, PageValue, ReadFailure};
use crate::model::{
    AppSnapshot, AppearancePreference, ContrastPreference, DensityPreference, MotionPreference,
    Note, PersistedDesktopState, PersistedProjectPhase, PersistentState, ProjectPhase, WindowSize,
    ZoomStep,
};
use crate::navigation::{Intent, PackageLane, PackageRoute, Route, reduce};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::{
    DesktopRuntime, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest,
    UiEntityGraph, wait,
};
use backend_runtime::WorkspacePaths;
use gpui::{TestAppContext, VisualTestContext, point, px, size};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // `/tmp`, not `temp_dir()`: the owner's socket path must fit `sockaddr_un`.
    PathBuf::from("/tmp").join(format!("nx-life-{tag}-{}-{nonce}", std::process::id()))
}

/// A workspace: a project folder, and an empty data directory.
fn workspace(tag: &str) -> (WorkspacePaths, PathBuf) {
    let root = scratch(tag);
    let project = root.join("project");
    let data = root.join("data");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::create_dir_all(&data).expect("data");
    let paths = WorkspacePaths::discover(
        Some(project),
        Some(data.clone()),
        Some(root.with_extension("sock")),
    )
    .expect("paths");
    (paths, data.join("desktop-state.json"))
}

fn folder(parent: &Path, name: &str) -> LocalProjectId {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).expect("folder");
    LocalProjectId::from_path(&path.canonicalize().expect("canonical")).expect("identity")
}

fn go(snapshot: &AppSnapshot, intent: Intent) -> AppSnapshot {
    reduce(snapshot, intent).snapshot
}

/// The state a person leaves behind: two projects (one ready, one stopped, the
/// stopped one active), Glacier, dense, high contrast, reduced motion, a text
/// zoom on one display, a resized window, the shelf folded, and a package page.
fn left_as_it_was(parent: &Path) -> (AppSnapshot, LocalProjectId, LocalProjectId, Route) {
    let (alpha, beta) = (folder(parent, "alpha"), folder(parent, "beta"));
    let mut snapshot = AppSnapshot::empty(VersionedRoot::unserved());
    for project in [&alpha, &beta] {
        snapshot = go(
            &snapshot,
            Intent::AddProject {
                project: project.clone(),
            },
        );
    }
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut project| {
            if project.id == alpha {
                project.phase = ProjectPhase::Ready;
            } else {
                project = project.with_error("local semantic compilation failed for src/lib.rs");
            }
            project
        })
        .collect::<Vec<_>>()
        .into();
    snapshot = snapshot.with_workspace(workspace);
    for intent in [
        Intent::ActivateProject(beta.clone()),
        Intent::SetAppearance(AppearancePreference::Glacier),
        Intent::SetDensity(DensityPreference::Dense),
        Intent::SetContrast(ContrastPreference::High),
        Intent::SetMotion(MotionPreference::Reduced),
        Intent::Zoom {
            display: Arc::from("wall"),
            step: ZoomStep::In,
        },
        Intent::Zoom {
            display: Arc::from("wall"),
            step: ZoomStep::In,
        },
        Intent::WindowResized {
            width: 1100,
            height: 800,
        },
        Intent::ToggleShelf,
    ] {
        snapshot = go(&snapshot, intent);
    }
    let page = Route::Package(PackageRoute {
        project: None,
        package: PackageId::new("/fixture/present").expect("package id"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    });
    snapshot = go(&snapshot, Intent::Navigate(page.clone()));
    (snapshot, alpha, beta, page)
}

#[test]
fn a_relaunch_gives_back_every_field_a_person_left() {
    let (paths, state_file) = workspace("relaunch");
    let parent = state_file
        .parent()
        .and_then(Path::parent)
        .expect("root")
        .to_path_buf();
    let (left, alpha, beta, page) = left_as_it_was(&parent);
    PersistentState::at(&state_file)
        .save(&PersistentState::project(&left))
        .expect("the last run saved");

    let boot = prepare(Ok(paths), |_, _| None);
    let back = boot.snapshot;
    let rows: Vec<_> = back
        .workspace()
        .projects
        .iter()
        .map(|project| (project.id.clone(), project.phase, project.error.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            (alpha.clone(), ProjectPhase::Ready, None),
            (
                beta.clone(),
                ProjectPhase::Failed,
                Some(Arc::from(
                    "local semantic compilation failed for src/lib.rs"
                ))
            ),
        ],
        "the shelf comes back with each project's phase, and the stopped one's reason"
    );
    assert_eq!(
        back.workspace().active.as_ref(),
        Some(&beta),
        "the active project comes back"
    );
    assert_eq!(back.route(), &page, "so does the page");
    let settings = back.settings();
    assert_eq!(settings.appearance, AppearancePreference::Glacier);
    assert_eq!(settings.density, DensityPreference::Dense);
    assert_eq!(settings.contrast, ContrastPreference::High);
    assert_eq!(settings.motion, MotionPreference::Reduced);
    assert!(settings.reduced_motion);
    assert_eq!(
        settings.zoom.percent("wall"),
        125,
        "two steps up the ladder from 100 is 125 % on that display"
    );
    assert_eq!(
        settings.zoom.percent("laptop"),
        100,
        "and only on that display"
    );
    assert_eq!(
        settings.window,
        Some(WindowSize {
            width: 1100,
            height: 800
        }),
        "the window comes back at its size"
    );
    assert!(!settings.shelf_open, "the folded shelf stays folded");
    assert!(
        back.workspace().notes.is_empty(),
        "a clean relaunch has nothing to explain"
    );
}

#[test]
fn a_damaged_session_is_kept_and_the_window_says_so() {
    let (paths, state_file) = workspace("damaged");
    std::fs::write(&state_file, b"{ this is not json").expect("damage it");
    let boot = prepare(Ok(paths), |_, _| None);
    let [Note::StateKept { backup, why }] = boot.snapshot.workspace().notes.as_ref() else {
        panic!(
            "the window does not say the session was kept: {:?}",
            boot.snapshot.workspace().notes
        );
    };
    assert_eq!(
        why.as_ref(),
        "invalid JSON or state shape",
        "why, in the words persistence gives"
    );
    assert_eq!(
        std::fs::read(backup.as_ref()).expect("the kept file"),
        b"{ this is not json",
        "the bytes are kept, not deleted"
    );
    assert!(
        boot.persistence.is_some(),
        "this launch saves over the default, the original is safe"
    );
}

#[test]
fn a_session_from_a_newer_build_is_kept_untouched_and_named() {
    let (paths, state_file) = workspace("newer");
    let mut value = serde_json::to_value(PersistedDesktopState::default()).expect("json");
    value["schema"] = serde_json::json!(99);
    let written = serde_json::to_vec(&value).expect("bytes");
    std::fs::write(&state_file, &written).expect("write");
    let boot = prepare(Ok(paths), |_, _| None);
    let [Note::StateKept { backup, why }] = boot.snapshot.workspace().notes.as_ref() else {
        panic!("not said: {:?}", boot.snapshot.workspace().notes);
    };
    assert_eq!(why.as_ref(), "schema 99 is incompatible with schema 1");
    assert_eq!(
        std::fs::read(backup.as_ref()).expect("the kept file"),
        written,
        "the newer build's file is exactly as it was"
    );
}

/// An owner that indexes whatever it is asked to, at once.
struct Indexes;

fn next(basis: VersionedRoot) -> (VersionedRoot, backend_library::Cursor) {
    let revision = backend_library::Cursor::at(basis.root(), basis.generation().saturating_add(1));
    (
        VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation()),
        revision,
    )
}

impl EngineClient for Indexes {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::IndexProject {
                request,
                project,
                basis,
                ..
            } => {
                let (key, revision) = next(*basis);
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
            _ => Err(EngineFault::Cancelled),
        }
    }
}

#[gpui::test]
fn a_finished_index_is_written_down_the_moment_it_finishes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = scratch("finished");
    std::fs::create_dir_all(&root).expect("dir");
    let state_file = root.join("desktop-state.json");
    let project = folder(&root, "project");
    let snapshot = AppSnapshot::empty(VersionedRoot::synthetic(
        backend_library::view_state_root(&[("lifecycle".to_owned(), "finished".to_owned())]),
        3,
    ));
    let runtime = DesktopRuntime::new(snapshot, EngineActor::start(Indexes, 8).expect("actor"));
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_reads(cx, runtime, Some(PersistentState::at(&state_file)), None)
    });
    let asked = project.clone();
    graph.root.update(cx, |root, cx| {
        root.dispatch(Intent::AddProject { project: asked }, cx)
    });
    wait::until("the index finished", || {
        cx.run_until_parked();
        graph.root.read_with(cx, |root, _| {
            root.snapshot()
                .workspace()
                .projects
                .iter()
                .any(|item| item.id == project && item.phase == ProjectPhase::Ready)
        })
    });
    // Nothing else asked for a save after the result arrived: what is on disk
    // is what the result wrote.
    let saved = PersistentState::at(&state_file)
        .load()
        .expect("the state file");
    assert_eq!(
        saved
            .shelf
            .iter()
            .map(|item| (item.local_path.clone(), item.phase, item.files_indexed))
            .collect::<Vec<_>>(),
        [(
            project.as_str().to_owned(),
            PersistedProjectPhase::Ready,
            Some(3)
        )],
        "a relaunch finds the project ready, with the files the owner counted"
    );
    std::fs::remove_dir_all(&root).ok();
}

// ── an owner that could not start ─────────────────────────────────────────

/// The engine actor of a window whose owner has not answered: waits on the
/// gate like the product's client, and answers nothing.
struct WaitsForOwner(OwnerGate);

impl EngineClient for WaitsForOwner {
    fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
        self.0.wait().map_err(|fault| {
            EngineFault::Failed(crate::core::ErrorValue::new(
                crate::core::FaultCode::Transport,
                fault.to_string(),
            ))
        })?;
        Err(EngineFault::Cancelled)
    }
}

/// The read pool's side: waits on the gate, then serves the shell's fixture.
struct PagesAfterOwner(OwnerGate);

impl PageReader for PagesAfterOwner {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        self.0.wait().map_err(|fault| {
            ReadFailure::Fault(crate::core::ErrorValue::new(
                crate::core::FaultCode::Transport,
                fault.to_string(),
            ))
        })?;
        crate::shell::tests::Fixture.read(request, context)
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// Draws until `done` holds.
fn until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait::until(what, || {
        draw(cx);
        done(cx)
    });
}

/// A window at the Library whose owner has not answered yet.
fn window_before_its_owner(
    cx: &mut TestAppContext,
    gate: &OwnerGate,
) -> (UiEntityGraph, gpui::Entity<crate::shell::Shell>) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
        crate::shell::bodies::graph::install_test_fixture(cx);
    });
    let snapshot = AppSnapshot::empty(VersionedRoot::unserved());
    let runtime = DesktopRuntime::new(
        snapshot,
        EngineActor::start(WaitsForOwner(gate.clone()), 8).expect("actor"),
    );
    let pages = gate.clone();
    let pool = ReadPool::start(2, move |_| PagesAfterOwner(pages.clone())).expect("pool");
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(cx, runtime, None, Some(pool), Some(gate.clone()), None)
    });
    let window_graph = UiEntityGraph {
        root: graph.root.clone(),
        store: graph.store.clone(),
    };
    let window = cx.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(1440.0), px(900.0)),
                ))),
                ..gpui::WindowOptions::default()
            },
            |window, cx| crate::shell::open_shell(&window_graph, window, cx),
        )
        .expect("window")
    });
    let shell = window.root(cx).expect("shell");
    VisualTestContext::from_window(window.into(), cx);
    (graph, shell)
}

/// Clicks the control the probe published under a key containing `id`.
fn press(cx: &mut VisualTestContext, id: &str) {
    cx.update(|_, cx| facet::probe::enable(cx));
    draw(cx);
    let ledger = cx.update(|_, cx| facet::probe::take(cx));
    let target = ledger
        .targets
        .iter()
        .find(|target| target.key.contains(id))
        .unwrap_or_else(|| {
            panic!(
                "no control `{id}` on screen: {:?}",
                ledger.targets.iter().map(|t| &t.key).collect::<Vec<_>>()
            )
        });
    let (x, y) = (
        target.bounds.x + target.bounds.width / 2.0,
        target.bounds.y + target.bounds.height / 2.0,
    );
    cx.simulate_click(point(px(x), px(y)), gpui::Modifiers::none());
}

#[gpui::test]
fn an_owner_that_could_not_start_is_named_on_the_first_screen_and_try_again_starts_it(
    cx: &mut TestAppContext,
) {
    let gate = OwnerGate::starting();
    let (_graph, shell) = window_before_its_owner(cx, &gate);
    let window = cx.windows().into_iter().next().expect("window");
    let cx = VisualTestContext::from_window(window, cx).into_mut();
    let _ = &shell;
    // Every text run the window drew (the failure plate paints its own words).
    let said = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| facet::probe::enable(cx));
        draw(cx);
        cx.update(|_, cx| facet::probe::take(cx))
            .texts
            .into_iter()
            .map(|text| text.content)
            .collect()
    };
    draw(cx);
    gate.publish(OwnerState::Failed(
        "could not own /tmp/demo and nothing answered on /tmp/demo.sock".into(),
    ));
    until(cx, "the Library names why", |cx| {
        said(cx)
            .iter()
            .any(|line| line == "The Library could not be read.")
    });
    let words = said(cx);
    assert!(
        words
            .iter()
            .any(|line| line.contains("The index could not start. could not own /tmp/demo")),
        "the owner's own words are on the first screen: {words:#?}"
    );
    assert!(
        !words
            .iter()
            .any(|line| line == "Read the code you depend on."),
        "a Library the index could not answer is not offered as an empty first run: {words:#?}"
    );
    press(cx, "retry");
    assert_eq!(
        gate.state(),
        OwnerState::Starting,
        "Try again starts the owner again"
    );
}

#[gpui::test]
fn the_owner_watch_lets_a_window_that_is_let_go_go(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let gate = OwnerGate::starting();
    let snapshot = AppSnapshot::empty(VersionedRoot::unserved());
    let runtime = DesktopRuntime::new(
        snapshot,
        EngineActor::start(WaitsForOwner(gate.clone()), 8).expect("actor"),
    );
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(cx, runtime, None, None, Some(gate.clone()), None)
    });
    let (root, store) = (graph.root.downgrade(), graph.store.downgrade());
    drop(graph);
    // Releasing the root lets go of the store it holds, one effect cycle on.
    for _ in 0..3 {
        cx.update(|_| {});
        cx.run_until_parked();
    }
    // D2: the watch held both for the life of the process ("leaked handles"
    // when an app that was quit is dropped).
    assert!(
        root.upgrade().is_none(),
        "the window's root is let go, not held by the owner's watch"
    );
    assert!(
        store.upgrade().is_none(),
        "the window's store is let go, not held by the owner's watch"
    );
    // The watch, still waiting on the gate, ends at the next state.
    gate.publish(OwnerState::Failed("the window is gone".into()));
    cx.run_until_parked();
}

#[gpui::test]
fn an_owner_that_could_not_start_is_said_in_the_foot_with_try_again_wherever_the_person_is(
    cx: &mut TestAppContext,
) {
    let gate = OwnerGate::starting();
    let (_graph, _shell) = window_before_its_owner(cx, &gate);
    let window = cx.windows().into_iter().next().expect("window");
    let cx = VisualTestContext::from_window(window, cx).into_mut();
    draw(cx);
    // No page painted from a launch snapshot: the foot says it all the same (R7).
    gate.publish(OwnerState::Failed("could not own /tmp/demo".into()));
    until(cx, "the foot names why", |cx| {
        cx.update(|_, cx| facet::probe::enable(cx));
        draw(cx);
        cx.update(|_, cx| facet::probe::take(cx))
            .targets
            .iter()
            .any(|target| target.key.contains("status-retry"))
    });
    press(cx, "status-retry");
    assert_eq!(
        gate.state(),
        OwnerState::Starting,
        "the foot's Try again starts the owner again"
    );
}

#[gpui::test]
fn the_foot_offers_try_again_beside_a_notice_that_the_index_could_not_start(
    cx: &mut TestAppContext,
) {
    let gate = OwnerGate::starting();
    let (graph, _shell) = window_before_its_owner(cx, &gate);
    let window = cx.windows().into_iter().next().expect("window");
    let cx = VisualTestContext::from_window(window, cx).into_mut();
    draw(cx);
    gate.publish(OwnerState::Failed("the lock is held".into()));
    draw(cx);
    draw(cx);
    graph.store.update(cx, |store, cx| {
        let snapshot = store.snapshot();
        store.set_notice(
            Some(crate::runtime::graph_focus::Notice {
                visit: snapshot.route().clone(),
                root: snapshot.key(),
                message: Arc::from("The index could not start, so this is the page as you left it. the lock is held"),
                retry: Some(PageKey::Orbit),
            }),
            cx,
        );
    });
    press(cx, "status-retry");
    assert_eq!(
        gate.state(),
        OwnerState::Starting,
        "the foot's Try again starts the owner again"
    );
}
