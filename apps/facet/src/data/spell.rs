//! Spell: a line of words and little marks painted as one element.
//!
//! The facts line under a hero, a compass row, a lens's figures and legend,
//! a legend under a map: each is a handful of runs in different faces (serif
//! words, mono numbers), small drawn marks (direction diamonds, separators,
//! a family line, an icon, a compass) and gaps. They wrap only at the break
//! points the builder marks, soft separators vanish at a line's edges, and
//! every run tagged with a part is a door: resting on it underlines it in
//! periwinkle and reports it.

use super::compass::{CompassSize, Directions, paint_arms};
use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::text::{Shaped, shape};
use crate::measure::Measure;
use crate::paint::geom::{Fill, Poly, pt};
use crate::paint::hatch::Hatch;
use crate::theme::ActiveFacet;
use crate::tokens::TypeRole;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Size, Style, Window,
    TransformationMatrix, point, px, size,
};
use std::cell::RefCell;
use std::rc::Rc;

/// One piece of a spelled line.
#[derive(Clone, Debug)]
pub enum Seg {
    /// Words in a role (resolved against the line's measure) and colour.
    Text {
        /// The words.
        text: SharedString,
        /// The type role, at 100 %.
        role: TypeRole,
        /// The ink.
        color: Hsla,
    },
    /// A rotated square: a direction mark (outlined) or a legend chip (filled).
    Diamond {
        /// Edge length before rotation, px at 100 %.
        size: f32,
        /// The colour.
        color: Hsla,
        /// Filled, or outlined at 1.2 px.
        filled: bool,
    },
    /// A cut square chip (a legend's stone).
    Chip {
        /// Edge, px at 100 %.
        size: f32,
        /// The colour.
        color: Hsla,
        /// Hatched instead of filled (private or gated).
        hatched: bool,
    },
    /// Words in a fixed-width cell (a lens's name column, a right-aligned
    /// count); cut with an ellipsis when they do not fit.
    Cell {
        /// The words.
        text: SharedString,
        /// The type role, at 100 %.
        role: TypeRole,
        /// The ink.
        color: Hsla,
        /// The cell's width, px at 100 %.
        width: f32,
        /// Right-aligned (counts), else left.
        right: bool,
    },
    /// Horizontal space, px at 100 %.
    Gap(f32),
    /// A monochrome SVG mark (an icon, a kind glyph), px at 100 %.
    Icon {
        /// Asset path.
        path: SharedString,
        /// Edge, px at 100 %.
        size: f32,
        /// Tint.
        color: Hsla,
    },
    /// A compass mark.
    Compass {
        /// The counts.
        dirs: Directions,
        /// Its size.
        size: CompassSize,
    },
    /// A thin proportional bar of family segments (the facts line's
    /// "what it is made of"), `(weight, colour, hatched)`.
    Line {
        /// Segments.
        parts: Vec<(f32, Hsla, bool)>,
        /// Width, px at 100 %.
        width: f32,
        /// Height, px at 100 %.
        height: f32,
    },
    /// The facts line's quiet separator: a 4 px ink4 diamond with 16 px of
    /// air each side. Hidden at the start or end of a wrapped line.
    Sep,
    /// A place the line may wrap.
    Break,
}

/// A spelled line (or lines, where it wraps). Build with [`spell`].
pub struct Spell {
    id: Option<ElementId>,
    segs: Vec<(Seg, Option<usize>)>,
    measure: Measure,
    door: Option<Door>,
    side: Side,
    hover: Option<usize>,
    row_gap: f32,
    wrap: bool,
}

/// An empty line for `measure`.
#[must_use]
pub fn spell(measure: &Measure) -> Spell {
    Spell {
        id: None,
        segs: Vec::new(),
        measure: *measure,
        door: None,
        side: Side::Below,
        hover: None,
        row_gap: 6.0,
        wrap: true,
    }
}

impl Spell {
    /// Keys the line's hover state (needed for doors).
    #[must_use]
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Appends a segment outside any part.
    #[must_use]
    pub fn seg(mut self, seg: Seg) -> Self {
        self.segs.push((seg, None));
        self
    }

    /// Appends a segment belonging to part `part`.
    #[must_use]
    pub fn part(mut self, part: usize, seg: Seg) -> Self {
        self.segs.push((seg, Some(part)));
        self
    }

    /// Appends words.
    #[must_use]
    pub fn text(self, text: impl Into<SharedString>, role: TypeRole, color: impl Into<Hsla>) -> Self {
        self.seg(Seg::Text {
            text: text.into(),
            role,
            color: color.into(),
        })
    }

    /// Appends words belonging to `part`.
    #[must_use]
    pub fn part_text(
        self,
        part: usize,
        text: impl Into<SharedString>,
        role: TypeRole,
        color: impl Into<Hsla>,
    ) -> Self {
        self.part(
            part,
            Seg::Text {
                text: text.into(),
                role,
                color: color.into(),
            },
        )
    }

    /// Appends horizontal space.
    #[must_use]
    pub fn gap(self, px: f32) -> Self {
        self.seg(Seg::Gap(px))
    }

    /// Parts open through `door`.
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Where parts prefer to open.
    #[must_use]
    pub const fn side(mut self, side: Side) -> Self {
        self.side = side;
        self
    }

    /// Shows `part` as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, part: Option<usize>) -> Self {
        self.hover = part;
        self
    }

    /// Never wraps (clips instead).
    #[must_use]
    pub const fn nowrap(mut self) -> Self {
        self.wrap = false;
        self
    }

    /// Vertical gap between wrapped lines, px at 100 %.
    #[must_use]
    pub const fn row_gap(mut self, gap: f32) -> Self {
        self.row_gap = gap;
        self
    }
}

impl IntoElement for Spell {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// A segment ready to place: its width, its vertical extent, and what paints it.
struct Placed {
    seg: Seg,
    part: Option<usize>,
    shaped: Option<Shaped>,
    width: f32,
    ascent: f32,
    descent: f32,
}

/// Where each placed segment landed, relative to the element's origin.
#[derive(Default)]
struct Flow {
    /// `(segment index, x, line index)`; hidden separators are absent.
    at: Vec<(usize, f32, usize)>,
    /// Each line's `(top, baseline, height)`.
    lines: Vec<(f32, f32, f32)>,
    width: f32,
    height: f32,
}

fn flow(placed: &[Placed], max: Option<f32>, row_gap: f32) -> Flow {
    // Units: runs between breaks. A unit never splits.
    let mut units: Vec<(usize, usize, f32)> = Vec::new();
    let mut start = 0;
    let mut width = 0.0;
    for (i, p) in placed.iter().enumerate() {
        if matches!(p.seg, Seg::Break) {
            if i > start {
                units.push((start, i, width));
            }
            start = i + 1;
            width = 0.0;
        } else {
            width += p.width;
        }
    }
    if start < placed.len() {
        units.push((start, placed.len(), width));
    }
    let is_sep = |i: usize| matches!(placed[i].seg, Seg::Sep);
    let unit_width = |(a, b, w): (usize, usize, f32), leading: bool| {
        // A separator at a unit's start is dropped when the unit opens a line.
        if leading && a < b && is_sep(a) { w - placed[a].width } else { w }
    };
    let mut lines: Vec<Vec<(usize, usize)>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for unit in units {
        let first = lines.last().is_none_or(Vec::is_empty);
        let w = unit_width(unit, first);
        if !first
            && let Some(max) = max
            && used + w > max + 0.5
        {
            lines.push(Vec::new());
            used = unit_width(unit, true);
        } else {
            used += w;
        }
        lines.last_mut().map(|line| line.push((unit.0, unit.1)));
    }
    let mut out = Flow::default();
    let mut top = 0.0_f32;
    for (li, line) in lines.iter().enumerate() {
        let mut ascent = 0.0_f32;
        let mut descent = 0.0_f32;
        let mut x = 0.0_f32;
        let mut first = true;
        let mut items = Vec::new();
        for &(a, b) in line {
            for i in a..b {
                if first && is_sep(i) {
                    continue;
                }
                first = false;
                items.push((i, x));
                x += placed[i].width;
                ascent = ascent.max(placed[i].ascent);
                descent = descent.max(placed[i].descent);
            }
        }
        // A trailing separator is dropped too.
        while items.last().is_some_and(|&(i, _)| is_sep(i)) {
            if let Some((i, _)) = items.pop() {
                x -= placed[i].width;
            }
        }
        let height = ascent + descent;
        for (i, at) in items {
            out.at.push((i, at, li));
        }
        out.lines.push((top, top + ascent, height));
        out.width = out.width.max(x);
        top += height + if li + 1 < lines.len() { row_gap } else { 0.0 };
    }
    out.height = top;
    out
}

#[doc(hidden)]
pub struct SpellLayout {
    placed: Vec<Placed>,
    flow: Rc<RefCell<Flow>>,
    live: Option<Entity<Live>>,
    keys: Option<AnyElement>,
    leaf: LayoutId,
}

#[doc(hidden)]
pub struct SpellPrepaint {
    hitbox: Option<Hitbox>,
    rects: Rc<Vec<(usize, Bounds<Pixels>)>>,
}

impl Element for Spell {
    type RequestLayoutState = SpellLayout;
    type PrepaintState = SpellPrepaint;

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
    ) -> (LayoutId, SpellLayout) {
        let scale = self.measure.scale();
        let live = self.id.is_some().then(|| live::live(window, cx));
        let base = self.measure.role(crate::tokens::ty::SMALL);
        let mark_extent = |edge: f32| (edge * 0.5 + 0.5, edge * 0.5 - 0.5);
        let placed: Vec<Placed> = self
            .segs
            .iter()
            .map(|(seg, part)| {
                let (shaped, width, ascent, descent) = match seg {
                    Seg::Text { text, role, color } => {
                        let shaped = shape(text.clone(), self.measure.role(*role), *color, window);
                        let (w, a, d) = (shaped.width(), shaped.ascent(), shaped.descent());
                        (Some(shaped), w, a, d)
                    }
                    Seg::Cell {
                        text,
                        role,
                        color,
                        width,
                        ..
                    } => {
                        let w = width * scale;
                        let shaped = super::text::shape_fit(text, self.measure.role(*role), *color, w, window);
                        let (a, d) = (shaped.ascent(), shaped.descent());
                        (Some(shaped), w, a, d)
                    }
                    Seg::Diamond { size, .. } => {
                        let edge = size * std::f32::consts::SQRT_2 * scale;
                        let (a, d) = mark_extent(base.size * 0.7);
                        (None, edge, a.max(edge * 0.5), d.max(edge * 0.5 - 1.0))
                    }
                    Seg::Chip { size, .. } | Seg::Icon { size, .. } => {
                        let edge = size * scale;
                        (None, edge, edge * 0.5 + base.size * 0.32, edge * 0.5 - base.size * 0.32)
                    }
                    Seg::Gap(w) => (None, w * scale, 0.0, 0.0),
                    Seg::Compass { size, .. } => {
                        let edge = size.edge() * scale;
                        (None, edge, edge * 0.5 + base.size * 0.32, edge * 0.5 - base.size * 0.32)
                    }
                    Seg::Line { width, height, .. } => {
                        let h = height * scale;
                        (None, width * scale, h * 0.5 + base.size * 0.32, h * 0.5 - base.size * 0.32)
                    }
                    Seg::Sep => (None, 36.0 * scale * self.measure.density().space(), 0.0, 0.0),
                    Seg::Break => (None, 0.0, 0.0, 0.0),
                };
                Placed {
                    seg: seg.clone(),
                    part: *part,
                    shaped,
                    width,
                    ascent,
                    descent,
                }
            })
            .collect();
        let shared = Rc::new(RefCell::new(Flow::default()));
        let cell = shared.clone();
        let wrap = self.wrap;
        let row_gap = self.row_gap * scale;
        // The measure closure needs the widths only; copy them out.
        let sizes: Vec<(Seg, f32, f32, f32)> = placed
            .iter()
            .map(|p| (p.seg.clone(), p.width, p.ascent, p.descent))
            .collect();
        let leaf = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let max = match (known.width, available.width) {
                (Some(w), _) => Some(f32::from(w)),
                (None, AvailableSpace::Definite(w)) if wrap => Some(f32::from(w)),
                _ => None,
            };
            let proxies: Vec<Placed> = sizes
                .iter()
                .map(|(seg, width, ascent, descent)| Placed {
                    seg: seg.clone(),
                    part: None,
                    shaped: None,
                    width: *width,
                    ascent: *ascent,
                    descent: *descent,
                })
                .collect();
            let laid = flow(&proxies, if wrap { max } else { None }, row_gap);
            let out = Size {
                width: px(laid.width),
                height: px(laid.height),
            };
            *cell.borrow_mut() = laid;
            out
        });
        // The words are a measured leaf; the focus child (when the line has
        // doors) sits over it, absolutely.
        let (keys, mut kids) = live::keys_for(live.as_ref(), window, cx);
        kids.insert(0, leaf);
        let mut outer = Style::default();
        outer.display = gpui::Display::Flex;
        outer.min_size.width = px(0.0).into();
        let layout_id = window.request_layout(outer, kids, cx);
        (
            layout_id,
            SpellLayout {
                placed,
                flow: shared,
                live,
                keys,
                leaf,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut SpellLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> SpellPrepaint {
        let row_gap = self.row_gap * self.measure.scale();
        // Re-flow against the width actually granted (the measure closure may
        // have run for a different probe width).
        let leaf = window.layout_bounds(layout.leaf);
        let laid = flow(&layout.placed, self.wrap.then(|| f32::from(leaf.size.width)), row_gap);
        let origin = bounds.origin;
        let mut rects: Vec<(usize, Bounds<Pixels>)> = Vec::new();
        for &(i, x, line) in &laid.at {
            let Some(part) = layout.placed[i].part else { continue };
            let (top, _, height) = laid.lines[line];
            let rect = Bounds::new(
                point(origin.x + px(x), origin.y + px(top)),
                size(px(layout.placed[i].width), px(height)),
            );
            match rects.iter_mut().find(|(p, r)| *p == part && (r.origin.y - rect.origin.y).abs() < px(0.5)) {
                Some((_, r)) => *r = r.union(&rect),
                None => rects.push((part, rect)),
            }
        }
        *layout.flow.borrow_mut() = laid;
        let hitbox = layout
            .keys
            .as_mut()
            .map(|keys| live::prepaint(bounds, keys, window, cx));
        SpellPrepaint {
            hitbox,
            rects: Rc::new(rects),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut SpellLayout,
        pre: &mut SpellPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let scale = self.measure.scale();
        let (lit, walked) = match &layout.live {
            Some(live) => (
                live.read(cx).hover.or(self.hover),
                live::walking(live, window, cx),
            ),
            None => (self.hover, None),
        };
        let flow = layout.flow.borrow();
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        for &(i, x, line) in &flow.at {
            let p = &layout.placed[i];
            let (top, baseline, height) = flow.lines[line];
            let (x, baseline, top) = (ox + x, oy + baseline, oy + top);
            let mid = baseline - self.measure.role(crate::tokens::ty::SMALL).size * 0.32;
            match &p.seg {
                Seg::Text { color, .. } => {
                    if let Some(shaped) = &p.shaped {
                        let lifted = lit.is_some() && p.part == lit;
                        if lifted {
                            // A rested run reads one ink stronger.
                            let bright = shape(
                                shaped_text(shaped),
                                shaped.role,
                                crate::paint::mix(*color, palette.ink0.into(), 0.45),
                                window,
                            );
                            bright.paint(x, baseline, window, cx);
                        } else {
                            shaped.paint(x, baseline, window, cx);
                        }
                    }
                }
                Seg::Cell { right, .. } => {
                    if let Some(shaped) = &p.shaped {
                        let at = if *right { x + p.width - shaped.width() } else { x };
                        shaped.paint(at, baseline, window, cx);
                    }
                }
                Seg::Diamond { size, color, filled } => {
                    let half = size * std::f32::consts::SQRT_2 * scale * 0.5;
                    let c = pt(x + half, mid);
                    let d = Poly::new([
                        pt(c.x, c.y - half),
                        pt(c.x + half, c.y),
                        pt(c.x, c.y + half),
                        pt(c.x - half, c.y),
                    ]);
                    let mut fill = Fill::new();
                    if *filled {
                        fill.poly(&d);
                    } else {
                        for q in d.offset(-0.6 * scale).stroke_ring(1.2 * scale) {
                            fill.poly(&q);
                        }
                    }
                    fill.paint(window, *color);
                }
                Seg::Chip { size, color, hatched } => {
                    let e = size * scale;
                    let chip = Poly::chamfer(x, mid - e * 0.5, e, e, 2.5 * scale);
                    if *hatched {
                        let frame = Bounds::new(point(px(x), px(mid - e * 0.5)), size_px(e, e));
                        Hatch::vertical(1.5 * scale, 3.5 * scale).paint(window, &chip, frame, *color);
                    } else {
                        let mut fill = Fill::new();
                        fill.poly(&chip);
                        fill.paint(window, *color);
                    }
                }
                Seg::Icon { path, size, color } => {
                    let e = size * scale;
                    let at = Bounds::new(point(px(x), px(mid - e * 0.5)), size_px(e, e));
                    window
                        .paint_svg(at, path.clone(), None, TransformationMatrix::unit(), *color, cx)
                        .ok();
                }
                Seg::Compass { dirs, size } => {
                    let e = size.edge() * scale;
                    let at = Bounds::new(point(px(x), px(mid - e * 0.5)), size_px(e, e));
                    paint_arms(window, at, *size, *dirs, scale, None, palette);
                }
                Seg::Line { parts, width, height } => {
                    let (w, h) = (width * scale, height * scale);
                    let total: f32 = parts.iter().map(|p| p.0.max(0.0)).sum::<f32>().max(1e-3);
                    let gaps = (parts.len().saturating_sub(1)) as f32 * scale;
                    let mut cx0 = x;
                    for (weight, color, hatched) in parts {
                        let seg_w = (w - gaps) * weight.max(0.0) / total;
                        let rect = Poly::rect(cx0, mid - h * 0.5, seg_w, h);
                        if *hatched {
                            let frame = Bounds::new(point(px(cx0), px(mid - h * 0.5)), size_px(seg_w, h));
                            Hatch::diagonal(2.0 * scale, 4.0 * scale).paint(window, &rect, frame, *color);
                        } else {
                            let mut fill = Fill::new();
                            fill.poly(&rect);
                            fill.paint(window, *color);
                        }
                        cx0 += seg_w + scale;
                    }
                }
                Seg::Sep => {
                    let half = 2.0 * std::f32::consts::SQRT_2 * scale * 0.5 * 1.0;
                    let c = pt(x + p.width * 0.5, mid);
                    let mut fill = Fill::new();
                    fill.poly(&Poly::new([
                        pt(c.x, c.y - half),
                        pt(c.x + half, c.y),
                        pt(c.x, c.y + half),
                        pt(c.x - half, c.y),
                    ]));
                    fill.paint(window, Hsla::from(palette.ink4));
                }
                Seg::Gap(_) | Seg::Break => {}
            }
            let _ = (top, height);
        }
        // The rested part's periwinkle underline; the walked part's doubled light.
        for (part, rect) in pre.rects.iter() {
            let hot = Some(*part) == lit;
            let walk = Some(*part) == walked;
            if !(hot || walk) {
                continue;
            }
            let weight = if walk { 2.0 } else { 1.0 } * scale.max(1.0);
            let y = f32::from(rect.origin.y + rect.size.height) - weight;
            let line = Poly::rect(f32::from(rect.origin.x), y, f32::from(rect.size.width), weight);
            let mut fill = Fill::new();
            fill.poly(&line);
            fill.paint(
                window,
                Hsla::from(if walk { palette.peri_hi } else { palette.peri.base }),
            );
        }
        drop(flow);
        if let (Some(live), Some(keys), Some(hitbox)) = (&layout.live, layout.keys.as_mut(), &pre.hitbox) {
            let rects = pre.rects.clone();
            let hit_rects = rects.clone();
            let parts: Vec<usize> = {
                let mut all: Vec<usize> = rects.iter().map(|(p, _)| *p).collect();
                all.sort_unstable();
                all.dedup();
                all
            };
            let count = parts.last().map_or(0, |p| p + 1);
            live::paint(
                Hooks {
                    mark: self.id.clone().unwrap_or_else(|| ElementId::Name("spell".into())),
                    live: live.clone(),
                    door: self.door.clone(),
                    side: self.side,
                    count,
                    hit: Rc::new(move |p| {
                        hit_rects
                            .iter()
                            .find(|(_, r)| r.contains(&p))
                            .map(|(i, _)| *i)
                    }),
                    anchor: Rc::new(move |i| rects.iter().find(|(p, _)| *p == i).map(|(_, r)| *r)),
                    step: Rc::new(move |current, key, _| {
                        let at = current.and_then(|c| parts.iter().position(|p| *p == c));
                        live::linear(at, key, parts.len()).map(|k| parts[k])
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

fn shaped_text(shaped: &Shaped) -> SharedString {
    shaped.text()
}

fn size_px(w: f32, h: f32) -> Size<Pixels> {
    size(px(w), px(h))
}
