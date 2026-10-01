//! The compass: a symbol's relational shape in 16–30 px.
//!
//! Four arms from a small outlined diamond — up **is** (contracts hue), down
//! **made of** (types hue), left **from** (quiet ink), right **to**
//! (callables hue) — each as long as the log of its count; a zero count
//! draws no arm at all. Three rungs:
//!
//! - [`compass`]: the mark (18 px in rows, 16 dense, 30 on a hero's facts
//!   line). Each arm is a door; resting on one lights it and dims the rest.
//!   Under ⌥ x-ray the mark spells its four counts beside itself.
//! - [`compass_row`]: the counts in words — serif direction, mono number,
//!   zeros omitted — each group a door.
//! - [`compass_bar`]: four cells (word, count, unit, a proportional bar) for
//!   a card; each cell a door.
//!
//! Geometry is the v4 board's (`v4/v4.css` `.compass`, `nx4.py arm()`).

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::spell::{Seg, Spell, spell};
use super::text::shape;
use crate::measure::Measure;
use crate::paint::geom::{Fill, Poly, Pt, pt};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, SharedString, Style, Window, point, px, size,
};
use std::rc::Rc;

/// The four relation counts a compass draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Directions {
    /// Up: what it **is** (traits it implements, interfaces, supertypes).
    pub is: usize,
    /// Down: what it is **made of** (fields, variants, members).
    pub made_of: usize,
    /// Left: where it comes **from** (who uses it, constructors).
    pub from: usize,
    /// Right: where it **goes** (what it calls, what it returns into).
    pub to: usize,
}

/// One of the four directions, in part order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Dir {
    /// Up.
    Is,
    /// Down.
    MadeOf,
    /// Left.
    From,
    /// Right.
    To,
}

impl Dir {
    /// Part order: is, made of, from, to.
    pub const ALL: [Self; 4] = [Self::Is, Self::MadeOf, Self::From, Self::To];

    /// The part index.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Is => 0,
            Self::MadeOf => 1,
            Self::From => 2,
            Self::To => 3,
        }
    }

    /// The direction for a part index.
    #[must_use]
    pub const fn of(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Is),
            1 => Some(Self::MadeOf),
            2 => Some(Self::From),
            3 => Some(Self::To),
            _ => None,
        }
    }

    /// The word the rose and the row use.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::MadeOf => "made of",
            Self::From => "from",
            Self::To => "to",
        }
    }

    /// The direction's ink.
    #[must_use]
    pub fn color(self, palette: &Palette) -> Hsla {
        match self {
            Self::Is => palette.f_con.hue.into(),
            Self::MadeOf => palette.f_type.hue.into(),
            Self::From => palette.ink2.into(),
            Self::To => palette.f_call.hue.into(),
        }
    }

    /// Unit vector on screen (y down).
    #[must_use]
    pub const fn vector(self) -> (f32, f32) {
        match self {
            Self::Is => (0.0, -1.0),
            Self::MadeOf => (0.0, 1.0),
            Self::From => (-1.0, 0.0),
            Self::To => (1.0, 0.0),
        }
    }
}

impl Directions {
    /// A new set of counts.
    #[must_use]
    pub const fn new(is: usize, made_of: usize, from: usize, to: usize) -> Self {
        Self {
            is,
            made_of,
            from,
            to,
        }
    }

    /// The count in one direction.
    #[must_use]
    pub const fn get(&self, dir: Dir) -> usize {
        match dir {
            Dir::Is => self.is,
            Dir::MadeOf => self.made_of,
            Dir::From => self.from,
            Dir::To => self.to,
        }
    }

    /// Whether every direction is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.is == 0 && self.made_of == 0 && self.from == 0 && self.to == 0
    }

    /// The largest count (at least 1).
    #[must_use]
    pub fn max(&self) -> usize {
        Dir::ALL.iter().map(|d| self.get(*d)).max().unwrap_or(0).max(1)
    }

    /// "is 3 · made of 3 · from 9 · to 4", zeros omitted (tooltips, a11y).
    #[must_use]
    pub fn words(&self) -> String {
        Dir::ALL
            .iter()
            .filter(|d| self.get(**d) > 0)
            .map(|d| format!("{} {}", d.word(), self.get(*d)))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// The compass's size ladder.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum CompassSize {
    /// 16 px: dense rows.
    S16,
    /// 18 px: the board default.
    #[default]
    S18,
    /// 30 px: the hero's facts line.
    S30,
}

/// `(box, diamond edge, diamond stroke, arm gap, arm width, longest arm)`.
struct Metrics {
    edge: f32,
    diamond: f32,
    stroke: f32,
    gap: f32,
    arm: f32,
    max: f32,
}

impl CompassSize {
    const fn metrics(self) -> Metrics {
        match self {
            Self::S16 => Metrics {
                edge: 16.0,
                diamond: 4.5,
                stroke: 1.1,
                gap: 3.6,
                arm: 1.4,
                max: 5.2,
            },
            Self::S18 => Metrics {
                edge: 18.0,
                diamond: 5.0,
                stroke: 1.2,
                gap: 4.0,
                arm: 1.5,
                max: 6.0,
            },
            Self::S30 => Metrics {
                edge: 30.0,
                diamond: 8.0,
                stroke: 1.2,
                gap: 6.0,
                arm: 1.5,
                max: 11.0,
            },
        }
    }

    /// Edge length at 100 % text.
    #[must_use]
    pub const fn edge(self) -> f32 {
        self.metrics().edge
    }

    /// The size a density picks for rows.
    #[must_use]
    pub const fn for_rows(density: crate::measure::Density) -> Self {
        match density {
            crate::measure::Density::Dense => Self::S16,
            _ => Self::S18,
        }
    }
}

/// An arm's length in px for a count `n`, clamped to `max`
/// (`arm(n, max) = n <= 0 ? 0 : min(max, 1.6 + log2(1 + n) * 1.25)`).
#[must_use]
pub fn arm_length(n: usize, max: f32) -> f32 {
    if n == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let n = n as f32;
        (1.6 + (1.0 + n).log2() * 1.25).min(max)
    }
}

/// How one arm is lit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArmLight {
    /// The arm rested on (the others dim).
    pub part: usize,
    /// Keyboard walk: the bevel light (periwinkle) instead of the hue.
    pub walk: bool,
}

/// A quad for a straight segment `a → b` of width `w`, positively oriented.
pub(crate) fn segment(a: Pt, b: Pt, w: f32) -> Poly {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (nx, ny) = (-dy / len * w * 0.5, dx / len * w * 0.5);
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

/// Paints a compass into `bounds` (any custom element may: the rose's
/// ledger rows, a spelled line). `scale` is the text scale.
pub fn paint_arms(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    size: CompassSize,
    dirs: Directions,
    scale: f32,
    light: Option<ArmLight>,
    palette: &Palette,
) {
    let m = size.metrics();
    let k = scale * (f32::from(bounds.size.width).min(f32::from(bounds.size.height)) / (m.edge * scale)).min(1.0);
    let cx = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
    let cy = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;

    // The centre: a 45° square, border-box `diamond` px, 1.2 px ink3 rim.
    let half = m.diamond * k / std::f32::consts::SQRT_2;
    let diamond = Poly::new([
        pt(cx, cy - half),
        pt(cx + half, cy),
        pt(cx, cy + half),
        pt(cx - half, cy),
    ]);
    let mut rim = Fill::new();
    for quad in diamond.offset(-m.stroke * k * 0.5).stroke_ring(m.stroke * k) {
        rim.poly(&quad);
    }
    rim.paint(window, Hsla::from(palette.ink3));

    for dir in Dir::ALL {
        let n = dirs.get(dir);
        if n == 0 {
            continue;
        }
        let lit = light.is_some_and(|l| l.part == dir.index());
        let dim = light.is_some() && !lit;
        let len = arm_length(n, m.max) * k + if lit { 1.5 * k } else { 0.0 };
        let (vx, vy) = dir.vector();
        let start = pt(cx + vx * m.gap * k, cy + vy * m.gap * k);
        let end = pt(start.x + vx * len, start.y + vy * len);
        let width = m.arm * k * if lit { 1.35 } else { 1.0 };
        let color = match light {
            Some(l) if lit && l.walk => palette.peri_hi.into(),
            _ => dir.color(palette),
        };
        let alpha = if dim { 0.32 } else if lit { 1.0 } else { 0.9 };
        let mut arm = Fill::new();
        arm.poly(&segment(start, end, width));
        arm.paint(window, color.opacity(alpha));
    }
}

/// The part (direction index) under `p` for a compass centred at `c`.
fn arm_at(p: Point<Pixels>, c: Pt, reach: f32, dirs: Directions) -> Option<usize> {
    let (dx, dy) = (f32::from(p.x) - c.x, f32::from(p.y) - c.y);
    if dx.abs().max(dy.abs()) > reach {
        return None;
    }
    let dir = if dy.abs() >= dx.abs() {
        if dy < 0.0 { Dir::Is } else { Dir::MadeOf }
    } else if dx < 0.0 {
        Dir::From
    } else {
        Dir::To
    };
    (dirs.get(dir) > 0).then_some(dir.index())
}

/// The compass mark. Build with [`compass`].
pub struct Compass {
    id: Option<ElementId>,
    dirs: Directions,
    size: CompassSize,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
    spell: Option<bool>,
}

/// A compass mark for `dirs`, sized for rows at `measure`'s density.
#[must_use]
pub fn compass(dirs: Directions, measure: &Measure) -> Compass {
    Compass {
        id: None,
        dirs,
        size: CompassSize::for_rows(measure.density()),
        measure: *measure,
        door: None,
        rest: None,
        spell: None,
    }
}

impl Compass {
    /// The size.
    #[must_use]
    pub const fn size(mut self, size: CompassSize) -> Self {
        self.size = size;
        self
    }

    /// Keys the mark's hover state (needed for doors).
    #[must_use]
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Arms open through `door` (part = [`Dir::index`]).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows an arm as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, dir: Option<Dir>) -> Self {
        self.rest = match dir {
            Some(d) => Some(d.index()),
            None => None,
        };
        self
    }

    /// Spells the counts beside the mark (x-ray does this by itself).
    #[must_use]
    pub const fn spell(mut self, spell: bool) -> Self {
        self.spell = Some(spell);
        self
    }

    fn spelled(&self) -> bool {
        self.spell.unwrap_or(self.measure.reveal().xray) && !self.dirs.is_empty()
    }
}

impl IntoElement for Compass {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct CompassLayout {
    live: Option<Entity<Live>>,
    keys: Option<AnyElement>,
    counts: Vec<(Dir, super::text::Shaped)>,
}

impl Element for Compass {
    type RequestLayoutState = CompassLayout;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
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
    ) -> (LayoutId, CompassLayout) {
        let live = self.id.is_some().then(|| live::live(window, cx));
        let (keys, kids) = live::keys_for(live.as_ref(), window, cx);
        let edge = self.size.edge() * self.measure.scale();
        let palette = cx.palette();
        let counts: Vec<(Dir, super::text::Shaped)> = if self.spelled() {
            let role = self.measure.role(ty::MONO_SMALL);
            Dir::ALL
                .iter()
                .filter(|d| self.dirs.get(**d) > 0)
                .map(|d| (*d, shape(self.dirs.get(*d).to_string(), role, d.color(palette), window)))
                .collect()
        } else {
            Vec::new()
        };
        let gap = 4.0 * self.measure.scale();
        let text: f32 = counts.iter().map(|(_, s)| s.width() + gap).sum();
        let mut style = Style::default();
        style.size.width = px(edge + text).into();
        style.size.height = px(edge).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, kids, cx), CompassLayout { live, keys, counts })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CompassLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        layout
            .keys
            .as_mut()
            .map(|keys| live::prepaint(bounds, keys, window, cx))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CompassLayout,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let scale = self.measure.scale();
        let edge = self.size.edge() * scale;
        let mark = Bounds::new(bounds.origin, size(px(edge), px(edge)));
        let (hover, walk) = match &layout.live {
            Some(live) => (live.read(cx).hover.or(self.rest), live::walking(live, window, cx)),
            None => (self.rest, None),
        };
        let light = walk
            .map(|part| ArmLight { part, walk: true })
            .or(hover.map(|part| ArmLight { part, walk: false }));
        paint_arms(window, mark, self.size, self.dirs, scale, light, palette);

        // X-ray: the counts, each in its direction's ink, beside the mark.
        let baseline = f32::from(bounds.origin.y) + edge * 0.5 + self.measure.role(ty::MONO_SMALL).size * 0.36;
        let mut x = f32::from(bounds.origin.x) + edge + 4.0 * scale;
        for (_, shaped) in &layout.counts {
            shaped.paint(x, baseline, window, cx);
            x += shaped.width() + 4.0 * scale;
        }

        if let (Some(live), Some(keys), Some(hitbox), Some(id)) =
            (&layout.live, layout.keys.as_mut(), hitbox.as_ref(), &self.id)
        {
            let c = pt(
                f32::from(mark.origin.x) + edge * 0.5,
                f32::from(mark.origin.y) + edge * 0.5,
            );
            let dirs = self.dirs;
            let m = self.size.metrics();
            let reach = edge * 0.5 + 2.0;
            let present: Vec<usize> = Dir::ALL
                .iter()
                .filter(|d| dirs.get(**d) > 0)
                .map(|d| d.index())
                .collect();
            live::paint(
                Hooks {
                    mark: id.clone(),
                    live: live.clone(),
                    door: self.door.clone(),
                    side: Side::Above,
                    count: 4,
                    hit: Rc::new(move |p| arm_at(p, c, reach, dirs)),
                    anchor: Rc::new(move |i| {
                        let dir = Dir::of(i)?;
                        let (vx, vy) = dir.vector();
                        let reach = (m.gap + arm_length(dirs.get(dir), m.max)) * scale;
                        let (x0, x1) = (c.x.min(c.x + vx * reach), c.x.max(c.x + vx * reach));
                        let (y0, y1) = (c.y.min(c.y + vy * reach), c.y.max(c.y + vy * reach));
                        let pad = 2.0 * scale;
                        Some(Bounds::new(
                            point(px(x0 - pad), px(y0 - pad)),
                            size(px(x1 - x0 + pad * 2.0), px(y1 - y0 + pad * 2.0)),
                        ))
                    }),
                    step: Rc::new(move |current, _key, _| {
                        // The walk visits present arms in part order.
                        if present.is_empty() {
                            return None;
                        }
                        let at = current.and_then(|c| present.iter().position(|p| *p == c));
                        let next = match (at, _key) {
                            (None, _) => 0,
                            (Some(i), "left" | "up") => i.saturating_sub(1),
                            (Some(i), "right" | "down") => (i + 1).min(present.len() - 1),
                            (Some(i), _) => i,
                        };
                        Some(present[next])
                    }),
                },
                keys,
                hitbox,
                window,
                cx,
            );
        }
    }
}

/// The counts in words: "◇ is 3 · ◇ made of 3 · ◇ from 9 · ◇ to 4", serif
/// words, mono numbers, zeros omitted. Each group is part [`Dir::index`].
#[must_use]
pub fn compass_row(dirs: Directions, measure: &Measure, palette: &Palette) -> Spell {
    const WORD: TypeRole = TypeRole {
        size: 12.5,
        line: 16.0,
        ..ty::CAPTION
    };
    const NUM: TypeRole = TypeRole {
        weight: 600.0,
        ..ty::MONO_SMALL
    };
    let mut line = spell(measure).side(Side::Below);
    let mut first = true;
    for dir in Dir::ALL {
        let n = dirs.get(dir);
        if n == 0 {
            continue;
        }
        if !first {
            line = line
                .seg(Seg::Break)
                .gap(7.0)
                .text("·", ty::MONO_SMALL, palette.ink4)
                .gap(7.0);
        }
        first = false;
        let part = dir.index();
        line = line
            .part(
                part,
                Seg::Diamond {
                    size: 5.0,
                    color: dir.color(palette),
                    filled: false,
                },
            )
            .part(part, Seg::Gap(4.0))
            .part_text(part, format!("{} ", dir.word()), WORD, palette.ink3)
            .part_text(part, n.to_string(), NUM, palette.ink1);
    }
    line
}

/// The four-cell compass bar for cards (`.cbar`). Build with [`compass_bar`].
pub struct CompassBar {
    id: Option<ElementId>,
    dirs: Directions,
    units: [SharedString; 4],
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// A compass bar for `dirs` at `measure`, units "traits / variants / uses / calls".
#[must_use]
pub fn compass_bar(dirs: Directions, measure: &Measure) -> CompassBar {
    CompassBar {
        id: None,
        dirs,
        units: [
            "traits".into(),
            "variants".into(),
            "uses".into(),
            "calls".into(),
        ],
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl CompassBar {
    /// The unit word under each count (is, made of, from, to).
    #[must_use]
    pub fn units(mut self, units: [&'static str; 4]) -> Self {
        self.units = units.map(SharedString::new_static);
        self
    }

    /// Keys the bar's hover state (needed for doors).
    #[must_use]
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Cells open through `door` (part = [`Dir::index`]).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a cell as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, dir: Option<Dir>) -> Self {
        self.rest = match dir {
            Some(d) => Some(d.index()),
            None => None,
        };
        self
    }

    fn metrics(&self) -> (f32, f32, f32, f32) {
        // (padding top, bottom, horizontal, row gap) at this measure.
        let s = self.measure.scale() * self.measure.density().space().max(0.8);
        (7.0 * s, 6.0 * s, 8.0 * s, 4.0 * s)
    }
}

impl IntoElement for CompassBar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

const BAR_LABEL: TypeRole = TypeRole {
    size: 12.0,
    line: 14.0,
    ..ty::CAPTION
};
const BAR_NUM: TypeRole = TypeRole {
    face: crate::tokens::Face::Mono,
    weight: 600.0,
    size: 15.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const BAR_UNIT: TypeRole = TypeRole {
    size: 11.0,
    line: 14.0,
    ..ty::MONO_SMALL
};

#[doc(hidden)]
pub struct BarLayout {
    live: Option<Entity<Live>>,
    keys: Option<AnyElement>,
}

impl Element for CompassBar {
    type RequestLayoutState = BarLayout;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
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
    ) -> (LayoutId, BarLayout) {
        let live = self.id.is_some().then(|| live::live(window, cx));
        let (keys, kids) = live::keys_for(live.as_ref(), window, cx);
        let (top, bottom, _, gap) = self.metrics();
        let m = &self.measure;
        let height = top
            + m.role(BAR_LABEL).line
            + gap
            + m.role(BAR_NUM).line
            + gap
            + 3.0 * m.scale()
            + bottom
            + 2.0;
        let mut style = Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = px(height).into();
        style.min_size.width = px(0.0).into();
        (window.request_layout(style, kids, cx), BarLayout { live, keys })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut BarLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        layout
            .keys
            .as_mut()
            .map(|keys| live::prepaint(bounds, keys, window, cx))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut BarLayout,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let m = self.measure;
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (top, _, pad_x, gap) = self.metrics();
        let (hover, walk) = match &layout.live {
            Some(live) => (live.read(cx).hover.or(self.rest), live::walking(live, window, cx)),
            None => (self.rest, None),
        };
        // The grid: 1 px line1 between and around four plate cells.
        let mut frame = Fill::new();
        frame.poly(&Poly::rect(x, y, w, h));
        frame.paint(window, Hsla::from(palette.line1));
        let cell_w = (w - 5.0) / 4.0;
        let max = self.dirs.max();
        let cells: Vec<Bounds<Pixels>> = (0..4)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let cx0 = x + 1.0 + i as f32 * (cell_w + 1.0);
                Bounds::new(point(px(cx0), px(y + 1.0)), size(px(cell_w), px(h - 2.0)))
            })
            .collect();
        for (i, dir) in Dir::ALL.iter().enumerate() {
            let cell = cells[i];
            let (cx0, cy0) = (f32::from(cell.origin.x), f32::from(cell.origin.y));
            let lit = hover == Some(i) || walk == Some(i);
            let mut plate = Fill::new();
            plate.poly(&Poly::rect(cx0, cy0, cell_w, h - 2.0));
            plate.paint(
                window,
                Hsla::from(if lit { palette.plate2 } else { palette.plate }),
            );
            if walk == Some(i) {
                // The walked cell carries the doubled periwinkle bevel.
                let ring = Poly::rect(cx0, cy0, cell_w, h - 2.0);
                let mut hi = Fill::new();
                for q in ring.offset(-1.0).stroke_ring(2.0) {
                    hi.poly(&q);
                }
                hi.paint(window, Hsla::from(palette.peri.base));
            }
            let n = self.dirs.get(*dir);
            let label = shape(dir.word(), m.role(BAR_LABEL), palette.ink3.into(), window);
            let label_base = cy0 + top + label.ascent();
            label.paint(cx0 + pad_x, label_base, window, cx);
            let num_role = m.role(BAR_NUM);
            let num = shape(
                n.to_string(),
                num_role,
                if n == 0 { palette.ink4 } else { palette.ink0 }.into(),
                window,
            );
            let num_top = cy0 + top + m.role(BAR_LABEL).line + gap;
            let num_base = num_top + num.ascent();
            num.paint(cx0 + pad_x, num_base, window, cx);
            let unit = shape(self.units[i].clone(), m.role(BAR_UNIT), palette.ink3.into(), window);
            let unit_x = cx0 + pad_x + num.width() + 5.0 * m.scale();
            if unit_x + unit.width() <= cx0 + cell_w - 2.0 {
                unit.paint(unit_x, num_base, window, cx);
            }
            let bar_y = num_top + num_role.line + gap;
            let bar_w = cell_w - pad_x * 2.0;
            let bar_h = 3.0 * m.scale();
            let mut track = Fill::new();
            track.poly(&Poly::rect(cx0 + pad_x, bar_y, bar_w, bar_h));
            track.paint(window, Hsla::from(palette.line2));
            #[allow(clippy::cast_precision_loss)]
            let share = n as f32 / max as f32;
            if share > 0.0 {
                let mut fill = Fill::new();
                fill.poly(&Poly::rect(cx0 + pad_x, bar_y, (bar_w * share).max(1.0), bar_h));
                fill.paint(window, dir.color(palette).opacity(if lit { 1.0 } else { 0.85 }));
            }
        }
        if let (Some(live), Some(keys), Some(hitbox), Some(id)) =
            (&layout.live, layout.keys.as_mut(), hitbox.as_ref(), &self.id)
        {
            live::paint(
                Hooks {
                    mark: id.clone(),
                    live: live.clone(),
                    door: self.door.clone(),
                    side: Side::Below,
                    count: 4,
                    // Four equal columns: the cell is the column index.
                    hit: Rc::new(move |p| {
                        let along = f32::from(p.x) - x;
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let i = ((along / (w / 4.0)).floor().max(0.0) as usize).min(3);
                        Some(i)
                    }),
                    anchor: Rc::new(move |i| cells.get(i).copied()),
                    step: Rc::new(live::linear),
                },
                keys,
                hitbox,
                window,
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dir, Directions, arm_at, arm_length};
    use crate::paint::geom::pt;
    use gpui::{point, px};

    #[test]
    fn arms_are_zero_only_at_zero_and_clamp() {
        assert!(arm_length(0, 6.0).abs() < f32::EPSILON);
        assert!(arm_length(1, 6.0) > 0.0);
        assert!((arm_length(10_000, 6.0) - 6.0).abs() < 1e-6);
        let mut last = 0.0;
        for n in 0..500 {
            let v = arm_length(n, 11.0);
            assert!(v >= last, "arm shrank at {n}");
            last = v;
        }
    }

    #[test]
    fn hit_test_finds_the_arm_under_the_pointer_and_skips_empty_arms() {
        let dirs = Directions::new(3, 0, 9, 4);
        let c = pt(100.0, 100.0);
        assert_eq!(arm_at(point(px(100.0), px(93.0)), c, 9.0, dirs), Some(Dir::Is.index()));
        assert_eq!(arm_at(point(px(92.0), px(101.0)), c, 9.0, dirs), Some(Dir::From.index()));
        assert_eq!(arm_at(point(px(107.0), px(99.0)), c, 9.0, dirs), Some(Dir::To.index()));
        // "made of" is zero: nothing is drawn there, so nothing opens.
        assert_eq!(arm_at(point(px(100.0), px(107.0)), c, 9.0, dirs), None);
        assert_eq!(arm_at(point(px(130.0), px(100.0)), c, 9.0, dirs), None);
    }

    #[test]
    fn words_omit_zeros() {
        assert_eq!(Directions::new(3, 0, 9, 4).words(), "is 3 · from 9 · to 4");
        assert_eq!(Directions::default().words(), "");
    }
}
