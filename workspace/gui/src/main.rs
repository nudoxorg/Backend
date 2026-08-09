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

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component_assets::Assets;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use lindsey::app::corpus::{self, CorpusChoice};
use lindsey::app::keymaps;
use lindsey::app::mcp::McpService;
use lindsey::highlight::TreeSitterHighlighter;
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::{PackageStore, SearchStore, SymbolStore};
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::shell::Shell;

use nudox_engine::runtime::{Engine, EngineConfig};
use nudox_engine::PackageSpec;

fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("lindsey=info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    lindsey::perf::init_from_env();

    // ── The engine (LR-9: it owns every runtime; lindsey links none) ─────────
    //
    // Started before the window so the corpus is loading while GPUI is still
    // creating its first frame. The GUI thread never awaits it — packages
    // arrive as `LoadEvent`s and the search index simply grows.
    // The engine owns *when* a code section is highlighted and the protocol
    // ordering around it; lindsey supplies *how*. That split is not taste — the
    // engine compiles into two workspaces whose graphs already have different,
    // mutually exclusive owners of the tree-sitter C library, so it cannot link
    // one itself. See `nudox_engine::highlight`.
    let config = EngineConfig {
        highlighter: Some(Arc::new(TreeSitterHighlighter)),
        ..EngineConfig::default()
    };

    // Which corpus? `app::corpus` resolves this from the environment and fails
    // loudly on anything ambiguous — see its module docs for why a silent
    // fallback to fixtures is the worst possible behaviour here.
    let choice = match corpus::from_env() {
        Ok(choice) => choice,
        Err(err) => {
            eprintln!("lindsey: {err}");
            std::process::exit(2);
        }
    };

    // `requested` seeds the status bar so the first frame can say
    // "loading axum…" rather than "no packages" for the thirty seconds
    // rust-analyzer needs. The engine has no "started" event, so this is the
    // only place that knowledge exists.
    let (engine, requested) = match choice {
        CorpusChoice::Fixtures => {
            tracing::info!("corpus: built-in fixtures");
            (Engine::start_with_fixtures(config), Vec::new())
        }
        CorpusChoice::Package(pkg) => {
            tracing::info!(
                package = %pkg.name,
                root = %pkg.root.display(),
                language = ?pkg.language,
                "corpus: live producer",
            );
            let requested = vec![pkg.name.clone()];
            let spec = PackageSpec {
                root: pkg.root,
                name: pkg.name,
                version: pkg.version,
                language: pkg.language,
            };
            (Engine::start_with_producer(config, vec![spec]), requested)
        }
    };
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

            // ── The hosted MCP server (GUI-LOCAL-PLAN §L6) ──────────────────
            //
            // "Started by lindsey after the engine, stopped on window close."
            // Until 2026-08-07 that sentence described an integration that did
            // not exist: the server was complete and tested and nothing ever
            // called it (LIMITATIONS.md L35).
            //
            // Started *here*, before the window, for the same reason the engine
            // is: the bind is a loopback `listen(2)` and completes in
            // microseconds, so an agent can attach as soon as the app is up,
            // and the very first frame already knows the port. A failure is not
            // fatal — `McpService::start` turns it into a visible
            // `McpStatus::Failed` in the status bar, because a documentation
            // browser that cannot host an agent endpoint is still a working
            // documentation browser.
            //
            // A GPUI global rather than shell state: §L6 is one endpoint per
            // process, and the server must outlive any particular window.
            cx.set_global(McpService::start(&engine));

            // Stop it on quit. GPUI runs quit handlers with a bounded timeout
            // before the process exits, which is the only hook that reliably
            // fires — `Drop` on a global is not guaranteed to run when AppKit
            // terminates the process. `McpHost::drop` still cancels the
            // listener as a backstop, so the socket closes either way; this
            // hook is what buys the clean drain of in-flight agent requests.
            cx.on_app_quit(|cx| {
                let outcome = cx.update_global::<McpService, _>(|service, _| service.stop());
                tracing::info!(?outcome, "mcp server stopped");
                async {}
            })
            .detach();

            // ── Keymap (Appendix B) ─────────────────────────────────────────
            //
            // `app::keymaps` is the single source of truth: the `?` cheat sheet
            // and the command palette both render from the same registry that
            // is handed to GPUI here, so a binding cannot exist without being
            // discoverable and cannot be documented without working.
            cx.bind_keys(keymaps::all_bindings());

            // ── Stores (LD-1: views subscribe to stores; stores own I/O) ────
            //
            // App-scoped, not window-scoped. A search query and a set of open
            // documents outlive any particular overlay or pane that shows them.
            let search = cx.new(|_| SearchStore::new(engine.clone()));
            let symbols = cx.new(|_| SymbolStore::new(engine.clone()));
            let packages =
                cx.new(|cx| PackageStore::new(engine.clone(), &requested, cx));

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        Shell::new(
                            search.clone(),
                            symbols.clone(),
                            packages.clone(),
                            window,
                            cx,
                        )
                    })
                },
            )
            .expect("failed to open window");

            cx.activate(true);
        });
}
