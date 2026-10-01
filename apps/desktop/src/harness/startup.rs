//! The native review window paints before fixture I/O starts. The worker
//! returns a prepared boot; only the actual shell mount happens on the UI.

use facet::measure::Measure;
use facet::tokens::ty;
use facet::{ActiveFacet, Set};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    thread,
};

use gpui::{
    AnyView, App, AppContext, Context, FocusHandle, Global, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, px,
};

pub(super) struct Status {
    settled: bool,
}
impl Global for Status {}

pub(super) fn settled(cx: &App) -> bool {
    cx.try_global::<Status>()
        .is_none_or(|status| status.settled)
}

pub(super) fn open(start: &'static str, _window: &mut Window, cx: &mut App) -> AnyView {
    cx.set_global(Status { settled: false });
    super::gallery::declare_quiet(super::quiet, cx);
    cx.new(|cx| {
        let mut startup = Startup::new(start, cx);
        startup.prepare(cx);
        startup
    })
    .into()
}

struct Startup {
    start: &'static str,
    generation: u64,
    prepared: Option<super::PreparedBoot>,
    mount_scheduled: bool,
    error: Option<SharedString>,
    ready: Option<AnyView>,
    loading: Option<Task<()>>,
    retry_focus: FocusHandle,
    copy_focus: FocusHandle,
    copied: bool,
    stage: SharedString,
    focus_error: bool,
    focus_root: bool,
    root_focus: FocusHandle,
    worker: Worker,
}

type Worker = std::rc::Rc<
    dyn Fn(
        &'static str,
        Progress,
        &gpui::BackgroundExecutor,
    ) -> Task<Result<super::PreparedBoot, String>>,
>;

const REVIEW_WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

fn prepare_worker(
    start: &'static str,
    progress: Progress,
    executor: &gpui::BackgroundExecutor,
) -> Task<Result<super::PreparedBoot, String>> {
    let worker_progress = progress.clone();
    threaded("nudox-review-fixture", progress, executor, move || {
        super::prepare_with_progress(start, |stage| {
            super::review_diag(&format!("worker stage: {stage}"));
            worker_progress.publish(stage);
        })
    })
}

/// Runs blocking fixture work off GPUI's GCD-backed executor, whose macOS
/// stacks are smaller than the recursive pinned-world walk needs.
fn threaded<T: Send + 'static>(
    name: &'static str,
    progress: Progress,
    executor: &gpui::BackgroundExecutor,
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Task<Result<T, String>> {
    threaded_with_stack(
        name,
        REVIEW_WORKER_STACK_BYTES,
        progress,
        executor,
        operation,
    )
}

fn threaded_with_stack<T: Send + 'static>(
    name: &'static str,
    stack_bytes: usize,
    progress: Progress,
    executor: &gpui::BackgroundExecutor,
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Task<Result<T, String>> {
    let completion = Completion::default();
    let thread_completion = completion.clone();
    let thread_progress = progress.clone();
    let spawn = thread::Builder::new()
        .name(name.to_owned())
        .stack_size(stack_bytes)
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&'static str>().copied())
                    .unwrap_or("non-string panic payload");
                Err(format!(
                    "{name} panicked during fixture preparation: {message}"
                ))
            });
            thread_progress.close();
            thread_completion.complete(result);
        });
    if let Err(error) = spawn {
        fail_spawn(name, error, progress, completion.clone());
    }
    executor.spawn(async move { completion.await })
}

struct Completion<T>(Arc<Mutex<CompletionState<T>>>);

struct CompletionState<T> {
    result: Option<T>,
    waker: Option<Waker>,
}

impl<T> Clone for Completion<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Default for Completion<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(CompletionState {
            result: None,
            waker: None,
        })))
    }
}

fn fail_spawn<T>(
    name: &str,
    error: std::io::Error,
    progress: Progress,
    completion: Completion<Result<T, String>>,
) {
    progress.close();
    completion.complete(Err(format!("could not start {name}: {error}")));
}

impl<T> Completion<T> {
    fn complete(&self, result: T) {
        let waker = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.result = Some(result);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Future for Completion<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Startup {
    fn new(start: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            start,
            generation: 0,
            prepared: None,
            mount_scheduled: false,
            error: None,
            ready: None,
            loading: None,
            stage: "Preparing the fixture owner".into(),
            focus_error: false,
            focus_root: true,
            root_focus: cx.focus_handle(),
            retry_focus: cx.focus_handle().tab_stop(true).tab_index(0),
            copy_focus: cx.focus_handle().tab_stop(true).tab_index(1),
            copied: false,
            worker: std::rc::Rc::new(prepare_worker),
        }
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.error = None;
        self.copied = false;
        cx.set_global(Status { settled: false });
        let generation = self.generation;
        let start = self.start;
        let progress = Progress::default();
        let work = (self.worker)(start, progress.clone(), cx.background_executor());
        self.loading = Some(cx.spawn(async move |startup, cx| {
            while let Some(stage) = progress.next().await {
                let _ = startup.update(cx, |startup, cx| {
                    if startup.generation == generation {
                        startup.stage = stage.into();
                        cx.notify();
                    }
                });
            }
            let prepared = work.await;
            let _ = startup.update(cx, |startup, cx| {
                if startup.generation != generation {
                    return;
                }
                startup.loading = None;
                match prepared {
                    Ok(prepared) => startup.prepared = Some(prepared),
                    Err(error) => {
                        startup.error = Some(error.into());
                        startup.focus_error = true;
                        cx.set_global(Status { settled: true });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(error) = &self.error {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!(
                "Desktop review startup at {}\n{}",
                self.start, error
            )));
            self.copied = true;
            cx.notify();
        }
    }

    fn finish_mount(&mut self, result: Result<AnyView, String>, cx: &mut Context<Self>) {
        match result {
            Ok(view) => self.ready = Some(view),
            Err(error) => {
                self.error = Some(error.into());
                self.focus_error = true;
            }
        }
        cx.set_global(Status { settled: true });
        cx.notify();
    }

    fn defer_handoff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        handoff: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.mount_scheduled {
            return;
        }
        self.mount_scheduled = true;
        cx.defer_in(window, move |startup, window, cx| {
            startup.mount_scheduled = false;
            handoff(startup, window, cx);
        });
    }
}

impl Render for Startup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.prepared.is_some() && !self.mount_scheduled {
            self.defer_handoff(window, cx, |startup, window, cx| {
                if let Some(prepared) = startup.prepared.take() {
                    let result = super::mount(prepared, window, cx);
                    startup.finish_mount(result, cx);
                }
            });
        }
        if let Some(view) = &self.ready {
            return div().size_full().child(view.clone()).into_any_element();
        }
        if self.focus_error {
            window.focus(&self.retry_focus, cx);
            self.focus_error = false;
        } else if self.focus_root {
            window.focus(&self.root_focus, cx);
            self.focus_root = false;
        }
        let palette = cx.facet().palette();
        let measure = Measure::new(window.viewport_size().width, &cx.facet());
        let failed = self.error.is_some();
        let mut body = div().w_full().max_w(px(620.0)).flex().flex_col().gap(px(18.0)).p(px(32.0))
            .child(div().flex().items_center().gap(px(10.0))
                .child(facet::icons::ui(if failed { facet::icons::Icon::Alert } else { facet::icons::Icon::Layers }, facet::icons::IconSize::S20, if failed { palette.coral.base } else { palette.peri.base }))
                .child(div().set(ty::SMALL, &measure).text_color(palette.ink2.hsla()).child("Desktop review")))
            .child(div().set(ty::DISPLAY, &measure).text_color(palette.ink0.hsla()).child(if failed { "This review could not start" } else { "Preparing your pages" }))
            .child(div().set(ty::BODY, &measure).text_color(palette.ink1.hsla()).child(if failed {
                "The window is ready. Retry the local fixture preparation, or copy the details for investigation."
            } else {
                "Reading local sources and building the fixture index. The page will appear here when it is ready."
            }))
            .child(div().set(ty::MONO_SMALL, &measure).text_color(palette.ink2.hsla()).child(self.start));
        if let Some(error) = &self.error {
            let weak = cx.weak_entity();
            let retry_weak = weak.clone();
            let copy_weak = weak.clone();
            let retry_key = weak.clone();
            let copy_key = weak;
            body = body
                .child(
                    div()
                        .id("startup-error-details")
                        .max_h(px(250.0))
                        .overflow_y_scroll()
                        .p(px(16.0))
                        .bg(palette.plate.hsla())
                        .border_l_2()
                        .border_color(palette.coral.base.hsla())
                        .set(ty::MONO_SMALL, &measure)
                        .text_color(palette.ink1.hsla())
                        .child(error.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.0))
                        .child(
                            div()
                                .id("startup-retry")
                                .track_focus(&self.retry_focus)
                                .cursor_pointer()
                                .px(px(16.0))
                                .py(px(10.0))
                                .bg(palette.plate2.hsla())
                                .border_1()
                                .border_color(palette.line2.hsla())
                                .focus_visible(|style| style.border_color(palette.peri.base.hsla()))
                                .set(ty::SMALL, &measure)
                                .text_color(palette.ink1.hsla())
                                .child("Retry")
                                .on_click(move |_, window, cx| {
                                    let _ = retry_weak.update(cx, |startup, cx| {
                                        window.focus(&startup.retry_focus, cx);
                                        startup.prepare(cx);
                                    });
                                })
                                .on_key_down(move |event, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        let _ =
                                            retry_key.update(cx, |startup, cx| startup.prepare(cx));
                                        cx.stop_propagation();
                                    }
                                }),
                        )
                        .child(
                            div()
                                .id("startup-copy")
                                .track_focus(&self.copy_focus)
                                .cursor_pointer()
                                .px(px(16.0))
                                .py(px(10.0))
                                .border_1()
                                .border_color(palette.line2.hsla())
                                .focus_visible(|style| style.border_color(palette.peri.base.hsla()))
                                .set(ty::SMALL, &measure)
                                .text_color(palette.ink1.hsla())
                                .child(if self.copied {
                                    "Details copied"
                                } else {
                                    "Copy details"
                                })
                                .on_click(move |_, window, cx| {
                                    let _ = copy_weak.update(cx, |startup, cx| {
                                        window.focus(&startup.copy_focus, cx);
                                        startup.copy(cx);
                                    });
                                })
                                .on_key_down(move |event, _, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                        let _ = copy_key.update(cx, |startup, cx| startup.copy(cx));
                                        cx.stop_propagation();
                                    }
                                }),
                        ),
                );
        } else {
            // No spinner or polling redraw: this static path is replaced once
            // by worker completion, keeping input and the native window alive.
            body = body
                .child(
                    div()
                        .set(ty::SMALL, &measure)
                        .text_color(palette.ink2.hsla())
                        .child(self.stage.clone()),
                )
                .child(div().h(px(2.0)).w(px(120.0)).bg(palette.peri.base.hsla()));
        }
        div()
            .id("desktop-startup")
            .debug_selector(|| "desktop-startup".to_owned())
            .track_focus(&self.root_focus)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(palette.g1.hsla())
            .child(body)
            .on_key_down(|event, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }
}

/// A single bounded progress slot, woken only by a real phase event. No
/// polling timer, queue of stale phases, or extra runtime dependency.
#[derive(Clone, Default)]
struct Progress(Arc<Mutex<ProgressState>>);
#[derive(Default)]
struct ProgressState {
    latest: Option<String>,
    closed: bool,
    waker: Option<Waker>,
}
impl Progress {
    fn publish(&self, stage: &str) {
        let wake = {
            let mut state = self.0.lock().expect("progress slot");
            state.latest = Some(stage.to_owned());
            state.waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
    fn close(&self) {
        let wake = {
            let mut state = self.0.lock().expect("progress slot");
            state.closed = true;
            state.waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
    async fn next(&self) -> Option<String> {
        std::future::poll_fn(|cx| {
            let mut state = self.0.lock().expect("progress slot");
            if let Some(stage) = state.latest.take() {
                Poll::Ready(Some(stage))
            } else if state.closed {
                Poll::Ready(None)
            } else {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{
        sync::{
            Condvar,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    struct ActiveWorker(Arc<AtomicUsize>);
    impl Drop for ActiveWorker {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct MountedMarker;
    impl Render for MountedMarker {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("startup-mounted-marker")
                .debug_selector(|| "startup-mounted-marker".to_owned())
                .child("Mounted page")
        }
    }

    #[gpui::test]
    fn successful_mount_handoff_invalidates_and_paints_without_input_or_resize(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            let _ = facet::fonts::install(cx);
            facet::theme::set_facet(facet::Facet::default(), cx);
            cx.set_global(Status { settled: false });
        });
        let (view, cx) = cx.add_window_view(|_, cx| Startup::new("test prepared page", cx));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("desktop-startup").is_some(),
            "the native loading shell paints first"
        );

        let mounted: AnyView = cx.update(|_, cx| cx.new(|_| MountedMarker).into());
        let notification_count = Arc::new(AtomicUsize::new(0));
        let observer_count = Arc::clone(&notification_count);
        cx.update(|_, cx| {
            cx.observe(&view, move |_, _| {
                observer_count.fetch_add(1, Ordering::SeqCst);
            })
            .detach();
        });
        cx.update(|window, cx| {
            view.update(cx, |startup, cx| {
                startup.defer_handoff(window, cx, move |startup, _window, cx| {
                    startup.finish_mount(Ok(mounted), cx)
                });
            });
        });
        cx.run_until_parked();
        assert!(
            notification_count.load(Ordering::SeqCst) > 0,
            "deferred successful handoff notified the mounted root"
        );
        assert!(cx.update(|_, cx| view.read(cx).ready.is_some()));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });

        assert!(
            cx.debug_bounds("startup-mounted-marker").is_some(),
            "the deferred handoff paints the mounted page without input or resize"
        );
        assert!(
            cx.debug_bounds("desktop-startup").is_none(),
            "the loading shell has left the rendered tree"
        );
        assert!(
            cx.update(|_, cx| settled(cx)),
            "settled follows successful mount"
        );
    }

    #[gpui::test]
    async fn the_window_paints_and_handles_focus_before_the_worker_finishes(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            let _ = facet::fonts::install(cx);
            facet::theme::set_facet(facet::Facet::default(), cx);
        });
        let attempt = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((Mutex::new(0usize), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker: Worker = {
            let attempt = attempt.clone();
            let active = active.clone();
            let maximum_active = maximum_active.clone();
            let release = release.clone();
            let started_tx = started_tx.clone();
            let finished_tx = finished_tx.clone();
            std::rc::Rc::new(move |_start, progress, executor| {
                let attempt = attempt.fetch_add(1, Ordering::SeqCst) + 1;
                progress.publish("Indexing fixture (1/10)");
                let active = active.clone();
                let maximum_active = maximum_active.clone();
                let release = release.clone();
                let started_tx = started_tx.clone();
                let finished_tx = finished_tx.clone();
                threaded(
                    "test-blocked-review-fixture",
                    progress,
                    executor,
                    move || {
                        let active_guard = ActiveWorker(active.clone());
                        let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum_active.fetch_max(now_active, Ordering::SeqCst);
                        started_tx
                            .send(attempt)
                            .expect("test receiver remains alive");
                        let (lock, wake) = &*release;
                        let mut released = lock.lock().expect("test release gate");
                        while *released < attempt {
                            released = wake.wait(released).expect("test release gate");
                        }
                        drop(released);
                        drop(active_guard);
                        finished_tx
                            .send(attempt)
                            .expect("test receiver remains alive");
                        Err(
                            "bootstrap fixture: complete original diagnostic\nsecond detail"
                                .to_owned(),
                        )
                    },
                )
            })
        };
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut startup = Startup::new("symbol fixture::Value", cx);
            startup.worker = worker;
            startup.prepare(cx);
            startup
        });
        cx.run_until_parked();
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("dedicated worker starts"),
            1
        );
        cx.update(|window, cx| {
            let startup = view.read(cx);
            assert_eq!(startup.stage.as_ref(), "Indexing fixture (1/10)");
            assert!(startup.error.is_none());
            assert!(startup.ready.is_none());
            assert!(startup.loading.is_some());
            assert!(
                startup.root_focus.is_focused(window),
                "the native tree already painted and accepts focus while I/O is unfinished"
            );
            assert!(
                !settled(cx),
                "capture cannot mistake a loading frame for a page"
            );
        });
        view.update(cx, |startup, cx| startup.prepare(cx));
        assert_eq!(
            attempt.load(Ordering::SeqCst),
            1,
            "a duplicate prepare is ignored while a worker is active"
        );
        assert!(
            started_rx.try_recv().is_err(),
            "duplicate prepare did not create another worker"
        );
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        {
            let (lock, wake) = &*release;
            *lock.lock().expect("test release gate") = 1;
            wake.notify_all();
        }
        assert_eq!(
            finished_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("first worker finishes"),
            1
        );
        cx.condition(&view, |startup, _| startup.error.is_some())
            .await;
        cx.update(|window, cx| {
            let startup = view.read(cx);
            assert_eq!(
                startup.error.as_ref().map(SharedString::as_ref),
                Some("bootstrap fixture: complete original diagnostic\nsecond detail")
            );
            assert!(startup.retry_focus.is_focused(window));
            assert!(settled(cx));
        });
        // Native tab stops, then Space activates Copy with the entire exact
        // diagnostic. Shift-Tab and Enter start a new worker generation.
        cx.simulate_keystrokes("tab space");
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()), Some("Desktop review startup at symbol fixture::Value\nbootstrap fixture: complete original diagnostic\nsecond detail".to_owned()));
            assert!(view.read(cx).copied);
        });
        cx.simulate_keystrokes("shift-tab enter");
        cx.run_until_parked();
        assert_eq!(
            started_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("retry starts one new worker"),
            2
        );
        cx.update(|_, cx| {
            let startup = view.read(cx);
            assert_eq!(startup.generation, 2);
            assert!(startup.error.is_none());
            assert!(startup.loading.is_some());
            assert!(!settled(cx));
        });
        assert_eq!(
            maximum_active.load(Ordering::SeqCst),
            1,
            "a retry cannot overlap an unfinished worker"
        );
        {
            let (lock, wake) = &*release;
            *lock.lock().expect("test release gate") = 2;
            wake.notify_all();
        }
        assert_eq!(
            finished_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("completion is delivered"),
            2
        );
        cx.condition(&view, |startup, _| {
            startup.loading.is_none() && startup.error.is_some()
        })
        .await;
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    async fn dedicated_worker_handles_a_stack_frame_larger_than_the_gcd_stack(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut startup = Startup::new("worker stack fixture", cx);
            let progress = Progress::default();
            startup.loading = Some(cx.spawn(async move |startup, cx| {
                let task = threaded(
                    "test-large-stack",
                    progress,
                    cx.background_executor(),
                    || {
                        let mut frame = [0u8; 600 * 1024];
                        frame[0] = 41;
                        frame[frame.len() - 1] = 17;
                        std::hint::black_box(&frame);
                        Ok(frame[0] as usize + frame[frame.len() - 1] as usize)
                    },
                );
                assert_eq!(task.await.expect("large stack worker succeeds"), 58);
                let _ = startup.update(cx, |startup, cx| {
                    startup.loading = None;
                    cx.notify();
                });
            }));
            startup
        });
        cx.condition(&view, |startup, _| startup.loading.is_none())
            .await;
    }

    #[gpui::test]
    async fn worker_panic_and_spawn_failure_complete_with_visible_errors(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut startup = Startup::new("worker error fixture", cx);
            startup.loading = Some(cx.spawn(async move |startup, cx| {
                let panicked = threaded("test-panicking-worker", Progress::default(), cx.background_executor(), || -> Result<(), String> {
                    panic!("synthetic fixture panic");
                }).await;
                assert_eq!(panicked.unwrap_err(), "test-panicking-worker panicked during fixture preparation: synthetic fixture panic");

                let completion = Completion::default();
                let progress = Progress::default();
                fail_spawn::<()>("injected-worker", std::io::Error::new(std::io::ErrorKind::Other, "injected spawn failure"), progress.clone(), completion.clone());
                assert_eq!(completion.0.lock().expect("completion lock").result.as_ref().unwrap().as_ref().unwrap_err(), "could not start injected-worker: injected spawn failure");
                assert!(progress.0.lock().expect("progress lock").closed);
                let _ = startup.update(cx, |startup, cx| { startup.loading = None; cx.notify(); });
            }));
            startup
        });
        cx.condition(&view, |startup, _| startup.loading.is_none())
            .await;
    }
}
