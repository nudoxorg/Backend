//! lindsey — the nudox GPUI shell.
//!
//! Boot order (GUI-PLAN §26): logging → theme → motion → engine → stores →
//! window. This file is deliberately thin: it installs globals and opens the
//! one window (LD-20), then hands every subsequent decision to the shell.

// `prelude::*` carries `VisualContext`, which is what provides `cx.new(...)`.
use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component_assets::Assets;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use lindsey::motion::tokens::MotionTokens;
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::shell::Shell;

fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("lindsey=info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // This gpui rev splits the platform out of the core crate: `Application`
    // has no `new()`, only `with_platform`. `gpui_platform::application()`
    // supplies the right platform for the target; its sibling `headless()` is
    // what the §25.3 perf harness will drive.
    gpui_platform::application()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            // gpui-component's theme must exist before ours: `NudoxThemeExt`
            // picks light or dark by asking `cx.theme().is_dark()` (LD-14 —
            // we layer over it rather than replacing it).
            gpui_component::init(cx);
            NudoxThemeExt::init(cx);

            // The motion global carries `MotionScale` and the loop-permit
            // census (§5.4). Every animation helper reads it, so it has to be
            // installed before the first frame renders.
            cx.set_global(MotionTokens::new(1.0));

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Shell::new(window, cx)),
            )
            .expect("failed to open window");

            cx.activate(true);
        });
}
