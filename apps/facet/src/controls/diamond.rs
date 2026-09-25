//! The diamond: FACET's one toggle shape ("no pills, no rounded pucks").
//! A 45°-rotated square inscribed in its box (inset on all four sides),
//! flat-filled and/or outlined. The switch thumb, the checkbox and the
//! radio family are all this one primitive at different sizes and states —
//! the same motif as the altimeter's `.d` marker and the gem (§5.8 "the
//! diamond → the gem").

use crate::paint::geom::{Fill, Poly, fill_poly, pt};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window,
};

/// A diamond painted inside its box. Size it like a div.
pub(crate) struct Diamond {
    style: StyleRefinement,
    inset: f32,
    fill: Option<Hsla>,
    outline: Option<(Hsla, f32)>,
}

/// A diamond with no fill or outline yet (paints nothing until configured).
pub(crate) fn diamond() -> Diamond {
    Diamond {
        style: StyleRefinement::default(),
        inset: 0.0,
        fill: None,
        outline: None,
    }
}

impl Diamond {
    /// Shrinks the diamond this many px in from its box on every side.
    pub(crate) const fn inset(mut self, inset: f32) -> Self {
        self.inset = inset;
        self
    }

    /// Fills the diamond flat.
    pub(crate) fn fill(mut self, color: Hsla) -> Self {
        self.fill = Some(color);
        self
    }

    /// Strokes the diamond's outline, centred on it, `width` px wide.
    pub(crate) fn outline(mut self, color: Hsla, width: f32) -> Self {
        self.outline = Some((color, width));
        self
    }
}

fn shape(bounds: Bounds<Pixels>, inset: f32) -> Option<Poly> {
    let x = f32::from(bounds.origin.x) + inset;
    let y = f32::from(bounds.origin.y) + inset;
    let w = f32::from(bounds.size.width) - 2.0 * inset;
    let h = f32::from(bounds.size.height) - 2.0 * inset;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    Some(Poly::new([
        pt(cx, y),
        pt(x + w, cy),
        pt(cx, y + h),
        pt(x, cy),
    ]))
}

impl Styled for Diamond {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Diamond {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Diamond {
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
        let Some(poly) = shape(bounds, self.inset) else {
            return;
        };
        if let Some(fill) = self.fill
            && fill.alpha > 0.0
        {
            fill_poly(window, &poly, fill);
        }
        if let Some((color, width)) = self.outline
            && color.alpha > 0.0
            && width > 0.0
        {
            let mut f = Fill::new();
            for ring in poly.stroke_ring(width) {
                f.poly(&ring);
            }
            f.paint(window, color);
        }
    }
}
