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
    InteractiveElement,
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    ParentElement, PathBuilder, Pixels, Refineable, RenderOnce, SharedString, Style, StyleRefinement,
    Styled, StyledText, Window, div, point, px,
};
use std::sync::Arc;

/// The `can` line. Build with [`can`].
#[derive(IntoElement)]
pub struct Can {
    id: ElementId,
    caps: Vec<Cap>,
    measure: Measure,
    bare: bool,
}

/// The `can` line for `caps` at `measure` (nothing when there are none).
#[must_use]
pub fn can(id: impl Into<ElementId>, caps: Vec<Cap>, measure: &Measure) -> Can {
    Can { id: id.into(), caps, measure: *measure, bare: false }
}

impl Can {
    /// Without the `can` heading (the graph's focus card has no section
    /// heads): the same marks, words and tips.
    #[must_use]
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }
}

fn mark(arrives: &Arrives, measure: &Measure, palette: &crate::tokens::Palette) -> impl IntoElement {
    let s = measure.scale();
    let (fill, stroke, dashed) = match arrives {
        Arrives::Derived => (None, Some(palette.ink3.hsla()), false),
        Arrives::Written => (Some(palette.ink1.hsla()), None, false),
        Arrives::Via(_) => (None, Some(palette.ink4.hsla()), true),
    };
    CapabilityMark { scale: s, fill, stroke, dashed, style: StyleRefinement::default() }
        .flex_none()
        .size(px(9.0 * s))
}

struct CapabilityMark {
    scale: f32,
    fill: Option<Hsla>,
    stroke: Option<Hsla>,
    dashed: bool,
    style: StyleRefinement,
}

impl Styled for CapabilityMark {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for CapabilityMark {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for CapabilityMark {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = px(9.0 * self.scale).into();
        style.size.height = px(9.0 * self.scale).into();
        style.flex_shrink = 0.0;
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        let center = bounds.center();
        let radius = px(3.6 * self.scale);
        let diamond = |path: &mut PathBuilder, radius| {
            path.move_to(point(center.x, center.y - radius));
            path.line_to(point(center.x + radius, center.y));
            path.line_to(point(center.x, center.y + radius));
            path.line_to(point(center.x - radius, center.y));
            path.close();
        };
        if let Some(color) = self.fill {
            let mut path = PathBuilder::fill();
            diamond(&mut path, radius);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        if let Some(color) = self.stroke {
            let mut path = PathBuilder::stroke(px(1.2));
            let radius = if self.dashed {
                path = path.dash_array(&[px(1.5), px(1.5)]);
                radius + px(1.0)
            } else {
                radius - px(0.6)
            };
            diamond(&mut path, radius);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
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
        if !self.bare {
            root = root.child(heading(
                ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::new_static("heading")),
                "can",
                &m,
                palette,
            ));
        }
        root = root.child(
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
