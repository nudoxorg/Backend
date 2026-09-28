//! The stone: a package's face, cut by its modules (`v6/gems/GEMS.md`).
//!
//! The outline is the language: seven cuts, one per ecosystem, distinct at
//! 12 px. The facets are the modules, by recursive weighted bisection with
//! straight cuts: every piece of a convex outline cut by a line is convex,
//! and each facet's area is its module's share of the declarations. Cut
//! angles come from a hash of the package, so a stone never reshuffles and
//! can be learned at a glance.
//!
//! A [`Cutting`] is computed once in a canonical box and mapped affinely into
//! any bounds. Affine maps keep area ratios, so the 14 px shelf stone, the
//! 64 px hero and the reader-wide floor plan are the same object, and a
//! stone growing into its floor plan moves every vertex along one line.
//!
//! Light: facets are silver at an opacity set by how squarely they face the
//! light. A facet turned full to it shows *fire*: its module's kind hue. A
//! lit facet (a query match, a use) is periwinkle, and while anything is lit
//! the fire goes out. Colour means one thing at a time.

use super::geom::{Fill, Poly, Pt, pt};
use crate::marks::eco::Eco;
use crate::theme::ActiveFacet;
use gpui::{
    App, Bounds, ColorExt, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window, px,
};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

/// The canonical box height every cutting is computed in.
const UNIT: f32 = 100.0;
/// No facet is cut smaller than this share of its stone: a one-declaration
/// module must still be a facet you can see and point at.
pub const FLOOR: f32 = 0.018;
/// A facet cuts its submodules only above this share of the stone.
const SUBCUT_FROM: f32 = 0.06;
/// How far a cut may turn from square across the longer side (radians).
const TURN: f32 = 0.45;
/// Below this height a stone is micro: outline and the first two cut levels.
pub const MICRO_BELOW: f32 = 26.0;

/// The language's cut.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Outline {
    /// Rust: a regular octagon.
    Octagon,
    /// Go: a chamfered 1.36 : 1 rectangle.
    Emerald,
    /// TypeScript and JavaScript: a shield.
    Shield,
    /// Python: an oval.
    Oval,
    /// Java: a marquise.
    Marquise,
    /// C#: a pointed hexagon.
    Hexagon,
    /// C and C++: a trillion.
    Trillion,
}

impl Outline {
    /// The cut for an ecosystem.
    #[must_use]
    pub const fn of(eco: Eco) -> Self {
        match eco {
            Eco::Crates => Self::Octagon,
            Eco::Go => Self::Emerald,
            Eco::Npm => Self::Shield,
            Eco::Pypi => Self::Oval,
            Eco::Maven => Self::Marquise,
            Eco::Nuget => Self::Hexagon,
            Eco::Cpp => Self::Trillion,
        }
    }

    /// Width over height.
    #[must_use]
    pub const fn aspect(self) -> f32 {
        match self {
            Self::Octagon | Self::Shield => 1.0,
            Self::Emerald => 1.36,
            Self::Oval => 0.82,
            Self::Marquise => 0.66,
            Self::Hexagon => 0.9,
            Self::Trillion => 1.14,
        }
    }

    /// The outline in a `w` × `h` box at the origin, positively oriented
    /// (clockwise on screen).
    #[must_use]
    pub fn poly(self, w: f32, h: f32) -> Poly {
        match self {
            Self::Octagon | Self::Emerald => {
                let c = if self == Self::Octagon { 0.29 } else { 0.16 } * w.min(h);
                Poly::new([
                    pt(c, 0.0),
                    pt(w - c, 0.0),
                    pt(w, c),
                    pt(w, h - c),
                    pt(w - c, h),
                    pt(c, h),
                    pt(0.0, h - c),
                    pt(0.0, c),
                ])
            }
            Self::Shield => Poly::new([
                pt(0.0, 0.0),
                pt(w, 0.0),
                pt(w, 0.6 * h),
                pt(w * 0.5, h),
                pt(0.0, 0.6 * h),
            ]),
            Self::Oval => ring(20, |t| {
                let a = t * std::f32::consts::TAU - FRAC_PI_2;
                pt(w * 0.5 + w * 0.5 * a.cos(), h * 0.5 + h * 0.5 * a.sin())
            }),
            Self::Marquise => {
                // Two arcs meeting in points at the top and the bottom.
                let mut points = Vec::with_capacity(16);
                for i in 0..=8 {
                    let t = i as f32 / 8.0;
                    points.push(pt(w * 0.5 + w * 0.5 * (std::f32::consts::PI * t).sin(), h * t));
                }
                for i in 1..8 {
                    let t = 1.0 - i as f32 / 8.0;
                    points.push(pt(w * 0.5 - w * 0.5 * (std::f32::consts::PI * t).sin(), h * t));
                }
                Poly::new(points)
            }
            Self::Hexagon => Poly::new([
                pt(w * 0.5, 0.0),
                pt(w, h * 0.25),
                pt(w, h * 0.75),
                pt(w * 0.5, h),
                pt(0.0, h * 0.75),
                pt(0.0, h * 0.25),
            ]),
            Self::Trillion => {
                let c = 0.12 * w;
                Poly::new([
                    pt(w * 0.5 - c * 0.6, 0.0),
                    pt(w * 0.5 + c * 0.6, 0.0),
                    pt(w, h - c),
                    pt(w - c, h),
                    pt(c, h),
                    pt(0.0, h - c),
                ])
            }
        }
    }
}

fn ring(n: usize, at: impl Fn(f32) -> Pt) -> Poly {
    Poly::new((0..n).map(|i| at(i as f32 / n as f32)))
}

/// One module to cut: its declarations and, optionally, its submodules'.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// Declarations (any non-negative weight).
    pub weight: f32,
    /// A stable key (the module's name, hashed): the facet's tilt, so the
    /// light falls on the same facets every time.
    pub key: u64,
    /// Submodules, cut inside this facet when it is big enough.
    pub children: Vec<Part>,
}

impl Part {
    /// A part with no submodules.
    #[must_use]
    pub fn new(weight: f32, key: u64) -> Self {
        Self { weight, key, children: Vec::new() }
    }
}

/// One facet of a cutting.
#[derive(Clone, Debug, PartialEq)]
pub struct Facet {
    /// The piece, in the cutting's canonical box.
    pub poly: Poly,
    /// Index of the top-level part (the module) this facet belongs to.
    pub part: usize,
    /// `Some(i)`: the facet is child `i` of that part (a submodule), or the
    /// part's own remainder when `i == children.len()`.
    pub child: Option<usize>,
    /// Which way the facet faces: where it sits in the stone plus a stable
    /// per-facet jitter (the cut). Unit-ish; lighting reads its direction.
    pub tilt: Pt,
}

/// A cut stone in its canonical box (`aspect * 100` × `100`).
#[derive(Clone, Debug, PartialEq)]
pub struct Cutting {
    /// The language's cut.
    pub outline: Outline,
    /// The outline polygon.
    pub rim: Poly,
    /// Every facet, top-level parts first in cut order, then submodules.
    pub facets: Vec<Facet>,
    /// Pieces not yet cut (a depth-limited reveal: the part still unread).
    pub rough: Vec<Poly>,
}

impl Cutting {
    /// The canonical box's width.
    #[must_use]
    pub fn width(&self) -> f32 {
        UNIT * self.outline.aspect()
    }

    /// Maps a canonical point into `bounds`.
    #[must_use]
    pub fn place(&self, p: Pt, bounds: Bounds<Pixels>) -> Pt {
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        pt(x + p.x / self.width() * w, y + p.y / UNIT * h)
    }

    /// A canonical polygon mapped into `bounds`.
    #[must_use]
    pub fn placed(&self, poly: &Poly, bounds: Bounds<Pixels>) -> Poly {
        Poly::new(poly.points().iter().map(|&p| self.place(p, bounds)))
    }

    /// The top-level facet of `part`, if it was cut.
    #[must_use]
    pub fn facet_of(&self, part: usize) -> Option<&Facet> {
        self.facets.iter().find(|f| f.part == part && f.child.is_none())
    }

    /// Where a `w` × `h` label centred on `part`'s facet would sit in
    /// `bounds`, if it fits entirely inside the facet (never clipped, never
    /// over an edge: law 2). Tries the centroid, then a little above and below.
    #[must_use]
    pub fn label_at(&self, part: usize, w: f32, h: f32, bounds: Bounds<Pixels>) -> Option<Pt> {
        let facet = self.facet_of(part)?;
        let placed = self.placed(&facet.poly, bounds);
        let c = centroid(&placed);
        [0.0, -0.18, 0.18].iter().find_map(|&shift| {
            let (min, max) = placed.bounds();
            let at = pt(c.x, c.y + shift * (max.y - min.y));
            let corners = [
                pt(at.x - w / 2.0, at.y - h / 2.0),
                pt(at.x + w / 2.0, at.y - h / 2.0),
                pt(at.x + w / 2.0, at.y + h / 2.0),
                pt(at.x - w / 2.0, at.y + h / 2.0),
            ];
            corners.iter().all(|&p| placed.contains(p)).then_some(at)
        })
    }
}

/// The centroid of a convex polygon.
#[must_use]
pub fn centroid(poly: &Poly) -> Pt {
    let pts = poly.points();
    let (mut a, mut x, mut y) = (0.0, 0.0, 0.0);
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        let f = p.x * q.y - q.x * p.y;
        a += f;
        x += (p.x + q.x) * f;
        y += (p.y + q.y) * f;
    }
    if a.abs() < 1e-9 {
        return pts.first().copied().unwrap_or_default();
    }
    pt(x / (3.0 * a), y / (3.0 * a))
}

/// FNV-1a over a name: the seed for a package's cuts, the key for a module's tilt.
#[must_use]
pub fn key(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// splitmix64: a stable unit float from a seed and a salt.
fn unit(seed: u64, salt: u64) -> f32 {
    let mut z = seed ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    #[allow(clippy::cast_precision_loss)]
    let v = (z >> 40) as f32 / (1u64 << 24) as f32;
    v
}

/// Cuts `outline` by `parts`. `seed` is the package's [`key`]; `levels`
/// limits the top-level bisection depth (a reveal; `u8::MAX` = the whole
/// stone), and uncut pieces are returned as [`Cutting::rough`].
#[must_use]
pub fn cut(outline: Outline, parts: &[Part], seed: u64, levels: u8) -> Cutting {
    let rim = outline.poly(UNIT * outline.aspect(), UNIT);
    let mut out = Cutting { outline, rim: rim.clone(), facets: Vec::new(), rough: Vec::new() };
    let total: f32 = parts.iter().map(|p| p.weight.max(0.0)).sum();
    if parts.is_empty() || total <= 0.0 {
        out.rough.push(rim);
        return out;
    }
    let items: Vec<(usize, f32)> =
        parts.iter().enumerate().map(|(i, p)| (i, p.weight.max(0.0).max(total * FLOOR))).collect();
    let mut leaves = Vec::new();
    bisect(rim.clone(), items, seed, 0, levels, &mut leaves, &mut out.rough);
    let whole = rim.area();
    let centre = pt(UNIT * outline.aspect() * 0.5, UNIT * 0.5);
    for (poly, part) in leaves {
        out.facets.push(facet(&poly, part, None, parts[part].key, centre, outline));
        let p = &parts[part];
        if p.children.is_empty() || poly.area() / whole < SUBCUT_FROM || levels != u8::MAX {
            continue;
        }
        let own = (p.weight - p.children.iter().map(|c| c.weight).sum::<f32>()).max(0.0);
        let mut kids: Vec<(usize, f32)> =
            p.children.iter().enumerate().map(|(i, c)| (i, c.weight.max(p.weight * 0.05))).collect();
        if own > 0.0 {
            kids.push((p.children.len(), own));
        }
        let mut sub = Vec::new();
        let mut unused = Vec::new();
        bisect(poly, kids, seed ^ p.key, 0, u8::MAX, &mut sub, &mut unused);
        for (poly, child) in sub {
            let k = p.children.get(child).map_or(p.key ^ 0x5eed, |c| c.key);
            out.facets.push(facet(&poly, part, Some(child), k, centre, outline));
        }
    }
    out
}

fn facet(poly: &Poly, part: usize, child: Option<usize>, key: u64, centre: Pt, outline: Outline) -> Facet {
    let c = centroid(poly);
    let (hw, hh) = (UNIT * outline.aspect() * 0.5, UNIT * 0.5);
    let tilt = pt(
        (c.x - centre.x) / hw * 0.55 + (unit(key, 1) - 0.5) * 1.3,
        (c.y - centre.y) / hh * 0.55 + (unit(key, 2) - 0.5) * 1.3,
    );
    Facet { poly: poly.clone(), part, child, tilt }
}

fn bisect(
    poly: Poly,
    mut items: Vec<(usize, f32)>,
    seed: u64,
    depth: u8,
    levels: u8,
    leaves: &mut Vec<(Poly, usize)>,
    rough: &mut Vec<Poly>,
) {
    if items.len() == 1 {
        leaves.push((poly, items[0].0));
        return;
    }
    if depth >= levels {
        rough.push(poly);
        return;
    }
    // Heaviest first, into whichever half is lighter: balanced halves, so
    // the cut tree stays shallow and the facets stay chunky.
    items.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut tl, mut tr) = (0.0_f32, 0.0_f32);
    for item in items {
        if tl <= tr {
            tl += item.1;
            left.push(item);
        } else {
            tr += item.1;
            right.push(item);
        }
    }
    let (min, max) = poly.bounds();
    let across = if max.x - min.x >= max.y - min.y { 0.0 } else { FRAC_PI_2 };
    #[allow(clippy::cast_lossless)]
    let turn = (unit(seed, u64::from(depth) << 8 | (left.len() + right.len()) as u64) - 0.5) * 2.0 * TURN;
    let (a, b) = split(&poly, across + turn, tl / (tl + tr));
    bisect(a, left, seed.rotate_left(7) ^ 0xa, depth + 1, levels, leaves, rough);
    bisect(b, right, seed.rotate_left(13) ^ 0xb, depth + 1, levels, leaves, rough);
}

/// Splits `poly` by a line with normal angle `angle` so the first piece
/// (`n·p <= d`) holds `ratio` of the area.
fn split(poly: &Poly, angle: f32, ratio: f32) -> (Poly, Poly) {
    let n = pt(angle.cos(), angle.sin());
    let dot = |p: &Pt| n.x * p.x + n.y * p.y;
    let (mut lo, mut hi) = poly
        .points()
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(dot(p)), hi.max(dot(p))));
    let total = poly.area();
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if below(poly, n, mid).area() / total < ratio {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let d = 0.5 * (lo + hi);
    (below(poly, n, d), above(poly, n, d))
}

/// The part of `poly` where `n·p <= d`.
fn below(poly: &Poly, n: Pt, d: f32) -> Poly {
    let a = pt(n.x * d, n.y * d);
    poly.clip_half_plane(a, pt(a.x - n.y, a.y + n.x))
}

/// The part of `poly` where `n·p >= d`.
fn above(poly: &Poly, n: Pt, d: f32) -> Poly {
    let a = pt(n.x * d, n.y * d);
    poly.clip_half_plane(pt(a.x - n.y, a.y + n.x), a)
}

/// The light's direction: from the upper left by default (the ground's
/// light); the pointer may move it.
pub const LIGHT: Pt = Pt { x: -0.55, y: -0.83 };

/// How much a facet facing `tilt` catches `light`: `-1` (away) to `1` (full).
#[must_use]
pub fn facing(tilt: Pt, light: Pt) -> f32 {
    let n = (tilt.x * tilt.x + tilt.y * tilt.y).sqrt().max(1e-6);
    let l = (light.x * light.x + light.y * light.y).sqrt().max(1e-6);
    -(tilt.x * light.x + tilt.y * light.y) / (n * l)
}

/// Above this facing a facet shows fire.
pub const FIRE_FROM: f32 = 0.8;

/// A facet's silver opacity.
#[must_use]
pub fn shade(tilt: Pt, light: Pt, micro: bool) -> f32 {
    let d = facing(tilt, light);
    let b = d.max(0.0).powf(1.6);
    if micro {
        0.06 + 0.2 * b
    } else {
        0.045 + 0.19 * b + if d > FIRE_FROM { 0.12 } else { 0.0 }
    }
}

/// A stone element. Build with [`stone`].
pub struct Stone {
    style: StyleRefinement,
    cutting: Arc<Cutting>,
    height: f32,
    width: Option<f32>,
    fire: Arc<[Hsla]>,
    lit: Option<Arc<[bool]>>,
    hot: Option<usize>,
    yours: bool,
    rough: bool,
    light: Pt,
}

/// A stone of `cutting`, `height` px tall at its language's aspect.
#[must_use]
pub fn stone(cutting: Arc<Cutting>, height: f32) -> Stone {
    Stone {
        style: StyleRefinement::default(),
        cutting,
        height,
        width: None,
        fire: Arc::from([]),
        lit: None,
        hot: None,
        yours: false,
        rough: false,
        light: LIGHT,
    }
}

impl Stone {
    /// Stretches the stone to `width` (the floor plan): an affine map, so
    /// every area stays honest and the facets are the hero's facets.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Each part's fire: the kind hue of its module's landmark.
    #[must_use]
    pub fn fire(mut self, hues: impl Into<Arc<[Hsla]>>) -> Self {
        self.fire = hues.into();
        self
    }

    /// Parts lit by a query or by uses (periwinkle); the fire goes out.
    #[must_use]
    pub fn lit(mut self, lit: impl Into<Arc<[bool]>>) -> Self {
        self.lit = Some(lit.into());
        self
    }

    /// The hovered part: its facet's edge lights (bevel = state).
    #[must_use]
    pub const fn hot(mut self, part: Option<usize>) -> Self {
        self.hot = part;
        self
    }

    /// Yours: a mint rim.
    #[must_use]
    pub const fn yours(mut self, yours: bool) -> Self {
        self.yours = yours;
        self
    }

    /// Not indexed (or not read yet): a dashed rim, no facets.
    #[must_use]
    pub const fn rough(mut self, rough: bool) -> Self {
        self.rough = rough;
        self
    }

    /// Where the light comes from (the pointer, or [`LIGHT`]).
    #[must_use]
    pub const fn light(mut self, light: Pt) -> Self {
        self.light = light;
        self
    }
}

impl Styled for Stone {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Stone {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Stone {
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
        let width = self.width.unwrap_or(self.height * self.cutting.outline.aspect());
        style.size.width = px(width).into();
        style.size.height = px(self.height).into();
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
        paint_stone(window, cx, bounds, self);
    }
}

/// Opacity buckets per 1/200: invisible steps, a handful of paths.
const QUANTUM: f32 = 200.0;

fn paint_stone(window: &mut Window, cx: &App, bounds: Bounds<Pixels>, stone: &Stone) {
    let palette = cx.palette();
    let opacity = stone.style.opacity.unwrap_or(1.0);
    let c = &stone.cutting;
    let h = f32::from(bounds.size.height);
    if h <= 0.0 {
        return;
    }
    let micro = h < MICRO_BELOW;
    let rim = c.placed(&c.rim, bounds);
    let silver: Hsla = palette.ink1.into();
    let rim_ink: Hsla = if stone.yours {
        palette.mint.base.into()
    } else if micro {
        palette.ink2.into()
    } else {
        silver.opacity(0.42)
    };
    let rim_width: f32 = if stone.yours { 1.5 } else if micro { 1.0 } else { 1.2 };

    let mut ground = Fill::new();
    ground.poly(&rim);
    ground.paint(window, Hsla::from(palette.g1).opacity(opacity));

    if stone.rough || c.facets.is_empty() {
        dashed(window, &rim, rim_width.min(1.0), silver.opacity(0.42 * opacity));
        return;
    }

    let any_lit = stone.lit.as_ref().is_some_and(|l| l.iter().any(|&on| on));
    let lit_part = |part: usize| stone.lit.as_ref().is_some_and(|l| l.get(part).copied().unwrap_or(false));
    let peri: Hsla = palette.peri.base.into();
    let peri_hi: Hsla = palette.peri_hi.into();

    // Fills, batched by colour and quantised opacity.
    let mut buckets: Vec<(Hsla, u32, Fill)> = Vec::new();
    for f in &c.facets {
        if micro && f.child.is_some() {
            continue;
        }
        let (color, alpha) = if lit_part(f.part) {
            (peri, 0.46)
        } else {
            let fire = !any_lit && facing(f.tilt, stone.light) > FIRE_FROM;
            let hue = stone.fire.get(f.part).copied().filter(|_| fire).unwrap_or(silver);
            (hue, shade(f.tilt, stone.light, micro))
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let q = (alpha * QUANTUM).round() as u32;
        let slot = match buckets.iter().position(|(h2, q2, _)| *q2 == q && *h2 == color) {
            Some(i) => i,
            None => {
                buckets.push((color, q, Fill::new()));
                buckets.len() - 1
            }
        };
        buckets[slot].2.poly(&c.placed(&f.poly, bounds));
    }
    for (color, q, fill) in buckets {
        #[allow(clippy::cast_precision_loss)]
        fill.paint(window, color.opacity(q as f32 / QUANTUM * opacity));
    }

    // Edges: submodule cuts a hairline lighter than module cuts.
    let hair = if micro { 0.6 } else { 1.0 };
    let mut sub_edges = Fill::new();
    let mut edges = Fill::new();
    let mut lit_edges = Fill::new();
    for f in &c.facets {
        if micro && f.child.is_some() {
            continue;
        }
        let placed = c.placed(&f.poly, bounds);
        let target = if lit_part(f.part) && f.child.is_none() {
            &mut lit_edges
        } else if f.child.is_some() {
            &mut sub_edges
        } else {
            &mut edges
        };
        for quad in placed.stroke_ring(hair) {
            target.poly(&quad);
        }
    }
    sub_edges.paint(window, silver.opacity(0.09 * opacity));
    edges.paint(window, silver.opacity(if micro { 0.34 } else { 0.2 } * opacity));
    lit_edges.paint(window, peri_hi.opacity(opacity));

    if let Some(facet) = stone.hot.and_then(|part| c.facet_of(part)) {
        let mut ring = Fill::new();
        for quad in c.placed(&facet.poly, bounds).stroke_ring(1.5) {
            ring.poly(&quad);
        }
        ring.paint(window, peri_hi.opacity(opacity));
    }

    for piece in &c.rough {
        dashed(window, &c.placed(piece, bounds), 1.0, silver.opacity(0.2 * opacity));
    }

    let mut ring = Fill::new();
    for quad in rim.stroke_ring(rim_width) {
        ring.poly(&quad);
    }
    ring.paint(window, rim_ink.opacity(opacity));
}

/// A dashed outline: 3 on, 3 off, along every edge.
fn dashed(window: &mut Window, poly: &Poly, width: f32, color: Hsla) {
    let pts = poly.points();
    let mut fill = Fill::new();
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if len <= 0.0 {
            continue;
        }
        let (ux, uy) = ((b.x - a.x) / len, (b.y - a.y) / len);
        let (nx, ny) = (-uy * width * 0.5, ux * width * 0.5);
        let mut t = 0.0;
        while t < len {
            let e = (t + 3.0).min(len);
            let (p, q) = (pt(a.x + ux * t, a.y + uy * t), pt(a.x + ux * e, a.y + uy * e));
            fill.poly(&Poly::new([
                pt(p.x - nx, p.y - ny),
                pt(q.x - nx, q.y - ny),
                pt(q.x + nx, q.y + ny),
                pt(p.x + nx, p.y + ny),
            ]));
            t += 6.0;
        }
    }
    fill.paint(window, color);
}

#[cfg(test)]
mod tests;
