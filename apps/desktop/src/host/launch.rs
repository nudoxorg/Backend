//! Process entry: reach the owner, hydrate one root, and open one window.
//! Preferences and the theme are installed before the first frame is drawn.
//! Nothing arrives as a late overlay; the window's geometry is final at once.
//!
//! Startup is deliberately sequential and deliberately loud. If the service
//! cannot be reached, or the first snapshot cannot be admitted, the process
//! says exactly which of those failed and exits with a distinct code rather
//! than opening an empty window that a reader would have to interrogate.

use super::lease::{DesktopHost, HostError};
use crate::reducer::model::Model;
use crate::store::prefs::{self, Preferences};
use crate::store::service::Endpoint;
use crate::theme::Theme;
use crate::transport::unix::UnixSubscriptionTransport;
use crate::ui::icon::Assets;
use crate::views::actions::{FIELD_CONTEXT, editing_bindings, window_bindings};
use crate::views::workspace::Workspace;
use backend_library::{Cursor, ViewRoot};
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use std::time::Duration;

/// How many times the whole bootstrap is retried before giving up.
const ATTEMPTS: usize = 12;

/// How long to wait between bootstrap attempts.
const RETRY: Duration = Duration::from_millis(120);

/// Exit code used when the local service could never be reached.
const EXIT_NO_SERVICE: u8 = 70;

/// Window size on a first run.
const WINDOW: (f32, f32) = (1380.0, 880.0);

/// Smallest window this layout supports.
const MINIMUM: (f32, f32) = (900.0, 600.0);

/// Starts the native desktop application and its local owner.
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

/// Everything the first frame needs, gathered before the platform starts.
struct Opened {
    host: DesktopHost,
    root: ViewRoot,
    cursor: Cursor,
    transport: UnixSubscriptionTransport,
}

fn run(opened: Opened) {
    let Opened {
        host,
        root,
        cursor,
        transport,
    } = opened;
    let Ok(model) = Model::try_new_at(root.clone(), cursor, root.basis().root) else {
        eprintln!("backend-desktop: the first snapshot was not coherent");
        return;
    };
    let endpoint = Endpoint::new(host.endpoint());
    let project = host.project().to_path_buf();
    let data = host.data().to_path_buf();
    let mode = host.mode();
    let prefs = prefs::load(&data);
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            install(cx, prefs);
            let options = window_options(cx);
            let opened = cx.open_window(options, move |_window, cx| {
                cx.new(|cx| {
                    Workspace::new(
                        endpoint, project, data, mode, root, model, transport, prefs, cx,
                    )
                })
            });
            if let Err(error) = opened {
                eprintln!("backend-desktop: open window: {error}");
            }
            cx.activate(true);
        });
    drop(host);
}

fn install(cx: &mut App, preferences: Preferences) {
    cx.bind_keys(editing_bindings().as_keybindings(Some(FIELD_CONTEXT)));
    cx.bind_keys(window_bindings());
    cx.set_global(Theme::new(
        preferences.appearance(),
        preferences.interface(),
        preferences.reduced_motion(),
    ));
    cx.set_reduce_motion(preferences.reduced_motion());
}

fn window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(WINDOW.0), px(WINDOW.1)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Nudox".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.0), px(14.0))),
        }),
        window_min_size: Some(size(px(MINIMUM.0), px(MINIMUM.1))),
        app_id: Some("dev.nudox.desktop".to_owned()),
        ..WindowOptions::default()
    }
}

/// Reaches the owner and hydrates one admitted root, retrying briefly.
fn open() -> Result<Opened, String> {
    let mut last = "the local service did not become ready".to_owned();
    for attempt in 0..ATTEMPTS {
        match attempt_once() {
            Ok(opened) => return Ok(opened),
            Err(message) => last = message,
        }
        if attempt.saturating_add(1) < ATTEMPTS {
            std::thread::sleep(RETRY);
        }
    }
    Err(last)
}

fn attempt_once() -> Result<Opened, String> {
    let host = DesktopHost::start().map_err(describe)?;
    let mut transport = UnixSubscriptionTransport::connect(host.endpoint())
        .map_err(|error| format!("open the subscription transport: {error}"))?;
    match transport.bootstrap_root() {
        Ok((root, cursor)) => Ok(Opened {
            host,
            root,
            cursor,
            transport,
        }),
        Err(error) => Err(format!("hydrate the first snapshot: {error}")),
    }
}

fn describe(error: HostError) -> String {
    format!("reach the local service at {}: {error}", error.operand())
}
