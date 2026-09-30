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
    App, Bounds, ColorExt as _, DispatchPhase, Element, ElementId, Entity, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, InteractiveElement, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, ParentElement, Pixels, Refineable,
    RenderOnce, SharedString, Style, StyleRefinement, Styled, Window, px,
};
use std::collections::BTreeMap;
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
        let thousands = (x / 1e3).round();
        if thousands >= 1_000.0 {
            format!("{:.1}M", x / 1e6)
        } else {
            format!("{thousands:.0}K")
        }
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
        self.blocks
            .iter()
            .fold(0usize, |sum, block| sum.saturating_add(block.sloc))
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
        if index >= self.blocks.len() {
            return Vec::new();
        }
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![index];
        let mut out = Vec::new();
        while let Some(at) = stack.pop() {
            for dep in &self.blocks[at].deps {
                if *dep < self.blocks.len() && !seen[*dep] && *dep != index {
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
        if index >= self.blocks.len() {
            return Vec::new();
        }
        let mut out = vec![index];
        let mut seen = vec![false; self.blocks.len()];
        seen[index] = true;
        let mut at = index;
        while let Some(parent) = self.blocks[at].parent {
            if parent >= self.blocks.len() || seen[parent] {
                break;
            }
            seen[parent] = true;
            out.push(parent);
            at = parent;
        }
        out.reverse();
        out
    }

    /// The lines `index` carries: itself and everything under it.
    #[must_use]
    pub fn carried(&self, index: usize) -> usize {
        self.blocks.get(index).map_or(0, |block| {
            self.keel(index).iter().fold(block.sloc, |sum, k| {
                sum.saturating_add(self.blocks[*k].sloc)
            })
        })
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
    let width = f32::from(measure.width()).max(0.0);
    let (placed, height) = place_at_scale(facts, width / s, s);
    let full_height = (height + 26.0) * s;
    placed
        .iter()
        .map(|p| {
            let (visual_w, visual_h) = (p.w * s, p.h * s);
            let target_w = visual_w.max(DOOR_SIZE).min(width);
            let target_h = visual_h.max(DOOR_SIZE).min(full_height);
            let x =
                (p.x * s + visual_w * 0.5 - target_w * 0.5).clamp(0.0, (width - target_w).max(0.0));
            let y = (p.y * s + visual_h * 0.5 - target_h * 0.5)
                .clamp(0.0, (full_height - target_h).max(0.0));
            Bounds::new(
                gpui::point(px(x), px(y)),
                gpui::size(px(target_w), px(target_h)),
            )
        })
        .collect()
}

/// The waterline's y, px at scale 1.
pub const WATERLINE: f32 = 38.0;
const ROW: f32 = 10.0;
const ROW_GAP: f32 = 2.0;
const DOOR_SIZE: f32 = 24.0;
// Keep a physical pixel between expanded doors after fractional-scale rounding.
const DOOR_PITCH: f32 = 25.0;

/// Lays the blocks out in a berg `width` px wide at scale 1: each dependency
/// layer is a row (dense layers continue below), heaviest at the centre and
/// the rest alternating outwards, each block as wide as the square root of
/// its lines.
#[must_use]
pub fn place(facts: &BergFacts, width: f32) -> (Vec<Placed>, f32) {
    place_at_scale(facts, width, 1.0)
}

/// The same layout, with enough space between stones for distinct keyboard
/// and pointer doors at the reader's actual text scale.
fn place_at_scale(facts: &BergFacts, width: f32, scale: f32) -> (Vec<Placed>, f32) {
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let inset = 10.0_f32.min(width * 0.05);
    let available = (width - 2.0 * inset).max(0.0);
    let door_pitch = DOOR_PITCH / scale;
    let row_pitch = (ROW + ROW_GAP).max(door_pitch);
    let weight = |b: &BergBlock| {
        #[allow(clippy::cast_precision_loss)]
        (b.sloc.max(1) as f32).sqrt()
    };
    let mut rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, block) in facts.blocks.iter().enumerate() {
        rows.entry(block.layer).or_default().push(index);
    }
    let max_per_row = if door_pitch > 0.0 && available >= door_pitch {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = (available / door_pitch).floor() as usize;
        count.max(1)
    } else {
        1
    };
    let mut strips: Vec<(Vec<usize>, f32)> = Vec::new();
    let mut block_scale = f32::INFINITY;
    for (_, mut row) in rows {
        row.sort_by(|a, b| facts.blocks[*b].sloc.cmp(&facts.blocks[*a].sloc));
        // When a layer is too dense for distinct 24 px doors, continue it on
        // another visual row. Each row keeps the heaviest stones at its centre.
        for chunk in row.chunks(max_per_row) {
            let mut left: Vec<usize> = Vec::with_capacity(chunk.len().div_ceil(2));
            let mut right: Vec<usize> = Vec::with_capacity(chunk.len() / 2);
            for (j, i) in chunk.iter().copied().enumerate() {
                if j % 2 == 1 {
                    right.push(i);
                } else {
                    left.push(i);
                }
            }
            left.reverse();
            left.extend(right);
            #[allow(clippy::cast_precision_loss)]
            let slot = if left.is_empty() {
                0.0
            } else {
                available / left.len() as f32
            };
            let heaviest = left
                .iter()
                .map(|i| weight(&facts.blocks[*i]))
                .fold(0.0_f32, f32::max);
            if heaviest > 0.0 {
                // The block is narrower than its focus slot, leaving a quiet
                // gap while the door itself remains at least 24 px wide.
                block_scale = block_scale.min(slot * 0.88 / heaviest);
            }
            strips.push((left, slot));
        }
    }
    if !block_scale.is_finite() {
        block_scale = 0.0;
    }
    let mut out = vec![
        Placed {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: ROW
        };
        facts.blocks.len()
    ];
    for (row_index, (order, slot)) in strips.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let y = WATERLINE + 4.0 + row_index as f32 * row_pitch;
        for (slot_index, index) in order.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let slot_x = inset + slot_index as f32 * slot;
            let block_width = (weight(&facts.blocks[*index]) * block_scale).min(slot * 0.88);
            out[*index] = Placed {
                x: slot_x + (slot - block_width) * 0.5,
                y,
                w: block_width,
                h: ROW,
            };
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let height = (150.0_f32).max(WATERLINE + 8.0 + strips.len() as f32 * row_pitch + 18.0);
    (out, height)
}

// ---------------------------------------------------------------- the glyph cell

const NUMBER: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 700.0,
    size: 13.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const OWN: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 500.0,
    size: 10.5,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};
const CAPTION: TypeRole = TypeRole {
    size: 12.0,
    line: 16.0,
    ..ty::SMALL
};

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
pub fn weight(
    id: impl Into<ElementId>,
    facts: Rc<BergFacts>,
    width: Pixels,
    height: Nominal,
    fold: Fold,
    measure: &Measure,
) -> WeightCell {
    WeightCell {
        id: id.into(),
        facts,
        measure: *measure,
        width,
        height,
        fold,
        on_toggle: None,
    }
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
        let hover = touch.motion.animate(
            track(&self.id, "hover"),
            if touch.hovered || self.fold == Fold::Open {
                1.0
            } else {
                0.0
            },
            spec::HOVER,
            window,
            cx,
        );
        let facts = &self.facts;
        let own = lines(facts.own);
        let beneath = lines(facts.below());
        let (peri, hi, ink0) = (
            palette.peri.base.into(),
            palette.peri_hi.into(),
            palette.ink0.into(),
        );
        let tip_h = 5.0 + (facts.share() * 22.0).round();
        let glyph = BergGlyph {
            scale,
            tip_h,
            beneath,
            own,
            peri,
            hi,
            ink0,
            hover,
            style: StyleRefinement::default(),
        }
        .flex_none()
        .w(px(76.0 * scale))
        .h(px(64.0 * scale));
        let count = facts.blocks.len();
        let mut words = format!(
            "{count} {} beneath",
            if count == 1 { "package" } else { "packages" }
        );
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
        let plate = super::crest::cell(
            &self.id,
            "Weight",
            None,
            Some("from its source"),
            &measure,
            palette,
        )
        .edge(edge)
        .fill(mix(palette.plate.into(), palette.plate2.into(), hover))
        .w(self.width)
        .h(self.height.at(scale))
        .child(glyph)
        .child(super::text::wrap(
            key(&self.id, "caption"),
            words,
            CAPTION,
            palette.ink2,
            &measure,
            Some(3),
        ))
        .id(self.id.clone());
        let plate = wire(plate, &touch, self.on_toggle.clone());
        hover_zone(plate, &touch, 9.0 * scale, true)
    }
}

struct BergGlyph {
    scale: f32,
    tip_h: f32,
    beneath: String,
    own: String,
    peri: Hsla,
    hi: Hsla,
    ink0: Hsla,
    hover: f32,
    style: StyleRefinement,
}

impl Styled for BergGlyph {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for BergGlyph {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for BergGlyph {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (scale, tip_h, beneath, own, peri, hi, ink0, hover) = (
            self.scale,
            self.tip_h,
            &self.beneath,
            &self.own,
            self.peri,
            self.hi,
            self.ink0,
            self.hover,
        );
        style.paint(bounds, window, cx, |window, cx| {
            let k = scale;
            let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let at = |x: f32, y: f32| pt(ox + x * k, oy + y * k);
            let mut tip = Fill::new();
            tip.poly(&Poly::new([
                at(26.0, 24.0),
                at(36.0, 24.0 - tip_h),
                at(41.0, 24.0 - tip_h * 0.75),
                at(50.0, 24.0),
            ]));
            tip.paint(window, Hsla::from(hi));
            let mut mass = Fill::new();
            let outline = Poly::new([
                at(14.0, 24.0),
                at(62.0, 24.0),
                at(70.0, 36.0),
                at(60.0, 52.0),
                at(44.0, 61.0),
                at(28.0, 60.0),
                at(12.0, 50.0),
                at(6.0, 36.0),
            ]);
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
            let number = shape(
                beneath.clone(),
                TypeRole {
                    size: 13.0 * k,
                    line: 16.0 * k,
                    ..NUMBER
                },
                ink0,
                window,
            );
            number.paint_centered(
                ox + 38.0 * k,
                oy + 44.0 * k + number.ascent() * 0.4,
                window,
                cx,
            );
            let tiny = shape(
                own.clone(),
                TypeRole {
                    size: 10.5 * k,
                    line: 14.0 * k,
                    ..OWN
                },
                hi,
                window,
            );
            tiny.paint(
                ox + 53.0 * k,
                oy + (24.0 - tip_h).max(9.0) * k + 4.0 * k,
                window,
                cx,
            );
        });
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
    Berg {
        id: id.into(),
        facts,
        measure: *measure,
        rest: None,
        on_go: None,
    }
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
    placed: Rc<[Placed]>,
    content_height: f32,
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

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, BergLayout) {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| State { hover: None });
        let scale = self.measure.scale();
        let (placed, content_height) =
            place_at_scale(&self.facts, f32::from(self.measure.width()) / scale, scale);
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px((content_height + 26.0) * scale).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [], cx),
            BergLayout {
                state,
                placed: placed.into(),
                content_height,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut BergLayout,
        window: &mut Window,
        _cx: &mut App,
    ) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    #[allow(clippy::too_many_lines)]
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut BergLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let width = f32::from(bounds.size.width);
        let facts = &self.facts;
        let (placed, height) = (layout.placed.clone(), layout.content_height);
        let hot = layout
            .state
            .read(cx)
            .hover
            .or(self.rest)
            .filter(|h| *h < placed.len());
        let keel: Vec<usize> = hot.map(|h| facts.keel(h)).unwrap_or_default();
        let wl = oy + WATERLINE * s;

        // The sea, the tip and the waterline.
        let mut sea = Fill::new();
        sea.poly(&Poly::rect(ox, wl, width, (height - WATERLINE) * s));
        sea.paint(window, Hsla::from(palette.peri.base).opacity(0.06));
        let share = facts.share();
        let base_width = (width / s).max(0.0);
        let max_tip_w = base_width * 0.6;
        let tip_w = (((facts.own.max(1) as f32).sqrt() * 0.4)
            .max(14.0)
            .min(max_tip_w))
            * s;
        let tip_h = (6.0 + share * 90.0).clamp(8.0, WATERLINE - 8.0) * s;
        let cx0 = ox + width * 0.5;
        let mut tip = Fill::new();
        tip.poly(&Poly::new([
            pt(cx0 - tip_w * 0.5, wl),
            pt(cx0 - tip_w * 0.18, wl - tip_h),
            pt(cx0 + tip_w * 0.1, wl - tip_h * 0.8),
            pt(cx0 + tip_w * 0.5, wl),
        ]));
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
            let at = batches
                .iter()
                .position(|(c, _)| *c == colour)
                .unwrap_or_else(|| {
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
            put(
                colour,
                &Poly::rect(ox + p.x * s, oy + p.y * s, p.w * s, p.h * s),
            );
        }
        for (colour, fill) in batches {
            fill.paint(window, colour);
        }

        // The tip's own words.
        let own_role = self.measure.role(TypeRole {
            size: 11.0,
            line: 15.0,
            ..OWN
        });
        let own_words = format!("its own · {}", lines(facts.own));
        let own_full = shape(own_words.clone(), own_role, palette.peri_hi.into(), window);
        let right_x = cx0 + tip_w * 0.5 + 6.0 * s;
        let right_room = (ox + width - 4.0 * s - right_x).max(0.0);
        let left_room = (cx0 - tip_w * 0.5 - 6.0 * s - (ox + 4.0 * s)).max(0.0);
        let (own_x, own_room) = if right_room >= left_room {
            (right_x, right_room)
        } else {
            (ox + 4.0 * s, left_room)
        };
        let ellipsis_width = shape("…", own_role, palette.peri_hi.into(), window).width();
        let own = if own_room >= ellipsis_width {
            shape_fit(
                &own_words,
                own_role,
                palette.peri_hi.into(),
                own_room,
                window,
            )
        } else {
            shape("", own_role, palette.peri_hi.into(), window)
        };
        let own_content = own.text();
        own.paint(own_x, wl - 6.0 * s, window, cx);
        let own_overflow = if own_content.as_ref() == own_words.as_str() {
            TextOverflow::Clip
        } else {
            TextOverflow::Ellipsis
        };
        let mut published: Vec<(
            Bounds<Pixels>,
            SharedString,
            TypeRole,
            f32,
            TextOverflow,
            &'static str,
        )> = vec![(
            Bounds::new(
                gpui::point(px(own_x), px(wl - 6.0 * s - own.ascent())),
                gpui::size(px(own.width()), px(own_role.line)),
            ),
            own_content,
            own_role,
            own_full.width(),
            own_overflow,
            "own",
        )];

        // The surfaced block: its plate above the waterline, a dashed line up to it.
        if let Some(h) = hot.filter(|h| *h < placed.len()) {
            let p = placed[h];
            let plate_role = self.measure.role(TypeRole {
                weight: 500.0,
                size: 11.5,
                line: 16.0,
                ..OWN
            });
            let mut rise = Fill::new();
            let mut y = oy + p.y * s;
            while y > wl - 4.0 * s {
                rise.poly(&Poly::rect(
                    ox + (p.x + p.w * 0.5) * s - 0.6,
                    y - 2.0 * s,
                    1.2,
                    2.0 * s,
                ));
                y -= 4.0 * s;
            }
            rise.paint(window, Hsla::from(palette.peri_hi));
            let words = format!("{} · {}", facts.blocks[h].name, lines(facts.blocks[h].sloc));
            let available = (width - 8.0 * s).max(0.0);
            let padding = (8.0 * s).min(available * 0.25);
            let text_room = (available - 2.0 * padding).max(0.0);
            let min_label = shape("…", plate_role, palette.ink0.into(), window).width();
            if text_room >= min_label {
                let full = shape(words.clone(), plate_role, palette.ink0.into(), window);
                let text = shape_fit(&words, plate_role, palette.ink0.into(), text_room, window);
                let content = text.text();
                let overflow = if content.as_ref() == words.as_str() {
                    TextOverflow::Clip
                } else {
                    TextOverflow::Ellipsis
                };
                let (pw, ph) = (text.width() + 2.0 * padding, 20.0 * s);
                let left = ox + 4.0 * s;
                let right = (ox + width - 4.0 * s - pw).max(left);
                let plate_x = (ox + (p.x + p.w * 0.5) * s - pw * 0.5).clamp(left, right);
                let py = wl - ph - 6.0 * s;
                let plate = Poly::rect(plate_x, py, pw, ph);
                let mut back = Fill::new();
                back.poly(&plate);
                back.paint(window, Hsla::from(palette.plate3));
                let mut edge = Fill::new();
                for ring in plate.offset(-0.5).stroke_ring(1.0) {
                    edge.poly(&ring);
                }
                edge.paint(window, Hsla::from(palette.peri_hi));
                let text_x = plate_x + padding;
                let baseline = py + (ph - plate_role.line) * 0.5 + text.ascent();
                text.paint(text_x, baseline, window, cx);
                published.push((
                    Bounds::new(
                        gpui::point(px(text_x), px(baseline - text.ascent())),
                        gpui::size(px(text.width()), px(plate_role.line)),
                    ),
                    content,
                    plate_role,
                    full.width(),
                    overflow,
                    "plate",
                ));
            }
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
            segments.push((
                format!(" of the weight beneath ({} lines", lines(carried)),
                false,
            ));
            if !keel.is_empty() {
                segments.push((format!(", {} under it", pluralise(keel.len())), false));
            }
            segments.push((")".to_owned(), false));
            let chain = facts.chain(h);
            if chain.len() > 1 {
                let via: Vec<String> = chain[..chain.len() - 1]
                    .iter()
                    .map(|i| facts.blocks[*i].name.to_string())
                    .collect();
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
            segments.push((
                format!(" lines beneath in {}", pluralise(facts.blocks.len())),
                false,
            ));
        }
        let mut natural_caption = 0.0;
        let segment_widths: Vec<f32> = segments
            .iter()
            .map(|(words, strong)| {
                let ink = if *strong { palette.ink0 } else { palette.ink3 };
                let role = if *strong {
                    TypeRole {
                        weight: 560.0,
                        ..caption_role
                    }
                } else {
                    caption_role
                };
                let width = shape(words.clone(), role, ink.into(), window).width();
                natural_caption += width;
                width
            })
            .collect();
        let mut x = ox;
        let mut joined = String::new();
        let mut truncated = false;
        let mut remaining_natural = natural_caption;
        for (index, (words, strong)) in segments.iter().enumerate() {
            let ink = if *strong { palette.ink0 } else { palette.ink3 };
            let role = if *strong {
                TypeRole {
                    weight: 560.0,
                    ..caption_role
                }
            } else {
                caption_role
            };
            let room = (ox + width - x).max(0.0);
            let ellipsis_width = shape("…", role, ink.into(), window).width();
            if room < ellipsis_width {
                truncated = index < segments.len();
                break;
            }
            let full = shape(words.clone(), role, ink.into(), window);
            let spare_after_full = room - full.width();
            let allowed = if remaining_natural > room && spare_after_full < ellipsis_width {
                (room - ellipsis_width).max(0.0)
            } else {
                room
            };
            let text = shape_fit(words, role, ink.into(), allowed, window);
            let content = text.text();
            truncated |= content.as_ref() != words.as_str();
            text.paint(x, caption_y + caption_role.size, window, cx);
            x += text.width();
            joined.push_str(content.as_ref());
            if content.as_ref() != words.as_str() {
                break;
            }
            remaining_natural -= segment_widths[index];
        }
        published.push((
            Bounds::new(
                gpui::point(px(ox), px(caption_y)),
                gpui::size(px(x - ox), px(caption_role.line)),
            ),
            joined.into(),
            caption_role,
            natural_caption,
            if truncated {
                TextOverflow::Ellipsis
            } else {
                TextOverflow::Clip
            },
            "caption",
        ));
        if probe::enabled(cx) {
            for (at, content, role, natural, overflow, name) in published {
                probe::record_text_in(
                    cx,
                    &ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(name)),
                    at,
                    TextSample {
                        key: String::new(),
                        bounds: probe::BoundsSample {
                            key: String::new(),
                            x: 0.0,
                            y: 0.0,
                            width: 0.0,
                            height: 0.0,
                        },
                        paint_clip: None,
                        scroll_ancestors: probe::current_scroll_ancestors(),
                        natural_width: natural,
                        overflow,
                        content: content.to_string(),
                        min_width: if overflow == TextOverflow::Clip {
                            natural
                        } else {
                            0.0
                        },
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
                    &ElementId::NamedChild(
                        Arc::new(self.id.clone()),
                        SharedString::from(format!("block-{i}")),
                    ),
                    Bounds::new(
                        gpui::point(px(ox + p.x * s), px(oy + p.y * s)),
                        gpui::size(px(p.w * s), px(p.h * s)),
                    ),
                );
            }
        }

        // The pointer.
        let hit = {
            let placed = placed.clone();
            Rc::new(move |x: f32, y: f32| -> Option<usize> {
                // Small stones still get a practical pointer target; where
                // the slop around neighbours meets, the closest stone wins.
                let slop = 12.0;
                let mut closest = None;
                let mut distance = f32::INFINITY;
                for (index, p) in placed.iter().enumerate() {
                    let (left, top, right, bottom) = (
                        ox + p.x * s,
                        oy + p.y * s,
                        ox + (p.x + p.w) * s,
                        oy + (p.y + p.h) * s,
                    );
                    let dx = if x < left {
                        left - x
                    } else if x > right {
                        x - right
                    } else {
                        0.0
                    };
                    let dy = if y < top {
                        top - y
                    } else if y > bottom {
                        y - bottom
                    } else {
                        0.0
                    };
                    let candidate = dx * dx + dy * dy;
                    if dx <= slop && dy <= slop && candidate < distance {
                        closest = Some(index);
                        distance = candidate;
                    }
                }
                closest
            })
        };
        {
            let (state, hitbox, hit) = (layout.state.clone(), hitbox.clone(), hit.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let next = hitbox
                    .is_hovered(window)
                    .then(|| hit(f32::from(event.position.x), f32::from(event.position.y)))
                    .flatten();
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
                    && let Some(block) =
                        hit(f32::from(event.position.x), f32::from(event.position.y))
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
    use super::{BergBlock, BergFacts, DOOR_SIZE, doors, lines, place, place_at_scale};
    use crate::measure::Measure;
    use crate::theme::Facet;
    use gpui::px;

    fn block(
        name: &str,
        sloc: usize,
        layer: usize,
        parent: Option<usize>,
        deps: &[usize],
    ) -> BergBlock {
        BergBlock {
            name: name.to_owned().into(),
            version: "1.0.0".into(),
            sloc,
            layer,
            parent,
            deps: deps.to_vec(),
        }
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
        assert_eq!(lines(999_499), "999K");
        assert_eq!(lines(999_500), "1.0M");
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
        assert!(
            facts.share() > 0.08 && facts.share() < 0.1,
            "{}",
            facts.share()
        );
    }

    #[test]
    fn blocks_never_overlap_within_a_layer_and_stay_inside_the_berg() {
        let facts = sample();
        for width in [400.0, 1000.0, 1500.0] {
            let (placed, height) = place(&facts, width);
            assert!(height >= 150.0);
            for (i, a) in placed.iter().enumerate() {
                assert!(
                    a.x >= 0.0 && a.x + a.w <= width + 0.01,
                    "{width}: block {i} leaves the berg"
                );
                for (j, b) in placed.iter().enumerate().skip(i + 1) {
                    if (a.y - b.y).abs() < 0.01 {
                        assert!(
                            a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01,
                            "{width}: blocks {i} and {j} overlap"
                        );
                    }
                }
            }
            // The heaviest of a layer sits at the centre.
            let centre = |i: usize| placed[i].x + placed[i].w * 0.5;
            assert!(
                (centre(4) - width * 0.5).abs() < placed[4].w,
                "{width}: windows-sys is not central: {}",
                centre(4)
            );
        }
    }

    #[test]
    fn keyboard_doors_cover_the_painted_blocks_at_double_text_size() {
        let facts = sample();
        let measure = Measure::new(
            px(480.0),
            &Facet {
                text_scale: 2.0,
                ..Facet::default()
            },
        );
        let targets = doors(&facts, &measure);
        let (placed, height) = place_at_scale(&facts, 240.0, 2.0);
        let surface_height = (height + 26.0) * 2.0;
        assert_eq!(targets.len(), placed.len());
        for (index, (target, block)) in targets.iter().zip(&placed).enumerate() {
            let (x, y, w, h) = (
                f32::from(target.origin.x),
                f32::from(target.origin.y),
                f32::from(target.size.width),
                f32::from(target.size.height),
            );
            let (center_x, center_y) = (
                (block.x + block.w * 0.5) * 2.0,
                (block.y + block.h * 0.5) * 2.0,
            );
            assert!(
                w >= DOOR_SIZE && h >= DOOR_SIZE,
                "block {index} has a usable keyboard/pointer door: {target:?}"
            );
            assert!(
                center_x >= x && center_x <= x + w && center_y >= y && center_y <= y + h,
                "door {index} missed its painted block center: {target:?} / {block:?}"
            );
            assert!(
                x >= 0.0 && y >= 0.0 && x + w <= 480.01 && y + h <= surface_height + 0.01,
                "door {index} escaped the exact berg layout: {target:?}"
            );
        }
    }

    #[test]
    fn a_narrow_or_non_finite_width_never_pushes_blocks_outside_the_berg() {
        let facts = sample();
        for (input, expected) in [
            (f32::NAN, 0.0),
            (f32::INFINITY, 0.0),
            (-20.0, 0.0),
            (0.0, 0.0),
            (8.0, 8.0),
            (19.0, 19.0),
        ] {
            let (placed, height) = place(&facts, input);
            assert!(height.is_finite() && height >= 150.0);
            for (index, block) in placed.iter().enumerate() {
                assert!(
                    block.x.is_finite() && block.w.is_finite() && block.w >= 0.0,
                    "{input}: block {index} has invalid geometry: {block:?}"
                );
                assert!(
                    block.x >= -0.001 && block.x + block.w <= expected + 0.001,
                    "{input}: block {index} leaves the berg: {block:?}"
                );
            }
        }
    }

    #[test]
    fn malformed_dependency_and_parent_edges_are_bounded() {
        let mut facts = sample();
        facts.blocks[0].deps = vec![1, usize::MAX];
        facts.blocks[1].deps = vec![0];
        facts.blocks[0].parent = Some(1);
        facts.blocks[1].parent = Some(0);
        assert_eq!(facts.keel(0), [1]);
        assert_eq!(facts.chain(0), [1, 0]);
        assert!(facts.keel(usize::MAX).is_empty());
        assert!(facts.chain(usize::MAX).is_empty());
        assert_eq!(facts.carried(usize::MAX), 0);
    }

    #[test]
    fn line_totals_saturate_instead_of_wrapping() {
        let mut facts = sample();
        facts.blocks[0].sloc = usize::MAX;
        facts.blocks[1].sloc = 1;
        assert_eq!(facts.below(), usize::MAX);
        assert_eq!(facts.carried(0), usize::MAX);
    }

    #[test]
    fn sparse_layer_numbers_do_not_create_unbounded_blank_layout() {
        let mut facts = sample();
        facts.blocks[4].layer = usize::MAX;
        let (placed, height) = place(&facts, 400.0);
        assert!(height.is_finite() && height < 200.0);
        assert!(placed[4].y < height);
    }

    #[test]
    fn dense_layers_keep_distinct_focus_doors_at_mobile_widths_and_double_text() {
        let label = "資料🛰️-überlange-dependency-name-".repeat(8);
        let facts = BergFacts {
            name: "package".into(),
            own: 1,
            blocks: (0..48)
                .map(|index| {
                    let layer = if index < 32 { 0 } else { 1 };
                    block(
                        &format!("{label}-{index}"),
                        100_000 - index,
                        layer,
                        None,
                        &[],
                    )
                })
                .collect(),
            missing: 0,
            basis: None,
        };

        for width in [240.0, 320.0, 390.0, 1440.0] {
            for text_scale in [0.85, 1.0, 1.5, 2.0] {
                let measure = Measure::new(
                    px(width),
                    &Facet {
                        text_scale,
                        ..Facet::default()
                    },
                );
                let scale = measure.scale();
                let (placed, height) = place_at_scale(&facts, width / scale, scale);
                let targets = doors(&facts, &measure);
                let content_height = (height + 26.0) * scale;
                assert_eq!(targets.len(), facts.blocks.len());

                for (index, (target, block)) in targets.iter().zip(&placed).enumerate() {
                    let (x, y, w, h) = (
                        f32::from(target.origin.x),
                        f32::from(target.origin.y),
                        f32::from(target.size.width),
                        f32::from(target.size.height),
                    );
                    let (center_x, center_y) = (
                        (block.x + block.w * 0.5) * scale,
                        (block.y + block.h * 0.5) * scale,
                    );
                    assert!(
                        w >= DOOR_SIZE && h >= DOOR_SIZE,
                        "{width}/{text_scale}: door {index} is too small: {target:?}"
                    );
                    assert!(
                        center_x >= x && center_x <= x + w && center_y >= y && center_y <= y + h,
                        "{width}/{text_scale}: door {index} misses its stone: {target:?} / {block:?}"
                    );
                    assert!(
                        x >= 0.0
                            && y >= 0.0
                            && x + w <= width + 0.01
                            && y + h <= content_height + 0.01,
                        "{width}/{text_scale}: door {index} leaves the measured berg: {target:?}"
                    );
                    for (other_index, other) in targets.iter().enumerate().skip(index + 1) {
                        assert!(
                            !target.intersects(other),
                            "{width}/{text_scale}: dense focus doors {index} and {other_index} overlap: {target:?} / {other:?}"
                        );
                    }
                }
            }
        }
    }
}
