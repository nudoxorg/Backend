//! Painting one frame of the world graph (app.js `draw`).
//!
//! Paint cost follows what is visible, not what exists:
//! - packages and modules are culled by box against the view;
//! - immutable bounds indices visit occupied items and crossing segments;
//!   members appear only when their item reaches reading scale;
//! - shapes are batched per (shape, tone, brightness) into one GPUI path
//!   each, so ~20 fills draw every symbol on screen (see [`Strategy`] for
//!   the measured alternatives);
//! - labels go through an occupancy grid by importance with a budget of
//!   40–160 per frame.
//!
//! Colour comes only from palette roles: `ink1` for symbols at rest, the
//! `line1` hue for territories and edges, `mint` for your code and every
//! symbol it reaches, `peri` for focus.

use super::camera::{View, smooth};
use super::discovery::{RoadStop, Search};
use super::interaction::{Reach, TourRoad};
use super::model::{Kind, NodeId};
use super::prism::PrismFrame;
use super::scene::{Scene, Terr};
use crate::data::text::{Shaped, shape};
use crate::motion::Camera;
use crate::paint::geom::{Fill, Poly, Pt, pt};
use crate::tokens::{Face, Palette, Tone, TypeRole};
use gpui::{App, Bounds, Hsla, SharedString, Window, fill, point, px, size};

/// How symbols reach the GPU (kept switchable so the choice stays measured,
/// see CHECKPOINT-2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Stars as quads inside one layer, shapes batched into one path per
    /// (shape, tone, brightness).
    #[default]
    Batched,
    /// Stars as triangles in the batched paths too (no quads at all).
    AllPaths,
    /// One path per shape and one quad per star, no layer (the naive port).
    Naive,
}

/// What the frame drew (the scene's annotation, the HUD).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Occupied item bounds tested by the spatial query.
    pub item_candidates: u32,
    /// Inner-edge bounds tested by the spatial query.
    pub edge_candidates: u32,
    /// Exact relations retained by the hovered neighbourhood.
    pub hover_relations: u32,
    /// Shared package/module routes submitted.
    pub hover_routes: u32,
    /// Leaf bounds tested by the hover endpoint query.
    pub hover_candidates: u32,
    /// Exact relations retained only for departure visuals.
    pub fading_hover_relations: u32,
    /// Departing shared semantic routes submitted.
    pub fading_hover_routes: u32,
    /// Leaf bounds tested for the departing visual packet.
    pub fading_hover_candidates: u32,
    /// Items visited on screen.
    pub items: u32,
    /// Members drawn.
    pub members: u32,
    /// Edges drawn.
    pub edges: u32,
    /// Labels painted.
    pub labels: u32,
    /// Paths handed to the scene.
    pub paths: u32,
    /// Quads handed to the scene.
    pub quads: u32,
}

/// One immutable exploration snapshot shared by the view and painter.
/// Its variants encode exclusivity instead of independent optional modes.
#[derive(Clone, Copy, Default)]
pub enum Exploration<'a> {
    /// The free map, hover, or focused prism.
    #[default]
    Free,
    /// A find query's cached constellation.
    Search(&'a Search),
    /// A selected or held value road.
    Chain {
        /// Folded type stops and exact calls.
        stops: &'a [RoadStop],
        /// The one-shot carried value, sampled by the view.
        progress: super::road::RoadSample,
    },
    /// A change reach and its current wave progress.
    Reach {
        /// Cached reach membership and threads.
        data: &'a Reach,
        /// Reveal progress, one unit per wave.
        wave: f32,
    },
    /// A package tour's numbered road.
    Tour(&'a TourRoad),
}

impl<'a> Exploration<'a> {
    fn reach(self) -> Option<&'a Reach> {
        if let Self::Reach { data, .. } = self {
            Some(data)
        } else {
            None
        }
    }
    fn reach_wave(self) -> f32 {
        if let Self::Reach { wave, .. } = self {
            wave
        } else {
            0.0
        }
    }
    fn search(self) -> Option<&'a Search> {
        if let Self::Search(data) = self {
            Some(data)
        } else {
            None
        }
    }
    fn chain(self) -> Option<&'a [RoadStop]> {
        if let Self::Chain { stops, .. } = self {
            Some(stops)
        } else {
            None
        }
    }
    fn road(self) -> Option<super::road::RoadSample> {
        if let Self::Chain { progress, .. } = self {
            Some(progress)
        } else {
            None
        }
    }
    fn tour(self) -> Option<&'a TourRoad> {
        if let Self::Tour(data) = self {
            Some(data)
        } else {
            None
        }
    }
}

/// Everything one frame depends on.
pub struct Look<'a> {
    /// The laid-out world.
    pub scene: &'a Scene,
    /// Where the canvas is.
    pub view: View,
    /// The camera.
    pub cam: Camera,
    /// The palette.
    pub palette: &'static Palette,
    /// Text scale.
    pub text_scale: f32,
    /// The hovered symbol.
    pub hover: Option<NodeId>,
    /// Its highlight, fading in 0 → 1.
    pub hover_a: f32,
    /// The hovered territory (no symbol under the pointer).
    pub hover_terr: Option<Terr>,
    /// One bounded departing packet, visual-only and never pickable.
    pub(crate) outgoing_hover: Option<(&'a super::scene::Neighbourhood, f32, f32)>,
    /// The focused symbol.
    pub focus: Option<NodeId>,
    /// The gathered prism, laid out.
    pub prism: Option<&'a PrismFrame>,
    /// Flow phase in px (dashes travel source → target).
    pub flow: f32,
    /// Finite motion overlay opacity, independent of wrapped phase.
    pub flow_alpha: f32,
    /// Visited symbols, oldest first.
    pub trail: &'a [NodeId],
    /// Measured native chrome bounds, frozen before this paint.
    pub reserved: &'a [Bounds<gpui::Pixels>],
    /// The measured focus card, reserved against foreground labels.
    pub occupied: Option<Bounds<gpui::Pixels>>,
    /// One canonical exploration snapshot; modes cannot paint over each other.
    pub exploration: Exploration<'a>,
    /// How to batch.
    pub strategy: Strategy,
}

/// A colour role at an exact opacity.
#[must_use]
pub fn tone(t: Tone, alpha: f32) -> Hsla {
    let mut h: Hsla = t.hsla();
    h.alpha = alpha.clamp(0.0, 1.0);
    h
}

/// A type role at a px size (text scale applied by the caller).
#[must_use]
pub const fn role(face: Face, weight: f32, size: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line: size * 1.3,
        tracking: 0.0,
        italic: matches!(face, Face::Serif),
    }
}

/// Screen-space stroke helpers on a triangle batch.
pub(crate) trait Strokes {
    /// A segment `a → b`, `w` px wide, butt ends.
    fn seg(&mut self, a: Pt, b: Pt, w: f32);
    /// A polyline.
    fn polyline(&mut self, pts: &[Pt], w: f32);
    /// Dashes clipped before tessellation, retaining phase along the whole path.
    fn dashed_in(&mut self, pts: &[Pt], w: f32, on: f32, off: f32, phase: f32, clip: [f32; 4]);
    /// A diamond of radius `s` about `(x, y)`.
    fn diamond(&mut self, x: f32, y: f32, s: f32);
    /// A diamond outline, `w` px wide, centred on radius `s`.
    fn diamond_ring(&mut self, x: f32, y: f32, s: f32, w: f32);
    /// An axis-aligned square.
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32);
}

impl Strokes for Fill {
    fn seg(&mut self, a: Pt, b: Pt, w: f32) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l = dx.hypot(dy);
        if l < 1e-3 {
            return;
        }
        let (nx, ny) = (-dy / l * w * 0.5, dx / l * w * 0.5);
        let (a1, a2, b1, b2) = (
            pt(a.x + nx, a.y + ny),
            pt(a.x - nx, a.y - ny),
            pt(b.x + nx, b.y + ny),
            pt(b.x - nx, b.y - ny),
        );
        self.triangle(a1, b1, b2);
        self.triangle(a1, b2, a2);
    }

    fn polyline(&mut self, pts: &[Pt], w: f32) {
        for pair in pts.windows(2) {
            self.seg(pair[0], pair[1], w);
        }
    }

    fn dashed_in(&mut self, pts: &[Pt], w: f32, on: f32, off: f32, phase: f32, clip: [f32; 4]) {
        clipped_dashes(pts, on, off, phase, clip, |a, b| self.seg(a, b, w));
    }

    fn diamond(&mut self, x: f32, y: f32, s: f32) {
        let (t, r, b, l) = (pt(x, y - s), pt(x + s, y), pt(x, y + s), pt(x - s, y));
        self.triangle(t, r, b);
        self.triangle(t, b, l);
    }

    fn diamond_ring(&mut self, x: f32, y: f32, s: f32, w: f32) {
        let d = w * 0.5 * std::f32::consts::SQRT_2;
        let (so, si) = (s + d, (s - d).max(0.0));
        let o = [pt(x, y - so), pt(x + so, y), pt(x, y + so), pt(x - so, y)];
        let i = [pt(x, y - si), pt(x + si, y), pt(x, y + si), pt(x - si, y)];
        for k in 0..4 {
            let n = (k + 1) % 4;
            self.triangle(o[k], o[n], i[n]);
            self.triangle(o[k], i[n], i[k]);
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (a, b, c, d) = (pt(x, y), pt(x + w, y), pt(x + w, y + h), pt(x, y + h));
        self.triangle(a, b, c);
        self.triangle(a, c, d);
    }
}

/// Clips a line parametrically; offscreen arc length still advances its dash
/// phase. Work and triangle count follow the visible portion, even when a
/// neighbouring package is thousands of screen pixels away.
fn clipped_dashes(
    pts: &[Pt],
    on: f32,
    off: f32,
    phase: f32,
    clip: [f32; 4],
    mut emit: impl FnMut(Pt, Pt),
) -> usize {
    let (on, period) = (f64::from(on), f64::from(on + off));
    if on <= 0.0 || period <= on || !period.is_finite() {
        return 0;
    }
    let mut at = f64::from(phase).rem_euclid(period);
    let mut inspected = 0;
    for pair in pts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (ax, ay) = (f64::from(a.x), f64::from(a.y));
        let (dx, dy) = (f64::from(b.x) - ax, f64::from(b.y) - ay);
        let len = dx.hypot(dy);
        if len < 1e-4 || !len.is_finite() {
            continue;
        }
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for (origin, delta, min, max) in [(ax, dx, clip[0], clip[2]), (ay, dy, clip[1], clip[3])] {
            if delta.abs() < 1e-12 {
                if origin < f64::from(min) || origin > f64::from(max) {
                    hi = -1.0;
                    break;
                }
            } else {
                let (t0, t1) = (
                    (f64::from(min) - origin) / delta,
                    (f64::from(max) - origin) / delta,
                );
                lo = lo.max(t0.min(t1));
                hi = hi.min(t0.max(t1));
            }
        }
        if lo < hi {
            let (start, visible) = (lo * len, (hi - lo) * len);
            let phase = (at + start).rem_euclid(period);
            // Enumerate precisely the periods intersecting this short visible
            // interval. No residual loop, phase toggling, or arbitrary work cap.
            // The bound is ceil(visible / period) + 1, independent of remote length.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let periods = ((visible + phase) / period).ceil() as usize;
            for index in 0..periods {
                inspected += 1;
                #[allow(clippy::cast_precision_loss)]
                let dash = index as f64 * period - phase;
                let (begin, end) = (dash.max(0.0), (dash + on).min(visible));
                if end - begin <= 1e-7 {
                    continue;
                }
                let (t0, t1) = ((start + begin) / len, (start + end) / len);
                #[allow(clippy::cast_possible_truncation)]
                let (a, b) = (
                    pt((ax + dx * t0) as f32, (ay + dy * t0) as f32),
                    pt((ax + dx * t1) as f32, (ay + dy * t1) as f32),
                );
                // The stroke painter drops these unrepresentable slivers too.
                if (b.x - a.x).hypot(b.y - a.y) >= 1e-3 {
                    emit(a, b);
                }
            }
        }
        at = (at + len).rem_euclid(period);
    }
    inspected
}

/// Samples a quadratic Bézier into `out` (without its first point).
fn quad_to(out: &mut Vec<Pt>, from: Pt, ctrl: Pt, to: Pt, steps: u32) {
    for s in 1..=steps {
        #[allow(clippy::cast_precision_loss)]
        let t = s as f32 / steps as f32;
        out.push(quadratic_point(from, ctrl, to, t));
    }
}

/// Samples a cubic Bézier into `out` (without its first point).
pub(crate) fn cubic_to(out: &mut Vec<Pt>, p0: Pt, c1: Pt, c2: Pt, p3: Pt, steps: u32) {
    for s in 1..=steps {
        #[allow(clippy::cast_precision_loss)]
        let t = s as f32 / steps as f32;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push(pt(
            a * p0.x + b * c1.x + c * c2.x + d * p3.x,
            a * p0.y + b * c1.y + c * c2.y + d * p3.y,
        ));
    }
}

/// The label occupancy grid: 8 px cells over the view; a label takes its
/// cells only when all are free.
pub struct Occupancy {
    cell: f32,
    cols: usize,
    rows: usize,
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    taken: Vec<bool>,
    clip: [f32; 4],
}

impl Occupancy {
    /// An empty grid over `view`.
    #[must_use]
    pub fn new(view: &View) -> Self {
        Self::anchored(view, (view.x, view.y))
    }

    /// Align collision cells with a projected world origin. A common pan of
    /// map labels and origin preserves their collision choices; foreground
    /// reservations and canvas clipping still use their actual window bounds.
    #[must_use]
    pub fn anchored(view: &View, anchor: (f32, f32)) -> Self {
        let cell = 8.0;
        let x0 = view.x - (view.x - anchor.0).rem_euclid(cell);
        let y0 = view.y - (view.y - anchor.1).rem_euclid(cell);
        let w = view.x + view.w - x0;
        let h = view.y + view.h - y0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (cols, rows) = (
            ((w / cell).ceil() as usize) + 1,
            ((h / cell).ceil() as usize) + 1,
        );
        Self {
            cell,
            cols,
            rows,
            x0,
            y0,
            w,
            h,
            taken: vec![false; cols * rows],
            clip: [view.x, view.y, view.x + view.w, view.y + view.h],
        }
    }

    /// Reserves a foreground box even when it intersects another reservation.
    /// A gathering prism can overlap itself; map text must still avoid its union.
    pub fn reserve(&mut self, [x0, y0, x1, y1]: [f32; 4]) {
        if x1 < self.x0 || y1 < self.y0 || x0 > self.x0 + self.w || y0 > self.y0 + self.h {
            return;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cell =
            |v: f32, o: f32, n: usize| (((v - o) / self.cell).floor().max(0.0) as usize).min(n - 1);
        let (a0, b0, a1, b1) = (
            cell(x0, self.x0, self.cols),
            cell(y0, self.y0, self.rows),
            cell(x1, self.x0, self.cols),
            cell(y1, self.y0, self.rows),
        );
        for b in b0..=b1 {
            for a in a0..=a1 {
                self.taken[a + b * self.cols] = true;
            }
        }
    }

    /// One composite label owns its overlapping line boxes together.
    pub fn take_pair(&mut self, a: [f32; 4], b: [f32; 4]) -> bool {
        self.take(
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        )
    }

    /// Takes the box (window px) if it is on screen and free.
    pub fn take(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite())
            || x0 < self.clip[0]
            || y0 < self.clip[1]
            || x1 > self.clip[2]
            || y1 > self.clip[3]
            || x1 < x0
            || y1 < y0
        {
            return false;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x0, y0, x1, y1) = (x0 - self.x0, y0 - self.y0, x1 - self.x0, y1 - self.y0);
        let cell = |v: f32, n: usize| ((v / self.cell).floor().max(0.0) as usize).min(n - 1);
        let (a0, b0, a1, b1) = (
            cell(x0, self.cols),
            cell(y0, self.rows),
            cell(x1, self.cols),
            cell(y1, self.rows),
        );
        for b in b0..=b1 {
            for a in a0..=a1 {
                if self.taken[a + b * self.cols] {
                    return false;
                }
            }
        }
        for b in b0..=b1 {
            for a in a0..=a1 {
                self.taken[a + b * self.cols] = true;
            }
        }
        true
    }
}

/// The label roles (px at 100 %).
pub mod roles {
    use super::role;
    use crate::tokens::{Face, TypeRole};
    /// Module paths.
    pub const MODULE: TypeRole = role(Face::Mono, 500.0, 11.5);
    /// Symbol names.
    pub const ITEM: TypeRole = role(Face::Mono, 500.0, 11.5);
    /// The highlighted symbol.
    pub const ITEM_BOLD: TypeRole = role(Face::Mono, 600.0, 12.5);
    /// Member names.
    pub const MEMBER: TypeRole = role(Face::Mono, 400.0, 10.5);
    /// A package's size line.
    pub const SUB: TypeRole = role(Face::Ui, 400.0, 11.0);
}

fn scaled(r: TypeRole, s: f32) -> TypeRole {
    TypeRole {
        size: r.size * s,
        line: r.line * s,
        ..r
    }
}

/// The shaped text of a label, centred vertically on `y` (the canvas's
/// `textBaseline = middle`).
fn baseline(t: &Shaped, y: f32) -> f32 {
    y + (t.ascent() - t.descent()) * 0.5
}

/// Batches of one shape: `[tone][brightness]`.
type Bank = [[Fill; 4]; 2];

fn bank() -> Bank {
    std::array::from_fn(|_| std::array::from_fn(|_| Fill::new()))
}

/// A star for the quad path: `(x, y, side, tone, brightness)`.
type Star = (f32, f32, f32, usize, usize);

/// Ambient context belongs to nearby territories. A soft96px edge keeps
/// endpoint relevance continuous during pan, while distant crossing-only
/// wires do not compete with exact hovered relations.
fn ambient_relevance(view: &View, [x0, y0, x1, y1]: [f32; 4]) -> f32 {
    let outside = (view.x - x1)
        .max(x0 - view.x - view.w)
        .max(view.y - y1)
        .max(y0 - view.y - view.h)
        .max(0.0);
    (1.0 - smooth(f64::from(outside), 0.0, 96.0)) as f32
}

fn hover_envelope(active: Option<f32>, outgoing: Option<f32>) -> f32 {
    active
        .unwrap_or(0.0)
        .max(outgoing.unwrap_or(0.0))
        .clamp(0.0, 1.0)
}

/// Fade a leaf's entire strand toward its aggregate route before its endpoint
/// leaves the indexed window margin. Interior strands retain full fidelity.
fn endpoint_fade(view: &View, x: f32, y: f32) -> f32 {
    let distance = (x - view.x)
        .min(view.x + view.w - x)
        .min(y - view.y)
        .min(view.y + view.h - y);
    smooth(f64::from(distance), -24.0, 8.0) as f32
}

/// Paints one frame; returns what it drew.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn paint(look: &Look<'_>, window: &mut Window, cx: &mut App) -> Stats {
    let mut st = Stats::default();
    let Look {
        scene,
        view,
        cam,
        palette,
        ..
    } = *look;
    let (world, layout) = (&*scene.world, &*scene.layout);
    let projected = scene.projection(view, cam);
    let k = projected.scale();
    let kf = k as f32;
    let sx = |x: f32| projected.x(x);
    let sy = |y: f32| projected.y(y);
    let in_view = |b: &super::layout::Box2| projected.visible(b);
    let bounds = Bounds {
        origin: point(px(view.x), px(view.y)),
        size: size(px(view.w), px(view.h)),
    };
    let stroke_clip = [
        view.x - 2.0,
        view.y - 2.0,
        view.x + view.w + 2.0,
        view.y + view.h + 2.0,
    ];
    let (ink, mint, peri, line) = (
        palette.ink1,
        palette.mint.base,
        palette.peri.base,
        palette.line1,
    );
    let yours_pkg = |p: u32| world.packages[p as usize].yours;
    let hi = look.focus.or(look.hover);
    let prism_g = look.prism.map_or(0.0, |p| p.e);
    let mode_dim = if look.exploration.reach().is_some()
        || look.exploration.tour().is_some()
        || look.exploration.chain().is_some()
        || look.exploration.search().is_some_and(|s| !s.lit.is_empty())
    {
        0.42
    } else {
        1.0
    };
    let dim_all = mode_dim
        * if prism_g > 0.0 {
            1.0 - 0.7 * prism_g
        } else {
            1.0 - 0.45
                * hover_envelope(
                    look.hover.map(|_| look.hover_a),
                    look.outgoing_hover.map(|(_, a, _)| a),
                )
        };
    // The lit neighbourhood of the hovered symbol (not while a prism shows).
    let lit_nb = look
        .hover
        .filter(|_| look.prism.is_none())
        .map(|h| scene.neighbourhood(h));
    let lit = lit_nb
        .as_ref()
        .map_or(&[][..], |neighbours| neighbours.lit.as_slice());
    let is_lit = |j: NodeId| lit.binary_search(&j).is_ok();
    let paint_fill = |f: Fill, color: Hsla, window: &mut Window, st: &mut Stats| {
        if !f.is_empty() && color.alpha > 0.0005 {
            st.paths += 1;
            f.paint(window, color);
        }
    };

    // The caller clips to the canvas (see `view`).
    window.paint_quad(fill(bounds, palette.g0));
    {
        // ---- territories: packages
        let vis_p: Vec<u32> = (0..layout.packages.len() as u32)
            .filter(|&p| in_view(&layout.packages[p as usize].bounds))
            .collect();
        let screen_poly =
            |hull: &[[f32; 2]]| Poly::new(hull.iter().map(|v| pt(sx(v[0]), sy(v[1]))));
        for &p in &vis_p {
            let t = &layout.packages[p as usize];
            let pxs = f64::from(t.r) * k;
            let poly = screen_poly(&t.hull);
            let terr_hi = look.hover_terr.is_some_and(|h| h.pkg == p);
            let fade = (1.0 - 0.85 * smooth(pxs, 500.0, 1800.0)) as f32;
            let (fc, sc) = if yours_pkg(p) {
                (tone(mint, 0.035), tone(mint, 0.22 * fade))
            } else {
                (
                    tone(line, if terr_hi { 0.035 } else { 0.014 }),
                    tone(line, (if terr_hi { 0.26 } else { 0.12 }) * fade),
                )
            };
            let mut f = Fill::new();
            f.poly(&poly);
            paint_fill(f, fc, window, &mut st);
            let mut s = Fill::new();
            for q in poly.stroke_ring(1.0) {
                s.poly(&q);
            }
            paint_fill(s, sc, window, &mut st);
        }
        // ---- territories: modules
        let mut vis_m: Vec<u32> = Vec::new();
        for &p in &vis_p {
            let show = smooth(f64::from(layout.packages[p as usize].r) * k, 160.0, 360.0) as f32;
            for &m in &layout.package_modules[p as usize] {
                let t = &layout.modules[m as usize];
                if !in_view(&t.bounds) {
                    continue;
                }
                vis_m.push(m);
                if show > 0.01 && f64::from(t.r) * k > 6.0 {
                    let th = look.hover_terr.is_some_and(|h| h.module == Some(m));
                    let poly = screen_poly(&t.hull);
                    let mut s = Fill::new();
                    for q in poly.stroke_ring(1.0) {
                        s.poly(&q);
                    }
                    let c = if yours_pkg(p) { mint } else { line };
                    paint_fill(
                        s,
                        tone(c, (if th { 0.22 } else { 0.075 }) * show),
                        window,
                        &mut st,
                    );
                    if th {
                        let mut f = Fill::new();
                        f.poly(&poly);
                        paint_fill(f, tone(line, 0.03), window, &mut st);
                    }
                }
            }
        }

        // ---- ambient edges, by level
        let (far, biggest_m) = scene.level_scales(k);
        let pkg_edge_a = (1.0 - smooth(far, 260.0, 700.0)) as f32;
        if pkg_edge_a > 0.01 {
            // Bucketed by colour so a few paths draw them all.
            let mut quiet = Fill::new();
            let mut yours_e = Fill::new();
            let mut on_e = Fill::new();
            for e in &layout.package_edges {
                if Some(e.from) == scene.std_pkg || Some(e.to) == scene.std_pkg {
                    continue;
                }
                let on = look
                    .hover_terr
                    .is_some_and(|h| h.pkg == e.from || h.pkg == e.to);
                if !on && e.weight < 40 {
                    continue;
                }
                let (a, b) = (
                    &layout.packages[e.from as usize],
                    &layout.packages[e.to as usize],
                );
                let (ax, ay, bx, by) = (sx(a.x), sy(a.y), sx(b.x), sy(b.y));
                let ctrl = pt(
                    (ax + bx) / 2.0 + (by - ay) * 0.12,
                    (ay + by) / 2.0 - (bx - ax) * 0.12,
                );
                let mut pts = vec![pt(ax, ay)];
                quad_to(&mut pts, pt(ax, ay), ctrl, pt(bx, by), 16);
                let w = 0.6 + (1.0 + e.weight as f32).log2() * 0.18;
                let target = if yours_pkg(e.from) {
                    &mut yours_e
                } else if on {
                    &mut on_e
                } else {
                    &mut quiet
                };
                target.polyline(&pts, w);
                st.edges += 1;
            }
            paint_fill(
                quiet,
                tone(line, 0.035 * pkg_edge_a * dim_all),
                window,
                &mut st,
            );
            paint_fill(
                yours_e,
                tone(mint, 0.035 * pkg_edge_a * dim_all),
                window,
                &mut st,
            );
            paint_fill(
                on_e,
                tone(peri, 0.32 * pkg_edge_a * dim_all),
                window,
                &mut st,
            );
        }
        let mod_edge_a =
            (smooth(far, 300.0, 700.0) * (1.0 - smooth(biggest_m, 110.0, 240.0))) as f32;
        if mod_edge_a > 0.01 {
            // Alpha by weight, in six buckets.
            let mut buckets: [Fill; 6] = std::array::from_fn(|_| Fill::new());
            for e in &layout.module_edges {
                if e.weight < 2 {
                    continue;
                }
                let (a, b) = (
                    &layout.modules[e.from as usize],
                    &layout.modules[e.to as usize],
                );
                let bounds = super::layout::Box2::around(&[[a.x, a.y], [b.x, b.y]]);
                if !projected.visible_with_margin(&bounds, 1.0) {
                    continue;
                }
                let relevance = ambient_relevance(
                    &view,
                    [
                        sx(a.bounds.x0),
                        sy(a.bounds.y0),
                        sx(a.bounds.x1),
                        sy(a.bounds.y1),
                    ],
                )
                .max(ambient_relevance(
                    &view,
                    [
                        sx(b.bounds.x0),
                        sy(b.bounds.y0),
                        sx(b.bounds.x1),
                        sy(b.bounds.y1),
                    ],
                ));
                if relevance <= 0.001 {
                    continue;
                }
                let bucket = (((0.02 + 0.012 * (e.weight as f32).log2()).min(0.12) - 0.02) / 0.02)
                    .floor()
                    .clamp(0.0, 5.0) as usize;
                buckets[bucket].seg(pt(sx(a.x), sy(a.y)), pt(sx(b.x), sy(b.y)), 0.8 * relevance);
                st.edges += 1;
            }
            for (q, f) in buckets.into_iter().enumerate() {
                let alpha = 0.02 + 0.02 * q as f32 + 0.01;
                paint_fill(
                    f,
                    tone(line, alpha.min(0.12) * mod_edge_a * dim_all),
                    window,
                    &mut st,
                );
            }
        }
        let item_edge_a = smooth(k, 3.2, 7.0) as f32;
        if item_edge_a > 0.01 {
            let mut f = Fill::new();
            st.edge_candidates = scene.visit_inner(&projected, |(a, b)| {
                let (ax, ay, bx, by) = (
                    sx(layout.x[a as usize]),
                    sy(layout.y[a as usize]),
                    sx(layout.x[b as usize]),
                    sy(layout.y[b as usize]),
                );
                let (lo_x, hi_x, lo_y, hi_y) = (ax.min(bx), ax.max(bx), ay.min(by), ay.max(by));
                if hi_x < view.x
                    || lo_x > view.x + view.w
                    || hi_y < view.y
                    || lo_y > view.y + view.h
                {
                    return;
                }
                f.seg(pt(ax, ay), pt(bx, by), 0.8);
                st.edges += 1;
            }) as u32;
            paint_fill(f, tone(line, 0.1 * item_edge_a * dim_all), window, &mut st);
        }

        // ---- symbols: batched per (shape, tone, brightness)
        let (mut dots, mut dia, mut sq, mut hollow, mut shells, mut msq, mut mdot) =
            (bank(), bank(), bank(), bank(), bank(), bank(), bank());
        let mut reach_shapes: [[Fill; 8]; 2] =
            std::array::from_fn(|_| std::array::from_fn(|_| Fill::new()));
        let mut stars: Vec<Star> = Vec::new();
        let mut labels: Vec<(f32, NodeId, f32, f32, f32)> = Vec::new();
        let naive = look.strategy == Strategy::Naive;
        let mut naive_shapes: Vec<(Fill, Hsla)> = Vec::new();
        st.item_candidates = scene.visit_items(&projected, 40.0, |i| {
            let (x, y) = (sx(layout.x[i as usize]), sy(layout.y[i as usize]));
            let ri = layout.r[i as usize] * kf;
            if x < view.x - ri - 40.0
                || y < view.y - ri - 40.0
                || x > view.x + view.w + ri + 40.0
                || y > view.y + view.h + ri + 40.0
            {
                return;
            }
            st.items += 1;
            let node = &world.nodes[i as usize];
            let glyph = Scene::glyph(node.kind, k);
            let core = glyph.core;
            let imp = world.importance[i as usize];
            if let Some(reach) = look.exploration.reach()
                && let Some(d @ 1..=8) = reach.depth[i as usize]
                && look.exploration.reach_wave() > f32::from(d - 1)
            {
                let radius = if d == 1 {
                    4.2
                } else if d == 2 {
                    3.2
                } else {
                    2.2
                };
                reach_shapes[usize::from(world.yours(i))][usize::from(d - 1)].diamond(x, y, radius);
                if d <= 2 {
                    labels.push((
                        (if d == 1 { 8.0 } else { 4.0 }) + imp * 2.0,
                        i,
                        x,
                        y,
                        radius,
                    ));
                }
            }
            let bright = ((imp * 4.0) as usize).min(3);
            let yours = yours_pkg(node.pkg);
            let t = usize::from(world.yours_in[i as usize] > 0 || yours);
            if core < 1.6 {
                let s =
                    (if core < 0.7 { 1.0 } else { 1.5 }) + if t == 1 && !yours { 0.6 } else { 0.0 };
                let (rx, ry) = (x - s / 2.0, y - s / 2.0);
                match look.strategy {
                    Strategy::AllPaths => dots[t][bright].rect(rx, ry, s, s),
                    _ => stars.push((rx, ry, s, t, bright)),
                }
            } else {
                let s = core.min(9.0) * 0.8;
                let bank = match node.kind {
                    Kind::Trait => &mut hollow,
                    Kind::Struct | Kind::Enum | Kind::Type | Kind::Union => &mut dia,
                    _ => &mut sq,
                };
                let mut one = Fill::new();
                let target = if naive {
                    &mut one
                } else {
                    &mut bank[t][bright]
                };
                match node.kind {
                    Kind::Trait => target.diamond_ring(x, y, s, 1.2),
                    Kind::Struct | Kind::Enum | Kind::Type | Kind::Union => target.diamond(x, y, s),
                    _ => target.rect(x - s * 0.55, y - s * 0.55, s * 1.1, s * 1.1),
                }
                if naive {
                    let c = if t == 1 { mint } else { ink };
                    naive_shapes.push((one, tone(c, (0.3 + 0.2 * bright as f32) * dim_all)));
                }
            }
            if core > 4.5 || is_lit(i) || Some(i) == hi {
                let prio = imp * smooth(f64::from(core), 4.5, 11.0) as f32
                    + if is_lit(i) { 10.0 } else { 0.0 }
                    + if Some(i) == hi { 20.0 } else { 0.0 };
                labels.push((prio, i, x, y, core.min(9.0) * 0.8));
            }
            // Members on their shells.
            let ma = scene.member_alpha(i, k) as f32;
            if ma > 0.02 && !world.kids(i).is_empty() {
                let level = (ma * 3.0).round() as usize;
                for shell in layout.shells_of(i) {
                    let ring = layout.shell(shell);
                    if ring.len() < 3 {
                        continue;
                    }
                    let poly: Vec<Pt> = ring
                        .iter()
                        .chain(std::iter::once(&ring[0]))
                        .map(|&j| pt(sx(layout.x[j as usize]), sy(layout.y[j as usize])))
                        .collect();
                    shells[t][level].polyline(&poly, 1.0);
                }
                let s = Scene::member_glyph(k).radius * 2.0;
                for &j in world.kids(i) {
                    let (mx, my) = (sx(layout.x[j as usize]), sy(layout.y[j as usize]));
                    if mx < view.x - 10.0
                        || my < view.y - 10.0
                        || mx > view.x + view.w + 10.0
                        || my > view.y + view.h + 10.0
                    {
                        continue;
                    }
                    st.members += 1;
                    let bank = if world.nodes[j as usize].kind == Kind::Method {
                        &mut msq
                    } else {
                        &mut mdot
                    };
                    bank[t][level].rect(mx - s / 2.0, my - s / 2.0, s, s);
                    if k > 38.0 {
                        let prio = world.importance[j as usize]
                            + 0.2
                            + if is_lit(j) { 10.0 } else { 0.0 }
                            + if Some(j) == hi { 20.0 } else { 0.0 };
                        labels.push((prio, j, mx, my, s / 2.0));
                    }
                }
            }
        }) as u32;
        for t in 0..2 {
            for b in 0..4 {
                let c = if t == 1 { mint } else { ink };
                let shell = std::mem::take(&mut shells[t][b]);
                paint_fill(
                    shell,
                    tone(
                        if t == 1 { mint } else { line },
                        0.09 * (b as f32 / 3.0) * dim_all,
                    ),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut dots[t][b]),
                    tone(c, (0.3 + 0.2 * b as f32) * dim_all * 0.9),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut dia[t][b]),
                    tone(c, (0.3 + 0.2 * b as f32) * dim_all),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut sq[t][b]),
                    tone(c, (0.3 + 0.2 * b as f32) * dim_all),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut hollow[t][b]),
                    tone(c, (0.3 + 0.2 * b as f32) * dim_all),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut msq[t][b]),
                    tone(c, 0.55 * (b as f32 / 3.0) * dim_all),
                    window,
                    &mut st,
                );
                paint_fill(
                    std::mem::take(&mut mdot[t][b]),
                    tone(c, 0.4 * (b as f32 / 3.0) * dim_all),
                    window,
                    &mut st,
                );
            }
        }
        for (f, c) in naive_shapes {
            paint_fill(f, c, window, &mut st);
        }
        // Stars: quads in one layer (one order, no bounds-tree insert each),
        // after every batched path so the paths above form one GPU pass.
        if !stars.is_empty() {
            let colors: [[Hsla; 4]; 2] = std::array::from_fn(|t| {
                std::array::from_fn(|b| {
                    tone(
                        if t == 1 { mint } else { ink },
                        (0.3 + 0.2 * b as f32) * dim_all * 0.9,
                    )
                })
            });
            let draw = |window: &mut Window| {
                for &(x, y, s, t, b) in &stars {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(x), px(y)),
                            size: size(px(s), px(s)),
                        },
                        colors[t][b],
                    ));
                }
            };
            if naive {
                draw(window);
            } else {
                window.paint_layer(bounds, draw);
            }
            st.quads += stars.len() as u32;
        }

        // ---- reach: constant-time depth lookup while visiting visible items
        if let Some(reach) = look.exploration.reach() {
            for (mine, waves) in reach_shapes.into_iter().enumerate() {
                for (d, batch) in waves.into_iter().enumerate() {
                    let fade = (look.exploration.reach_wave() - d as f32).clamp(0.0, 1.0);
                    let alpha: f32 = [0.95, 0.66, 0.34, 0.2, 0.2, 0.2, 0.2, 0.2][d];
                    paint_fill(
                        batch,
                        tone(
                            if mine == 1 { mint } else { peri },
                            (alpha + if mine == 1 { 0.1 } else { 0.0 }).min(1.0) * fade,
                        ),
                        window,
                        &mut st,
                    );
                }
            }
            let source = pt(
                sx(layout.x[reach.source as usize]),
                sy(layout.y[reach.source as usize]),
            );
            let mut threads = Fill::new();
            for &i in &reach.threads {
                let p = pt(sx(layout.x[i as usize]), sy(layout.y[i as usize]));
                if p.x >= stroke_clip[0]
                    && p.x <= stroke_clip[2]
                    && p.y >= stroke_clip[1]
                    && p.y <= stroke_clip[3]
                {
                    threads.seg(p, source, 0.8);
                }
            }
            paint_fill(
                threads,
                tone(peri, 0.16 * look.exploration.reach_wave().clamp(0.0, 1.0)),
                window,
                &mut st,
            );
            let mut ring = Fill::new();
            ring.diamond_ring(source.x, source.y, 9.0, 1.0);
            paint_fill(ring, tone(peri, 0.6), window, &mut st);
            labels.push((30.0, reach.source, source.x, source.y, 9.0));
        }
        // ---- search: no more than 400 cached matches, culled before geometry
        if let Some(search) = look.exploration.search() {
            let (mut halos, mut gems) = (Fill::new(), Fill::new());
            let radius = (1.6 + kf * 0.35).clamp(2.2, 5.0);
            for &i in &search.lit {
                let (x, y) = (sx(layout.x[i as usize]), sy(layout.y[i as usize]));
                if x < stroke_clip[0] - 12.0
                    || x > stroke_clip[2] + 12.0
                    || y < stroke_clip[1] - 12.0
                    || y > stroke_clip[3] + 12.0
                {
                    continue;
                }
                halo(&mut halos, x, y, radius + 4.0);
                gems.diamond(x, y, radius);
                labels.push((6.0 + world.importance[i as usize] * 2.0, i, x, y, radius));
            }
            paint_fill(halos, tone(peri, 0.13), window, &mut st);
            paint_fill(gems, tone(peri, 0.95), window, &mut st);
        }
        let mut road_labels = paint_roads(look, kf, &sx, &sy, stroke_clip, window, &mut st);

        // ---- the lit neighbourhood: bundled edges with flow, bright nodes
        for (neighbours, a, flow_alpha, promote) in look
            .outgoing_hover
            .map(|(nb, a, flow_alpha)| (nb, a, flow_alpha, false))
            .into_iter()
            .chain(
                lit_nb
                    .as_deref()
                    .map(|nb| (nb, look.hover_a, look.flow_alpha, true)),
            )
        {
            if a <= 0.001 || (!promote && look.hover == Some(neighbours.node)) {
                continue;
            }
            let h = neighbours.node;
            if promote {
                st.hover_relations = neighbours.edges.len() as u32;
            } else {
                st.fading_hover_relations = neighbours.edges.len() as u32;
            }
            let mut trunks: [Fill; 3] = std::array::from_fn(|_| Fill::new());
            let mut hubs: [Fill; 3] = std::array::from_fn(|_| Fill::new());
            let colour = |incoming: bool, other: NodeId| {
                if !incoming {
                    0
                } else if world.yours(other) {
                    2
                } else {
                    1
                }
            };
            let screen_points = |edge: &super::scene::HoverEdge| {
                let mut screen = [pt(0.0, 0.0); super::scene::ROUTE_SAMPLES];
                for (dst, p) in screen.iter_mut().zip(&edge.points) {
                    *dst = pt(sx(p[0]), sy(p[1]));
                }
                screen
            };
            // One continuous route per semantic group replaces thousands of
            // repeated remote leaf curves. Counts retain every exact relation.
            for bundle in &neighbours.bundles {
                let edge = &bundle.route;
                if !projected.visible_with_margin(&edge.bounds, 2.0) {
                    continue;
                }
                let screen = screen_points(edge);
                let width = (0.8 + (bundle.count as f32).log2() * 0.08).min(1.5);
                trunks[colour(edge.incoming, edge.other)]
                    .polyline(&screen[..edge.points.len()], width);
                if let Some(caption) = &bundle.caption {
                    let remote = screen[if edge.incoming { 0 } else { screen.len() - 1 }];
                    if projected.contains(remote.x, remote.y) {
                        let color = colour(edge.incoming, edge.other);
                        hubs[color].diamond_ring(remote.x, remote.y, 3.5, 1.0);
                        if promote {
                            road_labels.push((
                                caption.clone(),
                                remote.x,
                                remote.y,
                                3.5,
                                roles::SUB,
                                tone(
                                    if color == 0 {
                                        ink
                                    } else if color == 1 {
                                        peri
                                    } else {
                                        mint
                                    },
                                    0.65 * a,
                                ),
                            ));
                        }
                    }
                }
                if promote {
                    st.hover_routes += 1;
                } else {
                    st.fading_hover_routes += 1;
                }
                st.edges += 1;
            }
            for (q, batch) in trunks.into_iter().enumerate() {
                paint_fill(
                    batch,
                    tone(
                        if q == 0 {
                            ink
                        } else if q == 1 {
                            peri
                        } else {
                            mint
                        },
                        0.3 * a,
                    ),
                    window,
                    &mut st,
                );
            }
            for (q, batch) in hubs.into_iter().enumerate() {
                paint_fill(
                    batch,
                    tone(
                        if q == 0 {
                            ink
                        } else if q == 1 {
                            peri
                        } else {
                            mint
                        },
                        0.65 * a,
                    ),
                    window,
                    &mut st,
                );
            }
            // Unfold exact leaves only at visible endpoints and reading scale.
            // One batch per module gives a continuous alpha without quantized
            // fade steps; sorting work is proportional to visible candidates.
            let mut leaves = Vec::new();
            let work = neighbours.visit_leaves(&projected, |i| {
                let edge = &neighbours.edges[i];
                let m = world.node(edge.other).module;
                let alpha = smooth(f64::from(layout.modules[m as usize].r) * k, 24.0, 80.0) as f32;
                let coverage = endpoint_fade(
                    &view,
                    sx(layout.x[edge.other as usize]),
                    sy(layout.y[edge.other as usize]),
                );
                if alpha > 0.001 && coverage > 0.001 {
                    leaves.push((m, i, alpha, coverage));
                }
            });
            if promote {
                st.hover_candidates = work as u32;
            } else {
                st.fading_hover_candidates = work as u32;
            }
            leaves.sort_unstable_by_key(|&(module, i, _, _)| (module, i));
            let mut start = 0;
            while start < leaves.len() {
                let module = leaves[start].0;
                let alpha = leaves[start].2;
                let mut batches: [Fill; 3] = std::array::from_fn(|_| Fill::new());
                let mut lights: [Fill; 3] = std::array::from_fn(|_| Fill::new());
                let mut end = start;
                while end < leaves.len() && leaves[end].0 == module {
                    let edge = &neighbours.edges[leaves[end].1];
                    let route = scene.leaf_route(h, edge);
                    let screen = screen_points(&route);
                    let coverage = leaves[end].3;
                    let colour = colour(edge.incoming, edge.other);
                    // Coverage fading varies stroke width within one uniform
                    // color batch, so a thousand boundary leaves cannot create
                    // a thousand individual paint submissions.
                    if edge.shared {
                        batches[colour].polyline(&screen[..route.points.len()], 1.2 * coverage);
                    }
                    if flow_alpha > 0.0 {
                        lights[colour].dashed_in(
                            &screen[..route.points.len()],
                            coverage,
                            2.0,
                            7.0,
                            -look.flow,
                            stroke_clip,
                        );
                    }
                    if edge.shared || flow_alpha > 0.0 {
                        st.edges += 1;
                    }
                    end += 1;
                }
                for (q, (batch, light)) in batches.into_iter().zip(lights).enumerate() {
                    let color = if q == 0 {
                        ink
                    } else if q == 1 {
                        peri
                    } else {
                        mint
                    };
                    paint_fill(batch, tone(color, 0.5 * a * alpha), window, &mut st);
                    paint_fill(
                        light,
                        tone(color, 0.5 * a * alpha * flow_alpha),
                        window,
                        &mut st,
                    );
                }
                start = end;
            }
            let mut bright_mint = Fill::new();
            let mut bright_ink = Fill::new();
            let mut me = Fill::new();
            let mut ring = Fill::new();
            let mut labelled: Vec<NodeId> = labels.iter().map(|l| l.1).collect();
            labelled.sort_unstable();
            neighbours.visit_lit(&projected, |j| {
                let (x, y) = (sx(layout.x[j as usize]), sy(layout.y[j as usize]));
                if x < view.x - 14.0
                    || x > view.x + view.w + 14.0
                    || y < view.y - 14.0
                    || y > view.y + view.h + 14.0
                {
                    return;
                }
                if j == h {
                    me.diamond(x, y, 7.0);
                    ring.diamond_ring(x, y, 12.0, 1.0);
                } else if world.yours(j) || world.reached(j) {
                    bright_mint.diamond(x, y, 3.5);
                } else {
                    bright_ink.diamond(x, y, 3.5);
                }
                if promote && labelled.binary_search(&j).is_err() {
                    labels.push((
                        if j == h { 30.0 } else { 12.0 } + world.importance[j as usize],
                        j,
                        x,
                        y,
                        if j == h { 7.0 } else { 3.5 },
                    ));
                }
            });
            paint_fill(bright_ink, tone(ink, a), window, &mut st);
            paint_fill(bright_mint, tone(mint, a), window, &mut st);
            paint_fill(me, tone(peri, a), window, &mut st);
            paint_fill(ring, tone(peri, 0.5 * a), window, &mut st);
        }

        // ---- the trail: where you have been, joined in mint
        if look.trail.len() >= 2 {
            let pts: Vec<Pt> = look
                .trail
                .iter()
                .map(|&j| pt(sx(layout.x[j as usize]), sy(layout.y[j as usize])))
                .collect();
            let mut f = Fill::new();
            f.polyline(&pts, 1.2);
            paint_fill(f, tone(mint, 0.32), window, &mut st);
            let mut beads = Fill::new();
            for p in &pts[..pts.len() - 1] {
                beads.diamond(p.x, p.y, 3.0);
            }
            paint_fill(beads, tone(mint, 0.55), window, &mut st);
        }

        // ---- labels: the prism's rows first, then packages, modules, symbols
        let mut occ = Occupancy::anchored(&view, (projected.x(0.0), projected.y(0.0)));
        for chrome in look.reserved {
            occ.reserve([
                f32::from(chrome.origin.x),
                f32::from(chrome.origin.y),
                f32::from(chrome.right()),
                f32::from(chrome.bottom()),
            ]);
        }
        if let Some(card) = look.occupied {
            occ.reserve([
                f32::from(card.origin.x),
                f32::from(card.origin.y),
                f32::from(card.right()),
                f32::from(card.bottom()),
            ]);
        }
        if let Some(p) = look.prism {
            for sl in &p.slots {
                if let Some(l) = sl.label {
                    occ.reserve(l);
                }
            }
            for h in &p.heads {
                occ.reserve(h.label);
            }
            occ.reserve([p.fx - 60.0, p.fy - 14.0, p.fx + 60.0, p.fy + 14.0]);
        }
        let ts = look.text_scale;
        let mut texts: Vec<(Shaped, f32, f32)> = Vec::new();
        for (text, x, y, radius, r, color) in road_labels {
            let label = crate::data::text::shape_fit(
                &text,
                scaled(r, ts),
                color,
                (view.w - 24.0).max(1.0),
                window,
            );
            let (w, half) = (
                label.width(),
                (label.ascent() + label.descent()) * 0.5 + 3.0,
            );
            let spots = [
                (x + radius + 9.0, y),
                (x - radius - 9.0 - w, y),
                (x - w * 0.5, y + radius + half + 7.0),
                (x - w * 0.5, y - radius - half - 7.0),
            ];
            let mut chosen = None;
            for (lx, ly) in spots {
                if lx - 3.0 < view.x + 8.0
                    || lx + w + 3.0 > view.x + view.w - 8.0
                    || ly - half < view.y + 8.0
                    || ly + half > view.y + view.h - 8.0
                {
                    continue;
                }
                if occ.take(lx - 3.0, ly - half, lx + w + 3.0, ly + half) {
                    chosen = Some((lx, ly));
                    break;
                }
            }
            if let Some((lx, ly)) = chosen {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(px(lx - 3.0), px(ly - half)),
                        size: size(px(w + 6.0), px(half * 2.0)),
                    },
                    tone(palette.g0, 0.85),
                ));
                let base = baseline(&label, ly);
                texts.push((label, lx, base));
                st.labels += 1;
            }
        }
        for &p in &vis_p {
            let t = &layout.packages[p as usize];
            let pxs = f64::from(t.r) * k;
            let a = (smooth(pxs, 26.0, 60.0) * (1.0 - smooth(pxs, 1400.0, 2600.0))) as f32;
            if a < 0.02 {
                continue;
            }
            let size_px = (12.0 + pxs / 40.0).min(22.0).round() as f32;
            let r = scaled(role(Face::Display, 620.0, size_px), ts);
            let name = world.package_short(p);
            let c = if yours_pkg(p) {
                tone(mint, 0.9 * a)
            } else {
                tone(ink, 0.75 * a)
            };
            let label = shape(SharedString::from(name.to_owned()), r, c, window);
            let w = label.width();
            let x = sx(t.x) - w / 2.0;
            let lift = smooth(pxs, 220.0, 300.0) as f32;
            let y = sy(t.y) + (sy(t.bounds.y0) + 18.0 * ts - sy(t.y)) * lift;
            let half = r.size / 2.0 + 3.0;
            let title_box = [x - 4.0, y - half, x + w + 4.0, y + half];
            let subtitle_alpha =
                (smooth(pxs, 60.0, 90.0) * (1.0 - smooth(pxs, 420.0, 500.0))) as f32;
            let subtitle = if subtitle_alpha > 0.001 {
                let reach = scene.pkg_reach[p as usize];
                let text = if !yours_pkg(p) && reach > 0 {
                    format!(
                        "{} symbols · you use {reach}",
                        group(scene.pkg_size[p as usize])
                    )
                } else {
                    format!("{} symbols", group(scene.pkg_size[p as usize]))
                };
                let sub = shape(
                    SharedString::from(text),
                    scaled(roles::SUB, ts),
                    tone(ink, 0.32 * a * subtitle_alpha),
                    window,
                );
                let sy_ = y + r.size * 0.5 + 9.0 * ts;
                let sx_ = sx(t.x) - sub.width() / 2.0;
                let sub_half = (sub.ascent() + sub.descent()) * 0.5 + 2.0;
                let rect = [
                    sx_ - 2.0,
                    sy_ - sub_half,
                    sx_ + sub.width() + 2.0,
                    sy_ + sub_half,
                ];
                Some((sub, sx_, sy_, rect))
            } else {
                None
            };
            let with_sub = subtitle
                .as_ref()
                .is_some_and(|(_, _, _, rect)| occ.take_pair(title_box, *rect));
            if !with_sub && !occ.take(title_box[0], title_box[1], title_box[2], title_box[3]) {
                continue;
            }
            let base = baseline(&label, y);
            texts.push((label, x, base));
            st.labels += 1;
            if with_sub && let Some((sub, sx_, sy_, _)) = subtitle {
                let base = baseline(&sub, sy_);
                texts.push((sub, sx_, base));
                st.labels += 1;
            }
        }
        let mut by_size = vis_m.clone();
        by_size.sort_by(|&a, &b| {
            layout.modules[b as usize]
                .r
                .total_cmp(&layout.modules[a as usize].r)
                .then(a.cmp(&b))
        });
        for m in by_size {
            let t = &layout.modules[m as usize];
            let mr = f64::from(t.r) * k;
            let pr = f64::from(layout.packages[world.modules[m as usize].pkg as usize].r) * k;
            let a = (smooth(mr, 40.0, 80.0)
                * (1.0 - smooth(mr, 900.0, 1500.0))
                * smooth(pr, 200.0, 400.0)) as f32;
            if a < 0.03 {
                continue;
            }
            let path = &world.modules[m as usize].path;
            let name: SharedString = if path.is_empty() {
                "(root)".into()
            } else {
                path.clone()
            };
            let label = shape(
                name,
                scaled(roles::MODULE, ts),
                tone(ink, 0.42 * a * if look.focus.is_some() { 0.6 } else { 1.0 }),
                window,
            );
            let w = label.width();
            let x = sx((t.bounds.x0 + t.bounds.x1) / 2.0) - w / 2.0;
            let y = sy(t.bounds.y0) + 11.0 * ts;
            if !occ.take(x - 3.0, y - 8.0 * ts, x + w + 3.0, y + 8.0 * ts) {
                continue;
            }
            let base = baseline(&label, y);
            texts.push((label, x, base));
            st.labels += 1;
        }
        labels.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut budget = (40.0 + 120.0 * smooth(k, 4.0, 30.0)).round() as i32;
        let mut tried = 0;
        for &(prio, i, x, y, s) in &labels {
            if look
                .exploration
                .tour()
                .is_some_and(|road| road.stops.contains(&i))
                || look
                    .exploration
                    .chain()
                    .is_some_and(|road| road.iter().any(|stop| stop.node == i))
            {
                continue;
            }
            if budget <= 0 || tried > 1200 {
                break;
            }
            if x + s + 5.0 > view.x + view.w
                || y + 7.0 * ts < view.y
                || y - 7.0 * ts > view.y + view.h
            {
                continue;
            }
            tried += 1;
            let node = &world.nodes[i as usize];
            let member = node.parent.is_some();
            let sought = look
                .exploration
                .search()
                .is_some_and(|s| s.lit_set.contains(&i));
            let reached = look
                .exploration
                .reach()
                .and_then(|reach| reach.depth[world.top(i) as usize])
                .is_some_and(|d| {
                    d <= 2 && look.exploration.reach_wave() > f32::from(d.saturating_sub(1))
                });
            let strong = prio >= 10.0 || sought || reached;
            let r = if Some(i) == hi {
                roles::ITEM_BOLD
            } else if member {
                roles::MEMBER
            } else {
                roles::ITEM
            };
            let c = if Some(i) == hi || sought || (reached && !world.yours(i)) {
                tone(peri, 1.0)
            } else {
                let a = if strong {
                    0.95
                } else {
                    (if member {
                        0.45
                    } else {
                        0.4 + 0.45 * world.importance[i as usize]
                    }) * dim_all
                };
                let yours = world.yours(i) || (world.reached(i) && strong);
                tone(if yours { mint } else { ink }, a)
            };
            let label = shape(node.name.clone(), scaled(r, ts), c, window);
            let w = label.width();
            let lx = x + s + 5.0;
            if !occ.take(lx - 2.0, y - 7.0 * ts, lx + w + 2.0, y + 7.0 * ts) {
                continue;
            }
            budget -= 1;
            let base = baseline(&label, y);
            texts.push((label, lx, base));
            st.labels += 1;
        }
        // Labels land on whole device pixels: crisp, and one glyph raster
        // per glyph instead of one per sub-pixel phase while the map moves.
        let sf = window.scale_factor();
        window.paint_layer(bounds, |window| {
            for (label, x, base) in &texts {
                label.paint((x * sf).round() / sf, (base * sf).round() / sf, window, cx);
            }
        });
    }
    st
}

/// A flat, low-alpha halo; no gradient or per-symbol scene layer.
fn halo(batch: &mut Fill, x: f32, y: f32, r: f32) {
    let centre = pt(x, y);
    let mut previous = pt(x + r, y);
    for j in 1..=12 {
        let angle = j as f32 * std::f32::consts::TAU / 12.0;
        let next = pt(x + r * angle.cos(), y + r * angle.sin());
        batch.triangle(centre, previous, next);
        previous = next;
    }
}

fn quadratic_point(a: Pt, c: Pt, b: Pt, t: f32) -> Pt {
    let u = 1.0 - t;
    pt(
        u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
        u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
    )
}

type RoadLabel = (SharedString, f32, f32, f32, TypeRole, Hsla);

/// Short roads own their labels. Their geometry stays in three colour batches,
/// and dash clipping bounds work when a stop lies outside the current camera.
#[allow(clippy::cast_precision_loss)]
fn paint_roads(
    look: &Look<'_>,
    k: f32,
    sx: &impl Fn(f32) -> f32,
    sy: &impl Fn(f32) -> f32,
    clip: [f32; 4],
    window: &mut Window,
    st: &mut Stats,
) -> Vec<RoadLabel> {
    let (world, layout, p) = (&look.scene.world, &look.scene.layout, look.palette);
    let mut words = Vec::new();
    let mut road = Fill::new();
    let mut hinted = Fill::new();
    let mut ink = Fill::new();
    let mut peri = Fill::new();
    let mut mint = Fill::new();
    let mut halos = Fill::new();
    let mut pending: [Fill; 3] = std::array::from_fn(|_| Fill::new());
    let mut pending_halos = Fill::new();
    let mut bead = Fill::new();
    let mut bead_halo = Fill::new();
    let progress = look.exploration.road();
    let stops: Vec<_> = if let Some(chain) = look.exploration.chain() {
        chain
            .iter()
            .enumerate()
            .map(|(j, stop)| {
                (
                    stop.node,
                    stop.label.clone(),
                    j + 1 == chain.len(),
                    false,
                    stop.yours,
                )
            })
            .collect()
    } else if let Some(tour) = look.exploration.tour() {
        tour.stops
            .iter()
            .enumerate()
            .map(|(j, &i)| {
                (
                    i,
                    format!("{}  {}", j + 1, world.qual(i)),
                    j == tour.at,
                    j < tour.at,
                    false,
                )
            })
            .collect()
    } else {
        return words;
    };
    let r = (2.4 + k * 0.4).clamp(3.2, 6.5);
    for pair in stops.windows(2).enumerate() {
        let (j, pair) = pair;
        let (a, b) = (pair[0].0 as usize, pair[1].0 as usize);
        let (a, b) = (
            pt(sx(layout.x[a]), sy(layout.y[a])),
            pt(sx(layout.x[b]), sy(layout.y[b])),
        );
        let mut pts = vec![a];
        let ctrl = pt(
            (a.x + b.x) * 0.5 - (b.y - a.y) * 0.18,
            (a.y + b.y) * 0.5 + (b.x - a.x) * 0.18,
        );
        quad_to(&mut pts, a, ctrl, b, 20);
        if let Some((arc, t)) = progress.and_then(super::road::RoadSample::bead)
            && arc == j
        {
            let at = quadratic_point(a, ctrl, b, t);
            bead.diamond(at.x, at.y, 3.2);
            halo(&mut bead_halo, at.x, at.y, 8.0);
        }
        let near = look.exploration.chain().is_some()
            || look
                .exploration
                .tour()
                .is_some_and(|tour| j == tour.at || j + 1 == tour.at);
        if near {
            road.dashed_in(&pts, 1.2, 2.0, 5.0, 0.0, clip);
        } else {
            hinted.dashed_in(&pts, 1.2, 2.0, 5.0, 0.0, clip);
        }
        st.edges += 1;
    }
    for (index, (i, label, on, past, yours)) in stops.into_iter().enumerate() {
        let arrived = progress.is_none_or(|sample| sample.reached(index));
        let j = i as usize;
        let (x, y) = (sx(layout.x[j]), sy(layout.y[j]));
        if x < clip[0] - 16.0 || x > clip[2] + 16.0 || y < clip[1] - 16.0 || y > clip[3] + 16.0 {
            continue;
        }
        let radius = if on { r + 2.0 } else { r };
        let [pending_ink, pending_peri, pending_mint] = &mut pending;
        let mut_ink = if arrived { &mut ink } else { pending_ink };
        let mut_peri = if arrived { &mut peri } else { pending_peri };
        let mut_mint = if arrived { &mut mint } else { pending_mint };
        if yours {
            mut_mint.diamond_ring(x, y, radius + 2.5, 1.4);
        } else if on {
            halo(
                if arrived {
                    &mut halos
                } else {
                    &mut pending_halos
                },
                x,
                y,
                radius + 7.0,
            );
            mut_peri.diamond(x, y, radius);
        } else if past {
            mut_ink.diamond_ring(x, y, radius, 1.3);
        } else if look.exploration.chain().is_some() {
            mut_ink.diamond(x, y, radius);
        } else {
            mut_peri.diamond_ring(x, y, radius, 1.3);
        }
        let c = if yours {
            p.mint.base
        } else if on || !past {
            p.peri.base
        } else {
            p.ink1
        };
        words.push((
            label.into(),
            x,
            y,
            radius,
            if on { roles::ITEM_BOLD } else { roles::ITEM },
            tone(
                c,
                if progress.is_some() {
                    if arrived { 1.0 } else { 0.35 }
                } else if on || yours {
                    1.0
                } else {
                    0.75
                },
            ),
        ));
    }
    for (batch, color) in [
        (road, tone(p.peri.base, 0.65)),
        (hinted, tone(p.peri.base, 0.12)),
        (halos, tone(p.peri.base, 0.16)),
        (ink, tone(p.ink1, 0.75)),
        (peri, tone(p.peri.base, 0.95)),
        (mint, tone(p.mint.base, 0.95)),
        (pending_halos, tone(p.peri.base, 0.16 * 0.35)),
        (std::mem::take(&mut pending[0]), tone(p.ink1, 0.75 * 0.35)),
        (
            std::mem::take(&mut pending[1]),
            tone(p.peri.base, 0.95 * 0.35),
        ),
        (
            std::mem::take(&mut pending[2]),
            tone(p.mint.base, 0.95 * 0.35),
        ),
        (bead_halo, tone(p.mint.base, 0.2)),
        (bead, tone(p.mint.base, 1.0)),
    ] {
        if !batch.is_empty() {
            batch.paint(window, color);
            st.paths += 1;
        }
    }
    words
}

/// `12345` → `12,345`.
#[must_use]
pub fn group(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Occupancy, clipped_dashes, group};
    use crate::graph::camera::View;
    use crate::paint::geom::pt;

    #[test]
    fn offscreen_dashes_keep_direction_phase_and_bounded_work() {
        let mut got = Vec::new();
        clipped_dashes(
            &[pt(-1_000_000.0, 10.0), pt(1_000_000.0, 10.0)],
            2.0,
            7.0,
            0.0,
            [0.0, 0.0, 100.0, 20.0],
            |a, b| got.push((a.x, b.x)),
        );
        // A million skipped pixels leave phase 1: the first lit pixel is 0..1.
        assert_eq!(got[0], (0.0, 1.0));
        assert_eq!(got[1], (8.0, 10.0));
        assert_eq!(got.len(), 12, "cost depends on the 100 visible pixels");
        assert!(got.iter().all(|&(a, b)| a >= 0.0 && b <= 100.0));
        let mut after_corner = Vec::new();
        clipped_dashes(
            &[pt(-10.0, -10.0), pt(0.0, -10.0), pt(0.0, 20.0)],
            2.0,
            7.0,
            0.0,
            [-1.0, 0.0, 10.0, 10.0],
            |a, b| after_corner.push((a.y, b.y)),
        );
        assert_eq!(
            after_corner,
            vec![(7.0, 9.0)],
            "the offscreen first segment also advances phase"
        );
    }

    #[test]
    fn non_round_clipped_dash_tails_always_terminate() {
        for endpoint in [99.99999, 100.00001, 137.31, 199.99997, 339.17157] {
            for phase in [
                -137.123,
                -0.00000001,
                0.0,
                1.9999999,
                2.0000001,
                8.999999,
                981.123,
            ] {
                for offset in [-9000.137, -1.13, 0.131] {
                    let mut count = 0;
                    let inspected = clipped_dashes(
                        &[pt(offset, 13.71), pt(endpoint, 217.113)],
                        2.0,
                        7.0,
                        phase,
                        [0.13, 1.17, 100.31, 170.19],
                        |a, b| {
                            count += 1;
                            assert!(count < 200, "work is bounded by visible length");
                            assert!(
                                a.x.is_finite()
                                    && a.y.is_finite()
                                    && b.x.is_finite()
                                    && b.y.is_finite()
                            );
                        },
                    );
                    assert!(
                        inspected <= 24,
                        "clipped 100×170 viewport intersects at most24 nine-pixel periods"
                    );
                }
            }
        }
    }

    #[test]
    fn clipped_diagonal_and_reversed_dashes_preserve_numeric_geometry() {
        let clip = [0.13, 1.17, 100.31, 170.19];
        let paths = [
            (pt(-150.7, 23.11), pt(182.137, 85.713)),
            (pt(182.137, 85.713), pt(-150.7, 23.11)),
            (pt(30.17, -170.73), pt(82.91, 290.13)),
        ];
        for (from, to) in paths {
            let (dx, dy) = (f64::from(to.x - from.x), f64::from(to.y - from.y));
            let len = dx.hypot(dy);
            for phase in [-137.123, -0.00000001, 0.0, 1.9999999, 8.999999, 981.123] {
                let inspected = clipped_dashes(&[from, to], 2.0, 7.0, phase, clip, |a, b| {
                    for p in [a, b] {
                        assert!(
                            p.x >= clip[0] - 1e-3
                                && p.x <= clip[2] + 1e-3
                                && p.y >= clip[1] - 1e-3
                                && p.y <= clip[3] + 1e-3,
                            "endpoint stays inside clip"
                        );
                        let (px, py) = (f64::from(p.x - from.x), f64::from(p.y - from.y));
                        assert!(
                            (dx * py - dy * px).abs() / len < 2e-4,
                            "collinear with original directed segment"
                        );
                        let along = (px * dx + py * dy) / len;
                        let dash_phase = (f64::from(phase) + along).rem_euclid(9.0);
                        assert!(
                            dash_phase <= 2.002 || dash_phase >= 8.998,
                            "clipped endpoints stay in lit part of source phase: {dash_phase}"
                        );
                    }
                    let length = (b.x - a.x).hypot(b.y - a.y);
                    assert!(
                        length > 0.0 && length <= 2.001,
                        "a lit interval is positive and no longer than2px"
                    );
                    assert!(
                        f64::from(b.x - a.x) * dx + f64::from(b.y - a.y) * dy > 0.0,
                        "direction remains source-to-target"
                    );
                });
                assert!(
                    inspected <= 24,
                    "analytical work bound follows visible viewport"
                );
            }
        }
    }

    #[test]
    fn dash_work_and_phase_are_invariant_under_remote_extension() {
        let clip = [0.0, 0.0, 100.0, 20.0];
        let mut expected = Vec::new();
        clipped_dashes(
            &[pt(-90.0, 10.0), pt(90.0, 10.0), pt(180.0, 10.0)],
            2.0,
            7.0,
            0.0,
            clip,
            |a, b| expected.push((a.x, b.x)),
        );
        for remote in [900.0, 9_000.0, 90_000.0, 9_000_000.0] {
            let mut got = Vec::new();
            clipped_dashes(
                &[pt(-remote, 10.0), pt(remote, 10.0)],
                2.0,
                7.0,
                0.0,
                clip,
                |a, b| got.push((a.x, b.x)),
            );
            assert_eq!(
                got.len(),
                expected.len(),
                "remote length cannot increase visible work"
            );
            for (a, b) in got.iter().zip(&expected) {
                assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3);
            }
        }
        let mut subdivided = Vec::new();
        clipped_dashes(
            &[
                pt(-90.0, 10.0),
                pt(-18.0, 10.0),
                pt(36.0, 10.0),
                pt(180.0, 10.0),
            ],
            2.0,
            7.0,
            0.0,
            clip,
            |a, b| subdivided.push((a.x, b.x)),
        );
        assert_eq!(
            subdivided, expected,
            "segment boundaries do not reset direction phase"
        );
        let mut offscreen = 0;
        clipped_dashes(
            &[pt(-9_000_000.0, -10.0), pt(9_000_000.0, -10.0)],
            2.0,
            7.0,
            0.0,
            clip,
            |_, _| offscreen += 1,
        );
        assert_eq!(
            offscreen, 0,
            "entirely offscreen high-fanout edges add no geometry"
        );
    }

    #[test]
    fn labels_never_overlap_reserved_card_prism_or_road_at_any_text_scale() {
        for width in [480.0, 760.0, 1440.0, 2560.0] {
            for scale in [0.85, 1.0, 1.25, 1.5, 2.0] {
                let view = View {
                    x: 37.0,
                    y: 19.0,
                    w: width,
                    h: 900.0,
                };
                let mut occ = Occupancy::new(&view);
                let reserved = [
                    [
                        view.x + width - 180.0,
                        view.y + 20.0,
                        view.x + width - 12.0,
                        view.y + 220.0,
                    ],
                    [
                        view.x + 100.0,
                        view.y + 300.0,
                        view.x + 210.0,
                        view.y + 330.0,
                    ],
                    [
                        view.x + 150.0,
                        view.y + 320.0,
                        view.x + 280.0,
                        view.y + 350.0,
                    ],
                ];
                for rect in reserved {
                    occ.reserve(rect);
                }
                let mut accepted: Vec<[f32; 4]> = Vec::new();
                for row in 0..30 {
                    for col in 0..20 {
                        let x = view.x + col as f32 * width / 20.0;
                        let y = view.y + row as f32 * 28.0;
                        let rect = [x, y, x + 70.0 * scale, y + 17.0 * scale];
                        if occ.take(rect[0], rect[1], rect[2], rect[3]) {
                            for other in reserved.iter().chain(&accepted) {
                                assert!(
                                    !(rect[0] < other[2]
                                        && rect[2] > other[0]
                                        && rect[1] < other[3]
                                        && rect[3] > other[1]),
                                    "accepted labels overlap foreground or one another"
                                );
                            }
                            assert!(
                                rect[0] >= view.x
                                    && rect[1] >= view.y
                                    && rect[2] <= view.x + view.w
                                    && rect[3] <= view.y + view.h,
                                "no accepted text silently clips"
                            );
                            accepted.push(rect);
                        }
                    }
                }
                assert!(!accepted.is_empty());
            }
        }
    }

    #[test]
    fn ambient_endpoint_relevance_fades_continuously_at_territory_boundary() {
        let view = View {
            x: 37.0,
            y: 53.0,
            w: 480.0,
            h: 618.0,
        };
        let mut previous = 1.0;
        for n in 0..=1600 {
            let outside = -8.0 + n as f32 * 0.125;
            let relevance = super::ambient_relevance(
                &view,
                [
                    view.x + view.w + outside,
                    view.y + 100.0,
                    view.x + view.w + outside + 40.0,
                    view.y + 140.0,
                ],
            );
            assert!((0.0..=1.0).contains(&relevance));
            assert!(relevance <= previous + 1e-6);
            assert!((relevance - previous).abs() < 0.0021);
            previous = relevance;
            if outside >= 96.0 {
                assert_eq!(relevance, 0.0);
            }
        }
        assert_eq!(
            super::ambient_relevance(
                &view,
                [view.x + 20.0, view.y + 20.0, view.x + 60.0, view.y + 60.0]
            ),
            1.0
        );
    }
    #[test]
    fn package_two_line_label_reserves_itself_as_one_group() {
        let view = View {
            x: 37.0,
            y: 53.0,
            w: 480.0,
            h: 618.0,
        };
        let title = [200.0, 200.0, 280.0, 220.0];
        let subtitle = [190.0, 217.0, 290.0, 232.0];
        let mut old = Occupancy::new(&view);
        assert!(old.take(title[0], title[1], title[2], title[3]));
        assert!(!old.take(subtitle[0], subtitle[1], subtitle[2], subtitle[3]));
        let mut composite = Occupancy::new(&view);
        assert!(composite.take_pair(title, subtitle));
        assert!(!composite.take(200.0, 223.0, 210.0, 229.0));
        let mut blocked = Occupancy::new(&view);
        blocked.reserve([190.0, 226.0, 290.0, 236.0]);
        assert!(!blocked.take_pair(title, subtitle));
        assert!(blocked.take(title[0], title[1], title[2], title[3]));
    }

    #[test]
    fn carried_value_uses_the_exact_road_quadratic_under_projection() {
        for scale in [0.1, 1.0, 2.0, 26.0] {
            let a = super::pt(37.0, 53.0);
            let b = super::pt(137.0, 73.0);
            let c = super::pt(67.0, 103.0);
            let mut points = vec![a];
            super::quad_to(&mut points, a, c, b, 20);
            let transform = |p: super::Pt| super::pt(p.x * scale + 113.0, p.y * scale - 79.0);
            for (i, &sampled) in points.iter().enumerate() {
                let t = i as f32 / 20.0;
                let bead = super::quadratic_point(a, c, b, t);
                assert_eq!(bead.x, sampled.x);
                assert_eq!(bead.y, sampled.y);
                let projected = super::quadratic_point(transform(a), transform(c), transform(b), t);
                let expected = transform(bead);
                assert!(
                    (projected.x - expected.x).abs() < 0.001
                        && (projected.y - expected.y).abs() < 0.001
                );
                assert!(projected.x.is_finite() && projected.y.is_finite());
            }
        }
    }

    #[test]
    fn departing_hover_keeps_dim_continuous_and_returns_to_exact_free_state() {
        for step in 0..=100 {
            let alpha = 1.0 - step as f32 / 100.0;
            let active = super::hover_envelope(Some(alpha), None);
            let departing = super::hover_envelope(None, Some(alpha));
            assert_eq!(active, departing);
            assert_eq!(
                super::hover_envelope(Some(1.0 - alpha), Some(alpha)),
                alpha.max(1.0 - alpha)
            );
        }
        assert_eq!(super::hover_envelope(None, None), 0.0);
        assert_eq!(super::hover_envelope(None, Some(0.0)), 0.0);
    }

    #[test]
    fn visible_leaf_curve_fades_before_endpoint_query_boundary() {
        let view = View {
            x: 37.0,
            y: 53.0,
            w: 480.0,
            h: 618.0,
        };
        let mut previous = 1.0;
        for step in 0..=320 {
            let outside = -8.0 + step as f32 * 0.125;
            let alpha = super::endpoint_fade(&view, view.x + view.w + outside, view.y + 100.0);
            assert!(alpha <= previous + 1e-6 && alpha >= 0.0);
            assert!(
                (alpha - previous).abs() < 0.006,
                "abrupt curve disappearance at{outside}"
            );
            previous = alpha;
            if outside >= 24.0 {
                assert_eq!(alpha, 0.0);
            }
        }
        assert_eq!(
            super::endpoint_fade(&view, view.x + 32.0, view.y + 32.0),
            1.0
        );
    }

    #[test]
    fn anchored_occupancy_preserves_choices_under_subcell_pan() {
        let view = View {
            x: 31.0,
            y: 47.0,
            w: 480.0,
            h: 640.0,
        };
        let boxes = [
            [101.25, 107.5, 181.25, 121.5],
            [187.75, 107.5, 260.75, 121.5],
            [104.0, 122.5, 190.0, 136.5],
            [112.0, 162.0, 208.0, 176.0],
        ];
        let mut baseline = None;
        for step in -96..=96 {
            let dx = step as f32 * 0.125;
            let dy = step as f32 * -0.0625;
            let mut occ = Occupancy::anchored(&view, (83.125 + dx, 97.375 + dy));
            let accepted: Vec<_> = boxes
                .iter()
                .map(|&[a, b, c, d]| occ.take(a + dx, b + dy, c + dx, d + dy))
                .collect();
            if let Some(ref base) = baseline {
                assert_eq!(&accepted, base, "pan {dx},{dy}");
            } else {
                baseline = Some(accepted);
            }
            assert!(!occ.take(view.x - 1.0, view.y, view.x + 8.0, view.y + 8.0));
        }
    }

    #[test]
    fn occupancy_refuses_overlaps_and_offscreen_boxes() {
        let mut occ = Occupancy::new(&View {
            x: 100.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        });
        assert!(occ.take(110.0, 10.0, 190.0, 24.0));
        assert!(!occ.take(150.0, 20.0, 250.0, 30.0), "overlaps the first");
        assert!(occ.take(210.0, 40.0, 300.0, 54.0));
        assert!(!occ.take(0.0, 0.0, 90.0, 10.0), "left of the view");
        assert!(!occ.take(600.0, 0.0, 700.0, 10.0), "right of the view");
        occ.reserve([110.0, 10.0, 300.0, 24.0]);
        assert!(
            !occ.take(270.0, 10.0, 290.0, 24.0),
            "overlapping foreground reservations protect their union"
        );
        assert_eq!(group(1_930), "1,930");
        assert_eq!(group(53_860), "53,860");
        assert_eq!(group(331), "331");
    }
}
