//! The hatch: FACET's one texture for "not yet solid" — pending, sampled,
//! inferred, busy. A repeating stripe clipped to any convex polygon.
//!
//! The stripes are positioned exactly like a CSS
//! `repeating-linear-gradient(<angle>, color 0 <on>, transparent <on> <period>)`
//! laid over a frame box: the gradient line runs through the box centre at
//! `angle` (0° = to top, 90° = to right), and position 0 is the corner the
//! gradient starts from. So a hatch painted here lines up with the boards
//! stripe for stripe.

use super::geom::{Fill, Poly, Pt, pt};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window,
};

/// One stripe pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hatch {
    /// CSS gradient angle in degrees: 90 = vertical stripes marching right,
    /// 135 = diagonal stripes running bottom-left to top-right.
    pub angle: f32,
    /// Painted width of each stripe, px, measured along the gradient line.
    pub on: f32,
    /// Stripe period, px.
    pub period: f32,
    /// Running offset as a fraction of one period (`0..1`); animate it for
    /// the "working" hatch. Positive moves stripes along the gradient line.
    pub phase: f32,
}

impl Hatch {
    /// The universal pending texture, `--hatch`: 90°, 1.5 px every 5 px.
    #[must_use]
    pub const fn pending() -> Self {
        Self::vertical(1.5, 5.0)
    }

    /// The ghost bevel: 135°, 4 px every 8 px.
    #[must_use]
    pub const fn ghost() -> Self {
        Self::diagonal(4.0, 8.0)
    }

    /// The weave under a plate: 135°, 1 px every 9 px.
    #[must_use]
    pub const fn weave() -> Self {
        Self::diagonal(1.0, 9.0)
    }

    /// The seam's working/stalled stripe: 90°, 4 px every 8 px.
    #[must_use]
    pub const fn seam() -> Self {
        Self::vertical(4.0, 8.0)
    }

    /// Vertical stripes (a 90° gradient).
    #[must_use]
    pub const fn vertical(on: f32, period: f32) -> Self {
        Self {
            angle: 90.0,
            on,
            period,
            phase: 0.0,
        }
    }

    /// Diagonal stripes (a 135° gradient).
    #[must_use]
    pub const fn diagonal(on: f32, period: f32) -> Self {
        Self {
            angle: 135.0,
            on,
            period,
            phase: 0.0,
        }
    }

    /// Horizontal stripes (a 0° gradient).
    #[must_use]
    pub const fn horizontal(on: f32, period: f32) -> Self {
        Self {
            angle: 0.0,
            on,
            period,
            phase: 0.0,
        }
    }

    /// The same pattern at another angle.
    #[must_use]
    pub const fn angle(mut self, degrees: f32) -> Self {
        self.angle = degrees;
        self
    }

    /// The same pattern shifted by `phase` periods (running hatch).
    #[must_use]
    pub const fn phase(mut self, phase: f32) -> Self {
        self.phase = phase;
        self
    }

    /// The stripes of this pattern laid over the frame box
    /// `(origin, width, height)`, clipped to `clip`.
    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub fn stripes(&self, clip: &Poly, origin: Pt, w: f32, h: f32) -> Vec<Poly> {
        let mut out = Vec::new();
        if self.period <= 0.0 || self.on <= 0.0 || clip.is_empty() {
            return out;
        }
        let rad = self.angle.to_radians();
        let u = pt(rad.sin(), -rad.cos());
        let span = (w * u.x).abs() + (h * u.y).abs();
        let centre = pt(origin.x + w * 0.5, origin.y + h * 0.5);
        // Absolute projection of the gradient's 0 % point.
        let zero = centre.x * u.x + centre.y * u.y - span * 0.5;
        let (min, max) = clip.bounds();
        let corners = [min, pt(max.x, min.y), max, pt(min.x, max.y)];
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for c in corners {
            let d = c.x * u.x + c.y * u.y;
            lo = lo.min(d);
            hi = hi.max(d);
        }
        let shift = self.phase.rem_euclid(1.0) * self.period;
        let first = ((lo - zero - shift) / self.period).floor() - 1.0;
        let last = ((hi - zero - shift) / self.period).ceil() + 1.0;
        let mut k = first;
        while k <= last {
            let a = zero + shift + k * self.period;
            let piece = clip.clip_band(u, a, a + self.on.min(self.period));
            if !piece.is_empty() {
                out.push(piece);
            }
            k += 1.0;
        }
        out
    }

    /// Paints the stripes over `frame`, clipped to `clip`, in `color`.
    pub fn paint(&self, window: &mut Window, clip: &Poly, frame: Bounds<Pixels>, color: Hsla) {
        if color.alpha <= 0.0 {
            return;
        }
        let mut fill = Fill::new();
        let origin = pt(f32::from(frame.origin.x), f32::from(frame.origin.y));
        for stripe in self.stripes(
            clip,
            origin,
            f32::from(frame.size.width),
            f32::from(frame.size.height),
        ) {
            fill.poly(&stripe);
        }
        fill.paint(window, color);
    }
}

/// An element that fills its box (optionally chamfered) with a hatch.
///
/// ```ignore
/// hatch_fill(Hatch::pending(), palette.ink2.into()).w(px(120.)).h(px(8.))
/// ```
pub struct HatchFill {
    style: StyleRefinement,
    hatch: Hatch,
    color: Hsla,
    chamfer: f32,
}

/// A box filled with `hatch` in `color`. Size it like a div.
#[must_use]
pub fn hatch_fill(hatch: Hatch, color: Hsla) -> HatchFill {
    HatchFill {
        style: StyleRefinement::default(),
        hatch,
        color,
        chamfer: 0.0,
    }
}

impl HatchFill {
    /// Clips the hatch to the FACET cut with this chamfer (px).
    #[must_use]
    pub const fn chamfer(mut self, chamfer: f32) -> Self {
        self.chamfer = chamfer;
        self
    }
}

impl Styled for HatchFill {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for HatchFill {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for HatchFill {
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
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let clip = Poly::chamfer(x, y, w, h, self.chamfer);
        self.hatch.paint(window, &clip, bounds, self.color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_stripes_match_the_css_grid() {
        let rect = Poly::rect(0.0, 0.0, 20.0, 4.0);
        let stripes = Hatch::pending().stripes(&rect, pt(0.0, 0.0), 20.0, 4.0);
        // Stripes start at x = 0, 5, 10, 15, each 1.5 wide.
        let mut starts: Vec<f32> = stripes.iter().map(|s| s.bounds().0.x).collect();
        starts.sort_by(f32::total_cmp);
        assert_eq!(starts.len(), 4);
        for (i, x) in starts.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let expected = i as f32 * 5.0;
            assert!((x - expected).abs() < 1e-3, "{starts:?}");
        }
        let covered: f32 = stripes.iter().map(Poly::area).sum();
        assert!((covered - 4.0 * 1.5 * 4.0).abs() < 1e-3);
    }

    #[test]
    fn coverage_equals_duty_cycle() {
        let clip = Poly::chamfer(3.0, 7.0, 180.0, 64.0, 14.0);
        for hatch in [
            Hatch::ghost(),
            Hatch::pending(),
            Hatch::weave(),
            Hatch::horizontal(2.0, 4.0),
        ] {
            for phase in [0.0, 0.25, 0.8] {
                let h = hatch.phase(phase);
                let covered: f32 = h
                    .stripes(&clip, pt(3.0, 7.0), 180.0, 64.0)
                    .iter()
                    .map(Poly::area)
                    .sum();
                let duty = h.on / h.period;
                let ratio = covered / clip.area();
                assert!((ratio - duty).abs() < 0.03, "{h:?}: {ratio} vs {duty}");
            }
        }
    }

    #[test]
    fn diagonal_stripes_start_at_the_top_left_corner() {
        // A 135° gradient's 0 % point is the top-left corner: the first
        // stripe covers the corner itself.
        let clip = Poly::rect(0.0, 0.0, 40.0, 40.0);
        let stripes = Hatch::ghost().stripes(&clip, pt(0.0, 0.0), 40.0, 40.0);
        assert!(stripes.iter().any(|s| s.contains(pt(0.2, 0.2))));
        // 4 px along the diagonal from the corner is the first gap.
        let gap = 5.0 / std::f32::consts::SQRT_2;
        assert!(!stripes.iter().any(|s| s.contains(pt(gap, gap))));
    }
}
