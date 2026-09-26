//! A scene stage: the content, the float layer as its last child, and a
//! script of pointer moves dispatched on the virtual clock — so a scene
//! shows the real hover path (mark → door → float layer → lens with its
//! connector), not a picture of one.

use crate::overlay::float;
use crate::theme::{ActiveFacet, Facet, set_facet};
use crate::tokens::Appearance;
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, IntoElement, Modifiers, MouseMoveEvent,
    ParentElement, PlatformInput, Render, Styled, Window, div, point, px,
};
use std::time::Duration;

/// Builds the content for the stage's width.
pub(crate) type Content = fn(f32, &mut Window, &mut App) -> AnyElement;

pub(crate) struct Stage {
    content: Content,
    pokes: &'static [(u64, f32, f32)],
    steps: &'static [u64],
    thaw: bool,
    started: bool,
}

/// How many scripted steps have fired (a scene's content reads it to move
/// its data on: a stage completes, a width shrinks).
#[derive(Default)]
pub(crate) struct Step(pub usize);

impl gpui::Global for Step {}

/// The current scripted step.
pub(crate) fn step(cx: &App) -> usize {
    cx.try_global::<Step>().map_or(0, |s| s.0)
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.started = true;
            cx.set_global(Step(0));
            if self.thaw {
                crate::motion::pulse::thaw(cx);
            }
            for &at in self.steps {
                let view = cx.entity().downgrade();
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(at)).await;
                        let _ = cx.update(|_, cx| {
                            cx.default_global::<Step>().0 += 1;
                            let _ = view.update(cx, |_, cx| cx.notify());
                        });
                    })
                    .detach();
            }
            for &(at, x, y) in self.pokes {
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(at)).await;
                        let _ = cx.update(|window, cx| {
                            window.dispatch_event(
                                PlatformInput::MouseMove(MouseMoveEvent {
                                    position: point(px(x), px(y)),
                                    pressed_button: None,
                                    modifiers: Modifiers::none(),
                                }),
                                cx,
                            );
                        });
                    })
                    .detach();
            }
        }
        let width = f32::from(window.viewport_size().width);
        let palette = cx.palette();
        div()
            .size_full()
            .relative()
            .bg(palette.g1)
            .child((self.content)(width, window, cx))
            .child(float::layer(window, cx))
    }
}

/// A stage view. `Some(appearance)` pins the appearance (the `-glacier`
/// twins); `pokes` are `(ms, x, y)` pointer moves after the first frame.
pub(crate) fn stage(
    appearance: Option<Appearance>,
    pokes: &'static [(u64, f32, f32)],
    content: Content,
    cx: &mut App,
) -> AnyView {
    if let Some(appearance) = appearance {
        set_facet(
            Facet {
                appearance,
                ..cx.facet()
            },
            cx,
        );
    }
    cx.new(|_| Stage {
        content,
        pokes,
        steps: &[],
        thaw: false,
        started: false,
    })
    .into()
}

/// A stage that also fires `steps` (ms) and, with `thaw`, lets the ambient
/// pulse run (scenes that film flowing strands or a working gem).
pub(crate) fn scripted(
    pokes: &'static [(u64, f32, f32)],
    steps: &'static [u64],
    thaw: bool,
    content: Content,
    cx: &mut App,
) -> AnyView {
    cx.new(|_| Stage {
        content,
        pokes,
        steps,
        thaw,
        started: false,
    })
    .into()
}
