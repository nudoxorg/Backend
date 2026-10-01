//! The comb: one tick per ordered thing (a release, a file, a minute), calm
//! as texture, waking only under the pointer.
//!
//! At rest a release comb says three things and nothing else: the release
//! you pin (mint), the one you are reading (the brightest ink), and the
//! releases that touch your code (mint, quieter). Every other tick is ink4.
//! Resting on a tick lifts it and a few neighbours (a stretch, not a jump:
//! ticks stay on their baseline) and opens that release's lens.
//!
//! One custom element paints every tick: quiet ticks batch into one path, so
//! a 600-tick comb is a handful of draws. The wave is two motion tracks
//! however many ticks there are — a `centre` spring that keeps its velocity
//! as the pointer moves on, and a `strength` tween — and each tick's stretch
//! is a continuous function of its distance from the centre, so a sweep
//! never snaps a neighbour. Finding the tick under the pointer is one
//! division.
//!
//! The comb draws at the rung its room affords ([`Measure::rung`]):
//!
//! | rung | draws |
//! |---|---|
//! | Mark | a whisper: the ticks' silhouette, 12 px tall, 48 px wide |
//! | Tag | the whisper and "64 · 1.0.210" (count, latest) |
//! | Row | the comb |
//! | Card | the comb over one caption line: first and last release at the ends, one sentence between |
//!
//! Rung changes cross-fade on a motion track, so dragging a window across a
//! threshold never pops. The rested tick previews its lens before the lens
//! opens: while hover intent runs it widens and splits, by a hair, into what
//! the release added (the brightest ink, from the base) and what it changed
//! (quieter, above). Once the lens is open the tick is simply the brightest.

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::text::{Shaped, shape};
use crate::measure::{Measure, Needs, Rung};
use crate::motion::spec;
use crate::paint::geom::{Fill, Poly};
use crate::paint::hatch::Hatch;
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style, Window, point,
    px, size,
};
use std::rc::Rc;

/// What a tick says beyond "a release".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TickTone {
    /// The one you pin: mint.
    Pin,
    /// The one you are reading: the brightest ink.
    Current,
    /// Touches your code: mint, quieter.
    Touches,
    /// Breaking: coral.
    Breaking,
    /// An explicit ink (a language's tint on Orbit's comb).
    Ink(TickInk),
}

/// A colour carried as data (`Hsla` is not `Eq`); used by [`TickTone::Ink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TickInk(pub u32);

impl TickTone {
    fn resolve(self, palette: &Palette) -> Hsla {
        match self {
            Self::Pin => palette.mint.base.into(),
            Self::Current => palette.ink0.into(),
            Self::Touches => Hsla::from(palette.mint.base).opacity(0.6),
            Self::Breaking => palette.coral.base.into(),
            Self::Ink(TickInk(rgb)) => crate::tokens::hex(rgb).into(),
        }
    }
}

/// One tick.
#[derive(Clone, Debug, Default)]
pub struct Tick {
    /// Rest height in px at 100 % text (clamped to the comb).
    pub height: f32,
    /// A step brighter (a major version).
    pub major: bool,
    /// Hatched: sampled, not indexed.
    pub hatched: bool,
    /// What it says beyond "a release".
    pub tone: Option<TickTone>,
    /// Its name ("1.0.193"): the caption's ends and the tag use it.
    pub label: Option<SharedString>,
    /// The lens preview: `(added, changed)` shares of what it holds.
    pub split: Option<(f32, f32)>,
}

impl Tick {
    /// A plain tick `height` px tall.
    #[must_use]
    pub fn new(height: f32) -> Self {
        Self {
            height,
            ..Self::default()
        }
    }

    /// A step brighter.
    #[must_use]
    pub const fn major(mut self) -> Self {
        self.major = true;
        self
    }

    /// Hatched: sampled, not indexed.
    #[must_use]
    pub const fn hatched(mut self) -> Self {
        self.hatched = true;
        self
    }

    /// What it says.
    #[must_use]
    pub const fn tone(mut self, tone: TickTone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// Its name.
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// What it added and changed (drawn inside the tick as the wave peaks).
    #[must_use]
    pub const fn split(mut self, added: f32, changed: f32) -> Self {
        self.split = Some((added, changed));
        self
    }
}

/// How the ticks run and where parts open.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum CombOrientation {
    /// Left to right, lenses above.
    #[default]
    Horizontal,
    /// Left to right, lenses below (a comb under a margin head).
    Down,
    /// Top to bottom, lenses to the right (a file spine).
    Vertical,
}

impl CombOrientation {
    const fn horizontal(self) -> bool {
        !matches!(self, Self::Vertical)
    }

    const fn side(self) -> Side {
        match self {
            Self::Horizontal => Side::Above,
            Self::Down => Side::Below,
            Self::Vertical => Side::Right,
        }
    }
}

/// The wave's `(stretch, light)` at a (fractional) tick distance: the rested
/// tick stretches half again and takes the brightest ink; neighbours
/// stretch less and brighten less; linear between, gone by three.
#[must_use]
pub fn wave_shape(distance: f32) -> (f32, f32) {
    const STEPS: [(f32, f32); 4] = [(1.45, 1.0), (1.18, 0.45), (1.06, 0.15), (1.0, 0.0)];
    let d = distance.abs();
    if d >= 3.0 {
        return (1.0, 0.0);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = d.floor() as usize;
    let t = d - d.floor();
    let (a, b) = (STEPS[i.min(3)], STEPS[(i + 1).min(3)]);
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The rungs a comb needs, in effective px of its own width.
pub const NEEDS: Needs = Needs {
    tag: 110.0,
    row: 180.0,
    card: 420.0,
};

const WHISPER_W: f32 = 48.0;
const WHISPER_H: f32 = 12.0;
const ENDS: TypeRole = TypeRole {
    size: 11.0,
    line: 14.0,
    ..ty::MONO_SMALL
};
const SAYS: TypeRole = TypeRole {
    size: 12.5,
    line: 16.0,
    ..ty::SMALL
};
const SAYS_B: TypeRole = TypeRole {
    weight: 600.0,
    ..SAYS
};

/// Where tick `i` of `n` sits along `extent`: the first on the start edge,
/// the last on the end edge, evenly between (`justify-content: space-between`).
#[must_use]
pub fn tick_at(i: usize, n: usize, extent: f32, width: f32) -> f32 {
    if n <= 1 {
        return extent * 0.5;
    }
    #[allow(clippy::cast_precision_loss)]
    let step = (extent - width) / (n - 1) as f32;
    #[allow(clippy::cast_precision_loss)]
    let at = width * 0.5 + step * i as f32;
    at
}

/// The tick nearest `along` (one division), or `None` off the ends.
#[must_use]
pub fn tick_near(along: f32, n: usize, extent: f32, width: f32) -> Option<usize> {
    if n == 0 || along < 0.0 || along > extent {
        return None;
    }
    if n == 1 {
        return Some(0);
    }
    #[allow(clippy::cast_precision_loss)]
    let step = (extent - width) / (n - 1) as f32;
    if step <= 0.0 {
        return Some(0);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = ((along - width * 0.5) / step).round().max(0.0) as usize;
    Some(i.min(n - 1))
}

/// A comb. Build with [`comb`].
pub struct Comb {
    id: ElementId,
    ticks: Rc<[Tick]>,
    measure: Measure,
    orientation: CombOrientation,
    thickness: f32,
    door: Option<Door>,
    rest: Option<usize>,
    rung: Option<Rung>,
    caption: Vec<(SharedString, bool)>,
    ink: Option<TickTone>,
}

/// A comb of `ticks` for the width `measure` gives it.
#[must_use]
pub fn comb(id: impl Into<ElementId>, ticks: impl Into<Rc<[Tick]>>, measure: &Measure) -> Comb {
    Comb {
        id: id.into(),
        ticks: ticks.into(),
        measure: *measure,
        orientation: CombOrientation::Horizontal,
        thickness: 22.0,
        door: None,
        rest: None,
        rung: None,
        caption: Vec::new(),
        ink: None,
    }
}

impl Comb {
    /// Tick direction and lens side.
    #[must_use]
    pub const fn orientation(mut self, orientation: CombOrientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// The comb's height (width when vertical), px at 100 % text.
    #[must_use]
    pub const fn thickness(mut self, thickness: f32) -> Self {
        self.thickness = thickness;
        self
    }

    /// Ticks open through `door` (part = tick index).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a tick as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, tick: Option<usize>) -> Self {
        self.rest = tick;
        self
    }

    /// Pins the rung instead of letting the room pick it.
    #[must_use]
    pub const fn rung(mut self, rung: Rung) -> Self {
        self.rung = Some(rung);
        self
    }

    /// The card rung's one sentence, as runs; `true` runs are the numbers
    /// that matter to you (mint): `[("19 releases since your pin, ", false), ("2", true), (" touch your code", false)]`.
    #[must_use]
    pub fn caption(mut self, runs: impl IntoIterator<Item = (impl Into<SharedString>, bool)>) -> Self {
        self.caption = runs.into_iter().map(|(t, b)| (t.into(), b)).collect();
        self
    }

    /// The comb's quiet ink (Orbit's language comb is tinted by language).
    #[must_use]
    pub const fn ink(mut self, tone: TickTone) -> Self {
        self.ink = Some(tone);
        self
    }

    fn target_rung(&self) -> Rung {
        let rung = self.rung.unwrap_or_else(|| self.measure.rung(NEEDS));
        if self.orientation == CombOrientation::Vertical {
            // A spine has no room for a caption or a tag: a comb or a mark.
            rung.clamp(Rung::Mark, Rung::Row)
        } else {
            rung
        }
    }
}

impl IntoElement for Comb {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

const fn rung_index(rung: Rung) -> f32 {
    match rung {
        Rung::Mark => 0.0,
        Rung::Tag => 1.0,
        Rung::Row => 2.0,
        Rung::Card => 3.0,
    }
}

fn rung_at(index: f32) -> Rung {
    #[allow(clippy::cast_possible_truncation)]
    match index.round() as i32 {
        i32::MIN..=0 => Rung::Mark,
        1 => Rung::Tag,
        2 => Rung::Row,
        _ => Rung::Card,
    }
}

/// The card rung's caption: the ends and the sentence between.
struct Caption {
    start: Option<Shaped>,
    end: Option<Shaped>,
    middle: Vec<Shaped>,
}

#[doc(hidden)]
pub struct CombLayout {
    live: Entity<Live>,
    keys: AnyElement,
    /// The cross-fading rung position, 0 (mark) … 3 (card).
    rung: f32,
    tag: Option<Shaped>,
    caption: Option<Caption>,
}

/// The extent `(main, cross)` one rung occupies, px.
fn extent(rung: Rung, comb: &Comb, tag: Option<&Shaped>) -> (f32, f32) {
    let s = comb.measure.scale();
    let full = f32::from(comb.measure.width());
    match rung {
        Rung::Mark => (WHISPER_W * s, WHISPER_H * s),
        Rung::Tag => (
            WHISPER_W * s + 8.0 * s + tag.map_or(0.0, Shaped::width),
            (WHISPER_H * s).max(tag.map_or(0.0, |t| t.role.line)),
        ),
        Rung::Row => (full, comb.thickness * s),
        Rung::Card => (full, comb.thickness * s + 8.0 * s + comb.measure.role(SAYS).line),
    }
}

impl Element for Comb {
    type RequestLayoutState = CombLayout;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
    ) -> (LayoutId, CombLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let motion = live.read(cx).motion.clone();
        let target = rung_index(self.target_rung());
        let rung = motion.animate(live::key(&self.id, "rung"), target, spec::REVEAL, window, cx);
        let palette = cx.palette();
        let lo = rung_at(rung.floor());
        let hi = rung_at(rung.ceil());

        let tag = (lo <= Rung::Tag || hi <= Rung::Tag).then(|| {
            let latest = self
                .ticks
                .iter()
                .rev()
                .find_map(|t| t.label.clone())
                .unwrap_or_default();
            shape(
                format!("{} · {latest}", self.ticks.len()),
                self.measure.role(ENDS),
                palette.ink3.into(),
                window,
            )
        });
        let caption = (hi == Rung::Card).then(|| {
            let ends = self.measure.role(ENDS);
            let ink: Hsla = palette.ink4.into();
            Caption {
                start: self
                    .ticks
                    .first()
                    .and_then(|t| t.label.clone())
                    .map(|l| shape(l, ends, ink, window)),
                end: (self.ticks.len() > 1)
                    .then(|| self.ticks.last().and_then(|t| t.label.clone()))
                    .flatten()
                    .map(|l| shape(l, ends, ink, window)),
                middle: self
                    .caption
                    .iter()
                    .map(|(words, strong)| {
                        if *strong {
                            shape(words.clone(), self.measure.role(SAYS_B), palette.mint.base.into(), window)
                        } else {
                            shape(words.clone(), self.measure.role(SAYS), palette.ink3.into(), window)
                        }
                    })
                    .collect(),
            }
        });

        let (a, b) = (extent(lo, self, tag.as_ref()), extent(hi, self, tag.as_ref()));
        let f = rung - rung.floor();
        let (main, cross) = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
        let mut style = Style::default();
        if self.orientation.horizontal() {
            style.size.width = px(main).into();
            style.size.height = px(cross).into();
        } else {
            style.size.width = px(cross).into();
            style.size.height = px(main).into();
        }
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [kid], cx),
            CombLayout {
                live,
                keys,
                rung,
                tag,
                caption,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CombLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CombLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let rung = layout.rung;
        let (lo, hi) = (rung_at(rung.floor()), rung_at(rung.ceil()));
        let f = rung - rung.floor();
        let live = layout.live.clone();
        let hover = live.read(cx).hover.or(self.rest);
        let walk = live::walking(&live, window, cx);
        let active = hover.or(walk);

        // The wave: a handful of tracks however many ticks, in px along the
        // comb (so a spring's rest threshold is a fraction of a pixel).
        let field = self.field(bounds);
        let n = self.ticks.len();
        let horizontal = self.orientation.horizontal();
        let extent = if horizontal {
            f32::from(field.size.width)
        } else {
            f32::from(field.size.height)
        };
        let tw = tick_width(n, extent, s);
        #[allow(clippy::cast_precision_loss)]
        let step = if n > 1 { (extent - tw) / (n - 1) as f32 } else { extent.max(1.0) };
        let (at, _, strength) = live::wave(
            &self.id,
            &live,
            active.map(|i| (tick_at(i, n, extent, tw), 0.0)),
            window,
            cx,
        );
        let centre = (at - tw * 0.5) / step.max(1e-3);
        // The split previews the lens only while the lens is on its way:
        // once it is open, the tick is simply the brightest one.
        let preview = active.is_some_and(|i| {
            !crate::overlay::float::is_open(&super::door::part_key(&self.id, i), window, cx)
        });
        let wave = Wave {
            centre,
            strength,
            preview,
        };

        let (lo_alpha, hi_alpha) = (1.0 - f, if lo == hi { 0.0 } else { f });
        for (r, alpha) in [(lo, lo_alpha), (hi, hi_alpha)] {
            if alpha <= 0.01 {
                continue;
            }
            match r {
                Rung::Mark | Rung::Tag => self.paint_whisper(bounds, r, layout, alpha, window, cx),
                Rung::Row | Rung::Card => {
                    self.paint_ticks(field, wave, walk, alpha, palette, window, cx);
                    if r == Rung::Card
                        && let Some(caption) = &layout.caption
                    {
                        paint_caption(field, caption, s, alpha, window, cx);
                    }
                }
            }
        }

        let ticks = self.ticks.clone();
        let (fx, fy) = (f32::from(field.origin.x), f32::from(field.origin.y));
        let (fw, fh) = (f32::from(field.size.width), f32::from(field.size.height));
        let _ = (fw, fh);
        let thick = self.thickness * s;
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: self.orientation.side(),
                count: n,
                hit: Rc::new(move |p: Point<Pixels>| {
                    let along = if horizontal {
                        f32::from(p.x) - fx
                    } else {
                        f32::from(p.y) - fy
                    };
                    tick_near(along, n, extent, tw)
                }),
                // The tick's column, from its baseline to the comb's top: the
                // lens's connector lands on the tick itself.
                anchor: Rc::new(move |i| {
                    let tick = ticks.get(i)?;
                    let at = tick_at(i, n, extent, tw);
                    let len = (tick.height * s).min(thick);
                    let span = step.clamp(2.0, 8.0 * s);
                    Some(if horizontal {
                        Bounds::new(
                            point(px(fx + at - span * 0.5), px(fy + thick - len)),
                            size(px(span), px(len)),
                        )
                    } else {
                        Bounds::new(
                            point(px(fx), px(fy + at - span * 0.5)),
                            size(px(len), px(span)),
                        )
                    })
                }),
                step: Rc::new(live::linear),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}

/// The painted width of a tick: 2 px, thinner where the comb is crowded.
fn tick_width(n: usize, extent: f32, s: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let pitch = extent / n.max(1) as f32;
    (2.0 * s).min((pitch * 0.62).max(1.0))
}

#[derive(Clone, Copy)]
struct Wave {
    centre: f32,
    strength: f32,
    preview: bool,
}

impl Comb {
    /// The tick field: the comb's box without the caption.
    fn field(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let thick = px(self.thickness * self.measure.scale());
        if self.orientation.horizontal() {
            Bounds::new(bounds.origin, size(bounds.size.width, thick))
        } else {
            Bounds::new(bounds.origin, size(thick, bounds.size.height))
        }
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    fn paint_ticks(
        &self,
        field: Bounds<Pixels>,
        wave: Wave,
        walk: Option<usize>,
        alpha: f32,
        palette: &Palette,
        window: &mut Window,
        cx: &mut App,
    ) {
        let s = self.measure.scale();
        let n = self.ticks.len();
        if n == 0 {
            return;
        }
        let horizontal = self.orientation.horizontal();
        let (x, y) = (f32::from(field.origin.x), f32::from(field.origin.y));
        let (w, h) = (f32::from(field.size.width), f32::from(field.size.height));
        let extent = if horizontal { w } else { h };
        let thick = if horizontal { h } else { w };
        let tw = tick_width(n, extent, s);
        let quiet: Hsla = self
            .ink
            .map_or_else(|| Hsla::from(palette.ink4).opacity(0.8), |t| t.resolve(palette));
        let major: Hsla = self.ink.map_or(palette.ink3.into(), |t| t.resolve(palette));
        let bright: Hsla = palette.ink0.into();
        // Only ticks the window can show are drawn.
        let view = super::spatial::visible(field, window);
        let visible = if view.x1 <= view.x0 || view.y1 <= view.y0 {
            (0, 0)
        } else {
            let (lo, hi) = if horizontal { (view.x0 - x, view.x1 - x) } else { (view.y0 - y, view.y1 - y) };
            let slack = 2.0 * s;
            let first = tick_near((lo - slack).max(0.0), n, extent, tw).unwrap_or(0);
            let last = tick_near((hi + slack).min(extent), n, extent, tw).map_or(n, |i| (i + 1).min(n));
            (first, last.max(first))
        };
        super::spatial::count(cx, |p| p.ticks += (visible.1 - visible.0) as u64);

        let bar_at = |i: usize, len: f32, width: f32| {
            let mid = tick_at(i, n, extent, tw);
            if horizontal {
                Poly::rect(x + mid - width * 0.5, y + thick - len, width, len)
            } else {
                Poly::rect(x, y + mid - width * 0.5, len, width)
            }
        };

        // Batches: quiet ticks, majors, and one per tone; waving ticks alone.
        let mut plain = Fill::new();
        let mut majors = Fill::new();
        let mut toned: Vec<(TickTone, Fill)> = Vec::new();
        let mut single: Vec<(Poly, Hsla, bool, Option<(f32, f32)>, f32)> = Vec::new();
        for (i, tick) in self.ticks.iter().enumerate().take(visible.1).skip(visible.0) {
            #[allow(clippy::cast_precision_loss)]
            let d = (i as f32 - wave.centre).abs();
            let rest_len = (tick.height * s).min(thick);
            let base = tick.tone.map_or(if tick.major { major } else { quiet }, |t| t.resolve(palette));
            if d < 3.0 && wave.strength > 0.001 {
                let (stretch, light) = wave_shape(d);
                let stretch = 1.0 + (stretch - 1.0) * wave.strength;
                let light = light * wave.strength;
                let len = (rest_len * stretch).min(thick);
                // The rested tick previews its lens as the wave peaks.
                let peak = if d < 0.5 && wave.preview && tick.split.is_some() {
                    wave.strength
                } else {
                    0.0
                };
                let width = tw + peak * 1.5 * s;
                let color = if tick.tone.is_some() && d >= 0.5 {
                    base
                } else {
                    crate::paint::mix(base, bright, light)
                };
                single.push((
                    bar_at(i, len, width),
                    color.opacity(alpha),
                    tick.hatched,
                    tick.split.filter(|_| peak > 0.35),
                    peak,
                ));
                continue;
            }
            let bar = bar_at(i, rest_len, tw);
            if tick.hatched {
                single.push((bar, base.opacity(alpha), true, None, 0.0));
                continue;
            }
            match tick.tone {
                Some(tone) => {
                    if let Some((_, fill)) = toned.iter_mut().find(|(t, _)| *t == tone) {
                        fill.poly(&bar);
                    } else {
                        let mut fill = Fill::new();
                        fill.poly(&bar);
                        toned.push((tone, fill));
                    }
                }
                None if tick.major => majors.poly(&bar),
                None => plain.poly(&bar),
            }
        }
        plain.paint(window, quiet.opacity(alpha));
        majors.paint(window, major.opacity(alpha));
        for (tone, fill) in toned {
            fill.paint(window, tone.resolve(palette).opacity(alpha));
        }
        for (bar, color, hatched, split, peak) in single {
            let (min, max) = bar.bounds();
            let frame = Bounds::new(
                point(px(min.x), px(min.y)),
                size(px(max.x - min.x), px(max.y - min.y)),
            );
            if let Some((added, changed)) = split {
                // Two tones split by a hair: what it added (from the base,
                // brightest) and what it changed (above, quieter).
                let total = (added + changed).max(1e-3);
                let (bw, bh) = (max.x - min.x, max.y - min.y);
                let hair = s.max(1.0);
                let (solid, rest) = if horizontal {
                    let cut = (bh - hair) * added / total;
                    (
                        Poly::rect(min.x, max.y - cut, bw, cut),
                        Poly::rect(min.x, min.y, bw, (bh - cut - hair).max(0.0)),
                    )
                } else {
                    let cut = (bw - hair) * added / total;
                    (
                        Poly::rect(min.x, min.y, cut, bh),
                        Poly::rect(min.x + cut + hair, min.y, (bw - cut - hair).max(0.0), bh),
                    )
                };
                let mut fill = Fill::new();
                fill.poly(&solid);
                fill.paint(window, color);
                let mut quieter = Fill::new();
                quieter.poly(&rest);
                quieter.paint(window, color.opacity(0.45 + 0.1 * peak));
            } else if hatched {
                let hatch = if horizontal {
                    Hatch::horizontal(2.0 * s, 4.0 * s)
                } else {
                    Hatch::vertical(2.0 * s, 4.0 * s)
                };
                hatch.paint(window, &bar, frame, color);
            } else {
                let mut fill = Fill::new();
                fill.poly(&bar);
                fill.paint(window, color);
            }
        }
        // The keyboard walk: the bevel's periwinkle light under the walked tick.
        if let Some(i) = walk.filter(|i| *i < n) {
            let mid = tick_at(i, n, extent, tw);
            let mark = if horizontal {
                Poly::rect(x + mid - 3.0 * s, y + thick + 2.0 * s, 6.0 * s, 2.0 * s)
            } else {
                Poly::rect(x - 4.0 * s, y + mid - 3.0 * s, 2.0 * s, 6.0 * s)
            };
            let mut fill = Fill::new();
            fill.poly(&mark);
            fill.paint(window, Hsla::from(palette.peri_hi).opacity(alpha));
        }
    }

    /// Mark and tag: the silhouette, max-pooled into 2 px columns.
    fn paint_whisper(
        &self,
        bounds: Bounds<Pixels>,
        rung: Rung,
        layout: &CombLayout,
        alpha: f32,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (WHISPER_W * s, WHISPER_H * s);
        let top = y + (f32::from(bounds.size.height) - h).max(0.0) * 0.5;
        let n = self.ticks.len();
        if n == 0 {
            return;
        }
        let pitch = 2.0 * s;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = ((w / pitch) as usize).clamp(1, n);
        let tallest = self.ticks.iter().map(|t| t.height).fold(1.0_f32, f32::max);
        let quiet: Hsla = self.ink.map_or(palette.ink4.into(), |t| t.resolve(palette));
        let mut body = Fill::new();
        let mut accents: Vec<(Poly, Hsla)> = Vec::new();
        for c in 0..columns {
            let (a, b) = (c * n / columns, ((c + 1) * n / columns).max(c * n / columns + 1));
            let span = &self.ticks[a..b.min(n)];
            let peak = span.iter().map(|t| t.height).fold(0.0_f32, f32::max);
            let len = (peak / tallest * h).max(s);
            #[allow(clippy::cast_precision_loss)]
            let cx0 = x + c as f32 * pitch;
            let bar = Poly::rect(cx0, top + h - len, pitch * 0.6, len);
            match span.iter().find_map(|t| t.tone) {
                Some(tone) => accents.push((bar, tone.resolve(palette))),
                None => body.poly(&bar),
            }
        }
        body.paint(window, quiet.opacity(alpha));
        for (bar, color) in accents {
            let mut fill = Fill::new();
            fill.poly(&bar);
            fill.paint(window, color.opacity(alpha));
        }
        if rung == Rung::Tag
            && alpha > 0.5
            && let Some(tag) = &layout.tag
        {
            let baseline = y + f32::from(bounds.size.height) * 0.5 + tag.role.size * 0.36;
            tag.paint(x + w + 8.0 * s, baseline, window, cx);
        }
    }
}

/// The card rung's line: the first release left, the last right, the one
/// sentence centred between (it gives way before it touches the ends).
fn paint_caption(
    field: Bounds<Pixels>,
    caption: &Caption,
    s: f32,
    alpha: f32,
    window: &mut Window,
    cx: &mut App,
) {
    if alpha < 0.5 {
        return;
    }
    let (x, w) = (f32::from(field.origin.x), f32::from(field.size.width));
    let top = f32::from(field.origin.y + field.size.height) + 8.0 * s;
    let ascent = caption
        .middle
        .iter()
        .chain(caption.start.iter())
        .map(Shaped::ascent)
        .fold(0.0_f32, f32::max);
    let baseline = top + ascent;
    let start_w = caption.start.as_ref().map_or(0.0, Shaped::width);
    let end_w = caption.end.as_ref().map_or(0.0, Shaped::width);
    if let Some(start) = &caption.start {
        start.paint(x, baseline, window, cx);
    }
    if let Some(end) = &caption.end {
        end.paint(x + w - end_w, baseline, window, cx);
    }
    let middle_w: f32 = caption.middle.iter().map(Shaped::width).sum();
    if middle_w + start_w + end_w + 32.0 * s <= w {
        let mut mx = x + (w - middle_w) * 0.5;
        for run in &caption.middle {
            run.paint(mx, baseline, window, cx);
            mx += run.width();
        }
    }
}

/// One file's uses on a file comb.
#[derive(Clone, Debug)]
pub struct FileUses {
    /// The file's name (for its tip).
    pub name: SharedString,
    /// One height per use, px at 100 %.
    pub uses: Vec<f32>,
    /// In code you own (mint); else someone else's (quiet).
    pub yours: bool,
    /// The file you are in, or the one that uses it most: underlined.
    pub hot: bool,
}

impl FileUses {
    /// A file of yours with `uses`.
    #[must_use]
    pub fn new(name: impl Into<SharedString>, uses: impl Into<Vec<f32>>) -> Self {
        Self {
            name: name.into(),
            uses: uses.into(),
            yours: true,
            hot: false,
        }
    }

    /// Someone else's file.
    #[must_use]
    pub const fn theirs(mut self) -> Self {
        self.yours = false;
        self
    }

    /// Underlined.
    #[must_use]
    pub const fn hot(mut self) -> Self {
        self.hot = true;
        self
    }
}

/// The file comb: one tick per use, grouped by file. Build with [`fcomb`].
pub struct FileComb {
    id: Option<ElementId>,
    files: Rc<[FileUses]>,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// A file comb (18 px tall) of `files`.
#[must_use]
pub fn fcomb(files: impl Into<Rc<[FileUses]>>, measure: &Measure) -> FileComb {
    FileComb {
        id: None,
        files: files.into(),
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl FileComb {
    /// Keys the comb's hover state (needed for doors).
    #[must_use]
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Files open through `door` (part = file index).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a file as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, file: Option<usize>) -> Self {
        self.rest = file;
        self
    }

    /// Each file's `(x, width)` from the comb's left edge, px.
    fn spans(&self) -> Vec<(f32, f32)> {
        let s = self.measure.scale();
        let (tick, gap, group) = (2.0 * s, 2.0 * s, 5.0 * s);
        let mut x = 0.0;
        self.files
            .iter()
            .map(|f| {
                #[allow(clippy::cast_precision_loss)]
                let w = f.uses.len() as f32 * (tick + gap) - gap;
                let span = (x, w.max(0.0));
                x += w.max(0.0) + group;
                span
            })
            .collect()
    }
}

impl IntoElement for FileComb {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct FileCombLayout {
    live: Option<Entity<Live>>,
    keys: Option<AnyElement>,
    spans: Rc<Vec<(f32, f32)>>,
}

impl Element for FileComb {
    type RequestLayoutState = FileCombLayout;
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
    ) -> (LayoutId, FileCombLayout) {
        let live = self.id.is_some().then(|| live::live(window, cx));
        let (keys, kids) = live::keys_for(live.as_ref(), window, cx);
        let spans = self.spans();
        let width = spans.last().map_or(0.0, |(x, w)| x + w);
        let s = self.measure.scale();
        let mut style = Style::default();
        style.size.width = px(width + 4.0 * s).into();
        style.size.height = px(24.0 * s).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, kids, cx),
            FileCombLayout {
                live,
                keys,
                spans: Rc::new(spans),
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut FileCombLayout,
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
        layout: &mut FileCombLayout,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let x0 = f32::from(bounds.origin.x) + 2.0 * s;
        let base = f32::from(bounds.origin.y) + 18.0 * s;
        let (hover, walk) = match &layout.live {
            Some(live) => (live.read(cx).hover.or(self.rest), live::walking(live, window, cx)),
            None => (self.rest, None),
        };
        let mut yours = Fill::new();
        let mut theirs = Fill::new();
        let mut lit = Fill::new();
        let mut lit_theirs = Fill::new();
        for (i, (file, (fx, _))) in self.files.iter().zip(layout.spans.iter()).enumerate() {
            let on = hover == Some(i) || walk == Some(i) || file.hot;
            for (k, height) in file.uses.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let tx = x0 + fx + k as f32 * 4.0 * s;
                let len = (height * s).min(18.0 * s) * if hover == Some(i) { 1.15 } else { 1.0 };
                let bar = Poly::rect(tx, base - len, 2.0 * s, len);
                match (file.yours, on) {
                    (true, true) => lit.poly(&bar),
                    (true, false) => yours.poly(&bar),
                    (false, true) => lit_theirs.poly(&bar),
                    (false, false) => theirs.poly(&bar),
                }
            }
        }
        yours.paint(window, Hsla::from(palette.mint.base).opacity(0.85));
        lit.paint(window, Hsla::from(palette.mint.base));
        theirs.paint(window, Hsla::from(palette.ink3).opacity(0.6));
        lit_theirs.paint(window, Hsla::from(palette.ink2));
        // The underline: the hot file in mint, the rested one in periwinkle,
        // the walked one doubled.
        for (i, (file, (fx, fw))) in self.files.iter().zip(layout.spans.iter()).enumerate() {
            let color = if walk == Some(i) {
                Some((palette.peri_hi, 2.0))
            } else if hover == Some(i) {
                Some((palette.peri.base, 1.5))
            } else if file.hot {
                Some((palette.mint.base, 1.5))
            } else {
                None
            };
            if let Some((tone, weight)) = color {
                let mut fill = Fill::new();
                fill.poly(&Poly::rect(x0 + fx - 2.0 * s, base + 2.5 * s, fw + 4.0 * s, weight * s));
                fill.paint(window, Hsla::from(tone));
            }
        }
        if let (Some(live), Some(keys), Some(hitbox), Some(id)) =
            (&layout.live, layout.keys.as_mut(), hitbox.as_ref(), &self.id)
        {
            let spans = layout.spans.clone();
            let hit_spans = spans.clone();
            let top = f32::from(bounds.origin.y);
            live::paint(
                Hooks {
                    mark: id.clone(),
                    live: live.clone(),
                    door: self.door.clone(),
                    side: Side::Above,
                    count: self.files.len(),
                    // Files are few and sorted: a binary search on the left edges.
                    hit: Rc::new(move |p| {
                        let along = f32::from(p.x) - x0;
                        let i = hit_spans.partition_point(|(x, _)| *x <= along + 2.5 * s);
                        let i = i.checked_sub(1)?;
                        let (x, w) = hit_spans[i];
                        (along <= x + w + 2.5 * s).then_some(i)
                    }),
                    anchor: Rc::new(move |i| {
                        let (x, w) = *spans.get(i)?;
                        Some(Bounds::new(
                            point(px(x0 + x - 2.0 * s), px(top)),
                            size(px(w + 4.0 * s), px(24.0 * s)),
                        ))
                    }),
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
    use super::{Rung, rung_at, rung_index, tick_at, tick_near, wave_shape};

    #[test]
    fn the_wave_peaks_on_the_tick_and_is_continuous() {
        assert!((wave_shape(0.0).0 - 1.45).abs() < 1e-5 && (wave_shape(0.0).1 - 1.0).abs() < 1e-5);
        assert_eq!(wave_shape(3.0), (1.0, 0.0));
        assert_eq!(wave_shape(9.0), (1.0, 0.0));
        let mut last = wave_shape(0.0);
        let mut d = 0.0;
        while d < 4.0 {
            d += 0.01;
            let now = wave_shape(d);
            assert!((now.0 - last.0).abs() < 0.01, "stretch jumps at {d}");
            assert!(now.0 <= last.0 + 1e-6, "stretch grows away from the centre at {d}");
            last = now;
        }
    }

    #[test]
    fn nearest_tick_inverts_placement_everywhere() {
        for n in [1usize, 2, 7, 64, 600] {
            for extent in [48.0_f32, 333.0, 1100.0] {
                let w = 2.0;
                for i in 0..n {
                    let at = tick_at(i, n, extent, w);
                    assert_eq!(tick_near(at, n, extent, w), Some(i), "n={n} extent={extent} i={i}");
                }
                assert_eq!(tick_near(-1.0, n, extent, w), None);
                assert_eq!(tick_near(extent + 1.0, n, extent, w), None);
            }
        }
        // First on the start edge, last on the end edge.
        assert!((tick_at(0, 64, 640.0, 2.0) - 1.0).abs() < 1e-4);
        assert!((tick_at(63, 64, 640.0, 2.0) - 639.0).abs() < 1e-3);
    }

    #[test]
    fn rung_positions_round_trip() {
        for rung in [Rung::Mark, Rung::Tag, Rung::Row, Rung::Card] {
            assert_eq!(rung_at(rung_index(rung)), rung);
        }
        assert_eq!(rung_at(2.4), Rung::Row);
        assert_eq!(rung_at(2.6), Rung::Card);
    }
}
