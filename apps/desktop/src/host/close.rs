//! Graceful close runs while GPUI is still live, before its shutdown deadline.
//! Only an in-flight checkpoint task owns the graph; all native observers are weak.

use crate::runtime::{UiEntityGraph, UiRootEntity};
use crate::runtime::store::DataStore;
use gpui::{App, AppContext as _, AnyWindowHandle, Context, Entity, WeakEntity, Task,
    Render, Window, IntoElement, ParentElement as _, Styled as _, InteractiveElement as _, div, px};
use gpui::prelude::FluentBuilder as _;
use gpui_component::button::Button;
use gpui_component::FocusTrapElement as _;
use std::time::Duration;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseTarget { Window, Application }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attempt(u64);
#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase { Open, Saving(Attempt), NeedsDecision(Attempt, String), Committed }

pub(crate) struct GracefulClose {
    root: WeakEntity<UiRootEntity>,
    store: WeakEntity<DataStore>,
    window: Option<AnyWindowHandle>,
    focus: gpui::FocusHandle,
    previous_focus: Option<gpui::WeakFocusHandle>,
    phase: Phase,
    next: u64,
    target: CloseTarget,
    native_pending: bool,
    task: Option<Task<()>>,
    deadline: Duration,
    approved: Rc<Cell<bool>>,
}

impl GracefulClose {
    pub(crate) fn install(graph: &UiEntityGraph, cx: &mut App) -> Entity<Self> {
        let approved = Rc::new(Cell::new(false));
        let approval = approved.clone();
        let close = cx.new(|cx| Self { root: graph.root.downgrade(), store: graph.store.downgrade(),
            window: None, focus: cx.focus_handle(), previous_focus: None, phase: Phase::Open, next: 0, target: CloseTarget::Window,
            native_pending: false, task: None, deadline: Duration::from_secs(5), approved: approval });
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        let quit = close.downgrade();
        cx.on_action(move |_: &super::menus::Quit, cx| {
            let quit = quit.clone();
            cx.defer(move |cx| { let _ = quit.update(cx, |close, cx| close.request(CloseTarget::Application, cx)); });
        });
        let window_close = close.downgrade();
        cx.on_action(move |_: &super::menus::CloseWindow, cx| {
            let window_close = window_close.clone();
            cx.defer(move |cx| { let _ = window_close.update(cx, |close, cx| close.request(CloseTarget::Window, cx)); });
        });
        let native = close.downgrade();
        cx.on_app_should_quit(move |cx| {
            native.update(cx, |close, cx| {
                if close.phase == Phase::Committed { return true; }
                close.native_pending = true;
                close.request(CloseTarget::Application, cx);
                false
            }).unwrap_or(true)
        });
        let last = close.downgrade();
        cx.on_window_closed(move |cx, _| {
            if cx.windows().is_empty() {
                if approved.get() { cx.quit(); }
                else { let _ = last.update(cx, |close, cx| close.request(CloseTarget::Application, cx)); }
            }
        }).detach();
        close
    }

    pub(crate) fn attach(close: &Entity<Self>, window: &mut Window, cx: &mut App) {
        close.update(cx, |close, _| close.window = Some(window.window_handle()));
        let weak = close.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let Some(close) = weak.upgrade() else { return true; };
            if close.read(cx).phase == Phase::Committed { return true; }
            let weak = weak.clone();
            // Native callbacks currently borrow this window. Defer the save
            // admission/focus change until GPUI has put the window back.
            cx.defer(move |cx| { let _ = weak.update(cx, |close, cx| close.request(CloseTarget::Window, cx)); });
            false
        });
    }

    fn request(&mut self, target: CloseTarget, cx: &mut Context<Self>) {
        if self.phase == Phase::Committed { return; }
        if target == CloseTarget::Application { self.target = target; }
        if matches!(self.phase, Phase::Saving(_) | Phase::NeedsDecision(_, _)) { return; }
        self.target = target;
        let Some(next) = self.next.checked_add(1) else { return; };
        self.next = next;
        let attempt = Attempt(next);
        let Some(root) = self.root.upgrade() else { self.commit(cx); return; };
        let store = self.store.upgrade();
        if let Some(window) = self.window {
            let focus = self.focus.clone();
            let previous = window.update(cx, |_, window, cx| {
                let previous = window.focused(cx).map(|focus| focus.downgrade());
                window.focus(&focus, cx);
                previous
            }).ok().flatten();
            if self.previous_focus.is_none() { self.previous_focus = previous; }
        }
        let checkpoint = root.update(cx, |root, cx| root.begin_close(cx));
        let (saved, pages) = match checkpoint {
            Ok(checkpoint) => checkpoint,
            Err(error) => { self.phase = Phase::NeedsDecision(attempt, error.message.to_string()); cx.notify(); return; }
        };
        self.phase = Phase::Saving(attempt);
        cx.notify();
        let timer = cx.background_executor().timer(self.deadline);
        self.task = Some(cx.spawn(async move |this, cx| {
            // Cancel drops this foreground future immediately. A filesystem
            // operation already running owns only its immutable packet.
            let retained = (root, store);
            let result = {
            let work = async {
                saved.recv().await.map_err(|_| "The local writer closed before acknowledging the latest state.".to_owned())?
                    .map_err(|error| error.message.to_string())?;
                if let Some(pages) = pages {
                    cx.background_spawn(async move { pages.write("graceful close").map_err(|error| error.to_string()) }).await?;
                }
                Ok::<(), String>(())
            };
            use std::future::Future as _;
            let mut work = std::pin::pin!(work);
            let mut timer = std::pin::pin!(timer);
            std::future::poll_fn(|cx| {
                if let std::task::Poll::Ready(result) = work.as_mut().poll(cx) { return std::task::Poll::Ready(result); }
                if timer.as_mut().poll(cx).is_ready() {
                    return std::task::Poll::Ready(Err("Saving did not finish within five seconds. Previously saved state remains available.".into()));
                }
                std::task::Poll::Pending
            }).await
            };
            let _ = this.update(cx, |close, cx| {
                if close.phase != Phase::Saving(attempt) { return; }
                close.task = None;
                match result {
                    Ok(()) => close.commit(cx),
                    Err(error) => { close.phase = Phase::NeedsDecision(attempt, error); cx.notify(); }
                }
            });
            drop(retained);
        }));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Committed { return; }
        self.task = None;
        self.phase = Phase::Open;
        self.target = CloseTarget::Window;
        let _ = self.root.update(cx, |root, cx| root.cancel_close(cx));
        if std::mem::take(&mut self.native_pending) { cx.reply_to_app_quit(false); }
        if let Some(previous) = self.previous_focus.take().and_then(|focus| focus.upgrade()) {
            if let Some(window) = self.window {
                cx.defer(move |cx| { let _ = window.update(cx, |_, window, cx| window.focus(&previous, cx)); });
            }
        }
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let target = self.target;
        self.task = None;
        self.phase = Phase::Open;
        let _ = self.root.update(cx, |root, cx| root.cancel_close(cx));
        self.request(target, cx);
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Committed;
        self.approved.set(true);
        let _ = self.root.update(cx, |root, cx| root.commit_close(cx));
        cx.notify();
        if self.target == CloseTarget::Application {
            if std::mem::take(&mut self.native_pending) { cx.reply_to_app_quit(true); }
            else { cx.quit(); }
        } else if let Some(window) = self.window {
            cx.defer(move |cx| { let _ = window.update(cx, |_, window, _| window.remove_window()); });
        }
    }
}

/// The ordinary component Root remains the first window view; this child
/// blocks content input while keeping close decisions responsive and accessible.
pub(crate) struct CloseView {
    shell: Entity<crate::shell::Shell>,
    close: Entity<GracefulClose>,
}
impl CloseView {
    pub(crate) fn shell(&self) -> Entity<crate::shell::Shell> { self.shell.clone() }
    pub(crate) fn new(shell: Entity<crate::shell::Shell>, close: Entity<GracefulClose>, cx: &mut Context<Self>) -> Self {
        cx.observe(&close, |_, _, cx| cx.notify()).detach();
        Self { shell, close }
    }
}
impl Render for CloseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.close.read(cx).phase.clone();
        let focus = self.close.read(cx).focus.clone();
        let escape = self.close.downgrade();
        let cancel = self.close.downgrade();
        let retry = self.close.downgrade();
        let force = self.close.downgrade();
        let active = matches!(phase, Phase::Saving(_) | Phase::NeedsDecision(_, _));
        let failure = match &phase { Phase::NeedsDecision(_, error) => Some(error.clone()), _ => None };
        div().relative().size_full().child(self.shell.clone()).when(active, |view| view.child(
            div().id("graceful-close").role(gpui::Role::Dialog).aria_label("Save before closing")
                .focus_trap("graceful-close", &focus)
                .on_key_down(move |event, _, cx| {
                    if event.keystroke.key == "escape" { let _ = escape.update(cx, |close, cx| close.cancel(cx)); cx.stop_propagation(); }
                })
                .absolute().inset_0().occlude().flex().items_center().justify_center()
                .bg(gpui::rgba(0x17191eee)).child(div().flex().flex_col().gap(px(12.0)).p(px(24.0))
                    .text_color(gpui::rgb(0xffffff)).child(failure.clone().unwrap_or_else(|| "Saving your latest changes…".into()))
                    .child(Button::new("close-continue").label("Continue editing").on_click(move |_, _, cx| {
                        let _ = cancel.update(cx, |close, cx| close.cancel(cx));
                    }))
                    .when(failure.is_some(), |view| view
                        .child(Button::new("close-retry").label("Try saving again").on_click(move |_, _, cx| {
                            let _ = retry.update(cx, |close, cx| close.retry(cx));
                        }))
                        .child(Button::new("close-previous").label("Close with previously saved state").on_click(move |_, _, cx| {
                            let _ = force.update(cx, |close, cx| close.commit(cx));
                        })))
                )
        ))
    }
}

#[cfg(test)]
mod tests;
