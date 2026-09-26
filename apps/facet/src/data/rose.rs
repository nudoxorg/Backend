//! The rose: a symbol and everything it touches, in four directions — up
//! what it **is**, down what it is **made of**, left where it comes
//! **from**, right where it **goes**.
//!
//! Calm at rest (`v4/shots/SymbolPage.png`): strands in ink4, names in ink2,
//! kind glyphs faint, no boxes around the names. Resting on a direction — a
//! strand, or any of its members — lights that direction alone in its hue
//! and names it ("to") beside the hub; resting on a member also opens its
//! peek, resting on a strand the direction's lens.
//!
//! Parametric like the board's `rose(W, hover)`: the geometry is a pure
//! function of the width the rose gets (its effective width, so 200 % text
//! behaves like half the window), stepping down smoothly as the room
//! shrinks; at 560 effective px or less it becomes four quiet lines
//! (`word  name, name, name`), one per direction. On first sight the spokes
//! grow out of the hub, a direction at a time.

use super::compass::Dir;
use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::spell::{Seg, spell};
use super::strands::{self, Strand, Voice};
use super::text::{Shaped, shape};
use crate::icons::{Kind, Stroke, variant_path};
use crate::measure::Measure;
use crate::motion::{Spec, spec};
use crate::paint::geom::{Fill, Pt, pt};
use crate::paint::gem;
use crate::theme::ActiveFacet;
use crate::tokens::{TypeRole, motion as dur, ty};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Point, SharedString, Style,
    Styled, TransformationMatrix, Window, div, point, px, size,
};
use std::rc::Rc;

/// One thing the symbol touches.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    /// Its name as the page spells it.
    pub name: SharedString,
    /// Its kind (the glyph before the name).
    pub kind: Kind,
    /// It arrives through a blanket or auto impl (a dashed strand).
    pub via: bool,
    /// A value in flight right now (dashes marching on the pulse).
    pub flow: bool,
}

impl Member {
    /// A member written directly.
    #[must_use]
    pub fn new(name: impl Into<SharedString>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            kind,
            via: false,
            flow: false,
        }
    }

    /// Arrives through a blanket or auto impl.
    #[must_use]
    pub const fn via(mut self) -> Self {
        self.via = true;
        self
    }

    /// A value in flight right now: its strand's dashes march.
    #[must_use]
    pub const fn flow(mut self) -> Self {
        self.flow = true;
        self
    }
}

/// Members drawn per direction; the rest wait in the direction's lens.
pub const SHOWN: usize = 3;
/// At or below this effective width the rose is four lines.
pub const LIST_BELOW: f32 = 560.0;

/// The rose. Build with [`rose`].
pub struct Rose {
    id: ElementId,
    hub: Kind,
    members: [Rc<[Member]>; 4],
    measure: Measure,
    door: Option<Door>,
    member_door: Option<Door>,
    rest: Option<usize>,
    list: Option<bool>,
}

/// A rose around a `hub` of that kind, for the width `measure` gives it.
#[must_use]
pub fn rose(id: impl Into<ElementId>, hub: Kind, measure: &Measure) -> Rose {
    Rose {
        id: id.into(),
        hub,
        members: [Rc::from([]), Rc::from([]), Rc::from([]), Rc::from([])],
        measure: *measure,
        door: None,
        member_door: None,
        rest: None,
        list: None,
    }
}

impl Rose {
    /// The members in one direction (the first [`SHOWN`] are drawn).
    #[must_use]
    pub fn members(mut self, dir: Dir, members: impl Into<Rc<[Member]>>) -> Self {
        self.members[dir.index()] = members.into();
        self
    }

    /// Directions open through `door` (part = [`Dir::index`]): its lens.
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Members open through `door` (part = [`Rose::member_part`]): the peek.
    #[must_use]
    pub fn member_door(mut self, door: Door) -> Self {
        self.member_door = Some(door);
        self
    }

    /// Shows a part as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, part: Option<usize>) -> Self {
        self.rest = part;
        self
    }

    /// Forces the list (true) or the drawing (false).
    #[must_use]
    pub const fn list(mut self, list: bool) -> Self {
        self.list = Some(list);
        self
    }

    /// The part of member `i` in `dir`: directions first, then members in
    /// direction order.
    #[must_use]
    pub fn member_part(members: &[Rc<[Member]>; 4], dir: Dir, i: usize) -> usize {
        4 + members[..dir.index()].iter().map(|m| m.len()).sum::<usize>() + i
    }

    /// The `(direction, member)` a part names (`None` member = the direction).
    #[must_use]
    pub fn part(members: &[Rc<[Member]>; 4], part: usize) -> Option<(Dir, Option<usize>)> {
        if part < 4 {
            return Dir::of(part).map(|d| (d, None));
        }
        let mut at = part - 4;
        for dir in Dir::ALL {
            let n = members[dir.index()].len();
            if at < n {
                return Some((dir, Some(at)));
            }
            at -= n;
        }
        None
    }
}

/// Where the rose's pieces sit for a drawing `w` design px wide (300 tall);
/// multiply by the text scale to paint.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// The hub.
    pub hub: Pt,
    /// Each drawn member: `(dir, index, centre)`.
    pub nodes: Vec<(Dir, usize, Pt)>,
}

/// The board's parametric layout (`c_symbol.py rose`) for `counts` members
/// per direction (clamped to [`SHOWN`]).
#[must_use]
pub fn geometry(w: f32, counts: [usize; 4]) -> Geometry {
    let w = w.clamp(300.0, 760.0);
    let h = 300.0;
    let (cx, cy) = (w * 0.5, 150.0);
    let spread = (w / 4.0).min(150.0);
    let (left, right) = (70.0, w - 70.0);
    let mut nodes = Vec::new();
    for dir in Dir::ALL {
        let n = counts[dir.index()].min(SHOWN);
        let at: Vec<Pt> = match (dir, n) {
            (_, 0) => Vec::new(),
            (Dir::Is, 1) => vec![pt(cx, 34.0)],
            (Dir::Is, 2) => vec![pt(cx - spread, 34.0), pt(cx + spread, 34.0)],
            (Dir::Is, _) => vec![pt(cx - spread, 34.0), pt(cx, 22.0), pt(cx + spread, 34.0)],
            (Dir::MadeOf, 1) => vec![pt(cx, 284.0)],
            (Dir::MadeOf, 2) => vec![pt(cx - spread, 272.0), pt(cx + spread, 272.0)],
            (Dir::MadeOf, _) => vec![pt(cx - spread, 272.0), pt(cx, 284.0), pt(cx + spread, 272.0)],
            (Dir::From, 1) => vec![pt(left, 150.0)],
            (Dir::From, 2) => vec![pt(left, 124.0), pt(left, 176.0)],
            (Dir::From, _) => vec![pt(left, 104.0), pt(left - 4.0, 150.0), pt(left + 6.0, 196.0)],
            (Dir::To, 1) => vec![pt(right, 150.0)],
            (Dir::To, 2) => vec![pt(right, 124.0), pt(right, 176.0)],
            (Dir::To, _) => vec![pt(right, 104.0), pt(right + 4.0, 150.0), pt(right - 10.0, 196.0)],
        };
        nodes.extend(at.into_iter().enumerate().map(|(i, p)| (dir, i, p)));
    }
    Geometry {
        w,
        h,
        hub: pt(cx, cy),
        nodes,
    }
}

/// The strand from the hub to a node (design px).
#[must_use]
pub fn strand(dir: Dir, hub: Pt, node: Pt) -> Strand {
    let (cx, cy, x, y) = (hub.x, hub.y, node.x, node.y);
    match dir {
        Dir::Is => Strand {
            from: pt(cx, cy - 24.0),
            c0: pt(cx, cy - 70.0),
            c1: pt(x, y + 50.0),
            to: pt(x, y + 12.0),
        },
        Dir::MadeOf => Strand {
            from: pt(cx, cy + 24.0),
            c0: pt(cx, cy + 70.0),
            c1: pt(x, y - 50.0),
            to: pt(x, y - 12.0),
        },
        Dir::From => Strand {
            from: pt(cx - 24.0, cy),
            c0: pt(cx - 80.0, cy),
            c1: pt(x + 110.0, y),
            to: pt(x + 64.0, y),
        },
        Dir::To => Strand {
            from: pt(cx + 24.0, cy),
            c0: pt(cx + 80.0, cy),
            c1: pt(x - 110.0, y),
            to: pt(x - 64.0, y),
        },
    }
}

const NAME: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.0,
    line: 16.0,
    ..ty::MONO_ROW
};
const AXIS: TypeRole = TypeRole {
    size: 12.5,
    line: 16.0,
    ..ty::CAPTION
};
const WORD: TypeRole = TypeRole {
    size: 13.5,
    line: 20.0,
    ..ty::CAPTION
};
const NAMES: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.5,
    line: 20.0,
    ..ty::MONO_ROW
};

impl IntoElement for Rose {
    type Element = AnyElement;

    fn into_element(self) -> AnyElement {
        let list = self.list.unwrap_or(self.measure.effective() <= LIST_BELOW);
        if list {
            RoseList { rose: self }.into_any_element()
        } else {
            RoseField { rose: self }.into_any_element()
        }
    }
}

/// Four quiet lines: `is  Display, ToString`; each line a door.
#[derive(IntoElement)]
struct RoseList {
    rose: Rose,
}

impl gpui::RenderOnce for RoseList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let rose = self.rose;
        let palette = cx.palette();
        let m = rose.measure;
        let s = m.scale();
        let mut column = div().flex().flex_col().gap(px(8.0 * s));
        for dir in Dir::ALL {
            let members = &rose.members[dir.index()];
            if members.is_empty() {
                continue;
            }
            let names = members
                .iter()
                .map(|m| m.name.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let mut line = spell(&m)
                .part(
                    dir.index(),
                    Seg::Cell {
                        text: dir.word().into(),
                        role: WORD,
                        color: palette.ink3.into(),
                        width: 64.0,
                        right: false,
                    },
                )
                .part(dir.index(), Seg::Gap(10.0))
                .part_text(dir.index(), names, NAMES, palette.ink1);
            if let Some(door) = &rose.door {
                line = line
                    .id(ElementId::NamedChild(
                        std::sync::Arc::new(rose.id.clone()),
                        SharedString::from(format!("list-{}", dir.index())),
                    ))
                    .door(door.clone())
                    .side(Side::Below);
            }
            column = column.child(line);
        }
        column
    }
}

struct RoseField {
    rose: Rose,
}

impl IntoElement for RoseField {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct RoseLayout {
    live: Entity<Live>,
    keys: AnyElement,
    hub: AnyElement,
    names: Vec<(Dir, usize, Shaped, Shaped)>,
    axis: [Shaped; 4],
    arrive: f32,
}

impl RoseField {
    fn geometry(&self) -> Geometry {
        let counts = [0, 1, 2, 3].map(|i| self.rose.members[i].len());
        geometry(self.rose.measure.effective(), counts)
    }

    /// Design px → window px: centred in the container, times the text scale.
    fn origin(&self, bounds: Bounds<Pixels>, g: &Geometry) -> (f32, f32, f32) {
        let s = self.rose.measure.scale();
        let x = f32::from(bounds.origin.x) + (f32::from(bounds.size.width) - g.w * s) * 0.5;
        (x, f32::from(bounds.origin.y), s)
    }
}

impl Element for RoseField {
    type RequestLayoutState = RoseLayout;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.rose.id.clone())
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
    ) -> (LayoutId, RoseLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let palette = cx.palette();
        let m = self.rose.measure;
        let s = m.scale();
        let g = self.geometry();
        let motion = live.read(cx).motion.clone();
        let arrive = motion.animate_from(
            "arrive",
            0.0,
            1.0,
            Spec::tween(dur::SCENE, dur::GLIDE),
            window,
            cx,
        );
        let offset = (f32::from(m.width()) - g.w * s) * 0.5;
        let mut hub = gem(self.rose.hub)
            .size(40.0 * s)
            .absolute()
            .left(px(offset + (g.hub.x - 20.0) * s))
            .top(px((g.hub.y - 20.0) * s))
            .into_any_element();
        let hub_id = hub.request_layout(window, cx);
        let role = m.role(NAME);
        let names = g
            .nodes
            .iter()
            .filter_map(|(dir, i, _)| {
                let member = self.rose.members[dir.index()].get(*i)?;
                Some((
                    *dir,
                    *i,
                    shape(member.name.clone(), role, palette.ink2.into(), window),
                    shape(member.name.clone(), role, palette.ink0.into(), window),
                ))
            })
            .collect();
        let axis = Dir::ALL.map(|d| shape(d.word(), m.role(AXIS), palette.ink3.into(), window));
        let mut style = Style::default();
        style.size.width = px(f32::from(m.width())).into();
        style.size.height = px(g.h * s).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [kid, hub_id], cx),
            RoseLayout {
                live,
                keys,
                hub,
                names,
                axis,
                arrive,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut RoseLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        layout.hub.prepaint(window, cx);
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    #[allow(clippy::too_many_lines)]
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut RoseLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let g = self.geometry();
        let (ox, oy, s) = self.origin(bounds, &g);
        let at = |p: Pt| pt(ox + p.x * s, oy + p.y * s);
        let members = self.rose.members.clone();
        let live = layout.live.clone();
        let motion = live.read(cx).motion.clone();
        let hover = live.read(cx).hover.or(self.rose.rest);
        let walk = live::walking(&live, window, cx);
        let lit_part = hover.or(walk);
        let lit = lit_part.and_then(|p| Rose::part(&members, p)).map(|(d, _)| d);
        // Flowing strands march on the leased pulse (≤ 12 fps), only while
        // one is drawn.
        let march = if members.iter().any(|m| m.iter().any(|m| m.flow)) {
            crate::motion::pulse::lease(window, cx).phase(1.4)
        } else {
            0.0
        };
        // Each direction's light eases in and out on its own track.
        let light: [f32; 4] = Dir::ALL.map(|d| {
            motion.animate(
                ("light", d.index()),
                if lit == Some(d) { 1.0 } else { 0.0 },
                spec::REVEAL,
                window,
                cx,
            )
        });

        // Strands: quiet ones in one batch; each lit direction in its hue.
        let mut quiet = Fill::new();
        for dir in Dir::ALL {
            #[allow(clippy::cast_precision_loss)]
            let reveal = ((layout.arrive - dir.index() as f32 * 0.1) / 0.7).clamp(0.0, 1.0);
            let l = light[dir.index()];
            let mut hot = Fill::new();
            for (d, i, node) in g.nodes.iter().filter(|(d, _, _)| *d == dir) {
                let member = &members[d.index()][*i];
                let design = strand(*d, g.hub, *node);
                let scaled = Strand {
                    from: at(design.from),
                    c0: at(design.c0),
                    c1: at(design.c1),
                    to: at(design.to),
                };
                let voice = if member.flow {
                    Voice::Flow
                } else if member.via {
                    Voice::Via
                } else {
                    Voice::Written
                };
                // The flow marches away from the hub: two periods per cycle.
                let phase = if member.flow { march * 2.0 } else { 0.0 };
                if l > 0.01 {
                    strands::stroke(&mut hot, &scaled, (1.0 + 0.4 * l) * s, reveal, voice, phase, s);
                } else {
                    strands::stroke(&mut quiet, &scaled, s, reveal, voice, phase, s);
                }
            }
            if l > 0.01 {
                let ink = crate::paint::mix(Hsla::from(palette.ink4).opacity(0.7), dir.color(palette), l);
                hot.paint(window, ink);
            }
        }
        quiet.paint(window, Hsla::from(palette.ink4).opacity(0.7));

        layout.hub.paint(window, cx);

        // Members: a faint kind glyph and the name, centred on the node.
        let mut rects: Vec<(usize, Bounds<Pixels>)> = Vec::new();
        for ((dir, i, node), (_, _, quiet_name, lit_name)) in g.nodes.iter().zip(layout.names.iter()) {
            let member = &members[dir.index()][*i];
            let l = light[dir.index()];
            let name = if l > 0.5 { lit_name } else { quiet_name };
            let glyph = 14.0 * s;
            let gap = 6.0 * s;
            let width = glyph + gap + name.width();
            let c = at(*node);
            let left = c.x - width * 0.5;
            let rect = Bounds::new(
                point(px(left - 4.0 * s), px(c.y - 10.0 * s)),
                size(px(width + 8.0 * s), px(20.0 * s)),
            );
            let part = Rose::member_part(&members, *dir, *i);
            rects.push((part, rect));
            let glyph_ink = member.kind.hue(palette).opacity(0.7 + 0.3 * l);
            window
                .paint_svg(
                    Bounds::new(point(px(left), px(c.y - glyph * 0.5)), size(px(glyph), px(glyph))),
                    variant_path(member.kind.path(), Stroke::width(1.8)),
                    None,
                    TransformationMatrix::unit(),
                    glyph_ink,
                    cx,
                )
                .ok();
            name.paint(left + glyph + gap, c.y + name.role.size * 0.36, window, cx);
            if walk == Some(part) {
                let mut light = Fill::new();
                light.poly(&crate::paint::geom::Poly::rect(
                    left + glyph + gap,
                    c.y + 9.0 * s,
                    name.width(),
                    2.0 * s,
                ));
                light.paint(window, Hsla::from(palette.peri_hi));
            }
        }

        // The lit direction's word beside the hub.
        if let Some(dir) = lit {
            let word = &layout.axis[dir.index()];
            let (dx, dy) = match dir {
                Dir::Is => (0.0, -60.0),
                Dir::MadeOf => (0.0, 60.0),
                Dir::From => (-90.0, -22.0),
                Dir::To => (90.0, -22.0),
            };
            let c = at(pt(g.hub.x + dx, g.hub.y + dy));
            word.paint_centered(c.x, c.y + word.role.size * 0.36, window, cx);
        }

        // Doors: the members' rects, else the nearest strand's direction.
        let hit_rects = rects.clone();
        let design_strands: Vec<(Dir, Strand)> = g
            .nodes
            .iter()
            .map(|(d, _, node)| {
                let st = strand(*d, g.hub, *node);
                (
                    *d,
                    Strand {
                        from: at(st.from),
                        c0: at(st.c0),
                        c1: at(st.c1),
                        to: at(st.to),
                    },
                )
            })
            .collect();
        let hub = at(g.hub);
        let door = merged_door(self.rose.door.clone(), self.rose.member_door.clone(), members.clone());
        let order: Vec<usize> = (0..4)
            .filter(|d| !members[*d].is_empty())
            .chain(rects.iter().map(|(p, _)| *p))
            .collect();
        let count = Rose::member_part(&members, Dir::To, members[3].len());
        let anchor_rects = rects;
        let anchor_strands = design_strands.clone();
        live::paint(
            Hooks {
                mark: self.rose.id.clone(),
                live,
                door,
                side: Side::Below,
                count,
                hit: Rc::new(move |p: Point<Pixels>| {
                    if let Some((part, _)) = hit_rects.iter().find(|(_, r)| r.contains(&p)) {
                        return Some(*part);
                    }
                    let q = pt(f32::from(p.x), f32::from(p.y));
                    if ((q.x - hub.x).powi(2) + (q.y - hub.y).powi(2)).sqrt() < 22.0 * s {
                        return None;
                    }
                    design_strands
                        .iter()
                        .map(|(d, st)| (d.index(), st.distance(q)))
                        .filter(|(_, dist)| *dist <= 9.0 * s)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(d, _)| d)
                }),
                anchor: Rc::new(move |part| {
                    if let Some((_, r)) = anchor_rects.iter().find(|(p, _)| *p == part) {
                        return Some(*r);
                    }
                    // A direction anchors on the bounds of its strands' ends.
                    let ends: Vec<Pt> = anchor_strands
                        .iter()
                        .filter(|(d, _)| d.index() == part)
                        .flat_map(|(_, st)| [st.to, st.at(0.5)])
                        .collect();
                    let (x0, y0) = ends.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.x), a.1.min(p.y)));
                    let (x1, y1) = ends.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.x), a.1.max(p.y)));
                    (!ends.is_empty()).then(|| {
                        Bounds::new(point(px(x0), px(y0)), size(px((x1 - x0).max(2.0)), px((y1 - y0).max(2.0))))
                    })
                }),
                step: Rc::new(move |current, key, _| {
                    let at = current.and_then(|c| order.iter().position(|p| *p == c));
                    live::linear(at, key, order.len()).map(|k| order[k])
                }),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}

/// Direction and member doors behind one [`Door`].
fn merged_door(direction: Option<Door>, member: Option<Door>, members: [Rc<[Member]>; 4]) -> Option<Door> {
    if direction.is_none() && member.is_none() {
        return None;
    }
    let opens = member.as_ref().or(direction.as_ref()).map(Door::opens);
    let build = move |part: usize, m: &Measure, w: &mut Window, cx: &mut App| -> AnyElement {
        match Rose::part(&members, part) {
            Some((d, None)) => direction.as_ref().map(|door| door.build(d.index(), m, w, cx)),
            Some((_, Some(_))) => member.as_ref().map(|door| door.build(part, m, w, cx)),
            None => None,
        }
        .unwrap_or_else(|| IntoElement::into_any_element(gpui::Empty))
    };
    Some(match opens {
        Some(super::door::Opens::Tip) => Door::tip(build),
        Some(super::door::Opens::Lens) => Door::lens(build),
        _ => Door::peek(build),
    })
}

#[cfg(test)]
mod tests {
    use super::{Dir, Member, Rose, SHOWN, geometry, strand};
    use crate::icons::Kind;
    use std::rc::Rc;

    #[test]
    fn the_board_layout_at_760_is_the_targets() {
        let g = geometry(760.0, [2, 3, 3, 3]);
        assert_eq!(g.hub.x, 380.0);
        let to: Vec<_> = g.nodes.iter().filter(|(d, _, _)| *d == Dir::To).map(|(_, _, p)| (p.x, p.y)).collect();
        assert_eq!(to, vec![(690.0, 104.0), (694.0, 150.0), (680.0, 196.0)]);
        let is: Vec<_> = g.nodes.iter().filter(|(d, _, _)| *d == Dir::Is).map(|(_, _, p)| (p.x, p.y)).collect();
        assert_eq!(is, vec![(230.0, 34.0), (530.0, 34.0)]);
        // Strands leave the hub on its own side and end short of the name.
        let s = strand(Dir::From, g.hub, g.nodes[2].2);
        assert!(s.from.x < g.hub.x && s.to.x > g.nodes[2].2.x);
    }

    #[test]
    fn narrower_roses_pull_their_members_in_and_never_overlap_the_hub() {
        for w in [300.0, 420.0, 560.0, 680.0, 760.0, 1200.0] {
            let g = geometry(w, [3, 3, 3, 3]);
            assert!(g.w <= 760.0 && g.w >= 300.0);
            for (_, _, p) in &g.nodes {
                assert!(p.x >= 0.0 && p.x <= g.w, "{w}: {p:?}");
                let d = ((p.x - g.hub.x).powi(2) + (p.y - g.hub.y).powi(2)).sqrt();
                assert!(d > 60.0, "{w}: a member sits on the hub");
            }
        }
        assert_eq!(geometry(760.0, [9, 0, 0, 0]).nodes.len(), SHOWN);
    }

    #[test]
    fn parts_name_directions_then_members() {
        let members: [Rc<[Member]>; 4] = [
            Rc::from(vec![Member::new("Display", Kind::Trait)]),
            Rc::from(vec![]),
            Rc::from(vec![Member::new("a", Kind::Function), Member::new("b", Kind::Function)]),
            Rc::from(vec![Member::new("c", Kind::Method)]),
        ];
        assert_eq!(Rose::part(&members, 2), Some((Dir::From, None)));
        assert_eq!(Rose::part(&members, 4), Some((Dir::Is, Some(0))));
        assert_eq!(Rose::part(&members, 5), Some((Dir::From, Some(0))));
        assert_eq!(Rose::part(&members, 7), Some((Dir::To, Some(0))));
        assert_eq!(Rose::part(&members, 8), None);
        assert_eq!(Rose::member_part(&members, Dir::To, 0), 7);
    }
}
