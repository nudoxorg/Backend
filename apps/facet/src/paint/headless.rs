//! Headless tests of the paint primitives through a real GPUI window:
//! shape hover driven by real mouse events, and the ground's frame cost.

use crate::icons::Assets;
use gpui::{Context, IntoElement, Render, Window};
use std::sync::Arc;

/// A window that is nothing but the ground, or nothing at all.
struct GroundTimer {
    phase: f32,
    ground: bool,
}

impl Render for GroundTimer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::ActiveFacet;
        use gpui::{ParentElement, Styled, div};
        let base = div()
            .size_full()
            .relative()
            .bg(gpui::Hsla::from(cx.palette().g1));
        if self.ground {
            base.child(super::ground().phase(self.phase))
        } else {
            base
        }
    }
}

/// CPU frame time (render + layout + prepaint + paint) of a window holding
/// only the ground, against an empty window, while the twinkle phase moves
/// every frame. Run in release:
/// `cargo test --release -p backend-facet --features gallery --lib ground_timing -- --ignored --nocapture`
#[test]
#[ignore = "timing; run explicitly in release"]
fn ground_timing() {
    use gpui::{AppContext as _, HeadlessAppContext, px, size};
    const FRAMES: u32 = 240;
    for (w, h) in [(1440.0_f32, 900.0_f32), (2560.0, 1440.0)] {
        for scale in [1.0_f32, 2.0] {
            let mut stats = Vec::new();
            for ground in [false, true] {
                let platform = gpui_platform::current_platform(true);
                let mut cx = HeadlessAppContext::with_platform(
                    platform.text_system(),
                    Arc::new(Assets),
                    gpui_platform::current_headless_renderer,
                );
                let Ok(handle) = cx.open_window(size(px(w), px(h)), |window, cx| {
                    window.set_scale_factor(scale);
                    cx.new(|_| GroundTimer { phase: 0.0, ground })
                }) else {
                    return;
                };
                let any: gpui::AnyWindowHandle = handle.into();
                let mut times = Vec::new();
                for frame in 0..FRAMES {
                    let _ = cx.update_window(any, |view, window, cx| {
                        if let Ok(timer) = view.downcast::<GroundTimer>() {
                            timer.update(cx, |t, cx| {
                                #[allow(clippy::cast_precision_loss)]
                                let phase = frame as f32 / 84.0;
                                t.phase = phase;
                                cx.notify();
                            });
                        }
                        let started = std::time::Instant::now();
                        let clear = window.draw(cx);
                        clear.clear(cx);
                        times.push(started.elapsed());
                    });
                }
                times.sort_unstable();
                let p50 = times[times.len() / 2];
                let p95 = times[times.len() * 95 / 100];
                stats.push((ground, p50, p95));
            }
            for (ground, p50, p95) in stats {
                eprintln!(
                    "{}x{} @{scale}x {}: p50 {p50:?} p95 {p95:?}",
                    w,
                    h,
                    if ground { "ground" } else { "empty " }
                );
            }
        }
    }
}

/// A plate that reports shape-hover transitions into a log.
struct HoverProbe {
    log: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
}

impl Render for HoverProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{
            InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, div, px,
        };
        let log = self.log.clone();
        div().size_full().p(px(20.0)).child(
            super::cut()
                .w(px(180.0))
                .h(px(64.0))
                .on_shape_hover(move |inside, _window, _cx| log.borrow_mut().push(inside))
                // Configure the cut, then `.id` for the stateful div methods.
                .id("probe")
                .on_click(|_, _, _| {}),
        )
    }
}

/// The pointer over a cut-away corner is not over the plate; over the body
/// (or a square corner) it is. Driven through real GPUI mouse events.
#[test]
fn shape_hover_ignores_the_cut_corners() {
    use gpui::{
        AppContext as _, HeadlessAppContext, Modifiers, MouseMoveEvent, PlatformInput, point, px,
        size,
    };
    let platform = gpui_platform::current_platform(true);
    let mut cx =
        HeadlessAppContext::with_platform(platform.text_system(), Arc::new(Assets), || None);
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let probe_log = log.clone();
    let handle = cx
        .open_window(size(px(240.0), px(120.0)), move |_window, cx| {
            cx.new(|_| HoverProbe { log: probe_log })
        })
        .ok();
    let Some(handle) = handle else { return };
    let any: gpui::AnyWindowHandle = handle.into();
    let mut step = |x: f32, y: f32| {
        let _ = cx.update_window(any, |_, window, cx| {
            window.refresh();
            let clear = window.draw(cx);
            clear.clear(cx);
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(px(x), px(y)),
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        });
    };
    // Plate: 20..200 x 20..84, 14 px cuts at top-left and bottom-right.
    step(23.0, 23.0); // inside the box, in the cut-away top-left corner
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    step(100.0, 50.0); // the body
    assert_eq!(log.borrow().as_slice(), &[true]);
    step(197.0, 81.0); // the cut-away bottom-right corner
    assert_eq!(log.borrow().as_slice(), &[true, false]);
    step(197.0, 23.0); // the square top-right corner counts
    assert_eq!(log.borrow().as_slice(), &[true, false, true]);
}
