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
use super::bootstrap::{Binding, BindingReader, BoundWorkspace};
use crate::core::{ErrorValue, FaultCode, LocalProjectId, VersionedRoot};
use crate::model::{
    AppSnapshot, Note, PersistedDesktopState, PersistenceRecovery, PersistentState, SessionState,
    WindowSize,
};
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
use crate::runtime::reads::ReadPool;
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
    run(prepare_recovering(discovered, || super::paths::discover().map_err(|error| format!("find the local workspace: {error}"))));
    std::process::ExitCode::SUCCESS
}

/// Everything the window needs, gathered without waiting on the owner.
pub(crate) struct Boot {
    /// The restored route, shelf, settings and hand, at the unserved root.
    pub(crate) snapshot: AppSnapshot,
    /// One immutable workspace shared by all startup lanes.
    pub(crate) binding: Option<Binding>,
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
    let route = route.clone();
    let reader = file.clone();
    spawn_reading(move || reader.read_route(&route)).map(|seed| SnapshotRead { file, seed })
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
    let mut boot = prepare_window(discovered);
    if let Some(bound) = boot.client.binding.get() {
        boot.owner = start_owner(bound.paths.clone(), boot.gate.clone());
        if boot.owner.is_none() {
            boot.gate.disable_restart();
            if matches!(boot.gate.state(), OwnerState::Starting) {
                boot.gate.publish(OwnerState::Failed(OwnerFault::Host("the local owner could not install a startup worker".into())));
            }
        }
    } else {
        // This fixed-path preparation has no discovery producer. Production
        // uses prepare_recovering, which installs one before returning.
        boot.gate.disable_restart();
    }
    boot
}

fn prepare_recovering(
    discovered: Result<WorkspacePaths, String>,
    discover: impl FnMut() -> Result<WorkspacePaths, String> + Send + 'static,
) -> Boot {
    let mut boot = prepare_window(discovered);
    boot.owner = super::owner::spawn_discovering(boot.client.binding.clone(), boot.gate.clone(), discover);
    boot
}

fn prepare_window(discovered: Result<WorkspacePaths, String>) -> Boot {
    let restoring = Instant::now();
    let gate = OwnerGate::starting();
    let binding = Binding::default();
    let installation = discovered.and_then(|paths| binding.install(paths));
    let pending_binding = installation.is_err().then(|| binding.clone());
    let (snapshot, persistence, endpoint, keep) = match installation {
        Ok(bound) => {
            let snapshot = bound.snapshot.clone();
            let keep = read_snapshot(bound.paths.data(), snapshot.route());
            (snapshot, bound.persistence.clone(), Some(bound.paths.endpoint().to_path_buf()), keep)
        }
        Err(message) => {
            gate.publish(OwnerState::Failed(OwnerFault::from(message.as_str())));
            (AppSnapshot::empty(VersionedRoot::unserved()), None, None, None)
        }
    };
    crate::runtime::trace::span("boot.state", restoring, "desktop-state.json + cold shelf/settings/session");
    Boot { snapshot, persistence, client: BootClient::new(binding.clone(), gate.clone()), endpoint, binding: pending_binding, gate, owner: None, keep }
}

/// Restore before publishing the binding, so Ready cannot race persistence.
pub(crate) fn restore_binding(paths: WorkspacePaths, project: LocalProjectId) -> BoundWorkspace {
    let Restored { state, persistence, note } = restore(&paths);
    let snapshot = restored_snapshot(&paths, &project, &state, note);
    BoundWorkspace { paths, project, snapshot, persistence }
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
            let note = admitted.recovery.note().or_else(|| admitted.state.cargo_source_recovery_note());
            Restored { note, state: admitted.state, persistence: may_save.then_some(persistence) }
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
            reading: Default::default(),
            route: restored.route,
            overlay: restored.overlay,
            overlay_underlays: Default::default(),
            back: restored.back,
            forward: restored.forward,
            selected: restored.selected,
            pending_selection: restored.pending_selection,
            hand: restored.hand,
            whispered: restored.whispered,
            whisper: None,
            preview: None,
        })
}

/// The actor exists before discovery succeeds and binds its local client
/// exactly once on its own thread, after the host installs the workspace.
pub(crate) struct BootClient {
    binding: Binding,
    gate: OwnerGate,
    local: Option<Box<LocalEngineClient>>,
}

impl BootClient {
    pub(crate) fn new(binding: Binding, gate: OwnerGate) -> Self { Self { binding, gate, local: None } }
}

impl EngineClient for BootClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        if self.local.is_none() {
            let not_bound = |message: String| match request {
                EngineRequest::IndexProject { project, .. } => EngineFault::IndexNotSent {
                    project: project.clone(), error: ErrorValue::new(FaultCode::Transport,
                        format!("The first-send workspace could not be admitted: {message}. Nothing was sent.")),
                },
                _ if request.cancelled() => EngineFault::Cancelled,
                _ => EngineFault::Failed(ErrorValue::new(FaultCode::Transport, message)),
            };
            self.gate.wait_cancelled(request.cancellation()).map_err(|fault| not_bound(fault.to_string()))?;
            let bound = self.binding.get().ok_or_else(|| not_bound("the owner answered without a local workspace binding".into()))?;
            self.local = Some(Box::new(LocalEngineClient::gated(bound.paths.endpoint(), bound.project.clone(), self.gate.clone())));
        }
        self.local.as_mut().expect("client installed after workspace admission").execute(request)
    }
}

fn run(mut boot: Boot) {
    let reading = boot.keep.take();
    let Boot { snapshot, persistence, client, endpoint, binding, gate, owner, keep: _ } = boot;
    let window = snapshot.settings().window;
    let Some((runtime, reads)) = start_workers(snapshot, client, endpoint, &gate) else { return };
    let parts = AppParts { runtime, persistence, reads, binding, gate: gate.clone(), reading, window };
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
    binding: Option<Binding>,
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
    _endpoint: Option<PathBuf>,
    gate: &OwnerGate,
) -> Option<(DesktopRuntime, Option<ReadPool>)> {
    let starting_actor = Instant::now();
    let binding = client.binding.clone();
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
    let sessions = gate.clone();
    let reads = match ReadPool::start(READ_SESSIONS, |_| BindingReader::new(binding.clone(), sessions.clone())) {
        Ok(reads) => Some(reads),
        Err(error) => {
            eprintln!("backend-desktop: start read pool: {error}");
            None
        }
    };
    crate::runtime::trace::span("boot.read_pool", starting_reads, format_args!("{READ_SESSIONS} sessions"));
    Some((runtime, reads))
}

/// The platform's launch closure: assets, the data plane, the window.
fn open_the_window(cx: &mut App, parts: AppParts, starting_platform: Instant) {
    let AppParts { runtime, persistence, reads, binding, gate, reading, window: remembered } = parts;
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
    let graph = UiEntityGraph::install_with_bootstrap(cx, runtime, persistence, reads, Some(gate), keep, binding);
    super::menus::install_local_actions(&graph, cx);
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
mod index_startup_tests;

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
