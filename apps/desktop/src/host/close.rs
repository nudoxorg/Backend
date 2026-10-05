//! Graceful close runs while GPUI is still live, before its shutdown deadline.
//! Only an in-flight checkpoint task owns the graph; all native observers are weak.

use crate::runtime::store::DataStore;
use crate::runtime::{UiEntityGraph, UiRootEntity};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Task, WeakEntity, Window, div, px,
};
use gpui_component::FocusTrapElement as _;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use std::time::Duration;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseTarget {
    Window,
    Application,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attempt(u64);
#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Open,
    Saving(Attempt),
    NeedsDecision(Attempt, String),
    Finishing(Attempt),
    WorkerBlocked(Attempt),
    Committed,
}
#[derive(Clone, Copy)]
enum Choice {
    ContinueEditing,
    Retry,
    CloseWithPreviousState,
    WaitAgain,
}

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
    join_task: Option<Task<()>>,
    joined: Option<async_channel::Receiver<()>>,
    deadline: Duration,
    approved: Rc<Cell<bool>>,
    #[cfg(test)]
    extra_finish: Option<crate::runtime::worker_finish::WorkerFinish>,
}

impl GracefulClose {
    pub(crate) fn install(graph: &UiEntityGraph, cx: &mut App) -> Entity<Self> {
        let approved = Rc::new(Cell::new(false));
        let approval = approved.clone();
        let close = cx.new(|cx| Self {
            root: graph.root.downgrade(),
            store: graph.store.downgrade(),
            window: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            phase: Phase::Open,
            next: 0,
            target: CloseTarget::Window,
            native_pending: false,
            task: None,
            join_task: None,
            joined: None,
            deadline: Duration::from_secs(5),
            approved: approval,
            #[cfg(test)]
            extra_finish: None,
        });
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        let quit = close.downgrade();
        cx.on_action(move |_: &super::menus::Quit, cx| {
            let quit = quit.clone();
            cx.defer(move |cx| {
                let _ = quit.update(cx, |close, cx| close.request(CloseTarget::Application, cx));
            });
        });
        let window_close = close.downgrade();
        cx.on_action(move |_: &super::menus::CloseWindow, cx| {
            let window_close = window_close.clone();
            cx.defer(move |cx| {
                let _ = window_close.update(cx, |close, cx| close.request(CloseTarget::Window, cx));
            });
        });
        let native = close.downgrade();
        cx.on_app_should_quit(move |cx| {
            native
                .update(cx, |close, cx| {
                    if close.phase == Phase::Committed {
                        return true;
                    }
                    close.native_pending = true;
                    close.request(CloseTarget::Application, cx);
                    false
                })
                .unwrap_or(true)
        });
        let last = close.downgrade();
        cx.on_window_closed(move |cx, _| {
            if cx.windows().is_empty() {
                if approved.get() {
                    cx.quit();
                } else {
                    let _ =
                        last.update(cx, |close, cx| close.request(CloseTarget::Application, cx));
                }
            }
        })
        .detach();
        close
    }

    pub(crate) fn attach(close: &Entity<Self>, window: &mut Window, cx: &mut App) {
        close.update(cx, |close, _| close.window = Some(window.window_handle()));
        let weak = close.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let Some(close) = weak.upgrade() else {
                return true;
            };
            if close.read(cx).phase == Phase::Committed {
                return true;
            }
            let weak = weak.clone();
            // Native callbacks currently borrow this window. Defer the save
            // admission/focus change until GPUI has put the window back.
            cx.defer(move |cx| {
                let _ = weak.update(cx, |close, cx| close.request(CloseTarget::Window, cx));
            });
            false
        });
    }

    fn request(&mut self, target: CloseTarget, cx: &mut Context<Self>) {
        if self.phase == Phase::Committed {
            return;
        }
        if target == CloseTarget::Application {
            self.target = target;
        }
        if matches!(
            self.phase,
            Phase::Saving(_)
                | Phase::NeedsDecision(_, _)
                | Phase::Finishing(_)
                | Phase::WorkerBlocked(_)
        ) {
            return;
        }
        self.target = target;
        let Some(next) = self.next.checked_add(1) else {
            return;
        };
        self.next = next;
        let attempt = Attempt(next);
        let Some(root) = self.root.upgrade() else {
            self.finalize(cx);
            return;
        };
        let store = self.store.upgrade();
        if let Some(window) = self.window {
            let focus = self.focus.clone();
            let previous = window
                .update(cx, |_, window, cx| {
                    let previous = window.focused(cx).map(|focus| focus.downgrade());
                    window.focus(&focus, cx);
                    previous
                })
                .ok()
                .flatten();
            if self.previous_focus.is_none() {
                self.previous_focus = previous;
            }
        }
        let checkpoint = root.update(cx, |root, cx| root.begin_close(cx));
        let checkpoint = match checkpoint {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.phase = Phase::NeedsDecision(attempt, error.message.to_string());
                cx.notify();
                return;
            }
        };
        let crate::runtime::ui_graph::CloseCheckpoint {
            saved,
            pages,
            basis,
        } = checkpoint;
        debug_assert!(
            pages
                .as_ref()
                .is_none_or(|pages| pages.captured_root() == basis)
        );
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
                    cx.background_spawn(async move { pages.write_checkpoint().map_err(|error| error.to_string()) }).await?;
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
                    Ok(()) => close.begin_finish(attempt, cx),
                    Err(error) => { close.phase = Phase::NeedsDecision(attempt, error); cx.notify(); }
                }
            });
            drop(retained);
        }));
    }

    fn choose(&mut self, attempt: Attempt, choice: Choice, cx: &mut Context<Self>) {
        let current = match self.phase {
            Phase::Saving(current)
            | Phase::NeedsDecision(current, _)
            | Phase::Finishing(current)
            | Phase::WorkerBlocked(current) => current,
            _ => return,
        };
        if current != attempt {
            return;
        }
        match choice {
            Choice::ContinueEditing
                if matches!(self.phase, Phase::Saving(_) | Phase::NeedsDecision(_, _)) =>
            {
                self.cancel(cx)
            }
            Choice::Retry if matches!(self.phase, Phase::NeedsDecision(_, _)) => self.retry(cx),
            Choice::CloseWithPreviousState if matches!(self.phase, Phase::NeedsDecision(_, _)) => {
                self.begin_finish(attempt, cx)
            }
            Choice::WaitAgain if matches!(self.phase, Phase::WorkerBlocked(_)) => {
                self.monitor_finish(attempt, cx)
            }
            _ => {}
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.phase,
            Phase::Committed | Phase::Finishing(_) | Phase::WorkerBlocked(_)
        ) {
            return;
        }
        self.task = None;
        self.phase = Phase::Open;
        self.target = CloseTarget::Window;
        let _ = self.root.update(cx, |root, cx| root.cancel_close(cx));
        if std::mem::take(&mut self.native_pending) {
            cx.reply_to_app_quit(false);
        }
        if let Some(previous) = self.previous_focus.take().and_then(|focus| focus.upgrade()) {
            if let Some(window) = self.window {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| window.focus(&previous, cx));
                });
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

    /// The save decision is now irreversible. Transfer only Send join handles;
    /// window/root Drop cannot synchronously wait for these workers any more.
    fn begin_finish(&mut self, attempt: Attempt, cx: &mut Context<Self>) {
        let finish = self
            .root
            .update(cx, |root, cx| root.commit_close(cx))
            .unwrap_or_default();
        #[cfg(test)]
        let finish = {
            let mut finish = finish;
            if let Some(extra) = self.extra_finish.take() {
                finish.extend(extra);
            }
            finish
        };
        if finish.is_empty() {
            self.finalize(cx);
            return;
        }
        let (sent, received) = async_channel::bounded(1);
        self.joined = Some(received);
        let executor = cx.background_executor().clone();
        self.join_task = Some(cx.background_executor().spawn(async move {
            finish.wait(executor).await;
            let _ = sent.try_send(());
        }));
        self.monitor_finish(attempt, cx);
    }

    fn monitor_finish(&mut self, attempt: Attempt, cx: &mut Context<Self>) {
        let Some(joined) = self.joined.clone() else {
            return;
        };
        self.phase = Phase::Finishing(attempt);
        cx.notify();
        let timer = cx.background_executor().timer(self.deadline);
        self.task = Some(cx.spawn(async move |this, cx| {
            use std::future::Future as _;
            let stopped = {
                let mut joined = std::pin::pin!(joined.recv());
                let mut timer = std::pin::pin!(timer);
                std::future::poll_fn(|cx| {
                    if let std::task::Poll::Ready(result) = joined.as_mut().poll(cx) {
                        return std::task::Poll::Ready(result.is_ok());
                    }
                    if timer.as_mut().poll(cx).is_ready() {
                        return std::task::Poll::Ready(false);
                    }
                    std::task::Poll::Pending
                })
                .await
            };
            let _ = this.update(cx, |close, cx| {
                if close.phase != Phase::Finishing(attempt) {
                    return;
                }
                close.task = None;
                if stopped {
                    close.join_task = None;
                    close.joined = None;
                    close.finalize(cx);
                } else {
                    // The Send-only join remains tracked by this live window.
                    // Retry watches that same job; it never creates another.
                    close.phase = Phase::WorkerBlocked(attempt);
                    cx.notify();
                }
            });
        }));
    }

    fn finalize(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Committed;
        self.approved.set(true);
        cx.notify();
        if self.target == CloseTarget::Application {
            if std::mem::take(&mut self.native_pending) {
                cx.reply_to_app_quit(true);
            } else {
                cx.quit();
            }
        } else if let Some(window) = self.window {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
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
    pub(crate) fn shell(&self) -> Entity<crate::shell::Shell> {
        self.shell.clone()
    }
    pub(crate) fn new(
        shell: Entity<crate::shell::Shell>,
        close: Entity<GracefulClose>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&close, |_, _, cx| cx.notify()).detach();
        Self { shell, close }
    }
}
impl Render for CloseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.close.read(cx).phase.clone();
        let focus = self.close.read(cx).focus.clone();
        let escape = self.close.downgrade();
        let cancel = self.close.downgrade();
        let retry = self.close.downgrade();
        let force = self.close.downgrade();
        let active = matches!(
            phase,
            Phase::Saving(_)
                | Phase::NeedsDecision(_, _)
                | Phase::Finishing(_)
                | Phase::WorkerBlocked(_)
        );
        let can_cancel = matches!(phase, Phase::Saving(_) | Phase::NeedsDecision(_, _));
        let worker_blocked = matches!(phase, Phase::WorkerBlocked(_));
        let attempt = match &phase {
            Phase::Saving(attempt)
            | Phase::NeedsDecision(attempt, _)
            | Phase::Finishing(attempt)
            | Phase::WorkerBlocked(attempt) => *attempt,
            _ => Attempt(0),
        };
        let failure = match &phase {
            Phase::NeedsDecision(_, error) => Some(error.clone()),
            _ => None,
        };
        let message = match &phase {
            Phase::Finishing(_) => "Waiting for background requests to stop…".to_owned(),
            Phase::WorkerBlocked(_) => "A background request has not stopped. This window remains open. You can try waiting again.".to_owned(),
            _ => failure.clone().unwrap_or_else(|| "Saving your latest changes…".into()),
        };
        let wait = self.close.downgrade();
        let facet = cx.facet();
        let palette = facet.palette();
        let viewport = window.viewport_size();
        let inset = facet.px(8.0).min(viewport.width * 0.04);
        let width = (viewport.width - inset * 2.0)
            .max(px(1.0))
            .min(facet.px(480.0));
        let height = (viewport.height - inset * 2.0).max(px(1.0));
        let measure = Measure::new(width, &facet);
        let padding = measure.space(Space::Roomy).min(width * 0.08);
        let text_width = (width - padding * 2.0 - px(16.0)).max(px(1.0));
        let lines = crate::shell::text_fit::wrap_identifier(
            &message,
            &measure.role(ty::SMALL),
            text_width,
            cx,
        );

        let mut panel = div()
            .id("close-panel")
            .w(width)
            .max_h(height)
            .min_w_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Base))
            .p(padding)
            .bg(palette.plate2)
            .border_1()
            .border_color(palette.line2.hsla())
            .child(
                div()
                    .id("close-message-scroll")
                    .flex_shrink_0()
                    .max_h(height * 0.35)
                    .overflow_y_scroll()
                    .role(gpui::Role::Status)
                    .aria_label(message.clone())
                    .children(lines.into_iter().map(|line| {
                        crate::shell::kit::text(ty::SMALL, &measure, palette.ink1).child(line)
                    })),
            );
        if can_cancel {
            panel = panel.child(
                close_button("close-continue", "Keep editing", &measure, cx).on_click(
                    move |_, _, cx| {
                        let _ = cancel.update(cx, |close, cx| {
                            close.choose(attempt, Choice::ContinueEditing, cx)
                        });
                    },
                ),
            );
        }
        if failure.is_some() {
            panel = panel
                .child(crate::shell::kit::text(ty::SMALL, &measure, palette.ink2)
                    .child("Closing anyway may lose the latest changes."))
                .child(close_button("close-retry", "Try again", &measure, cx)
                    .on_click(move |_, _, cx| {
                        let _ = retry.update(cx, |close, cx| close.choose(attempt, Choice::Retry, cx));
                    }))
                .child(close_button("close-previous", "Close anyway", &measure, cx)
                    .accessibility_description("Close without confirming the latest save. The latest changes may be lost.")
                    .on_click(move |_, _, cx| {
                        let _ = force.update(cx, |close, cx| close.choose(attempt, Choice::CloseWithPreviousState, cx));
                    }));
        }
        if worker_blocked {
            panel = panel.child(
                close_button("close-wait-again", "Wait again", &measure, cx).on_click(
                    move |_, _, cx| {
                        let _ = wait
                            .update(cx, |close, cx| close.choose(attempt, Choice::WaitAgain, cx));
                    },
                ),
            );
        }
        div()
            .relative()
            .size_full()
            .child(self.shell.clone())
            .when(active, |view| {
                view.child(
                    div()
                        .id("graceful-close")
                        .role(gpui::Role::Dialog)
                        .aria_label("Save before closing")
                        .focus_trap("graceful-close", &focus)
                        .on_key_down(move |event, _, cx| {
                            if event.keystroke.key == "escape" {
                                let _ = escape.update(cx, |close, cx| {
                                    close.choose(attempt, Choice::ContinueEditing, cx)
                                });
                                cx.stop_propagation();
                            }
                        })
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(palette.g1.hsla())
                        .child(panel),
                )
            })
    }
}

// CE owns focus, native hit testing and keyboard activation. Facet owns the
// palette and scaled type; the custom content avoids CE's unscaled label font.
fn close_button(id: &'static str, label: &'static str, measure: &Measure, cx: &App) -> Button {
    let palette = cx.facet().palette();
    let focused = Rc::new(Cell::new(false));
    let observed = focused.clone();
    Button::new(id)
        .on_focus_observed(move |_, is_focused, _, _| observed.set(is_focused))
        .on_bounds_observed(move |bounds, window, _| {
            if focused.get() {
                window.request_autoscroll(bounds);
            }
        })
        .accessibility_label(label)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(palette.plate2.hsla())
                .foreground(palette.ink1.hsla())
                .hover(palette.plate3.hsla())
                .active(palette.plate.hsla()),
        )
        .rounded(px(0.0))
        .border_1()
        .border_color(palette.line2.hsla())
        .w_full()
        .flex_shrink_0()
        .h(measure.control(facet::Control::Medium))
        .child(crate::shell::kit::text(ty::SMALL, measure, palette.ink1).child(label))
}

#[cfg(test)]
mod tests;
