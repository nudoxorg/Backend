//! The machines a journey runs on.
//!
//! **Production** is `main`'s own launch, headless: the Finder-launch
//! workspace layout under one user root (`host::paths::ambient_paths`),
//! `host::launch::prepare` (the saved session restored, the owner started on
//! its own thread), `host::launch::start_workers` (the gated actor and read
//! pool), `UiEntityGraph::install_with_owner`, the shell, and the same quit
//! handlers `launch::open_the_window` registers. Nothing is pre-admitted and
//! no setting is injected: the facet is whatever the restored settings say.
//! Quitting drops the window (the quit handlers run) and then the owner's
//! thread (its host lets the workspace go), so a relaunch on the same root
//! is a real second launch.
//!
//! **Fixture** is the scenes' machine: the harness's fixture index with its
//! roots pre-admitted ([`super::super::boot`]). It is not an install.

use super::super::{Booted, ROOT, adapt, annotate, private_umask, quiet, sample_state};
use crate::host::launch::{self, Boot};
use crate::host::owner::OwnerThread;
use crate::runtime::UiEntityGraph;
use crate::runtime::owner::OwnerGate;
use backend_gui_harness::{Quiet, Session, SessionOptions, Viewport};
use facet::gallery::{self, SceneRoot};
use facet::probe;
use gpui::AppContext as _;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// The simulated frame period.
pub(super) const FRAME_MS: u64 = 16;

/// How long a production window may wait, in real time, for what is in
/// flight (reads, the owner answering) after one input instant.
const PRODUCTION_QUIET: Duration = Duration::from_secs(180);

/// A running production launch: what must outlive the window and be let go
/// after it.
pub(super) struct Launched {
    /// The owner's thread. Dropped after the window (a quit), it closes the
    /// gate and joins, and the host lets the workspace go.
    pub owner: Option<OwnerThread>,
    /// The owner's state as the window observes it.
    pub gate: OwnerGate,
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn options() -> SessionOptions {
    SessionOptions {
        asset_source: std::sync::Arc::new(facet::icons::Assets),
        frame_ms: FRAME_MS,
    }
}

/// Opens a production window over the user root `root` (created if absent,
/// owner-only), exactly as `main` launches from Finder.
///
/// # Errors
/// Fonts, the workspace paths, the engine actor, or GPUI failing.
pub(super) fn open_production(
    root: &Path,
    size: (u32, u32),
    scale: u8,
) -> Result<(Session, Launched), String> {
    facet::fonts::verify().map_err(err)?;
    private_umask();
    super::super::private_dir(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let paths = crate::host::paths::ambient_paths(&root)
        .map_err(|error| format!("workspace paths under {}: {error}", root.display()))?;
    paths
        .initialize()
        .map_err(|error| format!("initialize {}: {error}", root.display()))?;
    let mut boot = launch::prepare(Ok(paths), crate::host::owner::spawn);
    let owner = boot.owner.take();
    let reading = boot.keep.take();
    let Boot {
        snapshot,
        persistence,
        client,
        endpoint,
        gate,
        owner: _,
        keep: _,
        world_need,
    } = boot;
    if let Some(need) = world_need {
        crate::runtime::fixture_world::preload(need);
    }
    let (runtime, reads) =
        launch::start_workers(snapshot, client, endpoint, &gate).ok_or_else(|| {
            "the engine actor did not start (backend-desktop said why on stderr)".to_owned()
        })?;
    let viewport = Viewport::new(size.0, size.1, scale).map_err(err)?;
    let failure = Rc::new(RefCell::new(None::<String>));
    let window_gate = gate.clone();
    let mut session = Session::open(viewport, options(), {
        let failure = Rc::clone(&failure);
        move |window, cx| {
            // `gallery::bootstrap` is `launch::install` (component globals,
            // fonts) plus the probe and a frozen pulse; the facet it sets is
            // replaced by the product's own settings as the shell mounts.
            if let Err(error) = gallery::bootstrap(facet::Facet::default(), true, cx) {
                *failure.borrow_mut() = Some(error.0);
            }
            // `launch::open_the_window`'s quit handlers: release the workers
            // waiting on the owner, and save the route's pages.
            let quitting = window_gate.clone();
            cx.on_app_quit(move |_| {
                quitting.close();
                async {}
            })
            .detach();
            let keep = reading.map(launch::SnapshotRead::joined);
            let graph = UiEntityGraph::install_with_owner(
                cx,
                runtime,
                persistence,
                reads,
                Some(window_gate.clone()),
                keep,
            );
            let saved = graph.store.clone();
            cx.on_app_quit(move |cx| {
                if let Err(error) = saved.read(cx).save_now() {
                    eprintln!("backend-desktop: save the launch snapshot: {error}");
                }
                async {}
            })
            .detach();
            gallery::declare_quiet(quiet, cx);
            gallery::declare_adapter(adapt, cx);
            gallery::declare_annotator(annotate, cx);
            gallery::declare_state(sample_state, cx);
            let shell = ROOT(&graph, window, cx);
            cx.set_global(Booted {
                graph,
                fixture: None,
                gate: Some(window_gate),
                shell: shell.clone(),
                last: std::cell::Cell::new((crate::shell::RenderCounts::default(), 0)),
            });
            let root = cx.new(|cx| SceneRoot::new(shell.into(), cx));
            cx.new(|cx| gpui_component::Root::new(root, window, cx).bordered(false))
        }
    })
    .map_err(err)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    session.set_quiet(Some(Quiet {
        check: Box::new(quiet),
        deadline: PRODUCTION_QUIET,
    }));
    session.quiesce().map_err(err)?;
    session
        .update(|_, cx| {
            let _ = probe::take(cx);
        })
        .map_err(err)?;
    Ok((session, Launched { owner, gate }))
}

/// Quits a production launch: the app shuts down first (every quit handler
/// runs and the window closes), then the owner's thread goes (the host lets
/// the workspace go).
///
/// The shut-down app is not dropped: the owner watch task
/// (`runtime::owner::watch`) holds the root and the store for the app's
/// whole life and never ends, so dropping the app trips gpui's leak detector
/// on those two handles. A real process simply exits there; the harness
/// leaves the shut-down app to the end of its own process.
pub(super) fn quit(mut session: Session, launched: Launched) {
    // `shutdown` inside a window update: the window is out of the map while
    // it runs, so the update reports "window not found" once it is closed.
    let _ = session.update(|_, cx| cx.shutdown());
    std::mem::forget(session);
    launched.gate.close();
    drop(launched.owner);
}

/// Answers the native folder panel the product opened: the one substitution
/// a journey may make. `false`: no panel is open.
///
/// # Errors
/// The window is gone.
pub(super) fn answer_picker(
    session: &mut Session,
    chosen: Option<Vec<std::path::PathBuf>>,
) -> Result<bool, String> {
    session
        .update(|_, cx| {
            let Some(root) = cx
                .try_global::<Booted>()
                .map(|booted| booted.graph.root.clone())
            else {
                return false;
            };
            root.update(cx, |root, cx| root.answer_folder_picker(chosen, cx))
        })
        .map_err(err)
}
