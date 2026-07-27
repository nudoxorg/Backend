//! lindsey — the nudox GPUI shell.
//!
//! Boot order (GUI-PLAN §26): logging → theme → motion → engine → stores →
//! window. This file is deliberately thin: it installs globals, starts the
//! engine, constructs the stores, and opens the one window (LD-20). Every
//! subsequent decision belongs to the shell.
//!
//! # The engine starts before the window
//!
//! `Engine::start` spawns the corpus load immediately, so by the time the first
//! frame paints the fixture packages are already arriving. That ordering is
//! LR-10 in miniature: local data is not something we wait for, it is something
//! that is already there.

use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component_assets::Assets;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::search::SearchStore;
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::shell::Shell;

use nudox_engine::runtime::{Engine, EngineConfig};

fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("lindsey=info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // ── The engine (LR-9: it owns every runtime; lindsey links none) ─────────
    //
    // Started before the window so the corpus is loading while GPUI is still
    // creating its first frame. The GUI thread never awaits it — packages
    // arrive as `LoadEvent`s and the search index simply grows.
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    tracing::info!("engine started");

    // This gpui rev splits the platform out of the core crate: `Application`
    // has no `new()`, only `with_platform`. `gpui_platform::application()`
    // supplies the right platform for the target; its sibling `headless()` is
    // what the §25.3 perf harness will drive.
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            // gpui-component's theme must exist before ours: `NudoxThemeExt`
            // picks light or dark by asking `cx.theme().is_dark()` (LD-14 — we
            // layer over it rather than replacing it).
            gpui_component::init(cx);
            NudoxThemeExt::init(cx);

            // The motion global carries `MotionScale` and the loop-permit
            // census (§5.4). Every animation helper reads it, so it must be
            // installed before the first frame renders.
            cx.set_global(MotionTokens::new(1.0));

            // ── Stores (LD-1: views subscribe to stores; stores own I/O) ────
            //
            // TODO(shell): hand this to the shell so `cmd-K` can open the
            // omni-search overlay over it. Constructed here because store
            // lifetime is app-scoped, not window-scoped.
            let _search = cx.new(|_| SearchStore::new(engine.clone()));

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
