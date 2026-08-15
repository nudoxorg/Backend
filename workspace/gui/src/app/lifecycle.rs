//! Background residency: the window is a *view of* the process, not the process
//! (GUI-LOCAL-PLAN §L6, docs/LIMITATIONS.md **L35**).
//!
//! # Scope: this is a macOS lifecycle
//!
//! The residency model this module implements — dismiss the window, keep the
//! process hosting the MCP endpoint, return via a dock click or menu item —
//! rests on macOS `QuitMode::Explicit`. On Linux and Windows the default is
//! `QuitMode::LastWindowClosed`, so closing the window ends the process and
//! there is no windowless state to return from. The platform facts live in
//! [`crate::platform`] (`supports_background_residency`, `has_global_menu`);
//! `refresh_menus` below installs the bar only where one exists. The rest of
//! this module documents the macOS state table it was written for.
//!
//! # The defect this closes
//!
//! `QuitMode::Default` resolves to `QuitMode::Explicit` on macOS
//! (`gpui/src/app.rs:1683-1685`), so lindsey has *always* survived its window
//! closing. That was never the gap. The gap was that there was no way back:
//! close the window and you had an invisible process holding a loopback MCP
//! endpoint, with no route to the UI, no indication it was alive, and no way to
//! quit it short of `kill`. Surviving without being reachable is worse than not
//! surviving — the endpoint an agent depends on is up, and the human who owns
//! the machine cannot see or stop it.
//!
//! Two facts made that unreachable state permanent rather than merely awkward,
//! and both are fixed here rather than worked around:
//!
//! * **Keystrokes arrive through a window.** GPUI dispatches keys down the
//!   focused element's ancestor chain, so with zero windows *no* keymap entry
//!   can fire. A key-bound "show the window again" is structurally impossible.
//!   The only input surface AppKit still routes with no windows open is the
//!   **menu bar** — which is why [`crate::app::menus`] exists and why the way
//!   back is a menu item and a dock click, not a shortcut.
//! * **lindsey installed no menu bar at all.** `cx.set_menus` had zero call
//!   sites, so there was no Quit item and `cmd-Q` did nothing: gpui binds no
//!   default quit anywhere (`gpui_macos::MacPlatform::quit` is only ever
//!   reached from an explicit `App::quit`). The brief for this change assumed
//!   `cmd-Q` already worked; it did not.
//!
//! # The lifecycle, state by state
//!
//! | State | Reached by | What is alive | Way out |
//! |---|---|---|---|
//! | [`Presence::Onscreen`] | launch, dock click, Window ▸ Show lindsey | window, engine, MCP endpoint | dismiss, hide, quit |
//! | [`Presence::Onscreen`] (app hidden) | `cmd-H` | everything, incl. the `NSWindow` | dock click, `cmd-Tab` |
//! | [`Presence::Dismissed`] | red ✕, Window ▸ Close Window | engine, MCP endpoint | dock click, Window ▸ Show lindsey |
//! | (gone) | `cmd-Q`, lindsey ▸ Quit | nothing; `on_app_quit` drains first | — |
//!
//! Hiding and dismissing are deliberately *different* gestures with different
//! costs, which is what every document-less macOS app (Messages, Music, Mail)
//! does: `cmd-H` keeps the whole window alive and is instant; the red ✕ tears
//! the window down and frees its renderer, and coming back rebuilds it.
//!
//! # Why dismiss destroys the window instead of hiding it
//!
//! There is no `PlatformWindow::set_visible` in this gpui rev — the trait
//! (`gpui/src/platform.rs:620-665`) offers `activate`, `minimize`, `zoom` and
//! `toggle_fullscreen` and nothing else. So "dismiss without destroying" has
//! exactly two candidate spellings, and both were rejected:
//!
//! * **Minimise.** A minimised window is still on screen — it is a tile in the
//!   Dock, not a dismissal — and it leaves the reader with a window they did
//!   not ask to keep.
//! * **Veto the close and hide the whole app** (`on_window_should_close`
//!   returning `false` plus `App::hide`). This makes the red ✕ lie: on macOS
//!   ✕ closes a window and `cmd-H` hides an app, and swapping them is the kind
//!   of surprise that costs a user their trust in every other control. It also
//!   makes the dock-click route depend on AppKit reporting
//!   `hasVisibleWindows: NO` for an app-hidden window, because gpui only
//!   forwards the reopen event when that flag is false
//!   (`gpui_macos/src/platform.rs:1217-1226`) — an assumption this change
//!   cannot test, and therefore must not rest on.
//!
//! Destroying is also the only option that is *self-verifying*: with zero
//! windows, "the endpoint survived dismissal" is a claim about a process that
//! demonstrably has no UI left, not about a window that is merely off-screen.
//! `tests/mcp_endpoint.rs` makes exactly that request.
//!
//! # Why the window can be rebuilt at all
//!
//! Because the stores are app-scoped and the window is a projection of them.
//! `main` constructs `SearchStore`, `SymbolStore` and `PackageStore` on the
//! `App`, not on the window, so a dismissed window loses no corpus, no open
//! document, and no search index — only the pixels. `Shell::new` rehydrates its
//! tab strip from `SymbolStore`, so rebuilding is idempotent rather than
//! amnesiac. Anything that later moves state *into* `Shell` and not a store
//! will silently start being lost across a dismiss, which is why
//! `tests/screenshots.rs` asserts the restored frame matches the dismissed one
//! rather than merely asserting a window exists.

use std::rc::Rc;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Application, BorrowAppContext as _, Bounds,
    ClipboardItem, Global, Pixels, WindowBounds,
};

use crate::app::account::AccountStatus;
use crate::app::actions::{CopyMcpEndpoint, DismissWindow, HideApp, Quit, ShowWindow};
use crate::app::mcp::McpStatus;
use crate::app::menus;

/// Register everything that has to keep working with **no window open**.
///
/// Called by `main` before the first window exists, because that is also the
/// state the app returns to on every dismissal — wiring it once, up front,
/// means the windowless path is never a special case that only production
/// exercises.
///
/// Every handler here is an *app-level* listener rather than an element
/// handler. `App::dispatch_action` routes to the global listeners whenever
/// there is no active window (`gpui/src/app.rs:2230-2240`), which is exactly
/// how a menu click still lands after the UI is gone.
///
/// # Why the three window handlers defer
///
/// A window is *checked out of* `App::windows` for the duration of any
/// `update_window`, and `Window::dispatch_action` runs its handlers inside
/// exactly such an update (`gpui/src/window.rs:1991-1999`). So when one of
/// these actions arrives with a window open — a keystroke, or a menu click
/// while the UI is visible — a nested `update_window` on that same window
/// fails with "window not found", and the dismissal silently does nothing.
///
/// That was not hypothetical: the screenshot suite caught it on the first run
/// with `DismissWindow must really destroy the window; presence is Onscreen`.
/// `cx.defer` runs the body on the next effect flush, by which point the window
/// is back in the slot map. `WindowSession`'s own methods stay synchronous and
/// keep returning an honest [`Presence`], because a caller that is *not* inside
/// a window update (every test, and every menu click with no window) deserves
/// an answer rather than a promise.
pub fn wire(cx: &mut App) {
    cx.on_action(|_: &ShowWindow, cx| {
        cx.defer(|cx| {
            WindowSession::show(cx);
        });
    });
    cx.on_action(|_: &DismissWindow, cx| {
        cx.defer(|cx| {
            WindowSession::dismiss(cx);
        });
    });
    cx.on_action(|_: &HideApp, cx| cx.defer(WindowSession::hide));
    cx.on_action(|_: &Quit, cx| {
        // `App::quit` goes through `[NSApp terminate:]`, which fires
        // `applicationWillTerminate:` → gpui's `on_quit` hook → `App::shutdown`
        // → every `on_app_quit` observer (`gpui/src/app.rs:821-828, 866-878`).
        // That chain is what drains in-flight agent requests; `kill(1)`, the
        // only way to stop lindsey before this change, skipped all of it.
        cx.quit();
    });
    cx.on_action(|_: &CopyMcpEndpoint, cx| {
        let Some(url) = McpStatus::from_app(cx).url().cloned() else {
            // Unreachable from the menu, which omits the item when there is no
            // address — but the action is also palette-reachable, and putting
            // an empty string on the reader's clipboard would be worse than
            // doing nothing.
            tracing::warn!("no MCP endpoint to copy");
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
    });

    refresh_menus(cx);
}

/// Register the dock-click route back.
///
/// Separate from [`wire`] because `on_reopen` is a method on
/// [`Application`], not on `App` (`gpui/src/app.rs:224`) — it has to be
/// installed *before* `Application::run` consumes the builder, so `main` cannot
/// do it from inside the run closure with everything else. Kept in this module
/// anyway so the whole answer to "how does the reader get back?" is one file.
///
/// gpui forwards `applicationShouldHandleReopen:hasVisibleWindows:` only when
/// AppKit reports no visible windows (`gpui_macos/src/platform.rs:1217-1226`),
/// which is exactly the dismissed state.
///
/// The body is one call on purpose. `TestPlatform::on_reopen` is an empty stub
/// (`gpui/src/platform/test/platform.rs:413`), so no test can ever simulate a
/// dock click; putting the work in [`WindowSession::show`] means the part that
/// *can* be tested is, and the part that cannot is a single line.
pub fn wire_reopen(app: &Application) {
    // The dock-click reopen route exists only where background residency does
    // (macOS): on Linux/Windows closing the window ends the process, so there
    // is no dismissed process for a dock click to summon. `on_reopen` is a
    // harmless no-op off macOS, but the guard makes the intent explicit rather
    // than relying on the platform to ignore it.
    if !crate::platform::supports_background_residency() {
        return;
    }
    app.on_reopen(|cx| {
        WindowSession::show(cx);
    });
}

/// Rebuild the menu bar from the live MCP and account status.
///
/// # When this runs, precisely
///
/// From [`wire`], once — which used to be the whole story, because
/// `McpHost::start` returns only once the listener is bound (see
/// `app::mcp`'s "Why there is no `Starting`"), so `main` installs the service
/// *before* wiring and its status is already final at that point.
///
/// The account half is not final at that point: sign-in and sign-out both
/// change [`AccountStatus`] while the process runs, and each is exactly the
/// kind of transition the doc comment below already anticipated — "a restart
/// control, a rebind on port conflict" — for the MCP half. `workspace::shell`
/// calls this again after both, so the menu-bar item never advertises a
/// session that has already ended (the same stale-value failure
/// [`McpStatus::url`] refuses to commit in the status bar, restated for
/// sign-in/out rather than for bind/rebind).
///
/// It is `pub` and separate from [`wire`] because `set_menus` replaces the
/// whole bar and every later transition has to call this rather than build a
/// second, competing menu somewhere else.
pub fn refresh_menus(cx: &mut App) {
    let mcp = McpStatus::from_app(cx);
    let account = AccountStatus::from_app(cx);

    // macOS: gpui's platform renders this list as the AppKit global menu bar
    // (the only UI a dismissed window still has).
    cx.set_menus(menus::main_menu(&mcp, &account));

    // Windows/Linux: the same definition is read back through
    // `GlobalState::app_menus()` by `gpui_component::menu::AppMenuBar`, which
    // draws an in-window bar. Populate it now so a window that mounts one sees
    // the menu. `Menu` is not `Clone`, and `owned()` consumes, so the (pure,
    // cheap) list is rebuilt rather than cloned — the same shape
    // gpui-component's own story example uses. See `platform::has_global_menu`
    // for which of the two renderings a platform uses.
    gpui_component::GlobalState::global_mut(cx).set_app_menus(
        menus::main_menu(&mcp, &account)
            .into_iter()
            .map(|menu| menu.owned())
            .collect(),
    );
}

/// Whether the reader currently has a window.
///
/// Derived from `App::windows()` at every call rather than tracked in a field
/// beside it (doctrine §8: a tally kept in parallel with the thing it counts can
/// drift from it; one computed from it cannot). The red ✕ removes a window
/// without going through this module at all, so a stored flag would have been
/// wrong from the first user gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// A window exists. It may be app-hidden (`cmd-H`) — from lindsey's side
    /// that is the same state, because AppKit unhides it on the next
    /// activation without lindsey building anything.
    Onscreen,
    /// No window exists, and the process is still running: engine resident, MCP
    /// endpoint listening. The Dock icon and the menu bar are the whole UI.
    Dismissed,
}

impl Presence {
    /// The live window, when there is one.
    ///
    /// LD-20 is one window per process, so "the first" and "the only" are the
    /// same handle. Returning the handle rather than a `bool` keeps the
    /// question ("is there a window?") and the answer ("this one") from being
    /// two lookups that can disagree.
    fn live(cx: &App) -> Option<AnyWindowHandle> {
        cx.windows().first().copied()
    }

    /// Read the current presence off the app.
    pub fn of(cx: &App) -> Self {
        match Self::live(cx) {
            Some(_) => Self::Onscreen,
            None => Self::Dismissed,
        }
    }

    /// Whether the process is running without a window — the state that used to
    /// be a dead end.
    pub fn is_dismissed(self) -> bool {
        matches!(self, Self::Dismissed)
    }
}

/// How to build lindsey's one window, plus where it last was.
///
/// # Why the constructor is stored instead of called from two places
///
/// Before this existed, `main` opened the window inline. A restore path added
/// beside it would be a *second* place that knows how to build a `Shell` — and
/// the first time the two drifted, restoring would produce a subtly different
/// window than launching, in a way no test would catch because each half works.
/// Storing the closure means there is exactly one way to make a window and both
/// the launch path and the reopen path take it.
///
/// The closure is type-erased (`AnyWindowHandle`, not `WindowHandle<Shell>`) so
/// this module never names `Shell`. `app::` sits *below* `workspace::` in this
/// crate; a lifecycle that imported the shell would invert that edge and make
/// the shell impossible to test without the lifecycle.
pub struct WindowSession {
    /// Where to put the window next time it is built.
    ///
    /// Refreshed from the live window on every dismissal — including the red ✕,
    /// via `on_window_should_close`, which is the only hook that runs *before*
    /// the platform window is gone. `App::on_window_closed` fires after
    /// `cx.windows.remove(id)` (`gpui/src/app.rs:1660-1680`), so by the time it
    /// runs there is nothing left to measure.
    bounds: Bounds<Pixels>,
    /// The one window constructor. `Rc` because it is called from a `&mut App`
    /// and therefore cannot be borrowed out of the global across the call.
    open: Rc<dyn Fn(Bounds<Pixels>, &mut App) -> anyhow::Result<AnyWindowHandle>>,
}

impl Global for WindowSession {}

impl WindowSession {
    /// Install the session. Does **not** open a window — call
    /// [`open_first`](Self::open_first).
    ///
    /// Separated so `main` can install actions, the menu bar and the reopen
    /// handler *before* the first window exists, which is also the exact state
    /// the app returns to on every dismissal. If the first window came up by a
    /// different route than every later one, the dismissed state would only
    /// ever be exercised in production.
    pub fn install(
        bounds: Bounds<Pixels>,
        open: impl Fn(Bounds<Pixels>, &mut App) -> anyhow::Result<AnyWindowHandle> + 'static,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            bounds,
            open: Rc::new(open),
        });
    }

    /// The bounds the next window will be built at.
    ///
    /// Exposed so a test can distinguish "the geometry was forgotten" from "the
    /// geometry was remembered and the rebuild ignored it" — two failures that
    /// look identical from the outside. The pixel comparison in
    /// `tests/screenshots.rs` (`28-restored`) is the end-to-end proof; this is
    /// what makes it diagnosable when it breaks.
    pub fn bounds(cx: &App) -> Option<Bounds<Pixels>> {
        cx.try_global::<Self>().map(|session| session.bounds)
    }

    /// Record where the live window is, so a rebuild lands in the same place.
    ///
    /// Idempotent and safe to call with no window: a dismissed session keeps the
    /// last geometry it knew, which is the whole point.
    ///
    /// Every `WindowBounds` variant carries the *windowed* geometry — a
    /// maximised or fullscreen window reports its restore size there — so all
    /// three are unwrapped to the same thing. The rebuilt window is always
    /// `Windowed`: coming back from a dock click into a fullscreen space the
    /// reader did not ask for would take over their display.
    pub fn remember(cx: &mut App) {
        let Some(handle) = Presence::live(cx) else {
            return;
        };
        let Ok(measured) = cx.update_window(handle, |_, window, _| window.window_bounds()) else {
            return;
        };
        let (WindowBounds::Windowed(bounds)
        | WindowBounds::Maximized(bounds)
        | WindowBounds::Fullscreen(bounds)) = measured;
        if cx.try_global::<Self>().is_some() {
            cx.update_global::<Self, _>(|session, _| session.bounds = bounds);
        }
    }

    /// Take the window away without stopping the process.
    ///
    /// Returns the presence afterwards, so a caller can assert on a value rather
    /// than on the absence of a panic. Calling it with no window is a no-op that
    /// still reports [`Presence::Dismissed`] — dismissing twice is something a
    /// menu item can genuinely do, and it must not be an error.
    pub fn dismiss(cx: &mut App) -> Presence {
        Self::remember(cx);
        if let Some(handle) = Presence::live(cx) {
            // `App::update_window` drops the window from `cx.windows` in its own
            // trailing block the moment the closure marks it removed
            // (`gpui/src/app.rs:1655-1690`), so the presence read below is
            // already the post-removal answer — no effect flush in between.
            let _ = cx.update_window(handle, |_, window, _| window.remove_window());
        }
        Presence::of(cx)
    }

    /// Put the window back and bring lindsey forward.
    ///
    /// This is the *only* way back, and both routes to it — the dock click via
    /// `App::on_reopen` and the `Window ▸ Show lindsey` menu item — call this
    /// same function. Keeping it a plain function rather than logic buried in
    /// the `on_reopen` closure is what makes it testable at all:
    /// `TestPlatform::on_reopen` is an empty stub
    /// (`gpui/src/platform/test/platform.rs:413`), so a test can never make a
    /// simulated dock click happen. It can call this.
    ///
    /// Idempotent in three ways that all really occur: with a window already up
    /// it just activates (a dock click on a visible app); with the app hidden,
    /// `App::activate` unhides it (`gpui_macos/src/platform.rs:564-569` →
    /// `activateIgnoringOtherApps:`); with no window it builds one.
    pub fn show(cx: &mut App) -> Presence {
        if let Some(handle) = Presence::live(cx) {
            let _ = cx.update_window(handle, |_, window, _| window.activate_window());
            cx.activate(true);
            return Presence::Onscreen;
        }

        let Some(session) = cx.try_global::<Self>() else {
            // No session installed: every `#[gpui::test]` app, and any embedder
            // that drives lindsey's views without `main`. Nothing to build, and
            // inventing a window here would open one such a caller never asked
            // for.
            return Presence::of(cx);
        };
        let bounds = session.bounds;
        let open = Rc::clone(&session.open);

        match open(bounds, cx) {
            Ok(handle) => {
                Self::watch(handle, cx);
                cx.activate(true);
            }
            Err(error) => {
                // Not fatal and not silent. The process keeps hosting the
                // endpoint — which is the thing an agent depends on — and the
                // reason reaches the log rather than being swallowed into a
                // `Presence::Dismissed` that looks like the reader simply never
                // asked for a window.
                tracing::error!(%error, "could not rebuild the window");
            }
        }
        Presence::of(cx)
    }

    /// Hide the whole application, keeping the window intact (`cmd-H`).
    ///
    /// Distinct from [`dismiss`](Self::dismiss) on purpose: this is the cheap,
    /// lossless dismissal macOS users already know, and coming back from it
    /// costs nothing because nothing was torn down.
    pub fn hide(cx: &mut App) {
        // Record first: an app-hidden window still reports its bounds, and
        // hiding is frequently followed by quitting.
        Self::remember(cx);
        cx.hide();
    }

    /// Attach the pre-close hook to a freshly built window.
    ///
    /// The red ✕ and `NSWindow`'s own close routine never pass through
    /// [`dismiss`](Self::dismiss), so without this the geometry of every
    /// user-initiated close would be lost and the window would come back at
    /// wherever it was last *programmatically* dismissed. Registered here, in
    /// the one place windows are built, rather than in the constructor closure
    /// — a caller who forgot would produce a bug that only shows up two
    /// gestures later.
    fn watch(handle: AnyWindowHandle, cx: &mut App) {
        let _ = cx.update_window(handle, |_, window, cx| {
            window.on_window_should_close(cx, |window, cx| {
                let (WindowBounds::Windowed(bounds)
                | WindowBounds::Maximized(bounds)
                | WindowBounds::Fullscreen(bounds)) = window.window_bounds();
                if cx.try_global::<Self>().is_some() {
                    cx.update_global::<Self, _>(|session, _| session.bounds = bounds);
                }
                // Always allow the close. Vetoing here is what would turn the
                // red ✕ into a hide, which the module docs reject.
                true
            });
        });
    }

    /// Open the first window, wiring it the same way a restored one is wired.
    ///
    /// `main` calls this instead of `cx.open_window` so that launch and restore
    /// cannot diverge.
    pub fn open_first(cx: &mut App) -> Presence {
        Self::show(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, div, point, px, size};

    /// A view with no content: these tests are about window *existence*, and a
    /// real `Shell` would drag the whole engine in to prove nothing extra.
    struct Bare;

    impl gpui::Render for Bare {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            div()
        }
    }

    fn install(cx: &mut TestAppContext, bounds: Bounds<Pixels>) {
        cx.update(|cx| {
            WindowSession::install(
                bounds,
                |bounds, cx| {
                    cx.open_window(
                        gpui::WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        |_, cx| cx.new(|_| Bare),
                    )
                    .map(Into::into)
                },
                cx,
            );
        });
    }

    fn bounds_at(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    /// The defect, stated as an invariant: dismissing must be reversible.
    ///
    /// Asserts on [`Presence`] rather than on `cx.windows().len()` so the test
    /// fails against the vocabulary the rest of the app reads.
    #[gpui::test]
    async fn a_dismissed_window_can_be_summoned_back(cx: &mut TestAppContext) {
        install(cx, bounds_at(120.0, 80.0, 1440.0, 900.0));

        cx.update(|cx| {
            assert_eq!(
                Presence::of(cx),
                Presence::Dismissed,
                "installing a session must not open a window on its own",
            );
            assert_eq!(WindowSession::open_first(cx), Presence::Onscreen);
            assert_eq!(WindowSession::dismiss(cx), Presence::Dismissed);
            assert!(
                cx.windows().is_empty(),
                "dismissal must really destroy the window, not merely hide it",
            );
            assert_eq!(
                WindowSession::show(cx),
                Presence::Onscreen,
                "a dismissed lindsey must be able to build a window again",
            );
        });
    }

    /// Requirement 5: geometry survives the round trip.
    ///
    /// Moved *after* the window is open, so the value under test is one read
    /// back off a live window rather than the one handed to `install`.
    #[gpui::test]
    async fn a_restored_window_reopens_where_it_was_dismissed(cx: &mut TestAppContext) {
        // Opened somewhere non-default, then resized: the value under test is
        // one read back off a *live* window, not the one handed to `install`,
        // which a session that never measured anything would also return.
        let opened_at = bounds_at(310.0, 210.0, 1440.0, 900.0);
        let resized_to = size(px(1024.0), px(768.0));
        install(cx, opened_at);

        cx.update(|cx| {
            WindowSession::open_first(cx);
            let handle = cx.windows()[0];
            cx.update_window(handle, |_, window, _| window.resize(resized_to))
                .expect("resize the live window");
        });

        cx.update(|cx| {
            WindowSession::dismiss(cx);
            assert_eq!(
                WindowSession::bounds(cx),
                Some(Bounds {
                    origin: opened_at.origin,
                    size: resized_to,
                }),
                "the geometry the reader left the window at must be what a rebuild uses",
            );

            WindowSession::show(cx);
            let handle = cx.windows()[0];
            let rebuilt = cx
                .update_window(handle, |_, window, _| window.window_bounds())
                .expect("read the rebuilt window");
            assert_eq!(
                rebuilt,
                WindowBounds::Windowed(Bounds {
                    origin: opened_at.origin,
                    size: resized_to,
                }),
                "the rebuilt window must land where the dismissed one was",
            );
        });
    }

    /// Summoning an app that already has a window must not build a second one.
    ///
    /// This is a real gesture — clicking the dock icon of a visible app — and
    /// LD-20 is one window per process, so the failure it guards against is a
    /// duplicate window rather than a missing one.
    #[gpui::test]
    async fn summoning_a_visible_lindsey_activates_rather_than_duplicates(cx: &mut TestAppContext) {
        install(cx, bounds_at(0.0, 0.0, 800.0, 600.0));

        cx.update(|cx| {
            WindowSession::open_first(cx);
            assert_eq!(cx.windows().len(), 1);
            WindowSession::show(cx);
            WindowSession::show(cx);
            assert_eq!(
                cx.windows().len(),
                1,
                "repeated summons must activate the one window, not stack windows",
            );
        });
    }

    /// Every `#[gpui::test]` app and every embedder that drives lindsey's views
    /// without `main` is in this state. `show` must be inert there rather than
    /// conjuring a window nobody asked for.
    #[gpui::test]
    async fn without_a_session_summoning_does_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            assert_eq!(Presence::of(cx), Presence::Dismissed);
            assert_eq!(WindowSession::show(cx), Presence::Dismissed);
            assert!(cx.windows().is_empty());
            assert_eq!(WindowSession::bounds(cx), None);
        });
    }
}
