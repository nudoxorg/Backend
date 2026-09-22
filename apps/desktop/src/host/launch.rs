//! Native startup for the v3 desktop.
//!
//! Startup chooses the workspace owner, admits one producer root, restores
//! durable shelf/session state, and then hands one `UiRootEntity` to GPUI.
//! No legacy model or transport entity is created on this path.

use super::lease::{DesktopHost, HostError, HostMode};
use crate::core::VersionedRoot;
use crate::model::{AppSnapshot, PersistenceRecovery, PersistentState, SessionState};
use crate::runtime::{DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use crate::theme::Theme;
use backend_client::LocalSubscriptionTransport;
use gpui::{
    point, px, size, App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};
use std::sync::Arc;
use std::time::Duration;

const ATTEMPTS: usize = 12;
const RETRY: Duration = Duration::from_millis(120);
const DEADLINE: Duration = Duration::from_secs(20);
const EXIT_NO_SERVICE: u8 = 70;
const WINDOW: (f32, f32) = (1380.0, 880.0);
const MINIMUM: (f32, f32) = (640.0, 480.0);

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
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(crate::ui::icon::Assets)
        .run(move |cx: &mut App| {
            if let Err(error) = install(cx) {
                eprintln!("backend-desktop: install UI assets: {error}");
                return;
            }
            let graph = UiEntityGraph::install(cx, runtime, Some(persistence));
            let root = graph.root.clone();
            let options = window_options(cx);
            if let Err(error) = cx.open_window(options, move |window, cx| {
                root.update(cx, |root, cx| {
                    root.observe_window_activation(window, cx);
                });
                // gpui_component::Root is the native key/focus boundary. It
                // installs Tab/Shift-Tab dispatch and lets FocusTrapElement
                // contain modal traversal while UiRootEntity remains the
                // product state owner below it.
                cx.new(|cx| gpui_component::Root::new(root, window, cx).bordered(false))
            }) {
                eprintln!("backend-desktop: open window: {error}");
            }
            cx.activate(true);
        });
    drop(host);
}

fn install(cx: &mut App) -> gpui::Result<()> {
    gpui_component::init(cx);
    crate::theme::fonts::install(cx)?;
    let theme = Theme::default();
    crate::theme::sync_components(cx, &theme);
    cx.set_global(theme);
    Ok(())
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
    let mut subscription = LocalSubscriptionTransport::connect(host.endpoint())
        .map_err(|error| format!("open the local subscription: {error}"))?;
    let (view, _) = subscription
        .bootstrap_root()
        .map_err(|error| format!("hydrate the first snapshot: {error}"))?;
    let key = VersionedRoot::new(view.root(), 1)
        .with_generation(0)
        .observed_at(0);
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
    let host_path: Arc<str> = host.project().to_string_lossy().into_owned().into();
    workspace.host = Some(host_path);
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
    let client = LocalEngineClient::new(
        host.endpoint(),
        host.project().to_string_lossy().into_owned(),
    );
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
