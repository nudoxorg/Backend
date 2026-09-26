//! The `can` line: capabilities in plain words, each with a small diamond
//! that says how it arrives — hollow when derived, solid when written by
//! hand, dashed when given through another capability (`to text` via
//! `Display`). One line that wraps; resting on a capability names its trait
//! and how it arrives.

use super::{heading, k, roles};
use crate::measure::{Measure, Set};
use crate::overlay::float::{FloatKind, FloatRequest};
use crate::overlay::text::{Link, words};
use crate::semantics::caps::{Arrives, Cap};
use crate::theme::ActiveFacet;
use gpui::{
    App, ElementId, IntoElement, ParentElement, PathBuilder, RenderOnce, SharedString, Styled, StyledText, Window,
    canvas, div, point, px,
};
use std::sync::Arc;

/// The `can` line. Build with [`can`].
#[derive(IntoElement)]
pub struct Can {
    id: ElementId,
    caps: Vec<Cap>,
    measure: Measure,
}

/// The `can` line for `caps` at `measure` (nothing when there are none).
#[must_use]
pub fn can(id: impl Into<ElementId>, caps: Vec<Cap>, measure: &Measure) -> Can {
    Can { id: id.into(), caps, measure: *measure }
}

fn mark(arrives: &Arrives, measure: &Measure, palette: &crate::tokens::Palette) -> impl IntoElement {
    let s = measure.scale();
    let (fill, stroke, dashed) = match arrives {
        Arrives::Derived => (None, Some(palette.ink3.hsla()), false),
        Arrives::Written => (Some(palette.ink1.hsla()), None, false),
        Arrives::Via(_) => (None, Some(palette.ink4.hsla()), true),
    };
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let r = px(3.6 * s);
            let diamond = |b: &mut PathBuilder, r: gpui::Pixels| {
                b.move_to(point(c.x, c.y - r));
                b.line_to(point(c.x + r, c.y));
                b.line_to(point(c.x, c.y + r));
                b.line_to(point(c.x - r, c.y));
                b.close();
            };
            if let Some(fill) = fill {
                let mut b = PathBuilder::fill();
                diamond(&mut b, r);
                if let Ok(path) = b.build() {
                    window.paint_path(path, fill);
                }
            }
            if let Some(stroke) = stroke {
                let mut b = PathBuilder::stroke(px(1.2));
                if dashed {
                    b = b.dash_array(&[px(1.5), px(1.5)]);
                    diamond(&mut b, r + px(1.0));
                } else {
                    diamond(&mut b, r - px(0.6));
                }
                if let Ok(path) = b.build() {
                    window.paint_path(path, stroke);
                }
            }
        },
    )
    .flex_none()
    .size(px(9.0 * s))
}

impl RenderOnce for Can {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().id(self.id.clone()).flex().flex_col();
        if self.caps.is_empty() {
            return root;
        }
        let caps = self.caps.iter().enumerate().map(|(n, cap)| {
            let key = ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(format!("cap-{n}")));
            let word = cap.word.clone();
            let tip = SharedString::from(format!("{} — {}", cap.trait_name, cap.arrives.text()));
            let request_key = key.clone();
            let link = Link::new(key.clone(), move |rect| {
                let tip = tip.clone();
                FloatRequest::new(request_key.clone(), rect, FloatKind::Tip, move |measure, _, cx| {
                    let palette = cx.facet().palette();
                    div()
                        .set(roles::QUIET, measure)
                        .text_color(palette.ink1.hsla())
                        .px(px(8.0 * measure.scale()))
                        .py(px(4.0 * measure.scale()))
                        .child(tip.clone())
                        .into_any_element()
                })
            });
            let len = word.len();
            div()
                .flex()
                .items_center()
                .gap(k(&m, 7.0))
                .child(mark(&cap.arrives, &m, palette))
                .child(words(key, StyledText::new(word), vec![(0..len, link)], palette))
        });
        root = root.child(heading("can", &m, palette)).child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(k(&m, 14.0))
                .gap_y(k(&m, 4.0))
                .set(roles::CAP, &m)
                .text_color(palette.ink2.hsla())
                .children(caps),
        );
        root
    }
}
