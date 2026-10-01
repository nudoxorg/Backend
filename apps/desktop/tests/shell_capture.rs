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
    Coordinate, Intent, OrbitRoute, PackageLane, PackageRoute, Route, SymbolRoute, View,
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
use backend_library::{CommandReply, DeclarationKind, RowState};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Modifiers, Render, Styled, WeakEntity,
    Window, div, rgb,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

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
}

struct Places {
    page: Route,
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
    let coordinate = match reply.reply {
        CommandReply::Search(result) => result
            .root
            .rows()
            .iter()
            .find(|row| {
                row.label.ends_with("::RelationLabel")
                    && row.kind == Some(DeclarationKind::Enum)
                    && row.label.contains("glyph.rs")
            })
            .map(|row| row.label.clone())
            .expect("RelationLabel is indexed"),
        other => panic!("search answered {other:?}"),
    };
    let package = PackageId::new(project.to_str().expect("utf-8")).expect("package");
    Places {
        page: Route::Symbol(SymbolRoute {
            project: None,
            package: package.clone(),
            id: Coordinate::new(&coordinate).expect("coordinate"),
            at: None,
            view: View::Page,
            line: None,
            selected: None,
        }),
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
            Ok(())
        },
        |_, _, _| {},
        move |frame: &AnimationFrame, _, _, _, _| -> Result<_, CaptureError> {
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
    }
    assert!(built.get(), "the mounted shell graph survived through the final frame");
    eprintln!("captured {} ({} frames)", shot.name, set.frames.len());
}

fn frames_index(label: &str) -> usize {
    label[1..3].parse().unwrap_or(0)
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
