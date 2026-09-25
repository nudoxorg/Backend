//! The cut plate: every raised surface in Nudox.
//!
//! One 45° chamfer top-left, one bottom-right; a flat plate; a 1 px *hard*
//! two-tone bevel around it — lit top-left, shaded bottom-right, the split
//! running corner to corner along the top-right → bottom-left diagonal,
//! exactly `linear-gradient(to bottom right, hi 50%, lo 50%)`. The bevel is
//! the only state channel: focus doubles it in periwinkle, mint says yours,
//! a light travels it while working, amber waits, coral stops, a hatch says
//! pending.
//!
//! [`Cut`] is a container that behaves like a `div` (layout, padding,
//! children, listeners) and paints the plate under its children.
//! [`paint_cut`] paints the same plate from any custom element.

use super::geom::{Fill, Poly, Pt, clip_wedge, pt};
use super::hatch::Hatch;
use super::{clear, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{ABYSS, Palette, Tone, geo, hexa};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, ColorExt, Corners, Div, DivFrameState, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, InteractiveElement,
    Interactivity, IntoElement, LayoutId, MouseMoveEvent, ParentElement, Pixels, Point,
    StyleRefinement, Styled, Window, div, point, px, size,
};
use std::rc::Rc;

/// The chamfer sizes of the boards.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Chamfer {
    /// 9 px: rows, chips, compact plates (`.cut.sm`).
    Sm,
    /// 14 px: the default plate (`.cut`).
    #[default]
    Md,
    /// 22 px: dialogs, the ask plate (`.cut.lg`).
    Lg,
    /// 8 px: buttons (`.btn.cutb`).
    Button,
    /// 10 px: the here capsule and floating plates.
    Float,
    /// 6 px: tooltips, comb popups, nodules.
    Tip,
    /// 3 px: mosaic stones.
    Stone,
    /// Any other size, px.
    Px(f32),
}

impl Chamfer {
    /// The cut size in px.
    #[must_use]
    pub fn px(self) -> f32 {
        f32::from(match self {
            Self::Sm => geo::CUT_SM,
            Self::Md => geo::CUT,
            Self::Lg => geo::CUT_LG,
            Self::Button => geo::CUT_BTN,
            Self::Float => geo::CUT_FLOAT,
            Self::Tip => geo::CUT_TIP,
            Self::Stone => geo::CUT_STONE,
            Self::Px(value) => return value,
        })
    }
}

/// What the bevel says (the board's state table).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Bevel {
    /// Lit top-left, shaded bottom-right.
    #[default]
    Rest,
    /// Periwinkle, single width (`.cut.peri`).
    Peri,
    /// Keyboard focus: doubled, periwinkle.
    Focus,
    /// Yours: mint, your code reaches this.
    Hot,
    /// Working: light travels the edge (drive [`Cut::phase`]).
    Run,
    /// Waiting: amber, and still.
    Amber,
    /// Stopped: coral, with one reason.
    Coral,
    /// Pending: hatched — sampled, not sealed.
    Ghost,
}

/// The shaded side of the voiced bevels. The boards spell these as literal
/// dark-theme colours at 25 % in both appearances, so they are not palette
/// tokens.
const HOT_LO: Tone = hexa(0x0062_e6a6, 0.25);
const AMBER_LO: Tone = hexa(0x00f4_bb6a, 0.25);
const CORAL_LO: Tone = hexa(0x00ff_7a8a, 0.25);

/// The travelling light's share of the edge: `conic-gradient(mint 0 18%, …)`.
const RUN_SPAN: f32 = 0.18;

/// A fully resolved bevel: everything the edge paints, as numbers, so a
/// state change can be blended frame by frame ([`Edge::mix`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// The lit (top-left) half.
    pub hi: Hsla,
    /// The shaded (bottom-right) half.
    pub lo: Hsla,
    /// Rim width in px: 1 at rest, 2 when focused.
    pub rim: f32,
    /// Strength of the travelling light, `0..=1`.
    pub run: f32,
    /// The travelling light's colour.
    pub light: Hsla,
    /// Strength of the hatched (pending) rim, `0..=1`; the solid halves
    /// fade out as it rises.
    pub ghost: f32,
    /// The hatch colour of the pending rim.
    pub hatch: Hsla,
}

impl Edge {
    /// The edge a bevel state paints in a palette.
    #[must_use]
    pub fn of(bevel: Bevel, palette: &Palette) -> Self {
        let rest = Self {
            hi: palette.bevel_hi.into(),
            lo: palette.bevel_lo.into(),
            rim: f32::from(geo::BEVEL),
            run: 0.0,
            light: palette.mint.base.into(),
            ghost: 0.0,
            hatch: palette.line3.into(),
        };
        match bevel {
            Bevel::Rest => rest,
            Bevel::Peri => Self {
                hi: palette.peri_hi.into(),
                lo: palette.peri.base.into(),
                ..rest
            },
            Bevel::Focus => Self {
                hi: palette.peri_hi.into(),
                lo: palette.peri.base.into(),
                rim: f32::from(geo::BEVEL) * 2.0,
                ..rest
            },
            Bevel::Hot => Self {
                hi: palette.mint.base.into(),
                lo: HOT_LO.into(),
                ..rest
            },
            Bevel::Run => Self {
                hi: palette.bevel_lo.into(),
                lo: palette.bevel_lo.into(),
                run: 1.0,
                ..rest
            },
            Bevel::Amber => Self {
                hi: palette.amber.base.into(),
                lo: AMBER_LO.into(),
                ..rest
            },
            Bevel::Coral => Self {
                hi: palette.coral.base.into(),
                lo: CORAL_LO.into(),
                ..rest
            },
            Bevel::Ghost => Self {
                hi: clear(),
                lo: clear(),
                ghost: 1.0,
                ..rest
            },
        }
    }

    /// The edge `t` of the way from `self` to `other` (for animated state
    /// changes; drive `t` from the motion engine).
    #[must_use]
    pub fn mix(self, other: Self, t: f32) -> Self {
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        Self {
            hi: mix(self.hi, other.hi, t),
            lo: mix(self.lo, other.lo, t),
            rim: lerp(self.rim, other.rim),
            run: lerp(self.run, other.run),
            light: mix(self.light, other.light, t),
            ghost: lerp(self.ghost, other.ghost),
            hatch: mix(self.hatch, other.hatch, t),
        }
    }
}

/// What sits inside the bevel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Plate {
    /// One flat tone (`--plate`, or [`Cut::fill`]).
    #[default]
    Flat,
    /// A header tone over a body tone, split on the diagonal (`.cut.two`).
    Two,
    /// Recessed to table level (`.cut.deep`).
    Deep,
    /// The plate with the faint diagonal weave (`.cut.weave`).
    Weave,
}

/// Everything [`paint_cut`] needs; build with [`CutPaint::new`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutPaint {
    /// Chamfer, px.
    pub chamfer: f32,
    /// The bevel.
    pub edge: Edge,
    /// Where the travelling light is, as a fraction of a turn (`0..1`,
    /// clockwise from 12 o'clock). One turn takes 1.6 s on the boards.
    pub phase: f32,
    /// The plate inside the bevel.
    pub plate: Plate,
    /// Overrides the plate tone.
    pub fill: Option<Hsla>,
    /// Paints the floating drop shadow (tooltips, popups, menus, toasts,
    /// dialogs: "floating things carry their own shadow").
    pub floating: bool,
    /// Multiplies every colour (the element's own opacity).
    pub opacity: f32,
}

impl CutPaint {
    /// A resting plate with the default chamfer in `palette`.
    #[must_use]
    pub fn new(palette: &Palette) -> Self {
        Self {
            chamfer: Chamfer::Md.px(),
            edge: Edge::of(Bevel::Rest, palette),
            phase: 0.0,
            plate: Plate::Flat,
            fill: None,
            floating: false,
            opacity: 1.0,
        }
    }
}

/// The chamfered outline of a plate in `bounds`.
#[must_use]
pub fn outline(bounds: Bounds<Pixels>, chamfer: f32) -> Poly {
    let (x, y, w, h) = xywh(bounds);
    Poly::chamfer(x, y, w, h, chamfer)
}

/// Whether `point` hits the plate in `bounds`: the cut-away corners do not
/// count.
#[must_use]
pub fn contains(bounds: Bounds<Pixels>, chamfer: f32, point: Point<Pixels>) -> bool {
    outline(bounds, chamfer).contains(pt(f32::from(point.x), f32::from(point.y)))
}

/// `bounds` with its edges moved to the nearest device pixel.
#[must_use]
pub fn snap(bounds: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    let (x, y, w, h) = xywh(bounds);
    let round = |v: f32| (v * scale).round() / scale;
    let (x0, y0) = (round(x), round(y));
    let (x1, y1) = (round(x + w), round(y + h));
    Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)))
}

fn xywh(bounds: Bounds<Pixels>) -> (f32, f32, f32, f32) {
    (
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

/// Paints a cut plate into `bounds`: shadow (when floating), bevel, plate.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn paint_cut(window: &mut Window, bounds: Bounds<Pixels>, spec: &CutPaint, palette: &Palette) {
    // Snap to device pixels like quads do: a straight rim that straddles a
    // pixel boundary smears into two half-lit rows.
    let bounds = snap(bounds, window.scale_factor());
    let (x, y, w, h) = xywh(bounds);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let c = spec.chamfer;
    let o = spec.opacity;
    let outer = Poly::chamfer(x, y, w, h, c);

    if spec.floating {
        paint_float_shadow(window, bounds, c, palette, o);
    }

    // The bevel: the whole outline in the edge colours; the plate covers
    // all but the rim.
    let edge = &spec.edge;
    let solid = (1.0 - edge.ghost).clamp(0.0, 1.0);
    let hi = edge.hi.opacity(solid * o);
    let lo = edge.lo.opacity(solid * o);
    // An opaque plate covers everything but the rim, so the bevel is the
    // whole outline split on the diagonal. A plate that can be seen through
    // (a translucent fill, or the element fading) would show those halves
    // across its face, so there the bevel is painted as a ring instead.
    let see_through = o < 0.999
        || spec.fill.is_some_and(|fill| fill.alpha < 0.999)
        || matches!(spec.plate, Plate::Flat | Plate::Two | Plate::Weave)
            && spec.fill.is_none()
            && Hsla::from(palette.plate).alpha < 0.999;
    let (hi_parts, lo_parts) = bevel_parts(&outer, (x, y, w, h), c, edge.rim, see_through);
    if edge.run > 0.0 {
        let centre = pt(x + w * 0.5, y + h * 0.5);
        let turn = std::f32::consts::TAU;
        let from = spec.phase.rem_euclid(1.0) * turn;
        let to = from + RUN_SPAN * turn;
        for (parts, base) in [(&hi_parts, hi), (&lo_parts, lo)] {
            let lit = mix(base, edge.light.opacity(o), edge.run);
            let mut dark = Fill::new();
            let mut light = Fill::new();
            for half in parts {
                for piece in clip_wedge(half, centre, to, from + turn) {
                    dark.poly(&piece);
                }
                for piece in clip_wedge(half, centre, from, to) {
                    light.poly(&piece);
                }
            }
            dark.paint(window, base);
            light.paint(window, lit);
        }
    } else {
        for (parts, color) in [(&hi_parts, hi), (&lo_parts, lo)] {
            for part in parts {
                paint_poly(window, part, color);
            }
        }
    }
    if edge.ghost > 0.0 {
        Hatch::ghost().paint(window, &outer, bounds, edge.hatch.opacity(edge.ghost * o));
    }

    // The plate: inset by the rim, its chamfer `c - .5` inside the inset box
    // (the boards' `::before` clip-path), so the diagonal rim matches the
    // straight rims.
    let rim = edge.rim.max(0.0);
    let (ix, iy, iw, ih) = (x + rim, y + rim, w - 2.0 * rim, h - 2.0 * rim);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let inner = Poly::chamfer(ix, iy, iw, ih, c - 0.5);
    let base: Hsla = match spec.plate {
        Plate::Deep => spec.fill.unwrap_or_else(|| palette.table.into()),
        _ => spec.fill.unwrap_or_else(|| palette.plate.into()),
    };
    paint_poly(window, &inner, base.opacity(o));
    match spec.plate {
        Plate::Flat | Plate::Deep => {}
        Plate::Two => {
            let header = inner.clip_half_plane(pt(ix + iw, iy), pt(ix, iy + ih));
            let tone: Hsla = palette.plate2.into();
            paint_poly(window, &header, tone.opacity(o));
        }
        Plate::Weave => {
            // `--weave`: the ground hue at 2.8 % (Abyss) / 3 % (Glacier).
            let strength = if palette.g1 == ABYSS.g1 { 0.028 } else { 0.03 };
            let inner_box = Bounds::new(point(px(ix), px(iy)), size(px(iw), px(ih)));
            Hatch::weave().paint(
                window,
                &inner,
                inner_box,
                Hsla::from(palette.ground.alpha(strength)).opacity(o),
            );
        }
    }
}

/// The bevel's lit and shaded pieces: the outline halves (under an opaque
/// plate), or the rim alone — one quad per edge between the outline and the
/// plate's own outline, each split on the diagonal — when the plate can be
/// seen through.
fn bevel_parts(
    outer: &Poly,
    (x, y, w, h): (f32, f32, f32, f32),
    c: f32,
    rim: f32,
    see_through: bool,
) -> (Vec<Poly>, Vec<Poly>) {
    let top_right = pt(x + w, y);
    let bottom_left = pt(x, y + h);
    let rim = rim.max(0.0);
    let (iw, ih) = (w - 2.0 * rim, h - 2.0 * rim);
    if !see_through || iw <= 0.0 || ih <= 0.0 || rim <= 0.0 {
        return (
            vec![outer.clip_half_plane(top_right, bottom_left)],
            vec![outer.clip_half_plane(bottom_left, top_right)],
        );
    }
    // Keep the plate's chamfer positive so both outlines have the same
    // vertices, edge for edge.
    let inner_c = if c > 0.0 { (c - 0.5).max(0.01) } else { 0.0 };
    let inner = Poly::chamfer(x + rim, y + rim, iw, ih, inner_c);
    let (a, b) = (outer.points(), inner.points());
    if a.len() != b.len() {
        return (
            vec![outer.clip_half_plane(top_right, bottom_left)],
            vec![outer.clip_half_plane(bottom_left, top_right)],
        );
    }
    let mut hi = Vec::with_capacity(a.len());
    let mut lo = Vec::with_capacity(a.len());
    for i in 0..a.len() {
        let j = (i + 1) % a.len();
        let quad = Poly::new([a[i], a[j], b[j], b[i]]);
        let lit = quad.clip_half_plane(top_right, bottom_left);
        let shade = quad.clip_half_plane(bottom_left, top_right);
        if !lit.is_empty() {
            hi.push(lit);
        }
        if !shade.is_empty() {
            lo.push(shade);
        }
    }
    (hi, lo)
}

fn paint_poly(window: &mut Window, poly: &Poly, color: Hsla) {
    if color.alpha <= 0.0 || poly.is_empty() {
        return;
    }
    let mut fill = Fill::new();
    fill.poly(poly);
    fill.paint(window, color);
}

/// `filter: drop-shadow(0 18px 30px rgba(0,0,0,.7))` on the float wrapper.
/// The blur hides the chamfer, so a softly rounded box shadow of the same
/// footprint stands in for the polygon's own shadow.
fn paint_float_shadow(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    chamfer: f32,
    palette: &Palette,
    opacity: f32,
) {
    let shadow = BoxShadow {
        color: Hsla::from(palette.shadow).opacity(opacity),
        offset: point(px(0.0), px(18.0)),
        blur_radius: px(15.0),
        spread_radius: px(0.0),
        inset: false,
    };
    window.paint_chamfer_shadows(bounds, Corners { top_left: px(chamfer), bottom_right: px(chamfer), ..Corners::default() }, &[shadow]);
}

/// How the bevel is chosen: a named state resolved against the active
/// palette at paint time, or an explicit (e.g. mid-transition) edge.
#[derive(Clone, Copy, Debug)]
enum EdgeSource {
    State(Bevel),
    Explicit(Edge),
}

type ShapeHoverListener = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The cut plate as a container. Build with [`cut`]; style and fill it like
/// a `div`. Configure the cut first, then `.id(…)` for click handlers
/// (`Stateful<Cut>` keeps every div method but not these builders).
///
/// ```ignore
/// cut().chamfer(Chamfer::Sm).bevel(Bevel::Focus).p_3().child("Rest")
/// ```
pub struct Cut {
    div: Div,
    chamfer: f32,
    edge: EdgeSource,
    phase: f32,
    plate: Plate,
    fill: Option<Hsla>,
    lift: f32,
    floating: bool,
    on_shape_hover: Option<ShapeHoverListener>,
}

/// A cut plate container with the default chamfer and a resting bevel.
#[must_use]
pub fn cut() -> Cut {
    Cut {
        div: div(),
        chamfer: Chamfer::Md.px(),
        edge: EdgeSource::State(Bevel::Rest),
        phase: 0.0,
        plate: Plate::Flat,
        fill: None,
        lift: 0.0,
        floating: false,
        on_shape_hover: None,
    }
}

impl Cut {
    /// The chamfer size.
    #[must_use]
    pub fn chamfer(mut self, chamfer: Chamfer) -> Self {
        self.chamfer = chamfer.px();
        self
    }

    /// The bevel state.
    #[must_use]
    pub const fn bevel(mut self, bevel: Bevel) -> Self {
        self.edge = EdgeSource::State(bevel);
        self
    }

    /// An explicit edge, e.g. `Edge::of(from, p).mix(Edge::of(to, p), t)`
    /// while a state change animates.
    #[must_use]
    pub const fn edge(mut self, edge: Edge) -> Self {
        self.edge = EdgeSource::Explicit(edge);
        self
    }

    /// The travelling light's position, a fraction of a turn (`Bevel::Run`).
    #[must_use]
    pub const fn phase(mut self, phase: f32) -> Self {
        self.phase = phase;
        self
    }

    /// What sits inside the bevel.
    #[must_use]
    pub const fn plate(mut self, plate: Plate) -> Self {
        self.plate = plate;
        self
    }

    /// Overrides the plate tone (e.g. `plate2` for controls, `plate3` for
    /// menus).
    #[must_use]
    pub fn fill(mut self, color: impl Into<Hsla>) -> Self {
        self.fill = Some(color.into());
        self
    }

    /// Lifts the plate and its children towards the light: `1.0` is the
    /// board's hover lift `translate(-1px, -3px)`. Animated values (with
    /// overshoot) pass straight through. Layout does not move.
    #[must_use]
    pub const fn lift(mut self, amount: f32) -> Self {
        self.lift = amount;
        self
    }

    /// Paints the floating drop shadow outside the plate.
    #[must_use]
    pub const fn floating(mut self) -> Self {
        self.floating = true;
        self
    }

    /// Called with `true`/`false` when the pointer enters/leaves the plate's
    /// *outline* (the cut-away corners are outside). Fires on pointer moves
    /// whose result differs from the last painted frame, so dedupe in the
    /// listener before notifying.
    #[must_use]
    pub fn on_shape_hover(
        mut self,
        listener: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_shape_hover = Some(Rc::new(listener));
        self
    }

    fn lift_offset(&self) -> Point<Pixels> {
        point(px(-self.lift), px(-3.0 * self.lift))
    }
}

impl Styled for Cut {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Cut {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl InteractiveElement for Cut {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl IntoElement for Cut {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Prepaint state of a [`Cut`].
pub struct CutPrepaint {
    div: Option<Hitbox>,
    shape_hitbox: Option<Hitbox>,
    bounds: Bounds<Pixels>,
}

impl Element for Cut {
    type RequestLayoutState = DivFrameState;
    type PrepaintState = CutPrepaint;

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.div)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        Element::source_location(&self.div)
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.div.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let offset = self.lift_offset();
        let lifted = Bounds::new(bounds.origin + offset, bounds.size);
        let div = window.with_element_offset(offset, |window| {
            self.div
                .prepaint(id, inspector_id, lifted, request_layout, window, cx)
        });
        let shape_hitbox = self
            .on_shape_hover
            .is_some()
            .then(|| window.insert_hitbox(lifted, HitboxBehavior::Normal));
        CutPrepaint {
            div,
            shape_hitbox,
            bounds: lifted,
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let bounds = prepaint.bounds;
        let spec = CutPaint {
            chamfer: self.chamfer,
            edge: match self.edge {
                EdgeSource::State(bevel) => Edge::of(bevel, palette),
                EdgeSource::Explicit(edge) => edge,
            },
            phase: self.phase,
            plate: self.plate,
            fill: self.fill,
            floating: self.floating,
            opacity: self.div.style().opacity.unwrap_or(1.0),
        };
        paint_cut(window, bounds, &spec, palette);

        if let (Some(listener), Some(hitbox)) =
            (self.on_shape_hover.clone(), prepaint.shape_hitbox.clone())
        {
            let shape = outline(bounds, self.chamfer);
            let mouse = window.mouse_position();
            let painted = hitbox.is_hovered(window)
                && shape.contains(pt(f32::from(mouse.x), f32::from(mouse.y)));
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let at: Pt = pt(f32::from(event.position.x), f32::from(event.position.y));
                let inside = hitbox.is_hovered(window) && shape.contains(at);
                if inside != painted {
                    listener(inside, window, cx);
                }
            });
        }

        self.div.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            &mut prepaint.div,
            window,
            cx,
        );
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::tokens::GLACIER;

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn hit_test_excludes_the_cut_corners() {
        let bounds = b(10.0, 20.0, 180.0, 64.0);
        let c = Chamfer::Md.px();
        // Deep inside the cut-away top-left and bottom-right corners.
        assert!(!contains(bounds, c, point(px(11.0), px(21.0))));
        assert!(!contains(bounds, c, point(px(10.0 + 6.0), px(20.0 + 6.0))));
        assert!(!contains(bounds, c, point(px(189.0), px(83.0))));
        assert!(!contains(bounds, c, point(px(190.0 - 6.0), px(84.0 - 6.0))));
        // The other two corners are square and count.
        assert!(contains(bounds, c, point(px(189.0), px(21.0))));
        assert!(contains(bounds, c, point(px(11.0), px(83.0))));
        // Just inside the chamfer line (x + y = c from the corner).
        assert!(contains(bounds, c, point(px(10.0 + 7.5), px(20.0 + 7.5))));
        // The middle, and outside the box.
        assert!(contains(bounds, c, point(px(100.0), px(50.0))));
        assert!(!contains(bounds, c, point(px(5.0), px(50.0))));
    }

    #[test]
    fn edges_blend_endpoints_exactly() {
        for palette in [&ABYSS, &GLACIER] {
            let rest = Edge::of(Bevel::Rest, palette);
            let focus = Edge::of(Bevel::Focus, palette);
            assert_eq!(rest.mix(focus, 0.0).rim, rest.rim);
            assert!((rest.mix(focus, 1.0).rim - 2.0).abs() < 1e-6);
            assert!((rest.mix(focus, 0.5).rim - 1.5).abs() < 1e-6);
            let ghost = Edge::of(Bevel::Ghost, palette);
            assert!((rest.mix(ghost, 1.0).ghost - 1.0).abs() < 1e-6);
            assert!(rest.mix(ghost, 1.0).hi.alpha < 1e-4);
        }
    }

    #[test]
    fn chamfer_sizes_follow_the_tokens() {
        assert!((Chamfer::Sm.px() - 9.0).abs() < 1e-6);
        assert!((Chamfer::Md.px() - 14.0).abs() < 1e-6);
        assert!((Chamfer::Lg.px() - 22.0).abs() < 1e-6);
        assert!((Chamfer::Px(5.0).px() - 5.0).abs() < 1e-6);
    }
}
