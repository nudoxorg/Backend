//! Native startup for the v3 desktop.
//!
//! Startup chooses the workspace owner, admits one producer root, restores
//! durable shelf/session state, installs the state owner and the data plane,
//! and mounts the [`Shell`](crate::shell::Shell) as the window root.

use super::lease::{DesktopHost, HostError, HostMode};
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{AppSnapshot, PersistenceRecovery, PersistentState, SessionState};
use crate::runtime::reads::{ReadPool, SessionReader};
use crate::runtime::{DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use backend_client::LocalSubscriptionTransport;
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use std::time::Duration;

const ATTEMPTS: usize = 12;
const RETRY: Duration = Duration::from_millis(120);
const DEADLINE: Duration = Duration::from_secs(20);
const EXIT_NO_SERVICE: u8 = 70;
const WINDOW: (f32, f32) = (1380.0, 880.0);
const MINIMUM: (f32, f32) = (320.0, 480.0);
/// Read sessions in the page-data pool; index/admin work has its own lane.
const READ_SESSIONS: usize = 3;

/// Starts the native application and its embedded local-first owner.
#[must_use]
pub fn main_entry() -> std::process::ExitCode {
    let opened = match open() {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("backend-desktop: {message}");
            return std::process::ExitCode::from(EXIT_NO_SERVICE);
        }
    };
    run(opened);
    std::process::ExitCode::SUCCESS
}

struct Opened {
    host: DesktopHost,
    snapshot: AppSnapshot,
    persistence: PersistentState,
    client: LocalEngineClient,
}

fn run(opened: Opened) {
    let Opened {
        host,
        snapshot,
        persistence,
        client,
    } = opened;
    let actor = match EngineActor::start(client, 32) {
        Ok(actor) => actor,
        Err(error) => {
            eprintln!("backend-desktop: start engine actor: {error}");
            return;
        }
    };
    let runtime = DesktopRuntime::new(snapshot, actor);
    let endpoint = host.endpoint().to_path_buf();
    let reads = match ReadPool::start(READ_SESSIONS, |_| SessionReader::connect(&endpoint)) {
        Ok(reads) => Some(reads),
        Err(error) => {
            // The window still opens; every page then says it has no read lane.
            eprintln!("backend-desktop: start read pool: {error}");
            None
        }
    };
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(facet::icons::Assets)
        .run(move |cx: &mut App| {
            if let Err(error) = install(cx) {
                eprintln!("backend-desktop: install UI assets: {error}");
                return;
            }
            let graph = UiEntityGraph::install_with_reads(cx, runtime, Some(persistence), reads);
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
            if let Err(error) = cx.open_window(options, move |window, cx| {
                let shell = crate::shell::open_shell(&graph, window, cx);
                // gpui_component::Root hosts the component layer the Ask
                // field's input engine (IME) expects; the shell is its view.
                cx.new(|cx| gpui_component::Root::new(shell, window, cx).bordered(false))
            }) {
                eprintln!("backend-desktop: open window: {error}");
            }
            cx.activate(true);
        });
    drop(host);
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

fn open() -> Result<Opened, String> {
    let started = std::time::Instant::now();
    let mut last = "the local service did not become ready".to_owned();
    for attempt in 0..ATTEMPTS {
        match attempt_once() {
            Ok(opened) => return Ok(opened),
            Err(message) => last = message,
        }
        if started.elapsed() >= DEADLINE {
            return Err(format!(
                "{last} (gave up after {}s)",
                started.elapsed().as_secs()
            ));
        }
        if attempt.saturating_add(1) < ATTEMPTS {
            std::thread::sleep(RETRY);
        }
    }
    Err(last)
}

fn attempt_once() -> Result<Opened, String> {
    let host = DesktopHost::start().map_err(|error| describe(&error))?;
    let host_project = LocalProjectId::from_path(host.project()).map_err(|error| {
        format!("the discovered workspace path cannot be represented safely: {error}")
    })?;
    let mut subscription = LocalSubscriptionTransport::connect(host.endpoint())
        .map_err(|error| format!("open the local subscription: {error}"))?;
    let (view, revision) = subscription
        .bootstrap_root()
        .map_err(|error| format!("hydrate the first snapshot: {error}"))?;
    if revision.root() != view.root() {
        return Err("the local service returned mismatched startup identities".to_owned());
    }
    let key = VersionedRoot::from_revision(1, revision, 0);
    let persistence = PersistentState::at(host.data().join("desktop-state.json"));
    let admitted = persistence.load_recovering().map_err(|error| {
        format!(
            "admit desktop state at {}: {error}",
            persistence.path().display()
        )
    })?;
    if let PersistenceRecovery::Preserved { backup, reason } = &admitted.recovery {
        eprintln!(
            "backend-desktop: preserved unadmitted state at {}: {reason}",
            backup.display()
        );
    }
    let persisted = admitted.state;
    let host_project_admitted = super::paths::looks_like_project(host.project());
    let (shelf, mut workspace) =
        persistence.cold_shelf(&persisted, host_project_admitted.then_some(host.project()));
    workspace.host = Some(host_project.clone());
    let mut settings = persistence.cold_settings(&persisted);
    settings.service_mode = match host.mode() {
        HostMode::Embedded => crate::model::ServiceMode::Embedded,
        HostMode::Attached => crate::model::ServiceMode::Attached,
    };
    let mut snapshot = AppSnapshot::empty(key)
        .with_shelf(shelf)
        .with_workspace(workspace)
        .with_settings(settings);
    let restored = persistence.cold_reload(&persisted);
    snapshot = snapshot.with_session(SessionState {
        route: restored.route,
        overlay: restored.overlay,
        back: restored.back,
        forward: restored.forward,
        selected: restored.selected,
    });
    let client = LocalEngineClient::new(host.endpoint(), host_project.clone());
    Ok(Opened {
        host,
        snapshot,
        persistence,
        client,
    })
}

fn describe(error: &HostError) -> String {
    format!("reach the local service at {}: {error}", error.operand())
}
