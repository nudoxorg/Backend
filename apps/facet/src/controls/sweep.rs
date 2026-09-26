//! The facet sweep: a translucent light wedge that crosses a cut plate once
//! per hover-enter (`Controls.dc.html`'s `.btn:hover` effect —
//! `clip-path:polygon(0 0,38% 0,14% 100%,0 100%)` sliding from
//! `translateX(-45%)` to `translateX(250%)` over `--t-emph`/`--glide`).
//! Paints nothing at either end (`t <= 0` or `t >= 1`: the wedge is off the
//! plate), so a settled button paints and requests nothing extra.
//!
//! The light is the palette's bevel light at the intent's strength: the
//! same light that catches the top-left edge crosses the face.

use crate::paint::geom::{Fill, Poly, pt};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window,
};

/// A sweep overlay: size it like a div (normally `.absolute().inset_0()`
/// inside the plate it crosses).
pub(crate) struct Sweep {
    style: StyleRefinement,
    t: f32,
    chamfer: f32,
    light: Hsla,
}

/// A sweep at progress `t` (`0` = not started, `1` = fully crossed; both
/// invisible), clipped to a cut plate of `chamfer` px, painted in `light`.
pub(crate) fn sweep(t: f32, chamfer: f32, light: Hsla) -> Sweep {
    Sweep {
        style: StyleRefinement::default(),
        t: t.clamp(0.0, 1.0),
        chamfer,
        light,
    }
}

impl Styled for Sweep {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Sweep {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Sweep {
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
        if self.t <= 0.0 || self.t >= 1.0 || self.light.alpha <= 0.0 {
            return;
        }
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let clip = Poly::chamfer(x, y, w, h, self.chamfer);
        let dx = (-0.45 + 2.95 * self.t) * w;
        let wedge = Poly::new([
            pt(x + dx, y),
            pt(x + dx + 0.38 * w, y),
            pt(x + dx + 0.14 * w, y + h),
            pt(x + dx, y + h),
        ]);
        let piece = wedge.clip(&clip);
        if piece.is_empty() {
            return;
        }
        let mut fill = Fill::new();
        fill.poly(&piece);
        fill.paint(window, self.light);
    }
}
