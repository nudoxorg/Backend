//! The first-run scene: the product's own boot on a clean state directory.
//!
//! [`crate::host::launch::prepare`] restores `desktop-state.json`, the owner
//! starts on its own thread, and the gated actor and read pool wait on it,
//! exactly as `main` does. Nothing is pre-admitted: no fixture root, no
//! project, no read stand-in. What the window shows first is what a person
//! sees on first launch, and what it shows after `Intent::AddProject` is the
//! real embedded owner compiling the folder.
//!
//! The state directory is `NUDOX_HARNESS_STATE` (a directory this run owns).
//! A second boot on the same directory is a relaunch: the session, the
//! shelf and the settings are restored from `desktop-state.json`.
//!
//! Environment (read once, at build):
//! - `NUDOX_INSTALL_ADD=PATH` admits one folder at boot, by the product's own
//!   `Intent::AddProject` (the dialog's submit). Scripts that drive the dialog
//!   with keys do not need it.
//! - `NUDOX_INSTALL_REFUSE=1` answers every index request with the owner's real
//!   refusal of `serde_core` (recorded in `.local/lanes/wave6/index/`), instead
//!   of running the compile: the fixture roots the owner refuses are all large,
//!   and a capture of the failure states should not wait minutes for one. The
//!   rest of the boot (host, owner, reads, persistence) is the real one.
//! - `NUDOX_INSTALL_QUIET=owner|settled` (default `owner`): what a capture
//!   waits for. `owner`: the owner answered (or said why not). `settled`: the
//!   owner has also finished every index request in flight, and every package
//!   the projects build with has been added (or said why not).

use super::{ROOT, endpoint_for, private_dir, private_umask, settings_intents};
use crate::host::launch;
use crate::runtime::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::model::{AppearancePreference, ContrastPreference, DensityPreference, MotionPreference};
use crate::navigation::Intent;
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::runtime::store::DataStore;
use crate::runtime::{DesktopRuntime, EngineActor, UiEntityGraph, UiRootEntity};
use crate::model::PersistentState;
use backend_gui_harness::Act;
use backend_runtime::WorkspacePaths;
use facet::ActiveFacet;
use facet::gallery::{self, Scene};
use gpui::{AnyView, App, AppContext as _, Global, Window};
use std::path::PathBuf;

/// Read sessions in the page-data pool, as the product opens them.
const READ_SESSIONS: usize = 3;

/// The scenes this module serves.
pub(super) fn scenes() -> Vec<Scene> {
    vec![Scene {
        id: "desktop-install",
        title: "First run: the product's own boot on a clean state directory, no root admitted",
        size: (1440, 900),
        build: |window, cx| {
            boot(window, cx).unwrap_or_else(|error| panic!("desktop install boot: {error}"))
        },
    }]
}

/// What a capture waits for before it draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Wait {
    /// The owner answered (or said why it could not).
    Owner,
    /// The owner also finished every index request in flight.
    Settled,
}

impl Wait {
    fn from_env() -> Self {
        match std::env::var("NUDOX_INSTALL_QUIET").as_deref() {
            Ok("settled") => Self::Settled,
            _ => Self::Owner,
        }
    }
}

/// The booted product window, kept for the run.
struct Installed {
    graph: UiEntityGraph,
    shell: gpui::Entity<crate::shell::Shell>,
    gate: OwnerGate,
    wait: Wait,
    /// The owner's thread: dropping it lets the host go.
    _owner: Option<crate::host::owner::OwnerThread>,
}

impl Global for Installed {}

fn state_dir() -> Result<PathBuf, String> {
    let dir = std::env::var_os("NUDOX_HARNESS_STATE")
        .map(PathBuf::from)
        .ok_or_else(|| "NUDOX_HARNESS_STATE must name the clean state directory this run owns".to_owned())?;
    private_umask();
    private_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    dir.canonicalize().map_err(|error| format!("{}: {error}", dir.display()))
}

fn boot(window: &mut Window, cx: &mut App) -> Result<AnyView, String> {
    let state = state_dir()?;
    let data = state.join("data");
    let starter = state.join("starter");
    private_dir(&data).map_err(|error| format!("{}: {error}", data.display()))?;
    private_dir(&starter).map_err(|error| format!("{}: {error}", starter.display()))?;
    let endpoint = endpoint_for(&data)?;
    let paths = WorkspacePaths::discover(Some(starter), Some(data), Some(endpoint))
        .map_err(|error| format!("workspace paths: {error}"))?;
    let launched = launch::prepare(Ok(paths), crate::host::owner::spawn);
    let launch::Boot { snapshot, persistence, client, endpoint, gate, owner, keep, .. } = launched;
    let client = Refusing { inner: client, refuse: std::env::var_os("NUDOX_INSTALL_REFUSE").is_some() };
    let actor = EngineActor::start(client, 32).map_err(|error| format!("engine actor: {error}"))?;
    let runtime = DesktopRuntime::new(snapshot, actor);
    let reads = endpoint.and_then(|endpoint| {
        let sessions = gate.clone();
        ReadPool::start(READ_SESSIONS, |_| SessionReader::gated(&endpoint, sessions.clone())).ok()
    });
    let keep = keep.map(launch::SnapshotRead::joined);
    let graph = UiEntityGraph::install_with_owner(cx, runtime, persistence, reads, Some(gate.clone()), keep);
    gallery::declare_quiet(quiet, cx);
    gallery::declare_adapter(adapt, cx);
    // The shot's facet becomes the product's settings, through its own intents.
    let facet = cx.facet();
    for intent in settings_intents(&facet) {
        graph.root.update(cx, |root, cx| root.dispatch(intent, cx));
    }
    if let Some(folder) = std::env::var_os("NUDOX_INSTALL_ADD") {
        let project = crate::core::LocalProjectId::from_path(std::path::Path::new(&folder))
            .map_err(|error| format!("NUDOX_INSTALL_ADD {}: {error:?}", folder.to_string_lossy()))?;
        graph.root.update(cx, |root, cx| root.dispatch(Intent::AddProject { project }, cx));
    }
    let shell = ROOT(&graph, window, cx);
    // As `launch::open_the_window` does: a resize that rests is remembered.
    crate::host::window_size::remember(window, &graph.root, cx);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = (facet.text_scale * 100.0).round() as u16;
    let display = shell.read(cx).display_key();
    graph.root.update(cx, |root, cx| root.dispatch(Intent::ZoomTo { display, percent }, cx));
    cx.set_global(Installed { graph, shell: shell.clone(), gate, wait: Wait::from_env(), _owner: owner });
    Ok(shell.into())
}

/// The owner's real refusal of `serde_core-1.0.229`, as the harness recorded it.
const REFUSAL: &str = "command execution failed: local semantic compilation failed; prior selected semantic generation was preserved: package semantic compilation failed for src/de/mod.rs: Compile { attempted: CompilerAttempt { source: SourceAuthority { identity: ContentId(0e552256b8bf400484cece97ac9e923036a69df75ef95e30687ceb2cdac7017d), byte_len: 83879 }, recipe: ContentId(11ba653f7cc465c392332868a14a23d8dfc1e882fa7d1fe583fc36469ccc25b0) }, cause: Lowering(LoweringCause(RustGenericParameter)) }";

/// The product's client, except that (when asked) an index request is
/// refused with [`REFUSAL`].
struct Refusing {
    inner: launch::BootClient,
    refuse: bool,
}

impl EngineClient for Refusing {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::IndexProject { project, .. } if self.refuse => Err(EngineFault::IndexFailed {
                project: project.clone(),
                error: crate::core::ErrorValue::new(crate::core::FaultCode::Protocol, REFUSAL),
            }),
            other => self.inner.execute(other),
        }
    }
}

/// Holds the instant (real time, the virtual clock stands) until no project
/// is indexing and the packages they build with have landed: all of them, or
/// `enough`. Engine results and the worker's stages are taken as they come,
/// so what the window shows next is what a person would see at that moment.
fn await_install(enough: Option<usize>, root: &gpui::Entity<UiRootEntity>, cx: &mut App) {
    let started = std::time::Instant::now();
    loop {
        root.update(cx, |root, cx| root.drain_now(cx));
        crate::runtime::acquire::land_now(cx);
        let snapshot = root.read(cx).snapshot();
        let projects = &snapshot.workspace().projects;
        let indexing = projects.iter().any(|project| project.phase == crate::model::ProjectPhase::Indexing);
        let landed = projects
            .iter()
            .filter_map(|project| crate::runtime::acquire::project_packages(&project.id, crate::runtime::offload::Asker::Everyone, cx))
            .map(|packages| match packages {
                crate::runtime::acquire::ProjectPackages::Read(found) => {
                    found.iter().filter(|(_, stage)| stage.as_ref().is_some_and(|stage| !stage.working())).count()
                }
                _ => 0,
            })
            .sum::<usize>();
        let done = match enough {
            Some(enough) => landed >= enough,
            None => !indexing && !crate::runtime::acquire::working(cx),
        };
        if done || started.elapsed() > INSTALL_DEADLINE {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Whether the window has nothing left to wait for.
fn quiet(cx: &mut App) -> bool {
    let Some(installed) = cx.try_global::<Installed>() else {
        return true;
    };
    let (store, root, shell, wait) =
        (installed.graph.store.clone(), installed.graph.root.clone(), installed.shell.clone(), installed.wait);
    if matches!(installed.gate.state(), OwnerState::Starting) {
        return false;
    }
    store.update(cx, |store, cx| {
        store.drain(cx);
    });
    let ui_idle = shell.read(cx).graph_ready(cx) && super::in_flight().iter().all(|(_, count)| *count == 0);
    // The owner answers reads while it compiles (the compile runs off its
    // loop), so every read must have landed, whatever is being indexed; the
    // index requests themselves are what `settled` waits for.
    let indexing = store
        .read(cx)
        .snapshot()
        .workspace()
        .projects
        .iter()
        .any(|project| project.phase == crate::model::ProjectPhase::Indexing);
    // The packages a project builds with are indexed one by one after it.
    let adding = crate::runtime::acquire::working(cx);
    let reads_landed = store.read(cx).pool_load() == (0, 0) && !root.read(cx).has_pending_work_besides_indexing();
    match wait {
        Wait::Owner => ui_idle && reads_landed,
        Wait::Settled => ui_idle && !indexing && !adding && reads_landed,
    }
}

/// How long `route await …` holds its instant for the owner's work.
const INSTALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(40 * 60);

/// Settings acts through the product's own intents. `route` names no page
/// here (a first-run window has no fixture to name one in); it holds the
/// instant for the owner's real work instead:
/// - `route await install`: every project on the shelf is answered for and
///   every package they build with is added (or refused in words);
/// - `route await packages N`: at least N of those packages have landed.
fn adapt(act: &Act, _window: &mut Window, cx: &mut App) {
    let Some(installed) = cx.try_global::<Installed>() else {
        return;
    };
    let (root, shell) = (installed.graph.root.clone(), installed.shell.clone());
    let intent = match act {
        Act::Route { target } => {
            let words = target.split_whitespace().collect::<Vec<_>>();
            let enough = match words.as_slice() {
                ["await", "install"] => None,
                ["await", "packages", count] => Some(count.parse::<usize>().unwrap_or_else(|_| panic!("route {target}: not a count"))),
                _ => panic!("route {target}: the install scene serves `await install` and `await packages N`"),
            };
            await_install(enough, &root, cx);
            return;
        }
        Act::TextScale { percent } => Intent::ZoomTo { display: shell.read(cx).display_key(), percent: *percent },
        Act::Density { name } => Intent::SetDensity(match name.as_str() {
            "compact" => DensityPreference::Compact,
            "dense" => DensityPreference::Dense,
            _ => DensityPreference::Comfortable,
        }),
        Act::Theme { name } => Intent::SetAppearance(if name == "glacier" {
            AppearancePreference::Glacier
        } else {
            AppearancePreference::Abyss
        }),
        Act::Contrast { name } => Intent::SetContrast(if name == "high" {
            ContrastPreference::High
        } else {
            ContrastPreference::Normal
        }),
        Act::Motion { on } => Intent::SetMotion(if *on { MotionPreference::Full } else { MotionPreference::Reduced }),
        _ => return,
    };
    root.update(cx, |root, cx| root.dispatch(intent, cx));
}
