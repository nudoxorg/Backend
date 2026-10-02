//! Headless captures of the real shell over a real local index.
//!
//! An embedded `backend-locald` indexes `crates/present` (its state dir is
//! kept between runs, so later runs start at once); the shell then renders
//! `present::glyph::RelationLabel` — the declaration the calm targets draw —
//! through the product's own read pool, store and regions, in the headless
//! GPUI renderer, at every width, text size, density and appearance of the
//! verification matrix, plus filmstrips of the descent, ⌘ and ⌥ holds, the
//! shelf → spine resize and the focus walk.
//!
//! ```text
//! NUDOX_CAPTURE_OUT=<dir> .local/devenv/cargo test -p backend-desktop \
//!     --test shell_capture -- --ignored --nocapture
//! ```
//!
//! `NUDOX_CAPTURE_ONLY=<substring>` limits the run to matching shots.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines, missing_docs)]

use backend_client::Session;
use backend_desktop::core::{LocalProjectId, PackageId, VersionedRoot};
use backend_desktop::model::{
    AppSnapshot, AppearancePreference, DensityPreference, ProjectPhase, SessionState,
    SettingsState, WorkspaceProject, WorkspaceState,
};
use backend_desktop::navigation::{
    Coordinate, Intent, OrbitRoute, Overlay, PackageLane, PackageRoute, Route, SymbolRoute, View,
};
use backend_desktop::runtime::actor::EngineActor;
use backend_desktop::runtime::client::LocalEngineClient;
use backend_desktop::runtime::reads::{ReadPool, SessionReader};
use backend_desktop::runtime::store::DataStore;
use backend_desktop::runtime::{DesktopRuntime, UiEntityGraph, UiRootEntity};
use backend_desktop::shell::Shell;
use backend_gui_harness::{
    AnimationFrame, CaptureConfig, CaptureError, CaptureSession, GpuiCaptureOptions, GuiState,
    InputStep, ThemeState, Viewport, capture_gpui_state_with_adapters_result_and_semantics,
};
use backend_library::{
    CommandReply, DeclarationKind, PackageReference, RegistryEcosystem, RowState, SurfaceCommand,
    SurfaceReply,
};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Modifiers, Render, Styled, WeakEntity,
    Window, div, rgb,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveGuiHandoff {
    schema: String,
    workspace: PathBuf,
    endpoint: PathBuf,
    packages: Vec<String>,
    root: String,
    joined_receipt: PathBuf,
    joined_receipt_blake3: String,
    build_receipt: PathBuf,
    build_receipt_blake3: String,
    capture: String,
    requires_native_frame_and_accesskit: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinedLiveReceipt {
    schema: String,
    git_head: String,
    git_tree: String,
    cargo_lock_sha256: String,
    build_receipt: PathBuf,
    build_receipt_blake3: String,
    workspace: PathBuf,
    endpoint: PathBuf,
    owner_pid: u32,
    mcp_pid: u32,
    root: String,
    packages: Vec<String>,
    turso_backup: LiveTursoBackup,
    registry_facts: Vec<LiveRegistryFact>,
    cold_restart: String,
    turso_restore: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveTursoBackup {
    projection: String,
    index_authority: String,
    tursodb: String,
    tursodb_path: PathBuf,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRegistryFact {
    lane: String,
    coordinate: String,
    archive_bytes: u64,
    registry_record: serde_json::Value,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductBuildReceipt {
    schema: String,
    created_at: String,
    git_head: String,
    git_tree: String,
    cargo_lock_sha256: String,
    cargo_version: String,
    rustc_version: String,
    target_dir: PathBuf,
    binaries: BTreeMap<String, ProductBinaryReceipt>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductBinaryReceipt {
    path: PathBuf,
    sha256: String,
}

const INDEX_DEADLINE: Duration = Duration::from_mins(15);

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Starts an embedded owner and waits until every requested project is Ready.
fn serve(projects: &[PathBuf]) -> (backend_desktop::DesktopHost, PathBuf) {
    let primary = projects.first().expect("at least one indexed project");
    let state = PathBuf::from(std::env::var("NUDOX_CAPTURE_STATE").unwrap_or_else(|_| "/tmp/nx-shell-cap".to_owned()));
    let endpoint = PathBuf::from(format!("{}.sock", state.display()));
    // The real owner refuses state beneath a group-readable directory. Keep
    // the capture fixture subject to that same check, including first boot.
    let mut private_dir = std::fs::DirBuilder::new();
    private_dir.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        private_dir.mode(0o700);
    }
    private_dir
        .create(state.join("data"))
        .expect("private state dir");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(primary.clone()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .expect("workspace paths");
    let host = backend_desktop::DesktopHost::start_with_paths(paths).expect("embedded owner");
    let mut session = Session::connect(&endpoint).expect("session");
    for project in projects {
        session
            .index(project.to_str().expect("utf-8 path"))
            .expect("index request");
    }
    let started = Instant::now();
    let (mut last, mut stable) = (0, 0);
    loop {
        let ready = matches!(session.packages().map(|reply| reply.reply),
            Ok(CommandReply::Packages(snapshot)) if projects.iter().all(|project| {
                let root = project.to_str().expect("utf-8 project path");
                snapshot.root.rows().iter().any(|row| row.label == root && row.state == RowState::Ready)
            })
        );
        let rows = session.health().map_or(0, |health| health.row_count());
        stable = if ready && rows > 0 && rows == last { stable + 1 } else { 0 };
        last = rows;
        if stable >= 3 {
            eprintln!("index ready: {rows} rows after {:?}", started.elapsed());
            return (host, endpoint);
        }
        assert!(started.elapsed() < INDEX_DEADLINE, "indexing never settled");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// One capture: a window size, the settings, where the shell stands, and
/// what the frames do.
#[derive(Clone)]
struct Shot {
    name: String,
    width: u32,
    height: u32,
    percent: u16,
    density: DensityPreference,
    appearance: AppearancePreference,
    route: Route,
    frames: Vec<u64>,
    script: Script,
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// One settled frame.
    Still,
    /// Package → the page: the descent, then ⌘- back up.
    Descent,
    /// ⌘ held: key caps rise after the hold.
    HoldCommand,
    /// ⌥ held: x-ray.
    HoldOption,
    /// J walks the focus down the page, one step per frame.
    Walk,
    /// The window narrows through 900: the shelf becomes a spine.
    Narrow,
    /// Ask covers the live Library; the page beneath its veil is inert.
    Ask,
    /// Move the real shell's keyboard zone to a mounted project row.
    ShelfFocus,
    /// From the real Orbit, type and choose an indexed hit through the keyboard,
    /// read its code, go Back, reopen Ask, and resize a live preview.
    AskJourney,
}

#[derive(Clone)]
struct JourneyFrame {
    label: String,
    route: Route,
    overlay: Option<Overlay>,
    preview: Option<Route>,
    root: VersionedRoot,
    owner_serving: bool,
    /// Filled after the native frame drew, from Reader's rendered words.
    reader_text: Vec<String>,
    reader_hero: Vec<String>,
    reader_pages: usize,
}

struct Places {
    page: Route,
    alias: Option<Route>,
    package: Route,
}

/// Weak handles let the mounted window own the production graph throughout capture,
/// without retaining it past an early renderer or semantic error.
struct ShellCaptureEntities {
    root: WeakEntity<UiRootEntity>,
    store: WeakEntity<DataStore>,
    shell: WeakEntity<Shell>,
}

fn upgrade_capture_entity<T: 'static>(
    entity: &WeakEntity<T>,
    name: &'static str,
) -> Result<Entity<T>, CaptureError> {
    entity
        .upgrade()
        .ok_or_else(|| CaptureError::Gpui(format!("capture {name} entity was released early")))
}

struct EarlySemanticErrorRoot;

impl Render for EarlySemanticErrorRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x202020))
    }
}

#[test]
fn early_semantic_error_is_preserved_without_retaining_capture_entities() {
    let viewport = Viewport::new(64, 64, 1).expect("viewport");
    let frames = [AnimationFrame {
        label: "early-error".to_owned(),
        time_ms: 0,
    }];
    let slot = Rc::new(RefCell::new(None::<WeakEntity<EarlySemanticErrorRoot>>));
    let build_slot = Rc::clone(&slot);
    let semantic_slot = Rc::clone(&slot);
    let drew_frame = Rc::new(std::cell::Cell::new(false));
    let drew_frame_mark = Rc::clone(&drew_frame);
    let result = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new("early-semantic-error", None, None),
        &[],
        &frames,
        GpuiCaptureOptions {
            capture_native_accessibility: false,
            ..GpuiCaptureOptions::default()
        },
        |_frame, _window, _cx| Ok(()),
        |_step, _window, _cx| {},
        move |_frame, image, _viewport, _window, _cx| {
            assert_eq!(image.dimensions(), (64, 64));
            assert!(
                semantic_slot
                    .borrow()
                    .as_ref()
                    .and_then(WeakEntity::upgrade)
                    .is_some(),
                "the native root must remain alive while the semantic hook runs"
            );
            drew_frame_mark.set(true);
            Err(CaptureError::Accessibility(
                "intentional early semantic error".to_owned(),
            ))
        },
        move |_window, cx| {
            let root = cx.new(|_| EarlySemanticErrorRoot);
            *build_slot.borrow_mut() = Some(root.downgrade());
            root
        },
    );

    assert!(
        drew_frame.get(),
        "the renderer must draw before the injected error"
    );
    assert!(matches!(
        result,
        Err(CaptureError::Accessibility(message)) if message == "intentional early semantic error"
    ));
    assert!(
        slot.borrow()
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_none(),
        "the window should release the root after the capture returns"
    );
}

fn places(endpoint: &Path, project: &Path) -> Places {
    // Find the RelationLabel enum through the product's own read path.
    let mut session = Session::connect(endpoint).expect("session");
    let reply = session.search("RelationLabel", 50).expect("search");
    let (coordinate, alias_coordinate) = match reply.reply {
        CommandReply::Search(result) => {
            let rows = result.root.rows();
            let page = rows.iter().find(|row| {
                row.label.ends_with("::RelationLabel")
                    && row.kind == Some(DeclarationKind::Enum)
                    && row.label.contains("glyph.rs")
            })
            .map(|row| row.label.clone())
            .expect("RelationLabel enum is indexed");
            let alias = rows.iter().find(|row| row.label.ends_with("::lib.rs:103::RelationLabel"))
                .map(|row| row.label.clone());
            (page, alias)
        }
        other => panic!("search answered {other:?}"),
    };
    let package = PackageId::new(project.to_str().expect("utf-8")).expect("package");
    let page_route = |coordinate: &str, package: &PackageId| Route::Symbol(SymbolRoute {
        project: None,
        package: package.clone(),
        id: Coordinate::new(coordinate).expect("coordinate"),
        at: None,
        view: View::Page,
        line: None,
        selected: None,
    });
    Places {
        page: page_route(&coordinate, &package),
        alias: alias_coordinate.as_deref().map(|coordinate| page_route(coordinate, &package)),
        package: Route::Package(PackageRoute {
            project: None,
            package,
            lane: PackageLane::Overview,
            selected: None,
            at: None,
        }),
    }
}

/// Lands every read the store has asked for, waiting for the pool's worker
/// threads in real time (the capture's executor clock is virtual).
fn land(store: &Entity<DataStore>, cx: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        store.update(cx, |store, cx| {
            store.drain(cx);
        });
        let (queued, running) = store.read(cx).pool_load();
        let loading = store.read(cx).focused().iter().any(|key| store.read(cx).is_loading(key));
        if queued == 0 && running == 0 && !loading {
            return;
        }
        assert!(Instant::now() < deadline, "reads never landed");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn capture(
    shot: &Shot,
    endpoint: &Path,
    snapshot_key: VersionedRoot,
    projects: &[PathBuf],
    out: &Path,
) {
    let expected_journey_places = (shot.script == Script::AskJourney)
        .then(|| places(endpoint, projects.first().expect("indexed project")));
    assert_eq!(
        shot.frames.first().copied(),
        Some(0),
        "capture {} must include a time-zero frame",
        shot.name
    );
    let viewport = Viewport::new(shot.width, shot.height, 1).expect("viewport");
    let frames = shot
        .frames
        .iter()
        .enumerate()
        .map(|(index, time)| AnimationFrame {
            label: format!("f{index:02}-{time}ms"),
            time_ms: *time,
        })
        .collect::<Vec<_>>();
    let graph_slot: Rc<RefCell<Option<ShellCaptureEntities>>> = Rc::new(RefCell::new(None));
    let build_slot = Rc::clone(&graph_slot);
    let hook_slot = Rc::clone(&graph_slot);
    let last_slot = Rc::clone(&graph_slot);
    let journey = Rc::new(RefCell::new(Vec::<JourneyFrame>::new()));
    let journey_hook = Rc::clone(&journey);
    let journey_semantic = Rc::clone(&journey);
    let partial_out = (shot.script == Script::AskJourney)
        .then(|| out.join("partial-journey").join(&shot.name));
    let last_label = frames.last().map(|frame| frame.label.clone()).unwrap_or_default();
    let built = Rc::new(std::cell::Cell::new(false));
    let built_mark = Rc::clone(&built);
    let endpoint = endpoint.to_path_buf();
    let shot_build = shot.clone();
    let shot_hook = shot.clone();
    let active_project = LocalProjectId::from_path(
        projects.first().expect("at least one indexed project"),
    )
    .expect("active project identity");
    let workspace_projects = projects
        .iter()
        .map(|path| {
            let id = LocalProjectId::from_path(path).expect("workspace project identity");
            let mut project = WorkspaceProject::indexing_with_id(id);
            project.phase = ProjectPhase::Ready;
            project
        })
        .collect::<Vec<_>>();
    // The window narrows through 900 (the shelf becomes a spine) at 100 ms.
    let actions = match shot.script {
        Script::Narrow => vec![InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 880, height: shot.height }],
        // Ask opens at 700 ms. Type through the real keyboard path after that
        // paint so the next paired tree proves the editor consumed the text.
        Script::Ask => vec![InputStep::Wait { milliseconds: 760 }, InputStep::Text { value: "RelationLabel".to_owned() }],
        Script::AskJourney => vec![
            InputStep::Wait { milliseconds: 100 }, InputStep::key("cmd-k"),
            InputStep::Wait { milliseconds: 100 }, InputStep::Text { value: "RelationLabel".to_owned() },
            InputStep::Wait { milliseconds: 800 }, InputStep::key("down"),
            InputStep::Wait { milliseconds: 100 }, InputStep::key("tab"),
            InputStep::Wait { milliseconds: 100 }, InputStep::key("shift-tab"),
            InputStep::Wait { milliseconds: 100 }, InputStep::key("tab"),
            InputStep::Wait { milliseconds: 100 }, InputStep::key("down"),
            // Hold the Alias preview on the executor clock until a paired
            // settled Reader frame is captured before the Up event.
            InputStep::Wait { milliseconds: 750 }, InputStep::key("up"),
            InputStep::Wait { milliseconds: 200 }, InputStep::key("enter"),
            InputStep::Wait { milliseconds: 200 }, InputStep::key("cmd-."),
            // Keep the first Code frame as transition evidence, then let
            // that Reader settle before the real Back event.
            InputStep::Wait { milliseconds: 750 },
            InputStep::Wait { milliseconds: 200 }, InputStep::key("cmd-["),
            InputStep::Wait { milliseconds: 200 }, InputStep::key("cmd-k"),
            InputStep::Wait { milliseconds: 200 }, InputStep::key("escape"),
            InputStep::Wait { milliseconds: 200 }, InputStep::key("cmd-k"),
            InputStep::Wait { milliseconds: 100 }, InputStep::Text { value: "RelationLabel".to_owned() },
            InputStep::Wait { milliseconds: 500 }, InputStep::key("down"),
            InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 800, height: shot.height },
            InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 360, height: shot.height },
            InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 1440, height: shot.height },
            InputStep::Wait { milliseconds: 100 }, InputStep::key("escape"),
            InputStep::Wait { milliseconds: 400 },
            // Keep the old settled evidence, then capture the next modal
            // entrance, exit, interrupted reopen, and resize on real input.
            InputStep::Wait { milliseconds: 100 }, InputStep::key("cmd-k"),
            InputStep::Wait { milliseconds: 100 }, InputStep::Text { value: "RelationLabel".to_owned() },
            InputStep::Wait { milliseconds: 100 }, InputStep::key("escape"),
            InputStep::Wait { milliseconds: 80 }, InputStep::key("cmd-k"),
            InputStep::Wait { milliseconds: 20 }, InputStep::Text { value: "RelationLabel".to_owned() },
            InputStep::Wait { milliseconds: 60 }, InputStep::Resize { width: 800, height: shot.height },
            InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 1440, height: shot.height },
            InputStep::Wait { milliseconds: 500 }, InputStep::key("escape"),
            InputStep::Wait { milliseconds: 1000 },
        ],
        _ => Vec::new(),
    };
    let mut set = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new(shot.name.clone(), None, None),
        &actions,
        &frames,
        GpuiCaptureOptions {
            asset_source: std::sync::Arc::new(facet::icons::Assets),
            capture_native_accessibility: true,
            ..GpuiCaptureOptions::default()
        },
        move |frame: &AnimationFrame, window: &mut Window, cx: &mut App| -> Result<(), CaptureError> {
            let slot = hook_slot.borrow();
            let entities = slot
                .as_ref()
                .ok_or_else(|| CaptureError::Gpui("capture graph was not built".to_owned()))?;
            let root = upgrade_capture_entity(&entities.root, "state root")?;
            let store = upgrade_capture_entity(&entities.store, "data store")?;
            let shell = upgrade_capture_entity(&entities.shell, "shell")?;
            let index = frames_index(&frame.label);
            match (shot_hook.script, index) {
                (Script::Descent, 1) => {
                    root.update(cx, |root, cx| {
                        root.dispatch(Intent::Navigate(shot_hook.route.clone()), cx)
                    });
                    land(&store, cx);
                }
                (Script::Descent, 7) => {
                    root.update(cx, |root, cx| root.dispatch(Intent::ZoomOut, cx));
                    land(&store, cx);
                }
                (Script::HoldCommand, 1) => {
                    shell.update(cx, |shell, cx| {
                        shell.modifiers(Modifiers { platform: true, ..Modifiers::default() }, cx);
                    });
                }
                (Script::HoldOption, 1) => {
                    shell.update(cx, |shell, cx| {
                        shell.modifiers(Modifiers { alt: true, ..Modifiers::default() }, cx);
                    });
                }
                (Script::Walk, index) if index > 0 => {
                    shell.update(cx, |shell, cx| shell.walk(1, window, cx));
                }
                (Script::Ask, 1) => {
                    root.update(cx, |root, cx| root.dispatch(Intent::OpenCommandPalette, cx));
                    land(&store, cx);
                }
                (Script::ShelfFocus, 1) => {
                    shell.update(cx, |shell, cx| {
                        shell.cycle_zone(true, window, cx);
                        shell.cycle_zone(true, window, cx);
                        for _ in 0..32 {
                            if shell.focus_state(cx).1.as_deref().is_some_and(|id| {
                                id.starts_with("project-") && !id.starts_with("project-tree-")
                            }) {
                                break;
                            }
                            shell.walk(1, window, cx);
                        }
                        assert!(shell.focus_state(cx).1.as_deref().is_some_and(|id| {
                            id.starts_with("project-") && !id.starts_with("project-tree-")
                        }), "the live shelf has a reachable project row");
                    });
                }
                _ => {}
            }
            land(&store, cx);
            if shot_hook.script == Script::AskJourney {
                let data = store.read(cx);
                let snapshot = data.snapshot();
                journey_hook.borrow_mut().push(JourneyFrame {
                    label: frame.label.clone(),
                    route: snapshot.route().clone(),
                    overlay: snapshot.overlay(),
                    preview: snapshot.session().preview.clone(),
                    root: snapshot.key(),
                    owner_serving: data.owner_serving(),
                    reader_text: Vec::new(),
                    reader_hero: Vec::new(),
                    reader_pages: 0,
                });
            }
            Ok(())
        },
        |_, _, _| {},
        move |frame: &AnimationFrame, image, _, window, cx| -> Result<_, CaptureError> {
            if let Some(partial) = &partial_out {
                let frame_dir = partial.join(&frame.label);
                std::fs::create_dir_all(&frame_dir).map_err(|error| CaptureError::Gpui(error.to_string()))?;
                image.save(frame_dir.join("pixels.png")).map_err(|error| CaptureError::Gpui(error.to_string()))?;
                let tree = window.debug_a11y_tree_json().ok_or_else(|| {
                    CaptureError::Accessibility("the journey frame has no native tree".to_owned())
                })?;
                std::fs::write(frame_dir.join("accesskit.json"), tree)
                    .map_err(|error| CaptureError::Gpui(error.to_string()))?;
                if let Some(state) = journey_semantic.borrow().last() {
                    std::fs::write(frame_dir.join("route.txt"), format!("route={:?}\noverlay={:?}\npreview={:?}\nroot={:?}\nowner_serving={}\n",
                        state.route, state.overlay, state.preview, state.root, state.owner_serving))
                        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
                }
                // Capture the rendered Reader after this native frame draws.
                // Ask veils its background from AccessKit while it is open.
                let shell = {
                    let slot = last_slot.borrow();
                    let entities = slot.as_ref().ok_or_else(|| CaptureError::Gpui("capture graph was not built".to_owned()))?;
                    upgrade_capture_entity(&entities.shell, "shell")?
                };
                let (reader_text, reader_hero, reader_pages) = {
                    let shell = shell.read(cx);
                    (shell.reader_text(cx).into_iter().map(|line| line.to_string()).collect::<Vec<_>>(),
                        shell.hero_lines(cx), shell.reader_pages(cx))
                };
                let mut observed = journey_semantic.borrow_mut();
                let state = observed.last_mut().ok_or_else(|| CaptureError::Gpui("journey route was not recorded".to_owned()))?;
                state.reader_text = reader_text;
                state.reader_hero = reader_hero;
                state.reader_pages = reader_pages;
                std::fs::write(frame_dir.join("reader.txt"), format!("pages={}\nhero={:?}\ntext={:?}\n",
                    state.reader_pages, state.reader_hero, state.reader_text))
                    .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            }
            // Confirm the mounted Root still owns the graph through its final frame.
            if frame.label == last_label {
                let slot = last_slot.borrow();
                let entities = slot.as_ref().ok_or_else(|| {
                    CaptureError::Gpui("capture graph was not built".to_owned())
                })?;
                let _root = upgrade_capture_entity(&entities.root, "state root")?;
                let _store = upgrade_capture_entity(&entities.store, "data store")?;
                let _shell = upgrade_capture_entity(&entities.shell, "shell")?;
                built_mark.set(true);
            }
            Ok(None)
        },
        move |window: &mut Window, cx: &mut App| {
            gpui_component::init(cx);
            facet::fonts::install(cx).expect("fonts");
            let mut settings = SettingsState {
                density: shot_build.density,
                appearance: shot_build.appearance,
                ..SettingsState::default()
            };
            settings.shelf_open = true;
            let start = if shot_build.script == Script::Descent {
                match &shot_build.route {
                    Route::Symbol(page) => Route::Package(PackageRoute {
                        project: None,
                        package: page.package.clone(),
                        lane: PackageLane::Overview,
                        selected: None,
                        at: None,
                    }),
                    other => other.clone(),
                }
            } else {
                shot_build.route.clone()
            };
            let mut session = SessionState::default();
            session.route = start;
            session.back = vec![Route::Orbit(OrbitRoute::Home)].into();
            let snapshot = AppSnapshot::empty(snapshot_key)
                .with_settings(settings)
                .with_session(session);
            let workspace = WorkspaceState {
                projects: workspace_projects.clone().into(),
                active: Some(active_project.clone()),
                host: Some(active_project.clone()),
                ..WorkspaceState::default()
            };
            let snapshot = snapshot.with_workspace(workspace);
            let actor = EngineActor::start(LocalEngineClient::new(&endpoint, active_project.clone()), 32)
                .expect("live local owner actor");
            let runtime = DesktopRuntime::new(snapshot, actor);
            let reader_endpoint = endpoint.clone();
            let pool = ReadPool::start(3, move |_| SessionReader::connect(&reader_endpoint)).expect("pool");
            let graph = UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool));
            let shell = backend_desktop::shell::open_shell(&graph, window, cx);
            // Text size is the system's plus this display's ⌘± zoom.
            let display = shell.read(cx).display_key();
            let percent = shot_build.percent;
            graph.root.update(cx, |root, cx| root.dispatch(Intent::ZoomTo { display, percent }, cx));
            land(&graph.store, cx);
            let root = cx.new(|cx| gpui_component::Root::new(shell.clone(), window, cx).bordered(false));
            *build_slot.borrow_mut() = Some(ShellCaptureEntities {
                root: graph.root.downgrade(),
                store: graph.store.downgrade(),
                shell: shell.downgrade(),
            });
            root
        },
    )
    .expect("capture");
    // Keep the paired native pixels/tree when a semantic assertion fails:
    // the artifact is the evidence needed to diagnose the actual mounted UI.
    let mut config = CaptureConfig::deterministic(viewport);
    config.theme = match shot.appearance {
        AppearancePreference::Abyss => ThemeState::Abyss,
        AppearancePreference::Glacier => ThemeState::Glacier,
        AppearancePreference::System => {
            panic!("resolve System appearance before writing a capture manifest")
        }
    };
    config.data_revision = format!("locald-root:{snapshot_key}");
    let route_name = match &shot.route {
        Route::Orbit(_) => "orbit",
        Route::Package(_) => "package",
        Route::Symbol(_) => "symbol",
        _ => "other",
    };
    let script_name = match shot.script {
        Script::Still => "still",
        Script::Descent => "descent",
        Script::HoldCommand => "command-hold",
        Script::HoldOption => "option-hold",
        Script::Walk => "keyboard-walk",
        Script::Narrow => "resize-narrow",
        Script::Ask => "ask-modal",
        Script::ShelfFocus => "shelf-focus",
        Script::AskJourney => "ask-keyboard-journey",
    };
    let script_id = format!(
        "real-locald-shell|shot={}|route={route_name}|text-scale={}|density={:?}|actions={script_name}",
        shot.name, shot.percent, shot.density
    );
    let session = CaptureSession::new(config, out).expect("capture artifact session");
    session
        .write_set(&mut set, Some(&script_id), None)
        .expect("write PNG, paired native tree, and provenance manifest");
    if shot.name == "orbit-two-real-projects" {
        // This is the mounted production shell over two projects the embedded
        // owner actually indexed. Both frames must expose the same controls;
        // the second one exercises cached region layouts after the first draw.
        for frame in &set.frames {
            let native = frame.native_accessibility.as_ref().expect("paired native tree");
            let nodes = native.tree["nodes"].as_object().expect("AccessKit nodes");
            let named = |role: &str, label: &str| {
                nodes.values().filter(|node| {
                    node["aria"]["role"].as_str() == Some(role)
                        && node["aria"]["label"].as_str() == Some(label)
                }).collect::<Vec<_>>()
            };
            let clickable = |role: &str, label: &str| {
                let matches = named(role, label);
                assert_eq!(matches.len(), 1, "{role} {label:?} is missing or duplicated in {}", frame.label);
                let actions = matches[0]["aria"]["on_action"].as_array().expect("native actions");
                let bounds = &matches[0]["bounds"];
                assert!(bounds["width"].as_f64().is_some_and(|size| size > 0.0));
                assert!(bounds["height"].as_f64().is_some_and(|size| size > 0.0));
                for action in ["Click", "Focus"] {
                    assert!(
                        actions.iter().any(|value| value.as_str() == Some(action)),
                        "{role} {label:?} has no {action} action in {}",
                        frame.label,
                    );
                }
            };
            clickable("Button", "Ask anything, or find a package");
            clickable("Button", "Toggle the shelf");
            for project in ["present", "runtime"] {
                clickable("Link", &format!("Open {project}"));
                clickable("Link", &format!("{project} dependency tree"));
                clickable("Button", project);
            }
            for lens in ["Contents", "Versions", "Rests on", "Used by"] {
                clickable("Tab", lens);
            }
            assert_eq!(named("Heading", "Library, 2 projects · 2 packages").len(), 1);
            assert_eq!(named("TabList", "Library views").len(), 1);
            assert_eq!(named("Tab", "Contents")[0]["aria"]["selected"].as_bool(), Some(true));
            assert!(nodes.len() > 1, "the live shell cannot be a Window-only tree");
            let focused = native.tree["gpui_focus"].as_str().expect("real focused AccessKit node");
            let focused_node = nodes.get(focused).expect("focus belongs to this tree");
            if frame.time_ms == 0 {
                // The harness's real initial Tab stop is the titlebar button.
                assert_eq!(focused_node["aria"]["role"].as_str(), Some("Button"));
                assert_eq!(focused_node["aria"]["label"].as_str(), Some("Toggle the shelf"));
            } else {
                assert_eq!(focused_node["aria"]["role"].as_str(), Some("Application"));
                assert_eq!(focused_node["aria"]["label"].as_str(), Some("Nudox"));
                let descendant = native.tree["active_descendant_focus"].as_str().expect("shelf's selected project focus");
                assert_eq!(native.tree["accesskit_focus"].as_str(), Some(descendant));
                let node = nodes.get(descendant).expect("selected project belongs to this tree");
                assert_eq!(node["aria"]["role"].as_str(), Some("Button"));
                assert!(matches!(node["aria"]["label"].as_str(), Some("present" | "runtime")));
            }
        }
    }
    if shot.name == "orbit-ask-modal" {
        let before = set.frames.first().and_then(|frame| frame.native_accessibility.as_ref()).expect("Orbit tree");
        let opened = set.frames.get(1).and_then(|frame| frame.native_accessibility.as_ref()).expect("opened Ask tree");
        let after = set.frames.last().and_then(|frame| frame.native_accessibility.as_ref()).expect("typed Ask tree");
        assert!(before.has_label("Open present"), "the real project was present before Ask");
        assert!(!after.has_label("Open present"), "the veiled project must not remain accessible");
        assert!(!after.has_label("Library views"), "the veiled shelf must not remain accessible");
        let opened_nodes = opened.tree["nodes"].as_object().expect("opened Ask AccessKit nodes");
        let opened_fields = opened_nodes.values().filter(|node| {
            node["aria"]["role"].as_str() == Some("TextInput")
                && node["aria"]["label"].as_str() == Some("Ask anything, or find a package")
        }).collect::<Vec<_>>();
        assert_eq!(opened_fields.len(), 1, "the freshly opened Ask has one named editor");
        assert_eq!(opened_fields[0]["aria"]["value"].as_str(), Some(""));
        let nodes = after.tree["nodes"].as_object().expect("Ask AccessKit nodes");
        let fields = nodes.iter().filter(|(_, node)| {
            node["aria"]["role"].as_str() == Some("TextInput")
                && node["aria"]["label"].as_str() == Some("Ask anything, or find a package")
        }).collect::<Vec<_>>();
        assert_eq!(fields.len(), 1, "the live Ask input needs one named TextInput node");
        assert_eq!(after.tree["accesskit_focus"].as_str(), Some(fields[0].0.as_str()));
        assert_eq!(fields[0].1["aria"]["value"].as_str(), Some("RelationLabel"), "real keyboard typing must reach the focused Ask editor");
        let links = nodes.values().filter(|node| node["aria"]["role"].as_str() == Some("Link")).collect::<Vec<_>>();
        assert_eq!(links.len(), 9, "eight live indexed results and the complete results page must be accessible");
        let mut names = std::collections::HashSet::new();
        for link in links {
            let name = link["aria"]["label"].as_str().expect("a result needs a spoken destination");
            assert!(names.insert(name), "each indexed result needs a distinct accessible destination: {name}");
            let actions = link["aria"]["on_action"].as_array().expect("native result actions");
            for action in ["Click", "Focus"] {
                assert!(actions.iter().any(|value| value.as_str() == Some(action)), "{name} has no {action} action");
            }
            assert!(link["bounds"]["width"].as_f64().is_some_and(|size| size > 0.0));
            assert!(link["bounds"]["height"].as_f64().is_some_and(|size| size > 0.0));
        }
    }
    if shot.script == Script::AskJourney {
        let observed = journey.borrow();
        assert_eq!(observed.len(), 32, "record every real journey frame after writing paired evidence");
        let orbit = Route::Orbit(OrbitRoute::Home);
        for (index, expected) in [
            (&orbit, None),
            (&orbit, Some(Overlay::CommandPalette)),
            (&orbit, Some(Overlay::CommandPalette)),
        ].into_iter().enumerate() {
            assert_eq!((&observed[index].route, observed[index].overlay), (expected.0, expected.1), "{}", observed[index].label);
        }
        let Places { page: target, alias, .. } = expected_journey_places.expect("exact owner pages for the keyboard journey");
        let alias = alias.expect("the live owner indexed the lib.rs:103 RelationLabel alias");
        assert!(matches!(&alias, Route::Symbol(symbol) if symbol.id.as_str().ends_with("::lib.rs:103::RelationLabel")),
            "the second Link names the live owner's exact re-exported symbol");
        assert_eq!(observed[3].route, target, "Down previews the exact owner search result");
        assert_eq!(observed[3].preview, Some(orbit.clone()), "preview retains the departure place");
        assert_eq!(observed[3].overlay, Some(Overlay::CommandPalette));
        for index in 4..=6 {
            assert_eq!(observed[index].route, target, "Tab must not replace the previewed page");
            assert_eq!(observed[index].overlay, Some(Overlay::CommandPalette));
        }
        assert_eq!(observed[7].route, alias, "Down on a focused Link previews the exact lib.rs:103 destination");
        assert_eq!(observed[7].preview, Some(orbit.clone()));
        assert_eq!(observed[8].route, alias, "the Alias preview stays selected until its Reader settles");
        assert_eq!(observed[8].preview, Some(orbit.clone()));
        assert_eq!(observed[8].overlay, Some(Overlay::CommandPalette));
        assert!(observed[8].owner_serving && observed[8].root.same_authority(observed[7].root),
            "the settled Reader still belongs to the same serving live index");
        assert_eq!(observed[8].reader_pages, 1, "the Alias Reader has finished the page transition");
        assert!(observed[8].reader_hero.iter().any(|line| line.contains("RelationLabel")),
            "the settled Reader names the exact symbol");
        assert!(observed[8].reader_text.iter().any(|line| line == "lib.rs:103"),
            "the rendered Reader must show the live Alias source, not merely its route");
        assert!(!observed[8].reader_text.iter().any(|line| line == "glyph.rs:138"),
            "the prior glyph source must not remain the rendered Reader page");
        assert_eq!(observed[9].route, target, "Up on a focused Link returns to the first exact destination");
        assert_eq!(observed[9].preview, Some(orbit.clone()));
        let typed = set.frames.get(2).and_then(|frame| frame.native_accessibility.as_ref()).expect("typed Ask tree");
        assert!(typed.tree["nodes"].as_object().expect("native nodes").values().any(|node| {
            node["aria"]["role"].as_str() == Some("TextInput")
                && node["aria"]["value"].as_str() == Some("RelationLabel")
        }));
        let focus_id = |index: usize| {
            let tree = &set.frames[index].native_accessibility.as_ref().expect("native journey tree").tree;
            tree["accesskit_focus"].as_str().expect("native focus id").to_owned()
        };
        let focus_at = |index: usize| {
            let tree = &set.frames[index].native_accessibility.as_ref().expect("native journey tree").tree;
            tree["nodes"].as_object().expect("native nodes").get(&focus_id(index)).expect("focused native node").clone()
        };
        assert_eq!(focus_at(3)["aria"]["role"].as_str(), Some("TextInput"), "preview keeps native focus in Ask's editor");
        let row_focus = focus_at(4);
        assert_eq!(row_focus["aria"]["role"].as_str(), Some("Link"), "Tab reaches the live result, not the veiled shelf");
        assert!(row_focus["aria"]["label"].as_str().is_some_and(|name| name.contains("RelationLabel")));
        assert_eq!(focus_at(5)["aria"]["role"].as_str(), Some("TextInput"), "Shift-Tab returns to Ask's editor");
        assert_eq!(focus_id(6), focus_id(4), "Tab returns to the same exact result after a redraw");
        assert_eq!(focus_at(7)["aria"]["role"].as_str(), Some("Link"), "Down keeps native focus on a live result");
        assert_ne!(focus_id(7), focus_id(6), "Down moves the native Link focus to another exact result");
        assert_eq!(focus_id(8), focus_id(7), "native focus remains on the Alias Link while Reader settles");
        assert_eq!(focus_at(8)["aria"]["role"].as_str(), Some("Link"));
        assert_eq!(focus_id(9), focus_id(6), "Up restores the first native Link focus");
        assert_eq!(observed[10].route, target, "Enter on the focused exact result commits its page");
        assert!(observed[10].preview.is_none() && observed[10].overlay.is_none());
        let code = target.with_view(View::Code).expect("indexed symbol has a code view");
        assert_eq!(observed[11].route, code, "keyboard code command opens the indexed source");
        assert!(matches!(&code, Route::Symbol(symbol) if symbol.id.as_str().ends_with("::glyph.rs:138::RelationLabel")),
            "the Code route retains the live owner's exact glyph.rs:138 declaration");
        assert_eq!(observed[12].route, code, "the exact Code route remains active while its Reader settles");
        assert!(observed[12].overlay.is_none() && observed[12].preview.is_none(),
            "Code is a committed page, outside the Ask modal");
        assert!(observed[12].owner_serving && observed[12].root.same_authority(observed[11].root),
            "the settled Code Reader still belongs to the same serving live index");
        assert_eq!(observed[12].reader_pages, 1, "the old variants Page has left the Code Reader");
        assert!(observed[12].reader_hero.iter().any(|line| line.contains("RelationLabel")),
            "the settled Code Reader names the exact symbol");
        assert!(observed[12].reader_text.iter().any(|line| line.contains("glyph.rs") && line.contains("RelationLabel")),
            "the rendered Code crumb names the live glyph source");
        assert!(observed[12].reader_text.iter().any(|line| {
            line.contains("pub enum RelationLabel {")
                && line.contains("Typed(SemanticLinkKind, RelationDirection)")
                && line.contains("Neighbourhood,")
                && line.contains("Related,")
        }), "the settled Reader draws the indexed enum body, not the old variants Page");
        assert_eq!(focus_at(12)["aria"]["role"].as_str(), Some("Application"),
            "native focus returns to the shell after committing Code");
        assert_eq!(observed[13].route, orbit, "Back returns to the actual Orbit departure");
        assert_eq!(observed[14].overlay, Some(Overlay::CommandPalette), "Ask reopens after Back");
        assert_eq!(observed[15].overlay, None, "Escape dismisses the live Ask");
        assert_eq!(observed[16].overlay, Some(Overlay::CommandPalette), "keyboard shortcut reopens Ask");
        assert!(observed[14..17].iter().all(|frame| frame.route == orbit));
        for index in 17..=20 {
            assert_eq!(observed[index].route, target, "live Ask preview survives the resize frame");
            assert_eq!(observed[index].overlay, Some(Overlay::CommandPalette));
        }
        assert!(observed[21..=22].iter().all(|frame| frame.route == orbit && frame.overlay.is_none()),
            "Escape returns the preview to its departure, including the settled frame");
        for (index, width) in [(18, 800), (19, 360), (20, 1440)] {
            let frame = &set.frames[index];
            assert_eq!(frame.image.width(), width, "{} captures the resized real window", frame.label);
            assert!(frame.native_accessibility.as_ref().expect("paired native tree").has_label("Ask anything, or find a package"),
                "{} keeps the modal keyboard owner", frame.label);
        }
        assert_eq!(observed[23].overlay, Some(Overlay::CommandPalette), "the next opening has a live editor");
        assert_eq!(observed[24].overlay, Some(Overlay::CommandPalette), "typed Ask is live before interruption");
        assert_eq!(observed[25].overlay, None, "the exit has revoked Ask interaction");
        assert_eq!(observed[26].overlay, Some(Overlay::CommandPalette), "the close was interrupted by a real reopen key");
        assert!(observed[27..=29].iter().all(|frame| frame.overlay == Some(Overlay::CommandPalette)),
            "the reopened Ask stays authoritative through its resize");
        assert!(observed[30..].iter().all(|frame| frame.overlay.is_none()),
            "the final exit settles without restoring a modal");
        let native_has_results = |index: usize| {
            set.frames[index].native_accessibility.as_ref().expect("paired native tree")
                .has_label("Search results")
        };
        assert!(native_has_results(17), "the earlier populated Ask never exposed its results");
        let entering = &set.frames[24];
        let plate = entering.ledger.stacks.iter().flat_map(|stack| stack.entries.iter())
            .find(|entry| entry.key == "ask-plate").expect("entering Ask plate stack entry");
        assert_eq!(plate.phase, facet::probe::StackPhase::Entering,
            "the interrupted Ask entry was mislabeled Open");
        let width = plate.bounds.as_ref().expect("painted entering plate").width;
        assert!(width > 1.0 && width < 439.0,
            "the interrupted entry needs a genuinely clipped plate, got {width}");
        let tree = &entering.native_accessibility.as_ref().expect("paired entering native tree").tree;
        let nodes = tree["nodes"].as_object().expect("entering native nodes");
        let editors = nodes.iter().filter(|(_, node)| node["aria"]["role"].as_str() == Some("TextInput")
            && node["aria"]["label"].as_str() == Some("Ask anything, or find a package")).collect::<Vec<_>>();
        assert_eq!(editors.len(), 1, "the opening modal needs one native editor");
        assert_eq!(tree["accesskit_focus"].as_str(), Some(editors[0].0.as_str()),
            "a clipped result stole the editor's native focus");
        assert_eq!(editors[0].1["aria"]["value"].as_str(), Some("RelationLabel"));
        assert!(!nodes.values().any(|node| node["aria"]["role"].as_str() == Some("Link")
            && node["aria"]["on_action"].as_array().is_some_and(|actions|
                actions.iter().any(|action| action.as_str() == Some("Click")))),
            "a clipped entering result registered a native Click action");
        assert!(!native_has_results(25), "the painted exit retained native Ask results");
        assert!(native_has_results(29), "the reopened Ask restores native results after resize");
        assert!(!native_has_results(30) && !native_has_results(31), "the final exit retained native Ask results");
        for (index, width) in [(28, 800), (29, 1440)] {
            assert_eq!(set.frames[index].image.width(), width,
                "{} captures the interrupted modal at its real resized width", set.frames[index].label);
        }
    }
    assert!(built.get(), "the mounted shell graph survived through the final frame");
    eprintln!("captured {} ({} frames)", shot.name, set.frames.len());
}

fn frames_index(label: &str) -> usize {
    label[1..3].parse().unwrap_or(0)
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn hex_root(root: &[u8; 32]) -> String {
    hex_bytes(root)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex_bytes(&Sha256::digest(bytes))
}

fn read_typed_json<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> T {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("read {label} {}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("decode {label} {}: {error}", path.display()))
}

fn current_git_value(repository: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .expect("read current desktop capture source identity");
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(output.stdout).expect("git output UTF-8").trim().to_owned()
}

fn current_process_command(pid: u32) -> String {
    let pid = pid.to_string();
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", pid.as_str(), "-o", "command="])
        .output()
        .expect("read exact owner process identity");
    assert!(output.status.success(), "ps could not inspect owner PID {pid}");
    String::from_utf8(output.stdout)
        .expect("ps command output UTF-8")
        .trim()
        .to_owned()
}

fn checked_capture_artifact(root: &Path, relative: &str) -> PathBuf {
    let relative = Path::new(relative);
    assert!(!relative.is_absolute(), "capture manifest artifact path must be relative");
    assert!(
        relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "capture manifest artifact path contains traversal"
    );
    let canonical_root = root.canonicalize().expect("canonical capture root");
    let artifact = root
        .join(relative)
        .canonicalize()
        .expect("canonical manifest artifact");
    assert!(artifact.starts_with(&canonical_root), "capture artifact escaped its evidence root");
    artifact
}

fn capture_tree_evidence(root: &Path) -> Vec<serde_json::Value> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<serde_json::Value>) {
        let mut entries = std::fs::read_dir(directory)
            .expect("read capture artifact directory")
            .map(|entry| entry.expect("capture artifact entry"))
            .collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).expect("capture artifact metadata");
            assert!(!metadata.file_type().is_symlink(), "capture artifact tree contains a symlink");
            if metadata.is_dir() {
                visit(root, &path, files);
            } else {
                let bytes = std::fs::read(&path).expect("read capture artifact bytes");
                files.push(serde_json::json!({
                    "path": path.strip_prefix(root).expect("capture-relative path").display().to_string(),
                    "bytes": bytes.len(),
                    "sha256": sha256_hex(&bytes),
                }));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files
}

/// Consumes the joined real-registry receipt while its exact owner is still
/// serving, then captures the selected pinned package through the real GPUI
/// shell and its post-frame AccessKit readback.
#[test]
#[ignore = "requires the joined public-registry runner and a live product owner"]
fn live_registry_handoff_receipt_is_consumed_by_real_gpui_capture() {
    let handoff_path = std::env::var_os("NUDOX_LIVE_GUI_HANDOFF")
        .map(PathBuf::from)
        .expect("NUDOX_LIVE_GUI_HANDOFF is required");
    let handoff: LiveGuiHandoff = read_typed_json(&handoff_path, "typed GUI handoff");
    assert_eq!(handoff.schema, "nudox.gui.live-handoff.v2");
    assert!(handoff.requires_native_frame_and_accesskit);
    assert_eq!(
        handoff.capture,
        "apps/desktop shell_capture live_registry_handoff_receipt_is_consumed_by_real_gpui_capture"
    );

    let joined_bytes = std::fs::read(&handoff.joined_receipt).expect("read joined producer receipt");
    assert_eq!(
        blake3::hash(&joined_bytes).to_hex().to_string(),
        handoff.joined_receipt_blake3,
        "joined receipt bytes do not match the GUI handoff"
    );
    let joined: JoinedLiveReceipt = serde_json::from_slice(&joined_bytes)
        .expect("decode typed joined producer receipt");
    assert_eq!(joined.schema, "nudox.live.joined-registry-receipt.v1");
    assert_eq!(joined.workspace, handoff.workspace);
    assert_eq!(joined.endpoint, handoff.endpoint);
    assert_eq!(joined.root, handoff.root);
    assert_eq!(joined.packages, handoff.packages);
    assert_eq!(joined.build_receipt, handoff.build_receipt);
    assert_eq!(joined.build_receipt_blake3, handoff.build_receipt_blake3);
    assert!(joined.owner_pid > 0 && joined.mcp_pid > 0);
    assert_eq!(joined.cold_restart, "passed-on-same-workspace-and-mcp-process");
    assert_eq!(joined.turso_restore, "passed-in-place-from-vacuum-into-images");
    assert_eq!(joined.registry_facts.len(), 7);
    assert!(joined.registry_facts.iter().all(|fact| {
        fact.archive_bytes > 0
            && !fact.lane.is_empty()
            && fact.registry_record.is_object()
            && joined.packages.contains(&fact.coordinate)
    }));
    for digest in [
        &joined.turso_backup.projection,
        &joined.turso_backup.index_authority,
        &joined.turso_backup.tursodb,
    ] {
        assert_eq!(digest.len(), 64, "Turso evidence hash must be SHA-256");
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let expected_packages = [
        "pkg:cargo/hashbrown@0.14.5",
        "pkg:npm/@babel/parser@7.26.8",
        "pkg:pypi/tomli@2.2.1",
        "pkg:maven/com.fasterxml.jackson.core/jackson-annotations@2.16.1",
        "pkg:nuget/nullable@1.3.1",
        "pkg:golang/github.com/BurntSushi/toml@v1.4.0",
        "pkg:generic/conan/zlib@1.3.1",
    ];
    let expected_registry_facts = [
        ("rust", expected_packages[0]),
        ("typescript", expected_packages[1]),
        ("python", expected_packages[2]),
        ("java", expected_packages[3]),
        ("csharp", expected_packages[4]),
        ("golang", expected_packages[5]),
        ("cpp", expected_packages[6]),
    ]
    .map(|(lane, coordinate)| (lane.to_owned(), coordinate.to_owned()));
    let observed_registry_facts = joined
        .registry_facts
        .iter()
        .map(|fact| (fact.lane.clone(), fact.coordinate.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_registry_facts, expected_registry_facts,
        "producer receipt must bind exactly one public registry fact record to each exact pin"
    );
    assert_eq!(
        handoff.packages,
        expected_packages.map(ToOwned::to_owned),
        "handoff must contain exactly the seven pinned public releases"
    );
    assert!(joined.turso_backup.tursodb_path.is_absolute());
    let tursodb = joined
        .turso_backup
        .tursodb_path
        .canonicalize()
        .expect("canonical tursodb binary from producer receipt");
    assert_eq!(tursodb, joined.turso_backup.tursodb_path);
    assert_eq!(
        sha256_hex(&std::fs::read(&tursodb).expect("read receipt-bound tursodb")),
        joined.turso_backup.tursodb,
        "the recorded Turso executable differs from the one used for backup and restore"
    );

    let workspace = handoff.workspace.canonicalize().expect("canonical live workspace");
    assert_eq!(workspace, handoff.workspace, "handoff workspace must be canonical");
    assert!(workspace.is_dir(), "handoff workspace is absent");
    let endpoint = backend_runtime::derive_endpoint(&workspace);
    assert_eq!(endpoint, handoff.endpoint, "handoff endpoint is not derived from its exact workspace");
    assert!(endpoint.exists(), "joined product owner endpoint is not live");

    let repository = repo();
    assert!(
        current_git_value(&repository, &["status", "--porcelain", "--untracked-files=all"]).is_empty(),
        "GUI receipt consumer requires the same clean source tree"
    );
    let build_bytes = std::fs::read(&handoff.build_receipt).expect("read product build receipt");
    assert_eq!(
        blake3::hash(&build_bytes).to_hex().to_string(),
        handoff.build_receipt_blake3,
        "product build receipt hash changed after the live stage"
    );
    let build: ProductBuildReceipt = serde_json::from_slice(&build_bytes)
        .expect("decode typed product build receipt");
    assert_eq!(build.schema, "nudox.live.product-build.v1");
    assert!(!build.created_at.is_empty());
    assert!(!build.cargo_version.is_empty() && !build.rustc_version.is_empty());
    assert_eq!(build.git_head, joined.git_head);
    assert_eq!(build.git_tree, joined.git_tree);
    assert_eq!(build.cargo_lock_sha256, joined.cargo_lock_sha256);
    assert_eq!(build.target_dir.canonicalize().expect("canonical target"), build.target_dir);
    assert_eq!(
        sha256_hex(&std::fs::read(repository.join("Cargo.lock")).expect("read Cargo.lock")),
        joined.cargo_lock_sha256,
        "the product build lockfile changed before GUI consumption"
    );
    assert_eq!(current_git_value(&repository, &["rev-parse", "HEAD"]), joined.git_head);
    assert_eq!(current_git_value(&repository, &["rev-parse", "HEAD^{tree}"]), joined.git_tree);
    for name in ["locald", "cli", "mcp"] {
        let binary = build.binaries.get(name).unwrap_or_else(|| panic!("build receipt omitted {name}"));
        let path = binary.path.canonicalize().unwrap_or_else(|error| panic!("{name} binary: {error}"));
        assert_eq!(path, binary.path);
        assert!(path.starts_with(&build.target_dir), "{name} binary is outside the fresh target");
        assert_eq!(sha256_hex(&std::fs::read(path).expect("read current product binary")), binary.sha256);
    }

    let owner_command = current_process_command(joined.owner_pid);
    let owner_binary = &build.binaries["locald"].path;
    let expected_owner_prefix = format!(
        "{} --workspace {} --endpoint {}",
        owner_binary.display(),
        workspace.display(),
        endpoint.display(),
    );
    assert!(
        owner_command.starts_with(&expected_owner_prefix),
        "receipt owner PID is not the product locald for the exact workspace and endpoint: {owner_command}"
    );

    let mut owner_session = Session::connect(&endpoint).expect("connect the exact receipt-bound owner");
    let revision = owner_session.revision().expect("read the exact owner revision");
    assert_eq!(hex_root(revision.root.as_bytes()), handoff.root, "owner root differs from producer receipt");
    let package_snapshot = owner_session.packages().expect("read owner package view");
    let CommandReply::Packages(package_snapshot) = package_snapshot.reply else {
        panic!("owner package snapshot reply changed shape");
    };
    assert_eq!(hex_root(package_snapshot.root.root().as_bytes()), handoff.root);
    let expected_ecosystems = [
        RegistryEcosystem::Cargo,
        RegistryEcosystem::Npm,
        RegistryEcosystem::Pypi,
        RegistryEcosystem::Maven,
        RegistryEcosystem::Nuget,
        RegistryEcosystem::Golang,
        RegistryEcosystem::Cpp,
    ];
    for (coordinate, ecosystem) in handoff.packages.iter().zip(expected_ecosystems) {
        let package = PackageReference::parse(coordinate).expect("exact receipt package reference");
        let details = owner_session
            .surface(SurfaceCommand::Package { package: package.clone() })
            .expect("exact receipt package details");
        let SurfaceReply::Package(records) = details else {
            panic!("owner package details changed shape for {coordinate}: {details:?}");
        };
        assert_eq!(records.len(), 1, "owner returned duplicate package records for {coordinate}");
        assert_eq!(records[0].coordinate.as_str(), coordinate);
        assert_eq!(
            records[0].ecosystem, ecosystem,
            "package details for {coordinate} came from the wrong registry ecosystem"
        );
        let reply = owner_session
            .surface(SurfaceCommand::PackageProfile {
                package,
            })
            .expect("exact receipt package profile");
        let SurfaceReply::PackageProfile { latest: Some(latest), versions, .. } = reply else {
            panic!("owner omitted exact package profile for {coordinate}: {reply:?}");
        };
        assert_eq!(latest.coordinate.as_str(), coordinate);
        assert!(versions > 0, "owner returned an empty version history for {coordinate}");
    }
    assert_eq!(
        hex_root(owner_session.revision().expect("re-read exact owner revision").root.as_bytes()),
        handoff.root,
        "package checks did not remain on the handoff root"
    );

    let output = handoff
        .joined_receipt
        .parent()
        .expect("joined evidence directory")
        .join("gui");
    std::fs::create_dir(&output).expect("create unique GPUI evidence directory");
    let probe_evidence = output.join("probe");
    let capture_evidence = output.join("capture");
    std::fs::create_dir(&probe_evidence).expect("create native probe evidence directory");
    std::fs::create_dir(&capture_evidence).expect("create GPUI capture directory");
    // Select NuGet as the desktop consumer lane: it exercises the typed
    // package-record ecosystem projection alongside the route's exact PURL.
    let selected = handoff.packages[4].clone();
    assert_eq!(selected, "pkg:nuget/nullable@1.3.1");
    let package = PackageId::new(&selected).expect("exact pinned package route");
    let route = Route::Package(PackageRoute {
        project: None,
        package,
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    });
    let snapshot_key = VersionedRoot::from_revision(1, revision.cursor(), 0);
    let project = LocalProjectId::from_path(&workspace).expect("owner workspace project identity");
    let viewport = Viewport::new(1440, 900, 1).expect("live registry viewport");
    let frames = [
        AnimationFrame { label: "f00-0ms".to_owned(), time_ms: 0 },
        AnimationFrame { label: "f01-400ms".to_owned(), time_ms: 400 },
    ];
    let graph_slot: Rc<RefCell<Option<(WeakEntity<DataStore>, WeakEntity<Shell>)>>> =
        Rc::new(RefCell::new(None));
    let build_slot = Rc::clone(&graph_slot);
    let frame_slot = Rc::clone(&graph_slot);
    let capture_endpoint = endpoint.clone();
    let expected_root = handoff.root.clone();
    let probe_out = probe_evidence.clone();
    let mut set = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new("live-registry-package", None, None),
        &[],
        &frames,
        GpuiCaptureOptions {
            asset_source: std::sync::Arc::new(facet::icons::Assets),
            capture_native_accessibility: true,
            ..GpuiCaptureOptions::default()
        },
        |_frame, _window, _cx| Ok(()),
        |_step, _window, _cx| {},
        move |frame, image, _viewport, window, cx| {
            let handles = frame_slot.borrow();
            let (store_weak, shell_weak) = handles
                .as_ref()
                .ok_or_else(|| CaptureError::Gpui("live registry graph was not built".to_owned()))?;
            let store = upgrade_capture_entity(store_weak, "live registry data store")?;
            let shell = upgrade_capture_entity(shell_weak, "live registry shell")?;
            let snapshot = store.read(cx).snapshot();
            let actual_root = hex_root(snapshot.key().root().as_bytes());
            let route_text = format!("{:?}", snapshot.route());
            let reader = shell
                .read(cx)
                .reader_text(cx)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            let label = &frame.label;
            image
                .save(probe_out.join(format!("frame-{label}.png")))
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            std::fs::write(
                probe_out.join(format!("reader-{label}.json")),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "root": actual_root,
                    "route": route_text,
                    "reader": reader,
                }))
                .map_err(|error| CaptureError::Gpui(error.to_string()))?,
            )
            .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            let tree = window.debug_a11y_tree_json().ok_or_else(|| {
                CaptureError::Accessibility("the live registry frame has no GPUI AccessKit readback".to_owned())
            })?;
            std::fs::write(probe_out.join(format!("accesskit-{label}.json")), &tree)
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            if actual_root != expected_root {
                return Err(CaptureError::Gpui("live GPUI frame observed a different owner root".to_owned()));
            }
            if !route_text.contains("pkg:nuget/nullable@1.3.1") {
                return Err(CaptureError::Gpui("live GPUI route lost the selected pinned package".to_owned()));
            }
            Ok(())
        },
        move |window, cx| {
            let mut session = SessionState::default();
            session.route = route.clone();
            let snapshot = AppSnapshot::empty(snapshot_key).with_session(session);
            let actor = EngineActor::start(LocalEngineClient::new(&capture_endpoint, project.clone()), 32)
                .expect("start live registry UI actor");
            let runtime = DesktopRuntime::new(snapshot, actor);
            let read_endpoint = capture_endpoint.clone();
            let pool = ReadPool::start(3, move |_| SessionReader::connect(&read_endpoint))
                .expect("start live registry UI read pool");
            let graph = UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool));
            let shell = backend_desktop::shell::open_shell(&graph, window, cx);
            land(&graph.store, cx);
            *build_slot.borrow_mut() = Some((graph.store.downgrade(), shell.downgrade()));
            cx.new(|cx| gpui_component::Root::new(shell.clone(), window, cx).bordered(false))
        },
    )
    .expect("capture exact live-registry GPUI package page");

    let mut config = CaptureConfig::deterministic(viewport);
    config.data_revision = format!("live-owner-root:{}", handoff.root);
    let capture_session = CaptureSession::new(config, &capture_evidence).expect("create GPUI receipt session");
    capture_session
        .write_set(&mut set, Some("joined-live-registry|package=nullable@1.3.1|root"), None)
        .expect("write actual GPUI frame, AccessKit tree, and capture manifest");
    assert_eq!(set.frames.len(), frames.len(), "every declared live frame must be captured");
    assert!(set.frames.iter().all(|frame| frame.native_accessibility.is_some()), "every frame needs its paired native AccessKit tree");
    let capture_manifest = capture_evidence.join("manifests/live-registry-package.json");
    let manifest: serde_json::Value = read_typed_json(&capture_manifest, "real GPUI capture manifest");
    assert_eq!(manifest["schema"], 1);
    assert_eq!(manifest["state"]["id"], "live-registry-package");
    assert_eq!(manifest["config"]["data_revision"], format!("live-owner-root:{}", handoff.root));
    assert_eq!(manifest["provenance"]["source_revision"], joined.git_head);
    assert_eq!(manifest["provenance"]["dirty"], false, "GPUI capture provenance is dirty");
    let manifest_frames = manifest["frames"].as_array().expect("capture manifest frame array");
    assert_eq!(manifest_frames.len(), frames.len(), "capture manifest must bind every requested frame");
    let mut frame_evidence = Vec::with_capacity(frames.len());
    for (index, (requested, recorded)) in frames.iter().zip(manifest_frames).enumerate() {
        assert_eq!(recorded["label"], requested.label, "manifest frame label/order changed");
        assert_eq!(recorded["time_ms"], requested.time_ms);
        let persisted_path = checked_capture_artifact(
            &capture_evidence,
            recorded["path"].as_str().expect("manifest frame path"),
        );
        let persisted_bytes = std::fs::read(&persisted_path).expect("read manifest-bound GPUI frame");
        assert_eq!(
            sha256_hex(&persisted_bytes),
            recorded["sha256"].as_str().expect("frame SHA-256"),
            "persisted GPUI frame digest differs from its manifest"
        );
        let persisted_image = image::load_from_memory(&persisted_bytes)
            .expect("decode persisted GPUI frame")
            .to_rgba8();
        let probe_frame_path = probe_evidence.join(format!("frame-{}.png", requested.label));
        let probe_frame_bytes = std::fs::read(&probe_frame_path).expect("read raw GPUI frame probe");
        let probe_image = image::load_from_memory(&probe_frame_bytes)
            .expect("decode raw GPUI frame probe")
            .to_rgba8();
        assert_eq!(persisted_image, probe_image, "manifest frame differs from the exact GPUI probe pixels");
        assert_eq!(persisted_image.dimensions(), (1440, 900));

        let native = recorded["native_accessibility"].as_object()
            .expect("manifest must bind native AccessKit evidence for every frame");
        let accesskit_path = checked_capture_artifact(
            &capture_evidence,
            native["path"].as_str().expect("native AccessKit manifest path"),
        );
        let accesskit_bytes = std::fs::read(&accesskit_path).expect("read manifest-bound AccessKit tree");
        assert_eq!(
            sha256_hex(&accesskit_bytes),
            native["sha256"].as_str().expect("native AccessKit SHA-256"),
            "native AccessKit bytes differ from their capture manifest"
        );
        let accesskit: serde_json::Value = serde_json::from_slice(&accesskit_bytes)
            .expect("decode manifest-bound AccessKit tree");
        let native_nodes = accesskit["nodes"].as_object().expect("native AccessKit node map");
        assert!(!native_nodes.is_empty(), "native AccessKit readback is empty for {}", requested.label);

        let reader_path = probe_evidence.join(format!("reader-{}.json", requested.label));
        let reader_bytes = std::fs::read(&reader_path).expect("read per-frame rendered package evidence");
        let reader: serde_json::Value = serde_json::from_slice(&reader_bytes)
            .expect("decode per-frame rendered package evidence");
        let rendered = serde_json::to_string(&reader).expect("rendered reader JSON");
        assert_eq!(reader["root"], handoff.root, "rendered page evidence has the wrong owner root");
        assert!(
            reader["route"].as_str().is_some_and(|route| route.contains("pkg:nuget/nullable@1.3.1")),
            "the exact package route did not reach the GPUI frame: {reader}"
        );
        assert!(rendered.contains("nullable"), "the exact NuGet package did not reach the rendered Reader: {reader}");
        assert!(rendered.contains("1.3.1"), "the exact NuGet version did not reach the rendered Reader: {reader}");
        assert!(rendered.contains("NuGet"), "the rendered dossier did not retain its NuGet ecosystem identity: {reader}");
        frame_evidence.push(serde_json::json!({
            "label": requested.label,
            "time_ms": requested.time_ms,
            "probe_png": {
                "path": probe_frame_path.display().to_string(),
                "sha256": sha256_hex(&probe_frame_bytes),
                "bytes": probe_frame_bytes.len(),
            },
            "manifest_png": {
                "path": persisted_path.display().to_string(),
                "sha256": sha256_hex(&persisted_bytes),
                "bytes": persisted_bytes.len(),
            },
            "accesskit": {
                "path": accesskit_path.display().to_string(),
                "sha256": sha256_hex(&accesskit_bytes),
                "bytes": accesskit_bytes.len(),
                "nodes": native_nodes.len(),
                "focus": accesskit["accesskit_focus"],
            },
            "reader": {
                "path": reader_path.display().to_string(),
                "sha256": sha256_hex(&reader_bytes),
            },
            "manifest_index": index,
        }));
    }
    let capture_manifest_bytes = std::fs::read(&capture_manifest).expect("read GPUI capture manifest bytes");
    let capture_files = capture_tree_evidence(&capture_evidence);
    let consumer_receipt = serde_json::json!({
        "schema": "nudox.gui.live-consumer-receipt.v1",
        "handoff": handoff_path.display().to_string(),
        "handoff_blake3": blake3::hash(&std::fs::read(&handoff_path).expect("read handoff bytes")).to_hex().to_string(),
        "joined_receipt_blake3": handoff.joined_receipt_blake3,
        "workspace": workspace.display().to_string(),
        "endpoint": endpoint.display().to_string(),
        "packages_verified": handoff.packages,
        "root": handoff.root,
        "selected_package": "pkg:nuget/nullable@1.3.1",
        "frames": frame_evidence,
        "capture_manifest": capture_manifest.display().to_string(),
        "capture_manifest_sha256": sha256_hex(&capture_manifest_bytes),
        "capture_entries": capture_files,
        "outcome": "passed",
    });
    let receipt_path = handoff
        .joined_receipt
        .parent()
        .expect("joined receipt parent")
        .join("gui-consumer-receipt.json");
    std::fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&consumer_receipt).expect("encode GUI consumer receipt"),
    )
    .expect("preserve native GPUI consumer receipt");

    let mut run: serde_json::Value = read_typed_json(
        &handoff.joined_receipt.parent().expect("run evidence parent").join("run.json"),
        "joined run progress",
    );
    run["phase"] = serde_json::json!("complete");
    run["gui_consumer_receipt"] = serde_json::json!(receipt_path.display().to_string());
    std::fs::write(
        handoff.joined_receipt.parent().expect("run evidence parent").join("run.json"),
        serde_json::to_vec_pretty(&run).expect("encode completed run evidence"),
    )
    .expect("mark joined readiness complete");

    assert_eq!(
        hex_root(owner_session.revision().expect("final live owner revision").root.as_bytes()),
        handoff.root,
        "GPUI capture must remain attached to the exact owner root"
    );
}

#[test]
#[ignore = "captures real pixels over a real index; run with --ignored"]
fn capture_the_shell_over_a_real_index() {
    let Ok(out) = std::env::var("NUDOX_CAPTURE_OUT") else {
        eprintln!("set NUDOX_CAPTURE_OUT to capture");
        return;
    };
    let out = PathBuf::from(out);
    if out.exists() {
        assert_eq!(
            std::fs::read_dir(&out).expect("capture output").count(),
            0,
            "use a new empty NUDOX_CAPTURE_OUT for every run"
        );
    } else {
        std::fs::create_dir_all(&out).expect("out");
    }
    let projects = vec![repo().join("crates/present"), repo().join("crates/runtime")];
    let project = projects.first().expect("primary project");
    let (host, endpoint) = serve(&projects);
    let mut subscription = backend_client::LocalSubscriptionTransport::connect(&endpoint).expect("subscription");
    let (_, revision) = subscription.bootstrap_root().expect("root");
    let key = VersionedRoot::from_revision(1, revision, 0);
    let places = places(&endpoint, project);
    let only = std::env::var("NUDOX_CAPTURE_ONLY").ok();
    let still = |name: &str, width: u32, height: u32, percent: u16, density, appearance, route: &Route| Shot {
        name: name.to_owned(),
        width,
        height,
        percent,
        density,
        appearance,
        route: route.clone(),
        frames: vec![0, 700],
        script: Script::Still,
    };
    use AppearancePreference::{Abyss, Glacier};
    use DensityPreference::{Comfortable, Compact, Dense};
    let mut shots = vec![
        Shot {
            name: "orbit-two-real-projects".to_owned(),
            width: 1440,
            height: 900,
            percent: 100,
            density: Comfortable,
            appearance: Abyss,
            route: Route::Orbit(OrbitRoute::Home),
            frames: vec![0, 700, 900],
            script: Script::ShelfFocus,
        },
        Shot {
            name: "orbit-ask-modal".to_owned(),
            width: 1440,
            height: 900,
            percent: 100,
            density: Comfortable,
            appearance: Abyss,
            route: Route::Orbit(OrbitRoute::Home),
            frames: vec![0, 700, 900],
            script: Script::Ask,
        },
        Shot {
            name: "orbit-ask-keyboard-journey".to_owned(),
            width: 1440,
            height: 900,
            percent: 100,
            density: Comfortable,
            appearance: Abyss,
            route: Route::Orbit(OrbitRoute::Home),
            frames: vec![0, 100, 900, 1050, 1150, 1250, 1350, 1450, 2050, 2200, 2400, 2600, 3350, 3550, 3750,
                3950, 4150, 4750, 4850, 4950, 5050, 5150, 5550, 5650, 5750, 5850, 5890, 5930, 6000, 6100, 6600, 7600],
            script: Script::AskJourney,
        },
        still("flow-2560", 2560, 1440, 100, Comfortable, Abyss, &places.page),
        still("flow-1440", 1440, 900, 100, Comfortable, Abyss, &places.page),
        still("flow-1100", 1100, 900, 100, Comfortable, Abyss, &places.page),
        still("flow-760", 760, 900, 100, Comfortable, Abyss, &places.page),
        still("flow-480", 480, 900, 100, Comfortable, Abyss, &places.page),
        still("flow-1440-200pct", 1440, 900, 200, Comfortable, Abyss, &places.page),
        still("density-comfortable", 1440, 1100, 100, Comfortable, Abyss, &places.page),
        still("density-compact", 1440, 1100, 100, Compact, Abyss, &places.page),
        still("density-dense", 1440, 1100, 100, Dense, Abyss, &places.page),
        still("glacier-1440", 1440, 900, 100, Comfortable, Glacier, &places.page),
        still("glacier-760", 760, 900, 100, Comfortable, Glacier, &places.page),
        still("glacier-480-200pct", 480, 900, 200, Comfortable, Glacier, &places.page),
        still("package-1440", 1440, 900, 100, Comfortable, Abyss, &places.package),
        still("orbit-1440", 1440, 900, 100, Comfortable, Abyss, &Route::Orbit(OrbitRoute::Home)),
    ];
    let film = |name: &str, script, frames: Vec<u64>| Shot {
        name: name.to_owned(),
        width: 1440,
        height: 900,
        percent: 100,
        density: Comfortable,
        appearance: Abyss,
        route: places.page.clone(),
        frames,
        script,
    };
    shots.push(film("film-descent", Script::Descent, vec![0, 0, 60, 120, 200, 320, 700, 700, 760, 820, 900, 1400]));
    shots.push(film("film-hold-cmd", Script::HoldCommand, vec![0, 0, 120, 240, 280, 360, 520]));
    shots.push(film("film-hold-opt", Script::HoldOption, vec![0, 0, 240, 280, 360, 520]));
    shots.push(film("film-walk", Script::Walk, vec![0, 60, 120, 180, 240, 300, 360, 420, 900]));
    shots.push(Shot {
        width: 1100,
        ..film("film-narrow", Script::Narrow, vec![0, 100, 116, 148, 196, 260, 360, 520, 900])
    });
    for shot in shots {
        if only.as_ref().is_some_and(|only| !shot.name.contains(only.as_str())) {
            continue;
        }
        capture(&shot, &endpoint, key, &projects, &out);
    }
    drop(subscription);
    drop(host);
}
