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
use gpui_component::Root;
use gpui_component_assets::Assets;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use lindsey::app::corpus::{self, CorpusChoice};
use lindsey::app::keymaps;
use lindsey::app::lifecycle::{self, WindowSession};
use lindsey::app::account::AccountService;
use lindsey::app::mcp::McpService;
use lindsey::highlight::TreeSitterHighlighter;
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::index_jobs::IndexJobStore;
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
    //
    // The embedder is the *second* capability port, wired the same way (§1
    // "Capability ports"): `nudox-embed` sits beside the engine and speaks only
    // the port's vocabulary, so this line makes semantic search real without
    // `lindsey` ever naming `registry` or an IR type. `load_from_env` returns
    // `None` — the honest `Unavailable(NoEmbedder)` state — when this build has
    // no ONNX runtime (the default) or no model directory is configured; it
    // returns a live, lazily-loaded embedder when `NUDOX_EMBED_MODEL_DIR` points
    // at the pinned model in a build made with `--features onnx`. Handing the
    // engine an embedder is *all* it takes: the incremental indexer, the
    // `SectionState::{Building,Complete}` progress, and the ranked rows are
    // already built behind the port. See docs/LIMITATIONS.md L41.
    let embedder = nudox_embed::load_from_env();
    tracing::info!(
        semantic = embedder.is_some(),
        "engine config: semantic embedder {}",
        if embedder.is_some() { "installed" } else { "not configured" }
    );
    let config = EngineConfig {
        highlighter: Some(Arc::new(TreeSitterHighlighter)),
        embedder,
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
    let app = gpui_platform::application().with_assets(Assets);

    // The dock-click route back (`app::lifecycle`). Registered on the
    // `Application` rather than inside `run`, because `on_reopen` lives on the
    // builder and `run` consumes it.
    lifecycle::wire_reopen(&app);

    app.run(move |cx: &mut App| {
            // gpui-component's globals must exist before ours, because
            // `ThemeRegistry::init` writes *into* them — the palette is now
            // upstream of both theme globals rather than a reaction to one.
            //
            // This comment used to read "`NudoxThemeExt` picks light or dark by
            // asking `cx.theme().is_dark()`". That was accurate, and it was the
            // defect: `ThemeMode::default()` is `Light` and nothing here chose,
            // so the application's appearance was a property of the host
            // machine, and the set of themes it could ever have was two,
            // because the input was a boolean.
            gpui_component::init(cx);
            NudoxThemeExt::init(cx).expect("bundled themes parse and install");

            // The motion global carries `MotionScale` and the loop-permit
            // census (§5.4). Every animation helper reads it, so it must be
            // installed before the first frame renders.
            cx.set_global(MotionTokens::new(1.0));

            // ── The hosted MCP server (GUI-LOCAL-PLAN §L6) ──────────────────
            //
            // "Started by lindsey after the engine, stopped on window close."
            // Until 2026-08-07 that sentence described an integration that did
            // not exist: the server was complete and tested and nothing ever
            // called it (docs/LIMITATIONS.md L35).
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
            // The account gate is installed *before* the MCP server, because
            // the server takes it as a mandatory constructor argument: a
            // `NudoxMcpServer` with no gate would serve a paid product for
            // free, and `nudox-mcp` makes that unrepresentable rather than
            // discouraged. One gate, two readers — the server admits tool calls
            // through it and the status bar renders it. See `docs/auth.md`.
            cx.set_global(AccountService::start(&engine));
            let gate = cx.global::<AccountService>().gate();

            cx.set_global(McpService::start(&engine, gate));

            // Flush unreported usage on quit, for the same reason the MCP
            // server drains: the user's last few tool calls are billable, and
            // stranding them in a file that the *next* launch has to reconcile
            // works but bills late for no reason.
            cx.on_app_quit(|cx| {
                cx.update_global::<AccountService, _>(|service, _| service.stop());
                async {}
            })
            .detach();

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
            // Starts empty and stays empty until someone types a package URL
            // into ⌘K. That is the honest initial state: nothing has been
            // requested, so nothing is running.
            let index_jobs = cx.new(|_| IndexJobStore::new(engine.clone()));

            // ── The window, as a thing that can come back (§L6) ─────────────
            //
            // `QuitMode::Default` is `Explicit` on macOS
            // (`gpui/src/app.rs:1683-1685`), so lindsey has always survived its
            // window closing — and until now that was a *defect*, because there
            // was no route back and no menu bar to quit from. `app::lifecycle`
            // is that route; see its module docs for the state table and for
            // why dismissal destroys the window rather than hiding it.
            //
            // The window constructor is handed to the session rather than
            // called here, so launching and restoring are literally the same
            // code path. A second `cx.open_window` in this file would be a
            // second definition of "lindsey's window" that could drift from
            // this one without any test noticing.
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            WindowSession::install(
                bounds,
                move |bounds, cx| {
                    let search = search.clone();
                    let symbols = symbols.clone();
                    let packages = packages.clone();
                    let index_jobs = index_jobs.clone();
                    cx.open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                            ..Default::default()
                        },
                        |window, cx| {
                            let shell = cx.new(|cx| {
                                Shell::new(search, symbols, packages, index_jobs, window, cx)
                            });
                            // `gpui_component::input::Input` (used by
                            // `SignInView`) requires the window's first layer to
                            // be a `Root` — its own paint path calls
                            // `Root::read`/`Root::update` unconditionally, to
                            // track which `InputState` in the whole window is
                            // focused. `Shell` itself is still the actual
                            // content; `Root` is a thin wrapper every window in
                            // this app must carry now, not a second UI.
                            cx.new(|cx| Root::new(shell, window, cx))
                        },
                    )
                    .map(Into::into)
                },
                cx,
            );

            // Global actions, the dock-click hook, and the menu bar — all of
            // which must work with zero windows. Installed *before* the first
            // window so the windowless state is never a special case.
            lifecycle::wire(cx);

            WindowSession::open_first(cx);
            cx.activate(true);
        });
}
