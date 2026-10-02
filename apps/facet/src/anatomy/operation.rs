//! A callable as connected pieces: inputs enter the named operation and a
//! result leaves it. No source punctuation is needed to understand the path.

use super::text::{Line, Links, TypeInk};
use super::{k, roles};
use crate::measure::{Measure, Set};
use crate::semantics::types::{Spelled, Target};
use crate::tokens::Palette;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    ParentElement, PathBuilder, Pixels, Refineable, SharedString, Style, StyleRefinement, Styled, Window, div,
    point, px,
};
use std::sync::Arc;

/// Known inputs and result of one operation. Missing results stay unspecified;
/// callers must never turn an unavailable signature into a void result.
#[derive(Clone)]
pub struct Operation {
    pub name: SharedString,
    pub target: Option<Target>,
    pub inputs: Vec<Spelled>,
    pub result: Option<Spelled>,
    pub failure: Option<Option<Spelled>>,
    pub signature_known: bool,
}

/// A separately typed port, with links on its types and exact spelling under ⌥.
#[must_use]
pub fn port(id: ElementId, ty: &Spelled, measure: &Measure, links: &Links, palette: &Palette, result: bool) -> AnyElement {
    let mut line = Line::new();
    line.spelled(ty, &TypeInk::new(roles::TYPE, palette), links, measure.reveal().xray);
    div().min_w_0().flex().items_center().gap(k(measure, 6.0)).py(k(measure, 4.0))
        .child(div().flex_none().w(px(3.0)).h(k(measure, 13.0)).bg(if result { palette.mint.base.hsla() } else { palette.line1.hsla() }))
        .child(line.element(id, roles::TYPE, measure, links, palette)).into_any_element()
}

/// Compact native topology. In narrow rooms the same connected pieces wrap;
/// neither names nor author prose is ellipsized.
#[must_use]
pub fn operation(id: impl Into<ElementId>, data: Operation, measure: &Measure, links: &Links, palette: &Palette) -> gpui::Div {
    let id = id.into();
    let sub = |part: String| ElementId::NamedChild(Arc::new(id.clone()), part.into());
    let mut name = Line::new();
    if let Some(target) = data.target { name.link(&data.name, roles::ROW, palette.ink0.hsla(), target); }
    else { name.push(&data.name, roles::ROW, palette.ink0.hsla()); }
    let mut ports = div().flex().flex_wrap().items_center().min_w_0().gap(k(measure, 6.0));
    for (n, input) in data.inputs.iter().enumerate() {
        ports = ports.child(port(sub(format!("input-{n}")), input, measure, links, palette, false));
    }
    if !data.inputs.is_empty() {
        ports = ports.child(connector(measure, palette.line1.hsla(), false));
    }
    ports = ports.child(div().py(k(measure, 4.0)).px(k(measure, 6.0)).border_b_1().border_color(palette.peri.base.hsla())
        .child(name.element(sub("name".into()), roles::ROW, measure, links, palette)));
    let mut exits = div().flex().flex_col().gap(k(measure, 5.0));
    if let Some(result) = &data.result {
        exits = exits.child(div().flex().items_center().gap(k(measure, 6.0))
            .child(connector(measure, palette.mint.base.hsla(), false))
            .child(port(sub("result".into()), result, measure, links, palette, true)));
    }
    if let Some(error) = &data.failure {
        let mut failure = Line::new();
        if let Some(error) = error { failure.spelled(error, &TypeInk::new(roles::TYPE, palette), links, measure.reveal().xray); }
        else { failure.push("error type not resolved", roles::QUIET, palette.ink3.hsla()); }
        exits = exits.child(div().flex().items_center().gap(k(measure, 6.0))
            .child(connector(measure, palette.coral.base.hsla(), true))
            .child(div().set(roles::QUIET, measure).text_color(palette.ink1.hsla()).child("failure"))
            .child(failure.element(sub("failure".into()), roles::TYPE, measure, links, palette)));
    }
    if !data.signature_known {
        exits = exits.child(div().set(roles::QUIET, measure).text_color(palette.ink3.hsla()).child("signature not captured"));
    }
    ports = ports.child(exits);

    ports
}

/// A real path between ports. A bent branch means the alternative failure
/// exit; straight means the normal path. Paint is static at idle.
#[must_use]
pub fn connector(measure: &Measure, color: gpui::Hsla, branch: bool) -> impl IntoElement {
    let scale = measure.scale();
    OperationConnector { scale, color, branch, style: StyleRefinement::default() }
        .flex_none()
        .w(k(measure, 24.0))
        .h(k(measure, 20.0))
}

struct OperationConnector {
    scale: f32,
    color: Hsla,
    branch: bool,
    style: StyleRefinement,
}

impl Styled for OperationConnector {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for OperationConnector {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for OperationConnector {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Style, _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, style: &mut Style, _: &mut (), window: &mut Window, cx: &mut App) {
        let (scale, branch, color) = (self.scale, self.branch, self.color);
        style.paint(bounds, window, cx, |window, _| {
            let y = bounds.center().y;
            let mut path = PathBuilder::stroke(px(1.0));
            path.move_to(point(bounds.left(), if branch { bounds.top() } else { y }));
            if branch { path.line_to(point(bounds.left() + px(7.0 * scale), y)); }
            path.line_to(point(bounds.right() - px(4.0 * scale), y));
            path.move_to(point(bounds.right() - px(8.0 * scale), y - px(3.0 * scale)));
            path.line_to(point(bounds.right() - px(4.0 * scale), y));
            path.line_to(point(bounds.right() - px(8.0 * scale), y + px(3.0 * scale)));
            if let Ok(path) = path.build() { window.paint_path(path, color); }
        });
    }
}
