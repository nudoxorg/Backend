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
    AppSnapshot, AppearancePreference, DensityPreference, SessionState, SettingsState,
};
use backend_desktop::navigation::{
    Coordinate, Intent, OrbitRoute, PackageLane, PackageRoute, Route, SymbolRoute, View,
};
use backend_desktop::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use backend_desktop::runtime::reads::{ReadPool, SessionReader};
use backend_desktop::runtime::store::DataStore;
use backend_desktop::runtime::{DesktopRuntime, UiEntityGraph};
use backend_desktop::shell::Shell;
use backend_gui_harness::{
    AnimationFrame, CaptureError, GpuiCaptureOptions, GuiState, InputStep, Viewport,
    capture_gpui_state_with_adapters_result_and_semantics,
};
use backend_library::{CommandReply, DeclarationKind, RowState};
use gpui::{App, AppContext as _, Entity, Modifiers, Window};
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

/// The index/admin lane is not needed to read pages; this client answers
/// nothing, so the bootstrapped root stays the one every page is read at.
struct Idle;

impl EngineClient for Idle {
    fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
        Err(EngineFault::Cancelled)
    }
}

/// Starts (or reattaches to) an embedded owner over `crates/present`.
fn serve(project: &Path) -> (backend_desktop::DesktopHost, PathBuf) {
    let state = PathBuf::from(std::env::var("NUDOX_CAPTURE_STATE").unwrap_or_else(|_| "/tmp/nx-shell-cap".to_owned()));
    let endpoint = PathBuf::from(format!("{}.sock", state.display()));
    std::fs::create_dir_all(state.join("data")).expect("state dir");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(project.to_path_buf()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .expect("workspace paths");
    let host = backend_desktop::DesktopHost::start_with_paths(paths).expect("embedded owner");
    let mut session = Session::connect(&endpoint).expect("session");
    let root = project.to_str().expect("utf-8 path");
    session.index(root).expect("index request");
    let started = Instant::now();
    let (mut last, mut stable) = (0, 0);
    loop {
        let ready = matches!(
            session.packages().map(|reply| reply.reply),
            Ok(CommandReply::Packages(snapshot))
                if snapshot.root.rows().iter().any(|row| row.label == root && row.state == RowState::Ready)
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
}

struct Places {
    page: Route,
    package: Route,
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

fn capture(shot: &Shot, endpoint: &Path, snapshot_key: VersionedRoot, out: &Path) {
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
    let graph_slot: Rc<RefCell<Option<(UiEntityGraph, Entity<Shell>)>>> = Rc::new(RefCell::new(None));
    let build_slot = Rc::clone(&graph_slot);
    let hook_slot = Rc::clone(&graph_slot);
    let last_slot = Rc::clone(&graph_slot);
    let last_label = frames.last().map(|frame| frame.label.clone()).unwrap_or_default();
    let built = Rc::new(std::cell::Cell::new(false));
    let built_mark = Rc::clone(&built);
    let endpoint = endpoint.to_path_buf();
    let shot_build = shot.clone();
    let shot_hook = shot.clone();
    // The window narrows through 900 (the shelf becomes a spine) at 100 ms.
    let actions = if shot.script == Script::Narrow {
        vec![InputStep::Wait { milliseconds: 100 }, InputStep::Resize { width: 880, height: shot.height }]
    } else {
        Vec::new()
    };
    let set = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new(shot.name.clone(), None, None),
        &actions,
        &frames,
        GpuiCaptureOptions {
            asset_source: std::sync::Arc::new(facet::icons::Assets),
            ..GpuiCaptureOptions::default()
        },
        move |frame: &AnimationFrame, _window: &mut Window, cx: &mut App| -> Result<(), CaptureError> {
            let slot = hook_slot.borrow();
            let (graph, shell) = slot.as_ref().expect("built");
            let index = frames_index(&frame.label);
            match (shot_hook.script, index) {
                (Script::Descent, 1) => {
                    graph.root.update(cx, |root, cx| root.dispatch(Intent::Navigate(shot_hook.route.clone()), cx));
                    land(&graph.store, cx);
                }
                (Script::Descent, 7) => {
                    graph.root.update(cx, |root, cx| root.dispatch(Intent::ZoomOut, cx));
                    land(&graph.store, cx);
                }
                (Script::HoldCommand, 1) => shell.update(cx, |shell, cx| {
                    shell.modifiers(Modifiers { platform: true, ..Modifiers::default() }, cx);
                }),
                (Script::HoldOption, 1) => shell.update(cx, |shell, cx| {
                    shell.modifiers(Modifiers { alt: true, ..Modifiers::default() }, cx);
                }),
                (Script::Walk, index) if index > 0 => shell.update(cx, |shell, cx| shell.walk(1, cx)),
                _ => {}
            }
            land(&graph.store, cx);
            Ok(())
        },
        |_, _, _| {},
        move |frame: &AnimationFrame, _, _, _, _| {
            // Release the entity handles before the capture's app drops.
            if frame.label == last_label {
                built_mark.set(last_slot.borrow_mut().take().is_some());
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
            let snapshot = AppSnapshot::empty(snapshot_key)
                .with_settings(settings)
                .with_session(SessionState {
                    route: start,
                    back: vec![Route::Orbit(OrbitRoute::Home)].into(),
                    ..SessionState::default()
                });
            let mut workspace = snapshot.workspace().clone();
            workspace.host = LocalProjectId::from_path(&repo().join("crates/present")).ok();
            let snapshot = snapshot.with_workspace(workspace);
            let actor = EngineActor::start(Idle, 8).expect("actor");
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
            *build_slot.borrow_mut() = Some((graph, shell));
            root
        },
    )
    .expect("capture");
    let dir = out.join(&shot.name);
    std::fs::create_dir_all(&dir).expect("out dir");
    for record in &set.frames {
        let path = if set.frames.len() == 1 {
            out.join(format!("{}.png", shot.name))
        } else {
            dir.join(format!("{}.png", record.label))
        };
        record.image.save(&path).expect("png");
    }
    if set.frames.len() > 1 {
        strip(&set.frames.iter().map(|record| &record.image).collect::<Vec<_>>(), &out.join(format!("{}-strip.png", shot.name)));
    }
    assert!(built.get(), "the shell was built and released");
    eprintln!("captured {} ({} frames)", shot.name, set.frames.len());
}

fn frames_index(label: &str) -> usize {
    label[1..3].parse().unwrap_or(0)
}

/// Lays frames side by side at half size.
fn strip(frames: &[&image::RgbaImage], path: &Path) {
    let scaled = frames
        .iter()
        .map(|frame| image::imageops::resize(*frame, frame.width() / 2, frame.height() / 2, image::imageops::FilterType::Triangle))
        .collect::<Vec<_>>();
    let gap = 8;
    let width = scaled.iter().map(image::RgbaImage::width).sum::<u32>() + gap * (scaled.len() as u32 - 1);
    let height = scaled.iter().map(image::RgbaImage::height).max().unwrap_or(1);
    let mut canvas = image::RgbaImage::from_pixel(width, height, image::Rgba([12, 12, 16, 255]));
    let mut x = 0;
    for frame in &scaled {
        image::imageops::overlay(&mut canvas, frame, i64::from(x), 0);
        x += frame.width() + gap;
    }
    canvas.save(path).expect("strip png");
}

#[test]
#[ignore = "captures real pixels over a real index; run with --ignored"]
fn capture_the_shell_over_a_real_index() {
    let Ok(out) = std::env::var("NUDOX_CAPTURE_OUT") else {
        eprintln!("set NUDOX_CAPTURE_OUT to capture");
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("out");
    let project = repo().join("crates/present");
    let (host, endpoint) = serve(&project);
    let mut subscription = backend_client::LocalSubscriptionTransport::connect(&endpoint).expect("subscription");
    let (_, revision) = subscription.bootstrap_root().expect("root");
    let key = VersionedRoot::from_revision(1, revision, 0);
    let places = places(&endpoint, &project);
    let only = std::env::var("NUDOX_CAPTURE_ONLY").ok();
    let still = |name: &str, width: u32, height: u32, percent: u16, density, appearance, route: &Route| Shot {
        name: name.to_owned(),
        width,
        height,
        percent,
        density,
        appearance,
        route: route.clone(),
        frames: vec![700],
        script: Script::Still,
    };
    use AppearancePreference::{Abyss, Glacier};
    use DensityPreference::{Comfortable, Compact, Dense};
    let mut shots = vec![
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
        capture(&shot, &endpoint, key, &out);
    }
    drop(subscription);
    drop(host);
}
