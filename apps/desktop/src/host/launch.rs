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
    AppSnapshot, Note, PersistedDesktopState, PersistenceRecovery, PersistentState, SessionState,
    WindowSize,
};
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::runtime::snapshot::{Keep, Seed, SnapshotFile};
use crate::runtime::{
    DesktopRuntime, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest,
    LocalEngineClient, UiEntityGraph,
};
use backend_runtime::WorkspacePaths;
use gpui::{
    App, AppContext as _, Bounds, Pixels, Size, TitlebarOptions, WindowBounds, WindowOptions, point, px,
};
use std::path::PathBuf;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// The size a window opens at, and the least it can be resized to.
const OPENING: Size<Pixels> = Size { width: px(1380.0), height: px(880.0) };
const LEAST: Size<Pixels> = Size { width: px(320.0), height: px(480.0) };
/// Read sessions in the page-data pool; index/admin work has its own lane.
const READ_SESSIONS: usize = 3;

/// Starts the native application; the local-first owner starts beside it.
#[must_use]
pub fn main_entry() -> std::process::ExitCode {
    crate::runtime::trace::mark("boot.main", "window first");
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
    pub(crate) keep: Option<SnapshotRead>,
}

/// How long the window waits for the snapshot thread before it opens without
/// its pages: the read takes ~3 ms, so this is a bound on a file that blocks
/// (an evicted iCloud file, a dying disk), never a wait.
const SNAPSHOT_WAIT: Duration = Duration::from_millis(250);

/// The launch snapshot file, and the thread reading the route's pages from it.
pub(crate) struct SnapshotRead {
    file: SnapshotFile,
    seed: mpsc::Receiver<Option<Seed>>,
}

/// Starts reading the restored route's pages beside the platform's start.
fn read_snapshot(data: &std::path::Path, route: &crate::navigation::Route) -> Option<SnapshotRead> {
    let file = SnapshotFile::in_data(data);
    let wanted = crate::runtime::snapshot::kept_keys(route);
    let reader = file.clone();
    spawn_reading(move || reader.read(&wanted)).map(|seed| SnapshotRead { file, seed })
}

/// Runs `read` on the snapshot thread and returns where its answer will
/// arrive. A reader that panics is a launch with no snapshot, said once.
fn spawn_reading(read: impl FnOnce() -> Option<Seed> + Send + 'static) -> Option<mpsc::Receiver<Option<Seed>>> {
    let (sent, seed) = mpsc::channel();
    std::thread::Builder::new()
        .name("nudox-snapshot".to_owned())
        .spawn(move || {
            let read = std::panic::catch_unwind(AssertUnwindSafe(read)).unwrap_or_else(|panic| {
                eprintln!("backend-desktop: the launch snapshot reader panicked: {}", crate::runtime::offload::describe(panic.as_ref()));
                None
            });
            let _ = sent.send(read);
        })
        .ok()
        .map(|_| seed)
}

impl SnapshotRead {
    /// Waits for the snapshot thread, no longer than [`SNAPSHOT_WAIT`] (it has
    /// finished long before the window is built; the wait is traced to prove
    /// it).
    pub(crate) fn joined(self) -> Keep {
        let joining = Instant::now();
        let seed = match self.seed.recv_timeout(SNAPSHOT_WAIT) {
            Ok(seed) => seed,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("backend-desktop: the launch snapshot took over {} ms to read; opening without it", SNAPSHOT_WAIT.as_millis());
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };
        crate::runtime::trace::span("boot.snapshot_join", joining, "wait for nudox-snapshot");
        Keep { file: self.file, seed }
    }
}

/// Restores the window's state and starts the owner **without waiting for
/// it**: `start_owner` must return at once (the product's spawns a thread).
pub(crate) fn prepare(
    discovered: Result<WorkspacePaths, String>,
    start_owner: impl FnOnce(WorkspacePaths, OwnerGate) -> Option<OwnerThread>,
) -> Boot {
    let restoring = Instant::now();
    let gate = OwnerGate::starting();
    let unserved = |message: String, gate: OwnerGate| {
        gate.publish(OwnerState::Failed(OwnerFault::from(message.as_str())));
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
    let Restored { state: persisted, persistence, note } = restore(&paths);
    let snapshot = restored_snapshot(&paths, &project, &persisted, note);
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

/// The session file, admitted, and the store to save it to (`None` when it
/// could not be read: a file this run could not read is never overwritten by
/// it, and the window still opens on the default session).
fn restore(paths: &WorkspacePaths) -> Restored {
    let persistence = PersistentState::at(paths.data().join("desktop-state.json"));
    match persistence.load_recovering() {
        Ok(admitted) => {
            match &admitted.recovery {
                PersistenceRecovery::Preserved { backup, reason } => {
                    eprintln!("backend-desktop: preserved unadmitted state at {}: {reason}", backup.display());
                }
                PersistenceRecovery::RetainedAtSource { path, reason } => {
                    eprintln!("backend-desktop: unadmitted state retained at {}: {reason}", path.display());
                }
                PersistenceRecovery::Current | PersistenceRecovery::Missing => {}
            }
            let may_save = !matches!(&admitted.recovery, PersistenceRecovery::RetainedAtSource { .. });
            Restored { note: admitted.recovery.note(), state: admitted.state, persistence: may_save.then_some(persistence) }
        }
        Err(error) => {
            eprintln!("backend-desktop: admit desktop state at {}: {error}", persistence.path().display());
            let unread = Note::StateUnread {
                path: Arc::from(persistence.path().display().to_string()),
                why: Arc::from(error.to_string()),
            };
            Restored { state: PersistedDesktopState::default(), persistence: None, note: Some(unread) }
        }
    }
}

/// What `desktop-state.json` gave back.
struct Restored {
    /// The admitted session (the default one when the file could not be read).
    state: PersistedDesktopState,
    /// Where to save it; `None` when the file could not be read.
    persistence: Option<PersistentState>,
    /// What the window says about it, when the file was not admitted as it was.
    note: Option<Note>,
}

/// The session as it was left, at the unserved root: the owner's root
/// replaces it when the owner answers (`Intent::OwnerReady`).
fn restored_snapshot(
    paths: &WorkspacePaths,
    project: &LocalProjectId,
    persisted: &PersistedDesktopState,
    note: Option<Note>,
) -> AppSnapshot {
    let persistence = PersistentState::at(paths.data().join("desktop-state.json"));
    let host_project_admitted = super::paths::looks_like_project(paths.project());
    let (shelf, mut workspace) =
        persistence.cold_shelf(persisted, host_project_admitted.then_some(paths.project()));
    workspace.host = Some(project.clone());
    if let Some(note) = note {
        workspace.notes = Arc::from([note]);
    }
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

fn run(mut boot: Boot) {
    let reading = boot.keep.take();
    let Boot { snapshot, persistence, client, endpoint, gate, owner, keep: _ } = boot;
    let window = snapshot.settings().window;
    let Some((runtime, reads)) = start_workers(snapshot, client, endpoint, &gate) else { return };
    let parts = AppParts { runtime, persistence, reads, gate: gate.clone(), reading, window };
    let starting_platform = Instant::now();
    let application =
        gpui::Application::with_platform(gpui_platform::current_platform(false))
            .with_assets(facet::icons::Assets);
    application.on_reopen(|cx| {
        cx.activate(true);
        if let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
    });
    application.run(move |cx: &mut App| open_the_window(cx, parts, starting_platform));
    gate.close();
    drop(owner);
}

/// What the platform's launch closure needs, moved into it.
struct AppParts {
    runtime: DesktopRuntime,
    persistence: Option<PersistentState>,
    reads: Option<ReadPool>,
    gate: OwnerGate,
    reading: Option<SnapshotRead>,
    /// The size the person left the window at.
    window: Option<WindowSize>,
}

/// The engine actor and the read pool, gated on the owner: both start now and
/// wait for it in their own threads. `None` when the actor cannot start.
pub(crate) fn start_workers(
    snapshot: AppSnapshot,
    client: BootClient,
    endpoint: Option<PathBuf>,
    gate: &OwnerGate,
) -> Option<(DesktopRuntime, Option<ReadPool>)> {
    let starting_actor = Instant::now();
    let actor = match EngineActor::start(client, 32) {
        Ok(actor) => actor,
        Err(error) => {
            eprintln!("backend-desktop: start engine actor: {error}");
            return None;
        }
    };
    crate::runtime::trace::span("boot.actor", starting_actor, "EngineActor::start");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let starting_reads = Instant::now();
    let reads = endpoint.and_then(|endpoint| {
        let sessions = gate.clone();
        match ReadPool::start(READ_SESSIONS, |_| SessionReader::gated(&endpoint, sessions.clone())) {
            Ok(reads) => Some(reads),
            Err(error) => {
                // The window still opens; every page then says it has no read lane.
                eprintln!("backend-desktop: start read pool: {error}");
                None
            }
        }
    });
    crate::runtime::trace::span("boot.read_pool", starting_reads, format_args!("{READ_SESSIONS} sessions"));
    Some((runtime, reads))
}

/// The platform's launch closure: assets, the data plane, the window.
fn open_the_window(cx: &mut App, parts: AppParts, starting_platform: Instant) {
    let AppParts { runtime, persistence, reads, gate, reading, window: remembered } = parts;
    crate::runtime::trace::span("boot.platform", starting_platform, "Application::with_platform..run");
    let installing = Instant::now();
    if let Err(error) = install(cx) {
        eprintln!("backend-desktop: install UI assets: {error}");
        return;
    }
    super::menus::install(cx);
    crate::runtime::trace::span("boot.fonts", installing, "gpui_component::init + facet::fonts::install");
    crate::runtime::trace::frames(cx);
    crate::runtime::trace::mark("boot.app_running", "gpui");
    // Quitting before the owner answered: release the workers waiting on it
    // before their threads are joined with the app's entities.
    let quitting = gate.clone();
    cx.on_app_quit(move |_| {
        quitting.close();
        async {}
    })
    .detach();
    let keep = reading.map(SnapshotRead::joined);
    let installing_graph = Instant::now();
    let graph = UiEntityGraph::install_with_owner(cx, runtime, persistence, reads, Some(gate), keep);
    // Quitting saves the route's pages for the next launch.
    let saved = graph.store.clone();
    cx.on_app_quit(move |cx| {
        if let Err(error) = saved.read(cx).save_now() {
            eprintln!("backend-desktop: save the launch snapshot: {error}");
        }
        async {}
    })
    .detach();
    crate::runtime::trace::span("boot.ui_graph", installing_graph, "UiEntityGraph::install_with_owner");
    // Temporary: `NUDOX_DEBUG_PAGE="search:Engine;orbit;health"` opens a plain-text
    // window onto the data plane (see runtime::debug_page).
    if let Ok(spec) = std::env::var("NUDOX_DEBUG_PAGE") {
        crate::runtime::debug_page::open_window(cx, graph.store.clone(), crate::runtime::debug_page::parse_keys(&spec));
    }
    let options = window_options(cx, remembered);
    let opening = Instant::now();
    if let Err(error) = cx.open_window(options, move |window, cx| {
        // Native window + renderer creation, before the root is built.
        crate::runtime::trace::span("boot.window_native", opening, "NSWindow + renderer");
        let building = Instant::now();
        let shell = crate::shell::open_shell(&graph, window, cx);
        crate::runtime::trace::span("boot.shell", building, "shell::open_shell");
        super::window_size::remember(window, &graph.root, cx);
        // gpui_component::Root hosts the component layer the Ask field's input
        // engine (IME) expects; the shell is its view.
        cx.new(|cx| gpui_component::Root::new(shell, window, cx).bordered(false))
    }) {
        eprintln!("backend-desktop: open window: {error}");
    }
    crate::runtime::trace::span("boot.open_window", opening, "cx.open_window (incl. shell)");
    crate::runtime::trace::mark("boot.window_opened", "open_window returned");
    cx.activate(true);
}

fn install(cx: &mut App) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    gpui_component::init(cx);
    facet::fonts::install(cx)
}

fn window_options(cx: &mut App, saved: Option<WindowSize>) -> WindowOptions {
    let bounds = Bounds::centered(None, super::window_size::opening(saved, LEAST, OPENING), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Nudox".into()),
            appears_transparent: !cfg!(any(target_os = "linux", target_os = "freebsd")),
            traffic_light_position: Some(point(px(14.0), px(14.0))),
        }),
        window_min_size: Some(LEAST),
        app_id: Some("dev.nudox.desktop".to_owned()),
        ..WindowOptions::default()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn keep_for(seed: mpsc::Receiver<Option<Seed>>) -> SnapshotRead {
        SnapshotRead { file: SnapshotFile::in_data(std::path::Path::new("/nonexistent-i3")), seed }
    }

    #[test]
    fn a_snapshot_that_blocks_never_holds_the_window_past_its_bound() {
        // A reader stuck in a read (an evicted iCloud file, a dying disk).
        let (_never_sends, seed) = mpsc::channel();
        let started = Instant::now();
        let kept = keep_for(seed).joined();
        let waited = started.elapsed();
        assert!(kept.seed.is_none(), "the window opens without its pages");
        assert!(waited >= SNAPSHOT_WAIT && waited < SNAPSHOT_WAIT * 8, "it waited its bound and not for ever: {waited:?}");
    }

    #[test]
    fn a_snapshot_reader_that_panics_is_a_launch_with_no_snapshot_not_a_hang_or_a_crash() {
        // Timed from the moment the reader is gone, not from its spawn: a
        // panic waits on the process's panic-output lock, which another test
        // printing a backtrace can hold for longer than the bound.
        let (alive, gone) = mpsc::channel::<()>();
        let seed = spawn_reading(move || {
            let _alive = alive;
            panic!("the decoder tripped")
        })
        .expect("the reader thread");
        let _ = gone.recv();
        let started = Instant::now();
        let kept = keep_for(seed).joined();
        assert!(kept.seed.is_none(), "no snapshot this launch");
        assert!(started.elapsed() < SNAPSHOT_WAIT, "and the window did not wait out the bound for a reader that was already gone");
    }
}
