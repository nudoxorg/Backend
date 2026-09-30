//! A page chip or card plate: the exact chamfer and centred ring used by the
//! original page drawing, painted through GPUI's measured Element lifecycle.

use crate::paint::geom::{Poly, fill_poly};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window,
};

/// The page's chamfered plate behind chip or card content.
pub(super) fn plate(fill: Hsla, ring: Hsla, chamfer: f32, thickness: f32) -> AnyElement {
    PagePlate {
        fill,
        ring,
        chamfer,
        thickness,
        style: StyleRefinement::default(),
    }
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

struct PagePlate {
    fill: Hsla,
    ring: Hsla,
    chamfer: f32,
    thickness: f32,
    style: StyleRefinement,
}

impl Styled for PagePlate {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for PagePlate {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PagePlate {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (fill, ring, chamfer, thickness) = (self.fill, self.ring, self.chamfer, self.thickness);
        style.paint(bounds, window, cx, |window, _| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let poly = Poly::chamfer(x + 0.5, y + 0.5, w - 1.0, h - 1.0, chamfer);
            fill_poly(window, &poly, fill);
            for edge in poly.stroke_ring(thickness) {
                fill_poly(window, &edge, ring);
            }
        });
    }
}
