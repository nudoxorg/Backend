//! Weight as an iceberg. The package's own lines are the tip above the
//! waterline; everything it pulls in is the mass beneath, one layer per step
//! down, each package a block as wide as the square root of its lines (so
//! the heavy tail stays on the page).
//!
//! - **The glyph** sits in the crest: a small berg with the package's own
//!   lines at its tip and the lines beneath cut into its mass. Click it and
//!   the full berg opens under the hero.
//! - **The berg**: rest on a block and it surfaces: a plate with its name
//!   and weight rises above the waterline over it, everything it carries
//!   beneath (its keel) lights and the rest of the ice goes quiet, and the
//!   caption says what share of the weight is its doing and how it was
//!   reached. Click goes to that package. All of it inside the berg.

use super::state::{Fold, Nominal};
use super::text::key;
use crate::controls::button::wire;
use crate::controls::state::{Touch, hover_zone, track};
use crate::data::text::{shape, shape_fit};
use crate::measure::Measure;
use crate::motion::spec;
use crate::paint::geom::{Fill, Poly, pt};
use crate::paint::{Bevel, Edge, mix};
use crate::probe::{self, TextOverflow, TextSample};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use gpui::{
    App, Bounds, ColorExt as _, DispatchPhase, Element, ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    InteractiveElement, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, ParentElement, Pixels, RenderOnce,
    SharedString, Style, Styled, Window, canvas, px,
};
use std::rc::Rc;
use std::sync::Arc;

/// One package in the berg.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BergBlock {
    /// Its name.
    pub name: SharedString,
    /// Its version.
    pub version: SharedString,
    /// Its lines of code.
    pub sloc: usize,
    /// Steps below the package.
    pub layer: usize,
    /// The block it was first reached through.
    pub parent: Option<usize>,
    /// What it rests on, among the blocks.
    pub deps: Vec<usize>,
}

/// A package's weight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BergFacts {
    /// The package's name.
    pub name: SharedString,
    /// Its own lines.
    pub own: usize,
    /// Everything beneath, breadth first.
    pub blocks: Vec<BergBlock>,
    /// Dependencies whose source is not on this machine.
    pub missing: usize,
    /// What the weight was measured under.
    pub basis: Option<Basis>,
}

/// What a weight was measured under.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Basis {
    /// A project's lock file: the exact packages it builds.
    Lock,
    /// The crate's manifest, default features, this machine's target.
    #[default]
    Defaults,
}

impl Basis {
    /// How the caption says it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Lock => "in your lock",
            Self::Defaults => "default features",
        }
    }
}

/// `48,000` as `48K`, `3,300` as `3.3K`, `1,200,000` as `1.2M`.
#[must_use]
pub fn lines(n: usize) -> String {
    #[allow(clippy::cast_precision_loss)]
    let x = n as f64;
    if n >= 1_000_000 {
        format!("{:.1}M", x / 1e6)
    } else if n >= 10_000 {
        format!("{}K", (x / 1e3).round())
    } else if n >= 1_000 {
        format!("{:.1}K", x / 1e3)
    } else {
        n.to_string()
    }
}

impl BergFacts {
    /// Lines beneath the waterline.
    #[must_use]
    pub fn below(&self) -> usize {
        self.blocks.iter().map(|b| b.sloc).sum()
    }

    /// The share of all lines that is the package's own, `0..=1`.
    #[must_use]
    pub fn share(&self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let (own, below) = (self.own as f32, self.below() as f32);
        own / (own + below).max(1.0)
    }

    /// Every block under `index`, itself excluded.
    #[must_use]
    pub fn keel(&self, index: usize) -> Vec<usize> {
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![index];
        let mut out = Vec::new();
        while let Some(at) = stack.pop() {
            for dep in &self.blocks[at].deps {
                if !seen[*dep] && *dep != index {
                    seen[*dep] = true;
                    out.push(*dep);
                    stack.push(*dep);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// The way down to `index`: the blocks it was reached through, then itself.
    #[must_use]
    pub fn chain(&self, index: usize) -> Vec<usize> {
        let mut out = vec![index];
        let mut at = index;
        while let Some(parent) = self.blocks[at].parent {
            out.push(parent);
            at = parent;
            if out.len() > self.blocks.len() {
                break;
            }
        }
        out.reverse();
        out
    }

    /// The lines `index` carries: itself and everything under it.
    #[must_use]
    pub fn carried(&self, index: usize) -> usize {
        self.blocks[index].sloc + self.keel(index).iter().map(|k| self.blocks[*k].sloc).sum::<usize>()
    }
}

// ---------------------------------------------------------------- geometry

/// A laid-out block: x, y, width, height, px at scale 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// Where each block of the berg sits at rest, relative to the berg's own
/// corner: a box a host can put a keyboard door on.
#[must_use]
pub fn doors(facts: &BergFacts, measure: &Measure) -> Vec<Bounds<Pixels>> {
    let s = measure.scale();
    let (placed, _) = place(facts, f32::from(measure.width()) / s);
    placed.iter().map(|p| Bounds::new(gpui::point(px(p.x * s), px(p.y * s)), gpui::size(px(p.w * s), px(p.h * s)))).collect()
}

/// The waterline's y, px at scale 1.
pub const WATERLINE: f32 = 38.0;
const ROW: f32 = 10.0;
const ROW_GAP: f32 = 2.0;

/// Lays the blocks out in a berg `width` px wide (scale 1): each layer a row,
/// heaviest at the centre and the rest alternating outwards, each block as
/// wide as the square root of its lines.
#[must_use]
pub fn place(facts: &BergFacts, width: f32) -> (Vec<Placed>, f32) {
    let layers = facts.blocks.iter().map(|b| b.layer).max().map_or(0, |m| m + 1);
    let max_w = width - 20.0;
    let weight = |b: &BergBlock| {
        #[allow(clippy::cast_precision_loss)]
        (b.sloc.max(1) as f32).sqrt()
    };
    let mut widest = 0.0_f32;
    for layer in 0..layers {
        let row: Vec<&BergBlock> = facts.blocks.iter().filter(|b| b.layer == layer).collect();
        #[allow(clippy::cast_precision_loss)]
        let total = row.iter().map(|b| weight(b)).sum::<f32>() + row.len() as f32 * 1.5;
        widest = widest.max(total);
    }
    let scale = if widest > 0.0 { max_w / widest } else { 1.0 };
    let mut out = vec![Placed { x: 0.0, y: 0.0, w: 0.0, h: ROW }; facts.blocks.len()];
    for layer in 0..layers {
        let mut row: Vec<usize> = (0..facts.blocks.len()).filter(|i| facts.blocks[*i].layer == layer).collect();
        row.sort_by(|a, b| facts.blocks[*b].sloc.cmp(&facts.blocks[*a].sloc));
        // Heaviest at the centre, the rest alternating outwards.
        let mut order: Vec<usize> = Vec::new();
        for (j, i) in row.into_iter().enumerate() {
            if j % 2 == 1 {
                order.push(i);
            } else {
                order.insert(0, i);
            }
        }
        let widths: Vec<f32> = order.iter().map(|i| (weight(&facts.blocks[*i]) * scale).max(2.0)).collect();
        #[allow(clippy::cast_precision_loss)]
        let total: f32 = widths.iter().sum::<f32>() + (order.len().saturating_sub(1)) as f32 * 1.5;
        let mut x = width / 2.0 - total / 2.0;
        #[allow(clippy::cast_precision_loss)]
        for (i, w) in order.iter().zip(&widths) {
            out[*i] = Placed { x, y: WATERLINE + 4.0 + layer as f32 * (ROW + ROW_GAP), w: *w, h: ROW };
            x += w + 1.5;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let height = (150.0_f32).max(WATERLINE + 8.0 + layers as f32 * (ROW + ROW_GAP) + 18.0);
    (out, height)
}

// ---------------------------------------------------------------- the glyph cell

const NUMBER: TypeRole = TypeRole { face: Face::Mono, weight: 700.0, size: 13.0, line: 16.0, tracking: 0.0, italic: false };
const OWN: TypeRole = TypeRole { face: Face::Mono, weight: 500.0, size: 10.5, line: 14.0, tracking: 0.0, italic: false };
const CAPTION: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::SMALL };

/// The weight cell (see [`weight`]).
#[derive(IntoElement)]
pub struct WeightCell {
    id: ElementId,
    facts: Rc<BergFacts>,
    measure: Measure,
    width: Pixels,
    height: Nominal,
    fold: Fold,
    on_toggle: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// The weight cell for `facts`: `open` when the full berg is showing.
#[must_use]
pub fn weight(id: impl Into<ElementId>, facts: Rc<BergFacts>, width: Pixels, height: Nominal, fold: Fold, measure: &Measure) -> WeightCell {
    WeightCell { id: id.into(), facts, measure: *measure, width, height, fold, on_toggle: None }
}

impl WeightCell {
    /// Called on click (the berg opens or closes).
    #[must_use]
    pub fn on_toggle(mut self, on_toggle: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }
}

impl RenderOnce for WeightCell {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let hover = touch.motion.animate(track(&self.id, "hover"), if touch.hovered || self.fold == Fold::Open { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let facts = self.facts.clone();
        let own = lines(facts.own);
        let beneath = lines(facts.below());
        let (peri, hi, ink0) = (palette.peri.base, palette.peri_hi, palette.ink0);
        let tip_h = 5.0 + (facts.share() * 22.0).round();
        let glyph = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                let k = scale;
                let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                let at = |x: f32, y: f32| pt(ox + x * k, oy + y * k);
                let mut tip = Fill::new();
                tip.poly(&Poly::new([at(26.0, 24.0), at(36.0, 24.0 - tip_h), at(41.0, 24.0 - tip_h * 0.75), at(50.0, 24.0)]));
                tip.paint(window, Hsla::from(hi));
                let mut mass = Fill::new();
                let outline = Poly::new([at(14.0, 24.0), at(62.0, 24.0), at(70.0, 36.0), at(60.0, 52.0), at(44.0, 61.0), at(28.0, 60.0), at(12.0, 50.0), at(6.0, 36.0)]);
                mass.poly(&outline);
                mass.paint(window, Hsla::from(peri).opacity(0.16 + 0.12 * hover));
                let mut edge = Fill::new();
                for ring in outline.offset(-0.5).stroke_ring(1.0) {
                    edge.poly(&ring);
                }
                edge.paint(window, Hsla::from(peri));
                // The waterline, dashed.
                let mut line = Fill::new();
                let mut x = 2.0;
                while x < 74.0 {
                    line.poly(&Poly::rect(ox + x * k, oy + 24.0 * k - 0.5, 2.0 * k, 1.0));
                    x += 4.0;
                }
                line.paint(window, Hsla::from(peri));
                let number = shape(beneath.clone(), TypeRole { size: 13.0 * k, line: 16.0 * k, ..NUMBER }, ink0.into(), window);
                number.paint_centered(ox + 38.0 * k, oy + 44.0 * k + number.ascent() * 0.4, window, cx);
                let tiny = shape(own.clone(), TypeRole { size: 10.5 * k, line: 14.0 * k, ..OWN }, hi.into(), window);
                tiny.paint(ox + 53.0 * k, oy + (24.0 - tip_h).max(9.0) * k + 4.0 * k, window, cx);
            },
        )
        .flex_none()
        .w(px(76.0 * scale))
        .h(px(64.0 * scale));
        let count = facts.blocks.len();
        let mut words = format!("{count} {} beneath", if count == 1 { "package" } else { "packages" });
        if let Some(basis) = facts.basis {
            words = format!("{words} · {}", basis.words());
        }
        if facts.missing > 0 {
            words = format!("{words} · {} not on this machine", facts.missing);
        }
        let mut edge = Edge::of(Bevel::Rest, palette);
        edge.hi = palette.line3.into();
        edge.lo = palette.line2.into();
        let edge = edge.mix(Edge::of(Bevel::Peri, palette), hover);
        let plate = super::crest::cell(&self.id, "Weight", None, Some("from its source"), &measure, palette)
            .edge(edge)
            .fill(mix(palette.plate.into(), palette.plate2.into(), hover))
            .w(self.width)
            .h(self.height.at(scale))
            .child(glyph)
            .child(super::text::wrap(key(&self.id, "caption"), words, CAPTION, palette.ink2, &measure, Some(3)))
            .id(self.id.clone());
        let plate = wire(plate, &touch, self.on_toggle.clone());
        hover_zone(plate, &touch, 9.0 * scale, true)
    }
}

// ---------------------------------------------------------------- the berg

struct State {
    hover: Option<usize>,
}

/// The full berg (see [`berg`]).
pub struct Berg {
    id: ElementId,
    facts: Rc<BergFacts>,
    measure: Measure,
    rest: Option<usize>,
    on_go: Option<Rc<dyn Fn(usize, &mut Window, &mut App)>>,
}

/// The berg of `facts`, as wide as `measure` gives it.
#[must_use]
pub fn berg(id: impl Into<ElementId>, facts: Rc<BergFacts>, measure: &Measure) -> Berg {
    Berg { id: id.into(), facts, measure: *measure, rest: None, on_go: None }
}

impl Berg {
    /// Shows a block as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, block: Option<usize>) -> Self {
        self.rest = block;
        self
    }

    /// Called with the block clicked.
    #[must_use]
    pub fn on_go(mut self, on_go: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_go = Some(Rc::new(on_go));
        self
    }

    fn height(&self) -> f32 {
        place(&self.facts, f32::from(self.measure.width()) / self.measure.scale()).1 * self.measure.scale() + 26.0 * self.measure.scale()
    }
}

impl IntoElement for Berg {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// What the berg keeps between layout and paint.
pub struct BergLayout {
    state: Entity<State>,
}

impl Element for Berg {
    type RequestLayoutState = BergLayout;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, BergLayout) {
        let state = window.use_keyed_state("berg", cx, |_, _| State { hover: None });
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px(self.height()).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), BergLayout { state })
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut BergLayout, window: &mut Window, _cx: &mut App) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    #[allow(clippy::too_many_lines)]
    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, layout: &mut BergLayout, hitbox: &mut Hitbox, window: &mut Window, cx: &mut App) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let width = f32::from(bounds.size.width);
        let facts = self.facts.clone();
        let (placed, height) = place(&facts, width / s);
        let hot = layout.state.read(cx).hover.or(self.rest);
        let keel: Vec<usize> = hot.map(|h| facts.keel(h)).unwrap_or_default();
        let wl = oy + WATERLINE * s;

        // The sea, the tip and the waterline.
        let mut sea = Fill::new();
        sea.poly(&Poly::rect(ox, wl, width, (height - WATERLINE) * s));
        sea.paint(window, Hsla::from(palette.peri.base).opacity(0.06));
        let share = facts.share();
        let tip_w = ((facts.own.max(1) as f32).sqrt() * 0.4).clamp(14.0, width / s * 0.6) * s;
        let tip_h = (6.0 + share * 90.0).clamp(8.0, WATERLINE - 8.0) * s;
        let cx0 = ox + width * 0.5;
        let mut tip = Fill::new();
        tip.poly(&Poly::new([pt(cx0 - tip_w * 0.5, wl), pt(cx0 - tip_w * 0.18, wl - tip_h), pt(cx0 + tip_w * 0.1, wl - tip_h * 0.8), pt(cx0 + tip_w * 0.5, wl)]));
        tip.paint(window, Hsla::from(palette.peri_hi));
        let mut line = Fill::new();
        let mut x = 0.0;
        while x < width {
            line.poly(&Poly::rect(ox + x, wl - 0.5, 3.0 * s, 1.0));
            x += 6.0 * s;
        }
        line.paint(window, Hsla::from(palette.peri.base).opacity(0.8));

        // The blocks.
        let mut batches: Vec<(Hsla, Fill)> = Vec::new();
        let mut put = |colour: Hsla, poly: &Poly| {
            let at = batches.iter().position(|(c, _)| *c == colour).unwrap_or_else(|| {
                batches.push((colour, Fill::new()));
                batches.len() - 1
            });
            batches[at].1.poly(poly);
        };
        for (i, p) in placed.iter().enumerate() {
            let layer = facts.blocks[i].layer.min(4);
            let alpha = [0.78, 0.66, 0.56, 0.46, 0.38][layer];
            let colour: Hsla = if hot == Some(i) {
                palette.ink0.into()
            } else if keel.contains(&i) {
                palette.peri.base.into()
            } else if hot.is_some() {
                Hsla::from(palette.ink3).opacity(0.22)
            } else {
                Hsla::from(palette.ink3).opacity(alpha)
            };
            put(colour, &Poly::rect(ox + p.x * s, oy + p.y * s, p.w * s, p.h * s));
        }
        for (colour, fill) in batches {
            fill.paint(window, colour);
        }

        // The tip's own words.
        let own_role = self.measure.role(TypeRole { size: 11.0, line: 15.0, ..OWN });
        let own_words: SharedString = format!("its own · {}", lines(facts.own)).into();
        let own = shape(own_words.clone(), own_role, palette.peri_hi.into(), window);
        let own_x = cx0 + tip_w * 0.5 + 6.0 * s;
        own.paint(own_x, wl - 6.0 * s, window, cx);
        let mut published: Vec<(Bounds<Pixels>, SharedString, TypeRole, f32, &'static str)> = vec![(
            Bounds::new(gpui::point(px(own_x), px(wl - 6.0 * s - own.ascent())), gpui::size(px(own.width()), px(own_role.line))),
            own_words,
            own_role,
            own.width(),
            "own",
        )];

        // The surfaced block: its plate above the waterline, a dashed line up to it.
        if let Some(h) = hot.filter(|h| *h < placed.len()) {
            let p = placed[h];
            let plate_role = self.measure.role(TypeRole { weight: 500.0, size: 11.5, line: 16.0, ..OWN });
            let words: SharedString = format!("{} · {}", facts.blocks[h].name, lines(facts.blocks[h].sloc)).into();
            let text = shape(words.clone(), plate_role, palette.ink0.into(), window);
            let (pw, ph) = (text.width() + 16.0 * s, 20.0 * s);
            let px_ = (ox + (p.x + p.w * 0.5) * s - pw * 0.5).clamp(ox + 4.0, (ox + width - pw - 4.0).max(ox + 4.0));
            let py = wl - ph - 6.0 * s;
            let mut rise = Fill::new();
            let mut y = oy + p.y * s;
            while y > wl - 4.0 * s {
                rise.poly(&Poly::rect(ox + (p.x + p.w * 0.5) * s - 0.6, y - 2.0 * s, 1.2, 2.0 * s));
                y -= 4.0 * s;
            }
            rise.paint(window, Hsla::from(palette.peri_hi));
            let plate = Poly::rect(px_, py, pw, ph);
            let mut back = Fill::new();
            back.poly(&plate);
            back.paint(window, Hsla::from(palette.plate3));
            let mut edge = Fill::new();
            for ring in plate.offset(-0.5).stroke_ring(1.0) {
                edge.poly(&ring);
            }
            edge.paint(window, Hsla::from(palette.peri_hi));
            let baseline = py + (ph - plate_role.line) * 0.5 + text.ascent();
            text.paint(px_ + 8.0 * s, baseline, window, cx);
            published.push((
                Bounds::new(gpui::point(px(px_ + 8.0 * s), px(baseline - text.ascent())), gpui::size(px(text.width()), px(plate_role.line))),
                words,
                plate_role,
                text.width(),
                "plate",
            ));
        }

        // The caption: the whole berg at rest; the surfaced block when one is.
        let caption_role = self.measure.role(CAPTION);
        let caption_y = oy + height * s + 8.0 * s;
        let below = facts.below();
        let mut segments: Vec<(String, bool)> = Vec::new();
        if let Some(h) = hot.filter(|h| *h < placed.len()) {
            let carried = facts.carried(h);
            #[allow(clippy::cast_precision_loss)]
            let share = (carried as f32 / below.max(1) as f32 * 100.0).round();
            segments.push((facts.blocks[h].name.to_string(), true));
            segments.push((" carries ".to_owned(), false));
            segments.push((format!("{share:.0}%"), true));
            segments.push((format!(" of the weight beneath ({} lines", lines(carried)), false));
            if !keel.is_empty() {
                segments.push((format!(", {} under it", pluralise(keel.len())), false));
            }
            segments.push((")".to_owned(), false));
            let chain = facts.chain(h);
            if chain.len() > 1 {
                let via: Vec<String> = chain[..chain.len() - 1].iter().map(|i| facts.blocks[*i].name.to_string()).collect();
                segments.push((format!("  reached via {}", via.join(" › ")), false));
            } else {
                segments.push(("  it rests on it directly".to_owned(), false));
            }
            if self.on_go.is_some() {
                segments.push(("  click to go there".to_owned(), false));
            }
        } else {
            #[allow(clippy::cast_precision_loss)]
            let above = (share * 100.0).round();
            segments.push((format!("{above:.0}%"), true));
            segments.push((" above water · ".to_owned(), false));
            segments.push((lines(below), true));
            segments.push((format!(" lines beneath in {}", pluralise(facts.blocks.len())), false));
        }
        let mut x = ox;
        let mut joined = String::new();
        for (words, strong) in &segments {
            let ink = if *strong { palette.ink0 } else { palette.ink3 };
            let role = if *strong { TypeRole { weight: 560.0, ..caption_role } } else { caption_role };
            let text = shape_fit(words, role, ink.into(), (ox + width - x).max(20.0), window);
            text.paint(x, caption_y + caption_role.size, window, cx);
            x += text.width();
            joined.push_str(words);
        }
        published.push((
            Bounds::new(gpui::point(px(ox), px(caption_y)), gpui::size(px(x - ox), px(caption_role.line))),
            joined.into(),
            caption_role,
            x - ox,
            "caption",
        ));
        if probe::enabled(cx) {
            for (at, content, role, natural, name) in published {
                probe::record_text_in(
                    cx,
                    &ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(name)),
                    at,
                    TextSample {
                        key: String::new(),
                        bounds: probe::BoundsSample { key: String::new(), x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
                        paint_clip: None,
                        scroll_ancestors: probe::current_scroll_ancestors(),
                        natural_width: natural,
                        overflow: TextOverflow::Clip,
                        content: content.to_string(),
                        min_width: natural,
                        line_height: role.line,
                        size: role.size,
                        weight: role.weight,
                        region: probe::current_region(),
                    },
                    window,
                );
            }
            for (i, p) in placed.iter().enumerate() {
                probe::record_bounds(
                    cx,
                    &ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(format!("block-{i}"))),
                    Bounds::new(gpui::point(px(ox + p.x * s), px(oy + p.y * s)), gpui::size(px(p.w * s), px(p.h * s))),
                );
            }
        }

        // The pointer.
        let hit = {
            let placed = placed.clone();
            Rc::new(move |x: f32, y: f32| -> Option<usize> {
                placed.iter().position(|p| x >= ox + p.x * s - 1.0 && x <= ox + (p.x + p.w) * s + 1.0 && y >= oy + p.y * s - 1.0 && y <= oy + (p.y + p.h) * s + 1.0)
            })
        };
        {
            let (state, hitbox, hit) = (layout.state.clone(), hitbox.clone(), hit.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let next = hitbox.is_hovered(window).then(|| hit(f32::from(event.position.x), f32::from(event.position.y))).flatten();
                if state.read(cx).hover != next {
                    state.update(cx, |state, cx| {
                        state.hover = next;
                        cx.notify();
                    });
                }
            });
        }
        {
            let state = layout.state.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
                if phase == DispatchPhase::Capture && state.read(cx).hover.is_some() {
                    state.update(cx, |state, cx| {
                        state.hover = None;
                        cx.notify();
                    });
                }
            });
        }
        if let Some(go) = self.on_go.clone() {
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                    && let Some(block) = hit(f32::from(event.position.x), f32::from(event.position.y))
                {
                    go(block, window, cx);
                }
            });
        }
    }
}

fn pluralise(n: usize) -> String {
    format!("{n} {}", if n == 1 { "package" } else { "packages" })
}

#[cfg(test)]
mod tests {
    use super::{BergBlock, BergFacts, lines, place};

    fn block(name: &str, sloc: usize, layer: usize, parent: Option<usize>, deps: &[usize]) -> BergBlock {
        BergBlock { name: name.to_owned().into(), version: "1.0.0".into(), sloc, layer, parent, deps: deps.to_vec() }
    }

    fn sample() -> BergFacts {
        BergFacts {
            name: "tokio".into(),
            own: 49_187,
            blocks: vec![
                block("bytes", 12_000, 0, None, &[3]),
                block("mio", 20_000, 0, None, &[3, 4]),
                block("pin-project-lite", 1_000, 0, None, &[]),
                block("libc", 200_000, 1, Some(1), &[]),
                block("windows-sys", 300_000, 1, Some(1), &[]),
            ],
            missing: 0,
            basis: None,
        }
    }

    #[test]
    fn lines_read_the_way_the_board_writes_them() {
        assert_eq!(lines(999), "999");
        assert_eq!(lines(3_335), "3.3K");
        assert_eq!(lines(49_187), "49K");
        assert_eq!(lines(648_000), "648K");
        assert_eq!(lines(1_234_000), "1.2M");
    }

    #[test]
    fn the_keel_is_everything_beneath_and_the_chain_is_the_way_down() {
        let facts = sample();
        assert_eq!(facts.keel(1), [3, 4]);
        assert!(facts.keel(3).is_empty());
        assert_eq!(facts.chain(4), [1, 4]);
        assert_eq!(facts.chain(0), [0]);
        assert_eq!(facts.carried(1), 20_000 + 200_000 + 300_000);
        assert_eq!(facts.below(), 12_000 + 20_000 + 1_000 + 200_000 + 300_000);
        assert!(facts.share() > 0.08 && facts.share() < 0.1, "{}", facts.share());
    }

    #[test]
    fn blocks_never_overlap_within_a_layer_and_stay_inside_the_berg() {
        let facts = sample();
        for width in [400.0, 1000.0, 1500.0] {
            let (placed, height) = place(&facts, width);
            assert!(height >= 150.0);
            for (i, a) in placed.iter().enumerate() {
                assert!(a.x >= 0.0 && a.x + a.w <= width + 0.01, "{width}: block {i} leaves the berg");
                for (j, b) in placed.iter().enumerate().skip(i + 1) {
                    if (a.y - b.y).abs() < 0.01 {
                        assert!(a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01, "{width}: blocks {i} and {j} overlap");
                    }
                }
            }
            // The heaviest of a layer sits at the centre.
            let centre = |i: usize| placed[i].x + placed[i].w * 0.5;
            assert!((centre(4) - width * 0.5).abs() < placed[4].w, "{width}: windows-sys is not central: {}", centre(4));
        }
    }
}
