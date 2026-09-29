//! Every stroke on the page, painted in one pass from this frame's anchors:
//! the spine, the specimen's shape (a bracket's back and rungs, a fork's
//! trunk and tines), Getting one's rails, and each section's mark, rule and
//! stub. Strokes name their tier: solid is compiler-verified, dashed is
//! matched by name, dotted is optional or maybe. A stroke lights (1.5 px,
//! full hue) while its subject is hovered.
//!
//! The ink also records the spine, each rule and each stub as anchors, for
//! the transitions to fold the page into its node.

use super::{Anchors, Geometry, at, hue, section_subject};
use crate::anatomy::plan::{Dir, PagePlan, Part, SectionId, Spec, Tier};
use crate::hover;
use crate::tokens::{Palette, stroke};
use gpui::{AnyElement, Bounds, ColorExt, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled, Window, canvas, point, px, size};
use std::rc::Rc;

/// What the specimen asks the ink to draw.
enum Shape {
    None,
    Bracket(Vec<bool>),
    Fork { shared: usize, open: bool },
    Pipe { ports: usize, drops: usize },
    Socket { rows: Vec<(u8, bool)>, doers: usize },
}

/// The page's ink: an absolute canvas over the page.
#[must_use]
pub fn ink(plan: &PagePlan, geo: Geometry, anchors: &Rc<Anchors>, palette: &Palette) -> AnyElement {
    let anchors = Rc::clone(anchors);
    let kind = hue(plan.hero.fam, palette).hsla();
    let call = palette.f_call.hue.hsla();
    let coral = palette.coral.base.hsla();
    let rule = palette.line2.hsla();
    let ground = palette.g1.hsla();
    let sections = plan.sections.iter().map(|section| (section.id, section.dir, section.tier, section.count.is_some())).collect::<Vec<_>>();
    let shape = match &plan.spec {
        Spec::Record(record) => Shape::Bracket(record.rungs.iter().map(|rung| rung.optional).collect()),
        Spec::Choice(choice) => Shape::Fork { shared: choice.shared.len(), open: choice.open.is_some() },
        Spec::Callable(callable) => Shape::Pipe { ports: callable.ports.len(), drops: callable.drops.len() },
        Spec::Contract(contract) => Shape::Socket {
            rows: contract.write.iter().map(|slot| (0, slot.optional)).chain(contract.other.iter().map(|_| (1, false))).chain(contract.get.iter().map(|_| (2, false))).collect(),
            doers: contract.doers.names.len(),
        },
        Spec::None => Shape::None,
    };
    let maybe = plan.getting.iter().map(|rail| rail.maybe).collect::<Vec<_>>();
    canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let s = geo.scale;
            let x_spine = bounds.origin.x + geo.spine + px(0.5);
            let x_col = bounds.origin.x + geo.col;
            let x_end = x_col + geo.col_w;
            let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
            let pen = Pen { s };
            let gem_bottom = anchors.get(at(SectionId::Spec, Part::Gem)).map(|gem| gem.bottom() + px(2.0));
            let heads = sections
                .iter()
                .filter_map(|(id, dir, tier, counted)| anchors.get(at(*id, Part::Head)).map(|b| (*id, *dir, *tier, *counted, b)))
                .collect::<Vec<_>>();

            // The spine: the node's body, from the gem to the last section.
            if let Some(top) = gem_bottom {
                let bottom = heads.last().map_or(top + px(40.0 * s), |(.., b)| mid(b));
                pen.line(window, point(x_spine, top), point(x_spine, bottom), stroke::HAIR, kind.opacity(0.28), None);
                anchors.record(at(SectionId::Spec, Part::Spine), Bounds::new(point(x_spine - px(0.5), top), size(px(1.0), bottom - top)));
            }

            // The specimen.
            match &shape {
                Shape::None => {}
                Shape::Bracket(optional) => bracket(&pen, window, &anchors, optional, x_spine, x_col, gem_bottom, kind, ground),
                Shape::Fork { shared, open } => fork(&pen, window, &anchors, *shared, *open, x_spine, x_col, gem_bottom, kind, ground),
                Shape::Socket { rows, doers } => socket(&pen, window, &anchors, rows, *doers, x_spine, gem_bottom, kind),
                Shape::Pipe { ports, drops } => {
                    let reach = geo.margins().then(|| x_spine - (geo.reach - px(16.0 * s)).min(geo.col));
                    pipe(&pen, window, &anchors, *ports, *drops, reach, kind, coral);
                }
            }

            // Getting one's rails.
            let rails = anchors.rails();
            if !rails.is_empty() {
                let terminal = anchors.get(at(SectionId::Getting, Part::Terminal)).map_or_else(
                    || rails.iter().map(|(_, b)| b.right()).fold(x_col, Pixels::max) + px(20.0 * s),
                    |t| t.left() + px(20.0 * s),
                );
                let (mut top, mut bottom) = (None::<Pixels>, None::<Pixels>);
                for (n, plate) in &rails {
                    let y = mid(plate);
                    let dotted = maybe.get(*n as usize).copied().unwrap_or(false).then_some(stroke::OPTIONAL);
                    // From what you have to the step.
                    if let Some(from) = anchors.get(at(SectionId::Getting, Part::Row(*n))) {
                        pen.line(window, point(from.right() + px(8.0 * s), y), point(plate.left(), y), stroke::HAIR, kind.opacity(0.55), dotted);
                    }
                    // The step's plate, chamfered, in the maker's hue.
                    let c = 6.0 * s;
                    let (l, r, t, b) = (plate.left(), plate.right(), plate.top(), plate.bottom());
                    let outline = [
                        point(l + px(c), t), point(r, t), point(r, b - px(c)), point(r - px(c), b), point(l, b), point(l, t + px(c)),
                    ];
                    pen.poly(window, &outline, stroke::HAIR, call.opacity(0.8), None);
                    // On to the terminal, with a chevron.
                    pen.line(window, point(r, y), point(terminal - px(2.0), y), stroke::HAIR, kind.opacity(0.55), dotted);
                    let h = px(3.5 * s);
                    pen.path(window, &[point(terminal - px(2.0) - h, y - h), point(terminal - px(2.0), y), point(terminal - px(2.0) - h, y + h)], stroke::HAIR, kind.opacity(0.8));
                    top = Some(top.map_or(y, |top: Pixels| top.min(y)));
                    bottom = Some(bottom.map_or(y, |bottom: Pixels| bottom.max(y)));
                }
                if let (Some(top), Some(bottom)) = (top, bottom) {
                    pen.line(window, point(terminal, top - px(9.0 * s)), point(terminal, bottom + px(9.0 * s)), stroke::RELATION, kind, None);
                }
            }

            // What can go wrong: a coral tree off the spine, a tine per way.
            let branches = anchors.rows(SectionId::Fails);
            if let (Some(head), Some((_, last))) = (anchors.get(at(SectionId::Fails, Part::Head)), branches.last()) {
                let chamfer = px(7.0 * s);
                let top = mid(&head) + px(9.0 * s);
                let end = x_col - px(10.0 * s);
                pen.line(window, point(x_spine, top), point(x_spine, mid(last) - chamfer), stroke::RELATION, coral, None);
                for (_, b) in &branches {
                    let y = mid(b);
                    pen.path(window, &[point(x_spine, y - chamfer), point(x_spine + chamfer, y), point(end, y)], stroke::HAIR, coral);
                }
            }

            // Each section: its mark on the spine, the rule after its
            // heading, and its stub into the margin it relates to.
            for (id, dir, tier, counted, b) in &heads {
                let y = mid(b);
                let color = if *id == SectionId::Fails { coral } else { kind };
                let lit = hover::lit(&section_subject(*id), window, cx).is_lit();
                let r = px(8.0 * s);
                let knock = [point(x_spine - r, y - r), point(x_spine + r, y - r), point(x_spine + r, y + r), point(x_spine - r, y + r)];
                pen.poly(window, &knock, 0.0, ground, Some(ground));
                mark(window, *id, point(x_spine, y), s, color);
                // The rule stops short of a count that sits at the head's end.
                let inline = *counted && !(geo.margins() && *dir != Dir::None);
                let rule_end = if inline {
                    anchors.get(at(*id, Part::Count)).map_or(x_end, |count| count.left() - px(10.0 * s))
                } else {
                    x_end
                };
                let rule_start = b.right() + px(14.0 * s);
                if rule_end > rule_start {
                    pen.line(window, point(rule_start, y), point(rule_end, y), stroke::HAIR, rule, None);
                    anchors.record(at(*id, Part::Rule), Bounds::new(point(rule_start, y - px(0.5)), size(rule_end - rule_start, px(1.0))));
                }
                let dash = (*tier == Tier::Name).then_some(stroke::INFERRED);
                let (width, alpha) = if lit { (stroke::RELATION, 1.0) } else { (stroke::RELATION, 0.55) };
                if geo.margins() {
                    let stub = match dir {
                        Dir::In => Some((x_spine - px(10.0 * s) - (geo.reach - px(16.0 * s)).min(geo.col), x_spine - px(10.0 * s))),
                        Dir::Out => Some((x_end + px(8.0 * s), x_end + geo.reach - px(16.0 * s))),
                        Dir::None => None,
                    };
                    if let Some((from, to)) = stub {
                        pen.line(window, point(from, y), point(to, y), width, color.opacity(alpha), dash);
                        if *dir == Dir::In {
                            // It arrives: a chevron at the spine.
                            let h = px(3.5 * s);
                            pen.path(window, &[point(to - h, y - h), point(to, y), point(to - h, y + h)], width, color.opacity(alpha));
                        }
                        anchors.record(at(*id, Part::Stub), Bounds::new(point(from, y - px(0.75)), size(to - from, px(1.5))));
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Strokes at the page's scale.
struct Pen {
    s: f32,
}

impl Pen {
    fn line(&self, window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, width: f32, color: Hsla, dash: Option<[f32; 2]>) {
        let mut path = PathBuilder::stroke(px(width));
        if let Some([on, off]) = dash {
            path = path.dash_array(&[px(on * self.s), px(off * self.s)]);
        }
        path.move_to(from);
        path.line_to(to);
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }

    fn path(&self, window: &mut Window, points: &[Point<Pixels>], width: f32, color: Hsla) {
        let mut path = PathBuilder::stroke(px(width));
        for (n, p) in points.iter().enumerate() {
            if n == 0 { path.move_to(*p); } else { path.line_to(*p); }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }

    fn poly(&self, window: &mut Window, points: &[Point<Pixels>], width: f32, color: Hsla, fill: Option<Hsla>) {
        if let Some(fill) = fill {
            let mut path = PathBuilder::fill();
            path.add_polygon(points, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, fill);
            }
        }
        if width > 0.0 {
            let mut path = PathBuilder::stroke(px(width));
            path.add_polygon(points, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
}

/// A record's bracket: its back is the spine, one rung per field, a filled
/// square for a field that is always there, hollow and dotted for one that
/// may not be.
#[allow(clippy::too_many_arguments)]
fn bracket(pen: &Pen, window: &mut Window, anchors: &Anchors, optional: &[bool], x_spine: Pixels, x_col: Pixels, gem_bottom: Option<Pixels>, kind: Hsla, ground: Hsla) {
    let s = pen.s;
    let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
    let rows = anchors.rows(SectionId::Spec);
    let (Some((_, first)), Some((_, last))) = (rows.first(), rows.last()) else { return };
    let top = mid(first) - px(14.0 * s);
    let mut bottom = mid(last) + px(14.0 * s);
    if let Some(private) = anchors.get(at(SectionId::Spec, Part::Private)) {
        bottom = mid(&private) + px(8.0 * s);
    }
    let serif = px(7.0 * s);
    pen.path(window, &[point(x_spine + serif, top), point(x_spine, top), point(x_spine, bottom), point(x_spine + serif, bottom)], stroke::RELATION, kind);
    if let Some(gem) = gem_bottom {
        pen.line(window, point(x_spine, gem), point(x_spine, top), stroke::HAIR, kind.opacity(0.45), None);
    }
    for (n, b) in &rows {
        let y = mid(b);
        let opt = optional.get(*n as usize).copied().unwrap_or(false);
        let end = x_col - px(12.0 * s);
        pen.line(window, point(x_spine, y), point(end, y), stroke::HAIR, kind, opt.then_some(stroke::OPTIONAL));
        let sq = px(3.0 * s);
        let square = [point(end - sq, y - sq), point(end + sq, y - sq), point(end + sq, y + sq), point(end - sq, y + sq)];
        pen.poly(window, &square, stroke::HAIR, kind, Some(if opt { ground } else { kind }));
    }
    if let Some(private) = anchors.get(at(SectionId::Spec, Part::Private)) {
        // Private fields: short hatched rungs, no names.
        let y = mid(&private);
        for k in 0..3 {
            let x = x_spine + px((4.0 + 5.0 * k as f32) * s);
            pen.line(window, point(x, y + px(3.0 * s)), point(x + px(4.0 * s), y - px(3.0 * s)), stroke::HAIR, kind.opacity(0.6), None);
        }
    }
}

/// A choice's fork: the spine turns full hue at the count line and splits
/// into one tine per case, each peeling off with a 45° chamfer. Fields every
/// case shares are rungs on the trunk before it splits. An open choice runs
/// on, dashed; a folded one runs on to its "and N more".
#[allow(clippy::too_many_arguments)]
fn fork(pen: &Pen, window: &mut Window, anchors: &Anchors, shared: usize, open: bool, x_spine: Pixels, x_col: Pixels, gem_bottom: Option<Pixels>, kind: Hsla, ground: Hsla) {
    let s = pen.s;
    let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
    let chamfer = px(7.0 * s);
    let end = x_col - px(10.0 * s);
    let rows = anchors.rows(SectionId::Spec);
    let Some((_, last)) = rows.last() else { return };
    let top = anchors.get(at(SectionId::Spec, Part::Count)).map_or_else(|| mid(&rows[0].1) - px(12.0 * s), |count| count.top());
    if let Some(gem) = gem_bottom {
        pen.line(window, point(x_spine, gem), point(x_spine, top), stroke::HAIR, kind.opacity(0.45), None);
    }
    let more = anchors.get(at(SectionId::Spec, Part::More));
    let mut trunk_end = mid(last) - chamfer;
    if let Some(more) = &more {
        trunk_end = mid(more) - chamfer;
    }
    pen.line(window, point(x_spine, top), point(x_spine, trunk_end), stroke::RELATION, kind, None);
    // Shared fields: rungs with filled squares, before the split.
    for n in 0..shared {
        let Some(b) = anchors.get(at(SectionId::Spec, Part::Shared(n as u16))) else { continue };
        let y = mid(&b);
        let stop = x_col - px(12.0 * s);
        pen.line(window, point(x_spine, y), point(stop, y), stroke::HAIR, kind, None);
        let sq = px(3.0 * s);
        let square = [point(stop - sq, y - sq), point(stop + sq, y - sq), point(stop + sq, y + sq), point(stop - sq, y + sq)];
        pen.poly(window, &square, stroke::HAIR, kind, Some(ground));
    }
    for (_, b) in &rows {
        let y = mid(b);
        pen.path(window, &[point(x_spine, y - chamfer), point(x_spine + chamfer, y), point(end, y)], stroke::HAIR, kind);
    }
    if let Some(more) = &more {
        let y = mid(more);
        pen.path(window, &[point(x_spine, y - chamfer), point(x_spine + chamfer, y), point(x_spine + px(18.0 * s), y)], stroke::HAIR, kind.opacity(0.45));
    }
    if open && let Some(b) = anchors.get(at(SectionId::Spec, Part::Open)) {
        let y = mid(&b);
        pen.line(window, point(x_spine, trunk_end), point(x_spine, y - chamfer), stroke::RELATION, kind, Some(stroke::INFERRED));
        pen.line(window, point(x_spine, y - chamfer), point(x_spine + chamfer, y), stroke::HAIR, kind.opacity(0.7), None);
        pen.line(window, point(x_spine + chamfer, y), point(end, y), stroke::HAIR, kind.opacity(0.7), Some(stroke::INFERRED));
    }
}

/// A callable's pipe: the plate chamfered in its hue, each port's line
/// converging into it (from the reader's left edge when the margins carry
/// the edges), what it gives back leaving right with a chevron, what it is
/// called on entering from above, and each drop falling below in coral.
#[allow(clippy::too_many_arguments)]
fn pipe(pen: &Pen, window: &mut Window, anchors: &Anchors, ports: usize, drops: usize, reach: Option<Pixels>, kind: Hsla, coral: Hsla) {
    let s = pen.s;
    let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
    let Some(plate) = anchors.get(at(SectionId::Spec, Part::Plate)) else { return };
    let (l, r, t, b) = (plate.left(), plate.right(), plate.top(), plate.bottom());
    let c = 8.0 * s;
    let y_plate = mid(&plate);
    // The ports converge: straight along their row, then a 45° run into the
    // plate's left edge at its middle.
    for n in 0..ports {
        let Some(port) = anchors.get(at(SectionId::Spec, Part::Row(n as u16))) else { continue };
        let y = mid(&port);
        if let Some(edge) = reach {
            pen.line(window, point(edge, y), point(port.left() - px(10.0 * s), y), stroke::HAIR, kind.opacity(0.3), None);
        }
        let dy = y_plate - y;
        let run = dy.abs().min(l - port.right() - px(20.0 * s)).max(px(0.0));
        let knee = l - px(6.0 * s) - run;
        pen.path(window, &[point(port.right() + px(8.0 * s), y), point(knee, y), point(l - px(6.0 * s), y_plate), point(l, y_plate)], stroke::HAIR, kind.opacity(0.85));
    }
    let outline = [point(l + px(c), t), point(r, t), point(r, b - px(c)), point(r - px(c), b), point(l, b), point(l, t + px(c))];
    pen.poly(window, &outline, stroke::RELATION, kind, None);
    // What it gives back.
    if let Some(gives) = anchors.get(at(SectionId::Spec, Part::Gives)) {
        let end = gives.left() + px(28.0 * s);
        pen.line(window, point(r, y_plate), point(end, y_plate), stroke::RELATION, kind.opacity(0.8), None);
        let h = px(3.5 * s);
        pen.path(window, &[point(end - h, y_plate - h), point(end, y_plate), point(end - h, y_plate + h)], stroke::RELATION, kind.opacity(0.8));
    }
    // What it is called on.
    if let Some(receiver) = anchors.get(at(SectionId::Spec, Part::Receiver)) {
        let x = l + px(16.0 * s);
        pen.line(window, point(x, receiver.bottom() - px(2.0 * s)), point(x, t), stroke::RELATION, kind.opacity(0.8), None);
    }
    // Each way out that is not the answer.
    let x = l + px(16.0 * s);
    let mut last = None;
    for n in 0..drops {
        let Some(drop) = anchors.get(at(SectionId::Spec, Part::Drop(n as u16))) else { continue };
        let y = mid(&drop);
        pen.line(window, point(x, y), point(drop.left() - px(6.0 * s), y), stroke::HAIR, coral, None);
        last = Some(y);
    }
    if let Some(y) = last {
        pen.line(window, point(x, b), point(x, y), stroke::RELATION, coral, None);
    }
}

/// A contract's socket: its left edge is the spine, cut with a notch for
/// each member an implementor writes (dashed when it may be left out); its
/// right edge carries a tab for each it gets; who does it plugs in from the
/// left margin, converging on one junction and one bus into the spine.
#[allow(clippy::too_many_arguments)]
fn socket(pen: &Pen, window: &mut Window, anchors: &Anchors, rows: &[(u8, bool)], doers: usize, x_spine: Pixels, gem_bottom: Option<Pixels>, kind: Hsla) {
    let s = pen.s;
    let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
    let Some(body) = anchors.get(at(SectionId::Spec, Part::Plate)) else { return };
    let top = body.top() - px(6.0 * s);
    let bottom = body.bottom() + px(6.0 * s);
    let right = body.right() + px(12.0 * s);
    let c = px(10.0 * s);
    if let Some(gem) = gem_bottom {
        pen.line(window, point(x_spine, gem), point(x_spine, top), stroke::HAIR, kind.opacity(0.45), None);
    }
    pen.path(window, &[point(x_spine, top), point(right - c, top), point(right, top + c), point(right, bottom), point(x_spine, bottom)], stroke::RELATION, kind);
    // The spine as the left edge, notched.
    let (notch_w, notch_h) = (px(10.0 * s), px(7.0 * s));
    let mut y0 = top;
    for (n, (group, optional)) in rows.iter().enumerate() {
        let Some(row) = anchors.get(at(SectionId::Spec, Part::Row(n as u16))) else { continue };
        let y = mid(&row);
        if *group == 2 {
            let tab = [point(right, y - px(5.0 * s)), point(right + px(8.0 * s), y - px(5.0 * s)), point(right + px(8.0 * s), y + px(5.0 * s)), point(right, y + px(5.0 * s))];
            pen.poly(window, &tab, stroke::HAIR, kind, Some(kind));
            continue;
        }
        if *group != 0 {
            continue;
        }
        pen.line(window, point(x_spine, y0), point(x_spine, y - notch_h), stroke::RELATION, kind, None);
        let dash = optional.then_some(stroke::OPTIONAL);
        pen.line(window, point(x_spine, y - notch_h), point(x_spine + notch_w, y - notch_h), stroke::RELATION, kind, dash);
        pen.line(window, point(x_spine + notch_w, y - notch_h), point(x_spine + notch_w, y + notch_h), stroke::RELATION, kind, dash);
        pen.line(window, point(x_spine + notch_w, y + notch_h), point(x_spine, y + notch_h), stroke::RELATION, kind, dash);
        y0 = y + notch_h;
    }
    pen.line(window, point(x_spine, y0), point(x_spine, bottom), stroke::RELATION, kind, None);
    // The plug.
    let y_bus = (top + bottom) / 2.0;
    let junction = x_spine - px(22.0 * s);
    let mut any = false;
    for n in 0..doers {
        let Some(doer) = anchors.get(at(SectionId::Spec, Part::Doer(n as u16))) else { continue };
        let y = mid(&doer);
        let dy = (y_bus - y).abs().min(junction - doer.right() - px(14.0 * s)).max(px(0.0));
        let knee = junction - dy;
        pen.path(window, &[point(doer.right() + px(6.0 * s), y), point(knee, y), point(junction, y_bus)], stroke::HAIR, kind.opacity(0.7));
        any = true;
    }
    if any {
        pen.line(window, point(junction, y_bus), point(x_spine, y_bus), stroke::RELATION, kind, None);
        let r = px(2.5 * s);
        pen.poly(window, &[point(junction - r, y_bus - r), point(junction + r, y_bus - r), point(junction + r, y_bus + r), point(junction - r, y_bus + r)], stroke::HAIR, kind, Some(kind));
    }
}

/// A section's gutter mark: one glyph per section, 12 px.
fn mark(window: &mut Window, id: SectionId, c: Point<Pixels>, s: f32, color: Hsla) {
    let p = |x: f32, y: f32| point(c.x + px(x * s), c.y + px(y * s));
    let mut path = PathBuilder::stroke(px(1.4));
    match id {
        SectionId::Yours => { path.add_polygon(&[p(0.0, -6.0), p(6.0, 0.0), p(0.0, 6.0), p(-6.0, 0.0)], true); path.add_polygon(&[p(0.0, -2.5), p(2.5, 0.0), p(0.0, 2.5), p(-2.5, 0.0)], true); }
        SectionId::Getting => { path.move_to(p(-5.0, 0.0)); path.line_to(p(5.0, 0.0)); path.move_to(p(1.0, -4.0)); path.line_to(p(5.0, 0.0)); path.line_to(p(1.0, 4.0)); }
        SectionId::Does => { for (x, y) in [(-5.0, -5.0), (1.0, -5.0), (-5.0, 1.0), (1.0, 1.0)] { path.add_polygon(&[p(x, y), p(x + 4.0, y), p(x + 4.0, y + 4.0), p(x, y + 4.0)], true); } }
        SectionId::Fails => { path.move_to(p(0.0, -6.0)); path.line_to(p(0.0, -1.0)); path.line_to(p(-5.0, 5.0)); path.move_to(p(0.0, -1.0)); path.line_to(p(5.0, 5.0)); }
        SectionId::Uses => { path.move_to(p(-5.0, 0.0)); path.line_to(p(-1.0, 0.0)); path.line_to(p(5.0, -5.0)); path.move_to(p(-1.0, 0.0)); path.line_to(p(5.0, 0.0)); path.move_to(p(-1.0, 0.0)); path.line_to(p(5.0, 5.0)); }
        SectionId::Changed => { path.move_to(p(-6.0, 5.0)); path.line_to(p(6.0, 5.0)); for (x, h) in [(-5.0, 4.0), (-1.0, 9.0), (3.0, 6.0)] { path.move_to(p(x, 5.0)); path.line_to(p(x, 5.0 - h)); } }
        SectionId::Words => { for (y, w) in [(-5.0, 10.0), (-1.0, 10.0), (3.0, 6.0)] { path.move_to(p(-5.0, y)); path.line_to(p(-5.0 + w, y)); } }
        SectionId::Spec => { path.move_to(p(-4.0, 0.0)); path.line_to(p(4.0, 0.0)); }
    }
    if let Ok(path) = path.build() { window.paint_path(path, color); }
}
