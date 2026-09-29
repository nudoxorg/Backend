//! Native startup: the window first (W-Open I1).
//!
//! `main` finds the workspace (0.1 ms), restores `desktop-state.json`
//! (under 1 ms) and opens the window at once. The index owner starts beside
//! it on its own thread (`host::owner`). Its answer is a data event on the
//! window, never a wait in front of it (`runtime::owner`). An owner that
//! cannot start is a fault the window shows, in the host's own words, with a
//! way to try again; it is never an exit before the window exists.
//!
//! The pages the window last showed come back from the launch snapshot
//! (`runtime::snapshot`, W-Open I2), read on a thread while the platform starts,
//! so the first frame is the page, not a skeleton (I2).

use super::owner::OwnerThread;
use crate::core::{ErrorValue, FaultCode, LocalProjectId, VersionedRoot};
use crate::model::{
    AppSnapshot, PersistedDesktopState, PersistenceRecovery, PersistentState, SessionState,
};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::runtime::snapshot::{Keep, Seed, SnapshotFile};
use crate::runtime::{
    DesktopRuntime, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest,
    LocalEngineClient, UiEntityGraph,
};
use backend_runtime::WorkspacePaths;
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const WINDOW: (f32, f32) = (1380.0, 880.0);
const MINIMUM: (f32, f32) = (320.0, 480.0);
/// Read sessions in the page-data pool; index/admin work has its own lane.
const READ_SESSIONS: usize = 3;

/// Starts the native application; the local-first owner starts beside it.
#[must_use]
pub fn main_entry() -> std::process::ExitCode {
    crate::runtime::trace::mark("boot.main", "window first");
    // The world takes ~200 ms to be ready and nothing about it depends on
    // the workspace: start it before anything else (W-Open I3).
    crate::runtime::fixture_world::preload();
    let discovering = Instant::now();
    let discovered =
        super::paths::discover().map_err(|error| format!("find the local workspace: {error}"));
    crate::runtime::trace::span("boot.discover", discovering, "paths::discover");
    run(prepare(discovered, super::owner::spawn));
    std::process::ExitCode::SUCCESS
}

/// Everything the window needs, gathered without waiting on the owner.
pub(crate) struct Boot {
    /// The restored route, shelf, settings and hand, at the unserved root.
    pub(crate) snapshot: AppSnapshot,
    /// Where the session is saved; `None` when it could not be read.
    pub(crate) persistence: Option<PersistentState>,
    /// The engine actor's client, gated on the owner.
    pub(crate) client: BootClient,
    /// The owner's endpoint, when the workspace was found.
    pub(crate) endpoint: Option<PathBuf>,
    /// The owner's state, as the window will observe it.
    pub(crate) gate: OwnerGate,
    /// The owner's thread; dropping it lets the host go.
    pub(crate) owner: Option<OwnerThread>,
    /// The launch snapshot, and the thread reading the route's pages from it.
    pub(crate) keep: Option<Reading>,
}

/// The launch snapshot file, and the thread reading the route's pages.
pub(crate) type Reading = (SnapshotFile, std::thread::JoinHandle<Option<Seed>>);

/// Starts reading the restored route's pages beside the platform's start.
fn read_snapshot(data: &std::path::Path, route: &crate::navigation::Route) -> Option<Reading> {
    let file = SnapshotFile::in_data(data);
    let wanted = crate::runtime::snapshot::kept_keys(route);
    let reader = file.clone();
    std::thread::Builder::new()
        .name("nudox-snapshot".to_owned())
        .spawn(move || reader.read(&wanted))
        .ok()
        .map(|reading| (file, reading))
}

/// Joins the snapshot thread (it has finished long before the window is
/// built; the join is traced to prove it).
pub(crate) fn joined((file, reading): Reading) -> Keep {
    let joining = Instant::now();
    let seed = reading.join().ok().flatten();
    crate::runtime::trace::span("boot.snapshot_join", joining, "join nudox-snapshot");
    Keep { file, seed }
}

/// Restores the window's state and starts the owner **without waiting for
/// it**: `start_owner` must return at once (the product's spawns a thread).
#[allow(clippy::too_many_lines, reason = "one early exit per way the workspace can be missing")]
pub(crate) fn prepare(
    discovered: Result<WorkspacePaths, String>,
    start_owner: impl FnOnce(WorkspacePaths, OwnerGate) -> Option<OwnerThread>,
) -> Boot {
    let restoring = Instant::now();
    let gate = OwnerGate::starting();
    let unserved = |message: String, gate: OwnerGate| {
        gate.publish(OwnerState::Failed(Arc::from(message.as_str())));
        Boot {
            snapshot: AppSnapshot::empty(VersionedRoot::unserved()),
            persistence: None,
            client: BootClient::Unserved(Arc::from(message)),
            endpoint: None,
            gate,
            owner: None,
            keep: None,
        }
    };
    let paths = match discovered {
        Ok(paths) => paths,
        Err(message) => return unserved(message, gate),
    };
    let project = match LocalProjectId::from_path(paths.project()) {
        Ok(project) => project,
        Err(error) => {
            return unserved(
                format!("the discovered workspace path cannot be represented safely: {error}"),
                gate,
            );
        }
    };
    let persistence = PersistentState::at(paths.data().join("desktop-state.json"));
    let (persisted, persistence) = match persistence.load_recovering() {
        Ok(admitted) => {
            if let PersistenceRecovery::Preserved { backup, reason } = &admitted.recovery {
                eprintln!(
                    "backend-desktop: preserved unadmitted state at {}: {reason}",
                    backup.display()
                );
            }
            (admitted.state, Some(persistence))
        }
        // The window still opens, on the default session; a file this run
        // could not read is never overwritten by it.
        Err(error) => {
            eprintln!(
                "backend-desktop: admit desktop state at {}: {error}",
                persistence.path().display()
            );
            (PersistedDesktopState::default(), None)
        }
    };
    let snapshot = restored_snapshot(&paths, &project, &persisted);
    want_from_world(&snapshot);
    let keep = read_snapshot(paths.data(), snapshot.route());
    let client = BootClient::Local(Box::new(LocalEngineClient::gated(
        paths.endpoint(),
        project,
        gate.clone(),
    )));
    let endpoint = paths.endpoint().to_path_buf();
    crate::runtime::trace::span(
        "boot.state",
        restoring,
        "desktop-state.json + cold shelf/settings/session",
    );
    let owner = start_owner(paths, gate.clone());
    Boot {
        snapshot,
        persistence,
        client,
        endpoint: Some(endpoint),
        gate,
        owner,
        keep,
    }
}

/// Tells the world thread what the restored window will ask of it, so the
/// anatomy of the page it was left on (and the hand it held) is computed
/// there before the world is announced, and its first redraw already has
/// them.
fn want_from_world(snapshot: &AppSnapshot) {
    if let crate::navigation::Route::Symbol(route) = snapshot.route()
        && let Some(decl) = crate::model::pages::DeclRef::from_label(route.id.as_str(), None, None, None)
        && let Ok(package) = crate::model::pages::PackageRef::parse(route.package.as_str())
    {
        crate::runtime::fixture_world::want(crate::runtime::fixture_world::Want { decl, package });
    }
    crate::runtime::fixture_world::want_hand(snapshot.session().hand.held());
}

/// The session as it was left, at the unserved root: the owner's root
/// replaces it when the owner answers (`Intent::OwnerReady`).
fn restored_snapshot(
    paths: &WorkspacePaths,
    project: &LocalProjectId,
    persisted: &PersistedDesktopState,
) -> AppSnapshot {
    let persistence = PersistentState::at(paths.data().join("desktop-state.json"));
    let host_project_admitted = super::paths::looks_like_project(paths.project());
    let (shelf, mut workspace) =
        persistence.cold_shelf(persisted, host_project_admitted.then_some(paths.project()));
    workspace.host = Some(project.clone());
    let settings = persistence.cold_settings(persisted);
    let restored = persistence.cold_reload(persisted);
    AppSnapshot::empty(VersionedRoot::unserved())
        .with_shelf(shelf)
        .with_workspace(workspace)
        .with_settings(settings)
        .with_session(SessionState {
            route: restored.route,
            overlay: restored.overlay,
            back: restored.back,
            forward: restored.forward,
            selected: restored.selected,
            hand: restored.hand,
            whispered: restored.whispered,
            whisper: None,
            preview: None,
        })
}

/// The engine actor's client: the gated local session, or — when no
/// workspace was found — one that answers every request with why.
pub(crate) enum BootClient {
    /// The workspace's owner, waited for on the actor thread.
    Local(Box<LocalEngineClient>),
    /// No owner can exist for this window.
    Unserved(Arc<str>),
}

impl EngineClient for BootClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match self {
            Self::Local(client) => client.execute(request),
            Self::Unserved(message) => Err(EngineFault::Failed(ErrorValue::new(
                FaultCode::Transport,
                format!("the index could not start: {message}"),
            ))),
        }
    }
}

#[allow(clippy::too_many_lines, reason = "the platform's one launch closure, traced step by step")]
fn run(mut boot: Boot) {
    let reading = boot.keep.take();
    let Boot {
        snapshot,
        persistence,
        client,
        endpoint,
        gate,
        owner,
        keep: _,
    } = boot;
    let starting_actor = Instant::now();
    let actor = match EngineActor::start(client, 32) {
        Ok(actor) => actor,
        Err(error) => {
            eprintln!("backend-desktop: start engine actor: {error}");
            return;
        }
    };
    crate::runtime::trace::span("boot.actor", starting_actor, "EngineActor::start");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let starting_reads = Instant::now();
    let reads = endpoint.and_then(|endpoint| {
        let sessions = gate.clone();
        match ReadPool::start(READ_SESSIONS, |_| {
            SessionReader::gated(&endpoint, sessions.clone())
        }) {
            Ok(reads) => Some(reads),
            Err(error) => {
                // The window still opens; every page then says it has no read lane.
                eprintln!("backend-desktop: start read pool: {error}");
                None
            }
        }
    });
    crate::runtime::trace::span(
        "boot.read_pool",
        starting_reads,
        format_args!("{READ_SESSIONS} sessions"),
    );
    let window_gate = gate.clone();
    let starting_platform = Instant::now();
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(facet::icons::Assets)
        .run(move |cx: &mut App| {
            crate::runtime::trace::span(
                "boot.platform",
                starting_platform,
                "Application::with_platform..run",
            );
            let installing = Instant::now();
            if let Err(error) = install(cx) {
                eprintln!("backend-desktop: install UI assets: {error}");
                return;
            }
            crate::runtime::trace::span(
                "boot.fonts",
                installing,
                "gpui_component::init + facet::fonts::install",
            );
            crate::runtime::trace::frames(cx);
            crate::runtime::trace::mark("boot.app_running", "gpui");
            // Quitting before the owner answered: release the workers waiting
            // on it before their threads are joined with the app's entities.
            let quitting = window_gate.clone();
            cx.on_app_quit(move |_| {
                quitting.close();
                async {}
            })
            .detach();
            let keep = reading.map(joined);
            let installing_graph = Instant::now();
            let graph = UiEntityGraph::install_with_owner(
                cx,
                runtime,
                persistence,
                reads,
                Some(window_gate),
                keep,
            );
            // Quitting saves the route's pages for the next launch.
            let saved = graph.store.clone();
            cx.on_app_quit(move |cx| {
                if let Err(error) = saved.read(cx).save_now() {
                    eprintln!("backend-desktop: save the launch snapshot: {error}");
                }
                async {}
            })
            .detach();
            crate::runtime::trace::span(
                "boot.ui_graph",
                installing_graph,
                "UiEntityGraph::install_with_owner",
            );
            // Temporary: `NUDOX_DEBUG_PAGE="search:Engine;orbit;health"` opens a
            // plain-text window onto the data plane (see runtime::debug_page).
            if let Ok(spec) = std::env::var("NUDOX_DEBUG_PAGE") {
                crate::runtime::debug_page::open_window(
                    cx,
                    graph.store.clone(),
                    crate::runtime::debug_page::parse_keys(&spec),
                );
            }
            let options = window_options(cx);
            let opening = Instant::now();
            if let Err(error) = cx.open_window(options, move |window, cx| {
                // Native window + renderer creation, before the root is built.
                crate::runtime::trace::span("boot.window_native", opening, "NSWindow + renderer");
                let building = Instant::now();
                let shell = crate::shell::open_shell(&graph, window, cx);
                crate::runtime::trace::span("boot.shell", building, "shell::open_shell");
                // gpui_component::Root hosts the component layer the Ask
                // field's input engine (IME) expects; the shell is its view.
                cx.new(|cx| gpui_component::Root::new(shell, window, cx).bordered(false))
            }) {
                eprintln!("backend-desktop: open window: {error}");
            }
            crate::runtime::trace::span("boot.open_window", opening, "cx.open_window (incl. shell)");
            crate::runtime::trace::mark("boot.window_opened", "open_window returned");
            cx.activate(true);
        });
    gate.close();
    drop(owner);
}

fn install(cx: &mut App) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    gpui_component::init(cx);
    facet::fonts::install(cx)
}

fn window_options(cx: &mut App) -> WindowOptions {
    let (width, height) = opening_size();
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Nudox".into()),
            appears_transparent: !cfg!(any(target_os = "linux", target_os = "freebsd")),
            traffic_light_position: Some(point(px(14.0), px(14.0))),
        }),
        window_min_size: Some(size(px(MINIMUM.0), px(MINIMUM.1))),
        app_id: Some("dev.nudox.desktop".to_owned()),
        ..WindowOptions::default()
    }
}

const fn opening_size() -> (f32, f32) {
    WINDOW
}
