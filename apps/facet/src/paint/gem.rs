//! The gem: a kind mark cut as a twelve-facet stone. Hue = family, the
//! centred glyph = kind, and the stone *is* the progress bar — facets light
//! clockwise from 12 o'clock as work seals.
//!
//! Geometry and lighting are the boards' constants (a 48-unit viewBox):
//! an outer diamond, an inner "table" diamond, and twelve triangles between
//! them, lit from the top-left by a fixed opacity map.

use super::geom::{Fill, Poly, Pt, pt};
use crate::icons::{Kind, Stroke, variant_path};
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{
    App, Bounds, ColorExt, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled,
    TransformationMatrix, Window, point, px, size,
};

/// The twelve facets, clockwise from 12 o'clock (viewBox units).
pub const FACETS: [[(f32, f32); 3]; 12] = [
    [(24.0, 2.0), (35.0, 13.0), (24.0, 10.0)],
    [(24.0, 10.0), (35.0, 13.0), (38.0, 24.0)],
    [(35.0, 13.0), (46.0, 24.0), (38.0, 24.0)],
    [(46.0, 24.0), (35.0, 35.0), (38.0, 24.0)],
    [(38.0, 24.0), (35.0, 35.0), (24.0, 38.0)],
    [(35.0, 35.0), (24.0, 46.0), (24.0, 38.0)],
    [(24.0, 46.0), (13.0, 35.0), (24.0, 38.0)],
    [(24.0, 38.0), (13.0, 35.0), (10.0, 24.0)],
    [(13.0, 35.0), (2.0, 24.0), (10.0, 24.0)],
    [(2.0, 24.0), (13.0, 13.0), (10.0, 24.0)],
    [(10.0, 24.0), (13.0, 13.0), (24.0, 10.0)],
    [(13.0, 13.0), (24.0, 2.0), (24.0, 10.0)],
];

/// The fixed top-left lighting map: each facet's opacity when lit.
pub const LIGHT: [f32; 12] = [
    0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66,
];

/// An unlit facet's opacity.
pub const UNLIT: f32 = 0.05;

/// The outer diamond (`M24 2 46 24 24 46 2 24z`, stroke 1).
pub const OUTER: [(f32, f32); 4] = [(24.0, 2.0), (46.0, 24.0), (24.0, 46.0), (2.0, 24.0)];

/// The table (`M24 10 38 24 24 38 10 24z`, stroke .7 at 55 %).
pub const TABLE: [(f32, f32); 4] = [(24.0, 10.0), (38.0, 24.0), (24.0, 38.0), (10.0, 24.0)];

/// The hollow stone's dashed outline (`M24 3 45 24 24 45 3 24z`, 1.4, `3 3.5`).
const HOLLOW: [(f32, f32); 4] = [(24.0, 3.0), (45.0, 24.0), (24.0, 45.0), (3.0, 24.0)];

/// The glyph box: `translate(17.4 17.4) scale(.55)` of a 24-unit icon.
const GLYPH_AT: f32 = 17.4;
const GLYPH_SIZE: f32 = 24.0 * 0.55;
/// The glyph's stroke in its own 24-unit space (2.4 on the boards).
const GLYPH_STROKE: f32 = 2.4;

/// Below this size the glyph is a blur; the stone speaks alone.
pub const GLYPH_FROM: f32 = 20.0;
/// The glyph also needs this many device pixels (a 24 px gem shows its
/// glyph on a 2x display, not on a 1x one).
pub const GLYPH_MIN_DEVICE_PX: f32 = 8.0;

/// The crack across a cracked stone, from the rim into the table.
const CRACK: [(f32, f32); 4] = [(33.4, 11.4), (29.6, 13.2), (31.0, 15.4), (27.2, 17.0)];

/// What the stone says.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum GemState {
    /// Lit up to [`Gem::progress`] facets (12 = complete, 0 = to do).
    #[default]
    Normal,
    /// Not yet a stone: a dashed outline and a faint glyph.
    Hollow,
    /// Sealing: the lit facets flash in turn, clockwise (drive
    /// [`Gem::phase`] over a 2.4 s loop).
    Working,
    /// Waiting on something: amber.
    Stalled,
    /// Failed: coral, with a crack.
    Cracked,
    /// At rest but alive: a slow sparkle (drive [`Gem::phase`] over 5 s).
    Glint,
}

/// A gem element. Build with [`gem`].
pub struct Gem {
    style: StyleRefinement,
    kind: Option<Kind>,
    size: f32,
    hue: Option<Hsla>,
    state: GemState,
    progress: f32,
    phase: f32,
}

/// A 48 px gem for `kind`, complete (twelve facets lit).
#[must_use]
pub fn gem(kind: Kind) -> Gem {
    Gem {
        style: StyleRefinement::default(),
        kind: Some(kind),
        size: 48.0,
        hue: None,
        state: GemState::Normal,
        progress: 12.0,
        phase: 0.0,
    }
}

impl Gem {
    /// A bare stone with no glyph.
    #[must_use]
    pub fn bare() -> Self {
        Self {
            kind: None,
            ..gem(Kind::Unknown)
        }
    }

    /// Edge length in px (14–96 look right).
    #[must_use]
    pub const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Lit facets, `0..=12`; fractions light the next facet partway.
    #[must_use]
    pub const fn progress(mut self, facets: f32) -> Self {
        self.progress = facets;
        self
    }

    /// The state.
    #[must_use]
    pub const fn state(mut self, state: GemState) -> Self {
        self.state = state;
        self
    }

    /// Position in the state's loop (`0..1`): working 2.4 s, glint 5 s.
    #[must_use]
    pub const fn phase(mut self, phase: f32) -> Self {
        self.phase = phase;
        self
    }

    /// Overrides the family hue.
    #[must_use]
    pub fn hue(mut self, hue: impl Into<Hsla>) -> Self {
        self.hue = Some(hue.into());
        self
    }
}

/// Each facet's opacity for a state, progress and phase.
#[must_use]
pub fn facet_opacities(state: GemState, progress: f32, phase: f32) -> [f32; 12] {
    let mut out = [UNLIT; 12];
    for (i, slot) in out.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let index = i as f32;
        let lit = (progress - index).clamp(0.0, 1.0);
        let t = LIGHT[i];
        let on = match state {
            GemState::Working if lit >= 1.0 => {
                // facet-run: 2.4 s linear, delay i * 200 ms;
                // 0 % → opacity 1, 16 % → back to the facet's light.
                let local = (phase - index / 12.0).rem_euclid(1.0);
                if local < 0.16 {
                    1.0 + (t - 1.0) * (local / 0.16)
                } else {
                    t
                }
            }
            GemState::Glint if lit >= 1.0 => {
                // facet-glint: 5 s ease-in-out, delay i * 70 ms;
                // 0 %, 82 %, 100 % → t, 88 % → t + .4.
                let local = (phase - index * 0.07 / 5.0).rem_euclid(1.0);
                if (0.82..0.88).contains(&local) {
                    t + 0.4 * super::geom::ease_in_out((local - 0.82) / 0.06)
                } else if local >= 0.88 {
                    t + 0.4 * (1.0 - super::geom::ease_in_out((local - 0.88) / 0.12))
                } else {
                    t
                }
            }
            _ => t,
        };
        *slot = UNLIT + (on - UNLIT) * lit;
    }
    out
}

impl Styled for Gem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Gem {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Gem {
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
        style.size.width = px(self.size).into();
        style.size.height = px(self.size).into();
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
        cx: &mut App,
    ) {
        let palette = cx.palette();
        paint_gem(window, cx, bounds, self, palette);
    }
}

#[allow(clippy::too_many_lines)]
fn paint_gem(window: &mut Window, cx: &App, bounds: Bounds<Pixels>, gem: &Gem, palette: &Palette) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let s = w.min(h);
    if s <= 0.0 {
        return;
    }
    let k = s / 48.0;
    // Whole device pixels for the origin: at 48 px every vertex then lands
    // on the pixel grid and the 1 px rim renders as a crisp staircase.
    let scale = window.scale_factor();
    let x0 = ((f32::from(bounds.origin.x) + (w - s) * 0.5) * scale).round() / scale;
    let y0 = ((f32::from(bounds.origin.y) + (h - s) * 0.5) * scale).round() / scale;
    let at = |(x, y): (f32, f32)| pt(x0 + x * k, y0 + y * k);
    let poly = |points: &[(f32, f32)]| Poly::new(points.iter().map(|&p| at(p)));
    let opacity = gem.style.opacity.unwrap_or(1.0);

    let family: Hsla = gem
        .kind
        .map_or_else(|| palette.f_ns.hue.into(), |kind| kind.hue(palette));
    let hue = match gem.state {
        GemState::Stalled => palette.amber.base.into(),
        GemState::Cracked => palette.coral.base.into(),
        _ => gem.hue.unwrap_or(family),
    };

    let frame = Bounds::new(point(px(x0), px(y0)), size(px(s), px(s)));
    if gem.state == GemState::Hollow {
        paint_outline(window, frame, k, true, hue.opacity(0.8 * opacity));
        paint_glyph(window, cx, gem, s, x0, y0, hue.opacity(0.7 * 0.8 * opacity));
        return;
    }

    // The table sits at table level; the facets ring it.
    let table = poly(&TABLE);
    let mut fill = Fill::new();
    fill.poly(&table);
    fill.paint(window, Hsla::from(palette.table).opacity(opacity));

    let light = facet_opacities(gem.state, gem.progress, gem.phase);
    for (facet, alpha) in FACETS.iter().zip(light) {
        let mut fill = Fill::new();
        fill.triangle(at(facet[0]), at(facet[1]), at(facet[2]));
        fill.paint(window, hue.opacity(alpha.clamp(0.0, 1.0) * opacity));
    }

    paint_outline(window, frame, k, false, hue.opacity(opacity));

    if gem.state == GemState::Cracked {
        let width = (1.0 * k).max(0.9);
        let mut crack = Fill::new();
        for pair in CRACK.windows(2) {
            crack.poly(&segment(at(pair[0]), at(pair[1]), width));
        }
        crack.paint(window, Hsla::from(palette.table).opacity(opacity));
    }

    paint_glyph(window, cx, gem, s, x0, y0, hue.opacity(opacity));
}

/// The rim and the table's edge, or the hollow stone's dashed rim.
///
/// Drawn as convex quads through the path rasterizer (4x MSAA). A resvg mask
/// was measured as the alternative and gave the same 1x staircase (a lit
/// centre pixel, quarter-strength neighbours: the true geometric coverage of
/// a 1 px 45° line), only dimmer, so the cheaper, animatable paths stay.
fn paint_outline(window: &mut Window, frame: Bounds<Pixels>, k: f32, hollow: bool, color: Hsla) {
    let x0 = f32::from(frame.origin.x);
    let y0 = f32::from(frame.origin.y);
    let poly =
        |points: &[(f32, f32)]| Poly::new(points.iter().map(|&(x, y)| pt(x0 + x * k, y0 + y * k)));
    // The boards' 1 / .7 / 1.4 units, floored so a small stone keeps an edge.
    if hollow {
        let mut dashes = Fill::new();
        let unit = k.max(0.55);
        for piece in dashed_ring(&poly(&HOLLOW), (1.4 * k).max(1.0), 3.0 * unit, 3.5 * unit) {
            dashes.poly(&piece);
        }
        dashes.paint(window, color);
        return;
    }
    let mut rim = Fill::new();
    for quad in poly(&OUTER).stroke_ring(k.max(0.8)) {
        rim.poly(&quad);
    }
    rim.paint(window, color);
    let mut inner = Fill::new();
    for quad in poly(&TABLE).stroke_ring((0.7 * k).max(0.6)) {
        inner.poly(&quad);
    }
    inner.paint(window, color.opacity(0.55));
}

/// A dashed closed outline: dashes of `on` every `on + off`, continuous
/// around corners (a dash that turns a corner is split there).
#[allow(clippy::many_single_char_names)]
fn dashed_ring(outline: &Poly, width: f32, on: f32, off: f32) -> Vec<Poly> {
    let pts = outline.points();
    let n = pts.len();
    let mut out = Vec::new();
    let period = on + off;
    if n < 2 || on <= 0.0 || period <= 0.0 {
        return out;
    }
    let mut edges = Vec::with_capacity(n);
    let mut start = 0.0_f32;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        edges.push((a, b, start, len));
        start += len;
    }
    let perimeter = start;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = (perimeter / period).ceil() as usize;
    for k in 0..count {
        #[allow(clippy::cast_precision_loss)]
        let d0 = k as f32 * period;
        let d1 = (d0 + on).min(perimeter);
        for &(a, b, s0, len) in &edges {
            let lo = d0.max(s0);
            let hi = d1.min(s0 + len);
            if hi - lo > 1e-3 && len > 0.0 {
                let p = |d: f32| {
                    pt(
                        a.x + (b.x - a.x) * (d - s0) / len,
                        a.y + (b.y - a.y) * (d - s0) / len,
                    )
                };
                out.push(segment(p(lo), p(hi), width));
            }
        }
    }
    out
}

fn paint_glyph(window: &mut Window, cx: &App, gem: &Gem, s: f32, x0: f32, y0: f32, color: Hsla) {
    let Some(kind) = gem.kind else { return };
    let k = s / 48.0;
    let side = GLYPH_SIZE * k;
    // Below ~8 device px the glyph is a blot: the stone speaks alone.
    if s < GLYPH_FROM || side * window.scale_factor() < GLYPH_MIN_DEVICE_PX {
        return;
    }
    // Keep the glyph's stroke at least ~1 px on screen.
    let stroke = GLYPH_STROKE.max(24.0 / side);
    let path = variant_path(
        kind.path(),
        Stroke {
            width: (stroke * 10.0).round() / 10.0,
            facet: Some(0.5),
            dashed: false,
        },
    );
    let glyph = Bounds::new(
        point(px(x0 + GLYPH_AT * k), px(y0 + GLYPH_AT * k)),
        size(px(side), px(side)),
    );
    // A missing asset source only loses the glyph; the stone still speaks.
    window
        .paint_svg(glyph, path, None, TransformationMatrix::unit(), color, cx)
        .ok();
}

/// A straight segment `a → b` of width `w` as one quad.
fn segment(a: Pt, b: Pt, w: f32) -> Poly {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (nx, ny) = (-dy / len * w * 0.5, dx / len * w * 0.5);
    // Clockwise on screen: along the right-hand side first.
    let quad = Poly::new([
        pt(a.x - nx, a.y - ny),
        pt(b.x - nx, b.y - ny),
        pt(b.x + nx, b.y + ny),
        pt(a.x + nx, a.y + ny),
    ]);
    if quad.area() < 0.0 {
        Poly::new(quad.points().iter().rev().copied())
    } else {
        quad
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn facets_tile_the_ring_between_rim_and_table() {
        let area = |points: &[(f32, f32)]| Poly::new(points.iter().map(|&(x, y)| pt(x, y))).area();
        let facets: f32 = FACETS.iter().map(|f| area(f).abs()).sum();
        let ring = area(&OUTER) - area(&TABLE);
        assert!((facets - ring).abs() < 1e-3, "{facets} vs {ring}");
    }

    #[test]
    fn the_facet_map_is_the_boards() {
        assert_eq!(FACETS[0], [(24.0, 2.0), (35.0, 13.0), (24.0, 10.0)]);
        assert_eq!(FACETS[11], [(13.0, 13.0), (24.0, 2.0), (24.0, 10.0)]);
        assert_eq!(
            LIGHT,
            [
                0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66
            ]
        );
        // Lit from the top-left: the brightest facet is on the top-left edge,
        // the dimmest on the bottom-right.
        let brightest = LIGHT.iter().copied().fold(0.0, f32::max);
        let dimmest = LIGHT.iter().copied().fold(1.0, f32::min);
        assert!((LIGHT[11] - brightest).abs() < 1e-6);
        assert!((LIGHT[4] - dimmest).abs() < 1e-6);
        // Clockwise from 12 o'clock: each facet's centroid angle increases.
        let angle = |f: &[(f32, f32); 3]| {
            let (cx, cy) = f
                .iter()
                .fold((0.0, 0.0), |(x, y), p| (x + p.0 / 3.0, y + p.1 / 3.0));
            (cx - 24.0)
                .atan2(-(cy - 24.0))
                .rem_euclid(std::f32::consts::TAU)
        };
        for pair in FACETS.windows(2) {
            assert!(angle(&pair[1]) > angle(&pair[0]));
        }
    }

    #[test]
    fn progress_lights_facets_in_order() {
        let four = facet_opacities(GemState::Normal, 4.0, 0.0);
        assert_eq!(&four[..4], &LIGHT[..4]);
        assert!(four[4..].iter().all(|&o| (o - UNLIT).abs() < 1e-6));
        let todo = facet_opacities(GemState::Normal, 0.0, 0.0);
        assert!(todo.iter().all(|&o| (o - UNLIT).abs() < 1e-6));
        let half = facet_opacities(GemState::Normal, 4.5, 0.0);
        assert!((half[4] - (UNLIT + (LIGHT[4] - UNLIT) * 0.5)).abs() < 1e-6);
    }

    #[test]
    fn working_sweeps_clockwise() {
        // At phase 0 facet 0 flashes; a twelfth later facet 1 does.
        let a = facet_opacities(GemState::Working, 12.0, 0.0);
        assert!((a[0] - 1.0).abs() < 1e-6 && a[1] < 1.0);
        let b = facet_opacities(GemState::Working, 12.0, 1.0 / 12.0);
        assert!((b[1] - 1.0).abs() < 1e-4 && b[0] < 1.0);
        // Unlit facets never flash.
        let c = facet_opacities(GemState::Working, 8.0, 9.0 / 12.0);
        assert!((c[9] - UNLIT).abs() < 1e-6);
    }

    #[test]
    fn dashes_cover_the_duty_cycle() {
        let outline = Poly::new(HOLLOW.iter().map(|&(x, y)| pt(x, y)));
        let dashes = dashed_ring(&outline, 1.0, 3.0, 3.5);
        let covered: f32 = dashes.iter().map(Poly::area).sum();
        let perimeter = 4.0 * (21.0_f32 * 21.0 * 2.0).sqrt();
        assert!((covered - perimeter * 3.0 / 6.5).abs() < 3.0, "{covered}");
        assert!(dashes.iter().all(|d| d.area() > 0.0));
        // Scaled outlines terminate too (a float step once stalled here).
        for k in [0.29, 0.37, 0.5, 0.917, 1.33, 2.0] {
            let scaled = outline.transform(k, 3.7, 11.3);
            assert!(!dashed_ring(&scaled, 1.0, 3.0 * k, 3.5 * k).is_empty());
        }
    }
}
