//! The page drawn from its [`plan`](super::plan): one column, a spine in the
//! gutter, the specimen hanging off it, and every section head a mark on the
//! spine with its relations running out to the margins (DIRECTION.md §4).
//!
//! Text is laid out as elements; every stroke is painted by one [`ink`]
//! canvas that reads the bounds the elements recorded as [`anchor`]s in
//! prepaint, in the same frame. Anchor ids come from the plan (a section and
//! an index into it), never from layout order, so they hold at every width.

use super::plan::{AnchorId, Dir, Fam, Hero, Mark, PagePlan, Part, Record, Section, SectionId, Spec, Tier, Tok, TokKind, Ty, Wrap};
use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, Tone, rhythm, scale, stroke};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, InteractiveElement,
    IntoElement, LayoutId, ParentElement, PathBuilder, Pixels, Point, SharedString, StatefulInteractiveElement, Styled,
    Window, canvas, div, point, px,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

// ------------------------------------------------------------------ geometry

/// Where the page's parts sit across its width, in px from the page's left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// The spine.
    pub spine: Pixels,
    /// The column's left edge.
    pub col: Pixels,
    /// The column's width.
    pub col_w: Pixels,
    /// The hero gem's edge.
    pub gem: f32,
    /// How far stubs and margin notes reach past the column on each side.
    pub reach: Pixels,
    /// The text scale.
    pub scale: f32,
}

impl Geometry {
    /// The geometry of a page `width` wide, with `margin` more room on each
    /// side outside it (the reader's gutters), at text `scale`.
    #[must_use]
    pub fn new(width: Pixels, margin: Pixels, scale: f32) -> Self {
        let narrow = f32::from(width) < 720.0 * scale;
        let spine_off = if narrow { rhythm::SPINE_NARROW } else { rhythm::SPINE } * scale;
        let gem = if narrow { rhythm::GEM_NARROW } else { rhythm::GEM } * scale;
        let lead = spine_off + gem / 2.0;
        let col_w = (f32::from(width) - lead * 2.0).clamp(240.0, rhythm::COLUMN * scale);
        let col = (f32::from(width) - col_w) / 2.0;
        let col = col.max(lead);
        let reach = (f32::from(width) - col - col_w + f32::from(margin)).max(0.0);
        Self { spine: px(col - spine_off), col: px(col), col_w: px(col_w), gem, reach: px(reach), scale }
    }

    /// Whether margin notes and stubs have room.
    #[must_use]
    pub fn margins(&self) -> bool {
        f32::from(self.reach) >= 150.0 * self.scale
    }
}

/// The hue a family draws structure in.
#[must_use]
pub fn hue(fam: Fam, palette: &Palette) -> Tone {
    match fam {
        Fam::Namespace => palette.f_ns.hue,
        Fam::Type => palette.f_type.hue,
        Fam::Contract => palette.f_con.hue,
        Fam::Callable => palette.f_call.hue,
        Fam::Value => palette.f_val.hue,
    }
}

// ------------------------------------------------------------------ anchors

/// The bounds a frame's anchored elements recorded, read by [`ink`].
#[derive(Debug, Default)]
pub struct Anchors {
    map: RefCell<BTreeMap<AnchorId, Bounds<Pixels>>>,
}

impl Anchors {
    /// A shared, empty table.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Where `id` was laid out this frame.
    #[must_use]
    pub fn get(&self, id: AnchorId) -> Option<Bounds<Pixels>> {
        self.map.borrow().get(&id).copied()
    }

    /// Every anchor recorded in `section`, in plan order.
    #[must_use]
    pub fn rows(&self, section: SectionId) -> Vec<(u16, Bounds<Pixels>)> {
        self.map
            .borrow()
            .iter()
            .filter_map(|(id, bounds)| match id.part {
                Part::Row(n) if id.section == section => Some((n, *bounds)),
                _ => None,
            })
            .collect()
    }

    fn record(&self, id: AnchorId, bounds: Bounds<Pixels>) {
        self.map.borrow_mut().insert(id, bounds);
    }
}

/// Wraps `child` so its bounds are recorded under `id` in prepaint.
pub fn anchor(id: AnchorId, anchors: &Rc<Anchors>, child: impl IntoElement) -> Anchor {
    Anchor { id, anchors: Rc::clone(anchors), child: child.into_any_element() }
}

/// See [`anchor`].
pub struct Anchor {
    id: AnchorId,
    anchors: Rc<Anchors>,
    child: AnyElement,
}

impl IntoElement for Anchor {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Anchor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, (): &mut (), window: &mut Window, cx: &mut App) {
        self.anchors.record(self.id, bounds);
        self.child.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, (): &mut (), (): &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
    }
}

fn at(section: SectionId, part: Part) -> AnchorId {
    AnchorId { section, part }
}

// ------------------------------------------------------------------ text

fn said(key: impl Into<SharedString>, content: impl Into<SharedString>, role: crate::tokens::TypeRole, color: Tone, measure: &Measure) -> AnyElement {
    let key = key.into();
    let content = content.into();
    probe::text(
        ElementId::Name(key),
        content.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Wrap,
        div().set(role, measure).text_color(color.hsla()).child(content),
    )
    .into_any_element()
}

/// A type in plain words: its glyph, then its tokens.
#[must_use]
pub fn ty_element(key: &str, ty: &Ty, measure: &Measure, palette: &Palette, xray: bool) -> AnyElement {
    let mut row = div().flex().flex_none().items_baseline().gap(px(5.0 * measure.scale()));
    if let Some(head) = ty.head().filter(|tok| tok.kind != TokKind::Var) {
        row = row.child(glyph(head, ty.wrap(), palette, measure.scale()));
    }
    for (n, tok) in ty.toks.iter().enumerate() {
        let (role, color) = tok_style(tok, palette);
        row = row.child(said(format!("{key}-tok-{n}"), tok.text.clone(), role, color, measure));
    }
    if xray && !ty.exact.is_empty() {
        row = row.child(said(format!("{key}-exact"), ty.exact.clone(), scale::LABEL_MONO, palette.ink3, measure));
    }
    row.into_any_element()
}

fn tok_style(tok: &Tok, palette: &Palette) -> (crate::tokens::TypeRole, Tone) {
    match tok.kind {
        TokKind::Word | TokKind::Punct => (scale::BODY, palette.ink3),
        TokKind::Prim => (scale::BODY, palette.ink2),
        TokKind::Named => (scale::MONO, palette.ink1),
        TokKind::Var => (scale::MONO_NAME, palette.ink0),
        TokKind::Lit => (scale::MONO, palette.ink1),
    }
}

/// The only icon a type has: a stone for a plain value, a plate for a named
/// type, a diamond for a contract; stacked for a list, dotted for maybe.
fn glyph(head: &Tok, wrap: Wrap, palette: &Palette, scale: f32) -> AnyElement {
    let color: Hsla = match head.kind {
        TokKind::Prim | TokKind::Lit => palette.ink3.hsla(),
        _ => hue(head.fam, palette).hsla(),
    };
    let kind = head.kind;
    let fam = head.fam;
    let size = 10.0 * scale;
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let o = bounds.origin;
            let s = size / 10.0;
            let shape = |dx: f32, dy: f32| -> Vec<Point<Pixels>> {
                let p = |x: f32, y: f32| point(o.x + px((x + dx) * s), o.y + px((y + dy) * s));
                match (kind, fam) {
                    (TokKind::Prim | TokKind::Lit, _) => (0..12).map(|k| {
                        let a = k as f32 / 12.0 * std::f32::consts::TAU;
                        p(5.0 + 3.2 * a.cos(), 5.0 + 3.2 * a.sin())
                    }).collect(),
                    (_, Fam::Contract) => vec![p(5.0, 0.8), p(9.2, 5.0), p(5.0, 9.2), p(0.8, 5.0)],
                    _ => vec![p(2.6, 1.2), p(9.2, 1.2), p(9.2, 7.4), p(7.4, 9.2), p(0.8, 9.2), p(0.8, 3.0)],
                }
            };
            let draw = |window: &mut Window, points: &[Point<Pixels>], filled: bool, alpha: f32| {
                let mut outline = PathBuilder::stroke(px(1.1 * s));
                if wrap == Wrap::Maybe { outline = outline.dash_array(&[px(1.2 * s), px(1.6 * s)]); }
                outline.add_polygon(points, true);
                if let Ok(path) = outline.build() { window.paint_path(path, color.opacity(alpha)); }
                if filled {
                    let mut fill = PathBuilder::fill();
                    fill.add_polygon(points, true);
                    if let Ok(path) = fill.build() { window.paint_path(path, color.opacity(0.35 * alpha)); }
                }
            };
            let filled = wrap != Wrap::Maybe;
            if wrap == Wrap::List { draw(window, &shape(2.0, -2.0), filled, 0.5); }
            draw(window, &shape(0.0, 0.0), filled, 1.0);
        },
    )
    .w(px(size))
    .h(px(size))
    .flex_none()
    .into_any_element()
}

// ------------------------------------------------------------------ hero

/// The top of the page: the gem on the spine, the name, the author's lede,
/// and the marks.
#[must_use]
pub fn hero(plan: &Hero, gem_kind: crate::icons::Kind, geo: &Geometry, anchors: &Rc<Anchors>, measure: &Measure, palette: &Palette) -> AnyElement {
    let m = measure;
    let gem = crate::paint::gem(gem_kind).size(geo.gem).hue(hue(plan.fam, palette).hsla());
    let gem = anchor(at(SectionId::Spec, Part::Gem), anchors, div().w(px(geo.gem)).h(px(geo.gem)).child(gem));
    let gem = div().absolute().left(geo.spine - geo.col - px(geo.gem / 2.0)).top(px(-6.0 * geo.scale)).child(gem);
    let mut name = div().flex().flex_wrap().items_baseline();
    if let Some(owner) = &plan.owner {
        name = name.child(said("page-owner", format!("{owner}."), scale::DISPLAY, palette.ink3, m));
    }
    name = name.child(said("page-title", plan.name.clone(), scale::DISPLAY, palette.ink0, m));
    let mut column = div().relative().flex().flex_col().child(gem).child(anchor(at(SectionId::Spec, Part::Title), anchors, name));
    if let Some(lede) = &plan.lede {
        column = column.child(div().mt(px(6.0 * geo.scale)).child(said("page-lede", plain(lede), scale::LEDE, palette.ink2, m)));
    }
    let mut marks = div().flex().flex_wrap().items_center().gap_x(px(20.0 * geo.scale)).gap_y(px(6.0 * geo.scale)).mt(px(16.0 * geo.scale));
    for (n, mark) in plan.marks.iter().enumerate() {
        marks = marks.child(mark_element(n, mark, m, palette));
    }
    column.child(marks).into_any_element()
}

/// Markup (`code`, [links]) as plain words.
fn plain(markup: &str) -> String {
    crate::overlay::text::parse(markup).iter().map(crate::overlay::text::Piece::text).collect()
}

fn mark_element(n: usize, mark: &Mark, m: &Measure, palette: &Palette) -> AnyElement {
    let (icon, text, struck) = match mark {
        Mark::Package { name, version } => (crate::icons::Icon::Package, version.as_ref().map_or_else(|| name.clone(), |v| format!("{name} {v}")), false),
        Mark::Path { text } => (crate::icons::Icon::Folder, text.clone(), false),
        Mark::Deprecated(d) => (crate::icons::Icon::Alert, d.since.as_ref().map_or_else(|| "deprecated".to_owned(), |s| format!("deprecated {s}")), true),
    };
    let mut words = div().set(scale::LABEL_MONO, m).text_color(palette.ink3.hsla());
    if struck { words = words.line_through(); }
    div()
        .id(SharedString::from(format!("page-mark-{n}")))
        .flex()
        .items_center()
        .gap(px(7.0 * m.scale()))
        .whitespace_nowrap()
        .hover(|style| style.text_color(palette.ink1.hsla()))
        .child(crate::icons::ui(icon, crate::icons::IconSize::S12, palette.ink3).size(m.icon(12.0)))
        .child(probe::text(
            ElementId::Name(SharedString::from(format!("page-mark-{n}-text"))),
            SharedString::from(text.clone()),
            m.role(scale::LABEL_MONO),
            1.0,
            TextOverflow::Wrap,
            words.child(text),
        ))
        .into_any_element()
}

// ------------------------------------------------------------------ specimens

/// The drawing at the centre of the page, when the plan has one.
#[must_use]
pub fn specimen(spec: &Spec, geo: &Geometry, anchors: &Rc<Anchors>, measure: &Measure, palette: &Palette) -> Option<AnyElement> {
    match spec {
        Spec::None => None,
        Spec::Record(record) => Some(bracket(record, geo, anchors, measure, palette)),
    }
}

fn count_words(record: &Record) -> Vec<(String, TokKind)> {
    let n = record.rungs.len() + record.private as usize;
    let optional = record.rungs.iter().filter(|rung| rung.optional).count();
    let mut out = vec![("holds".to_owned(), TokKind::Word), (n.to_string(), TokKind::Prim)];
    if optional > 0 { out.push((format!(", {optional} optional"), TokKind::Word)); }
    if record.all_readonly { out.push(("· all readonly".to_owned(), TokKind::Word)); }
    for base in &record.extends { out.push(("· extends".to_owned(), TokKind::Word)); out.push((base.clone(), TokKind::Named)); }
    for generic in &record.generics {
        out.push(("·".to_owned(), TokKind::Word));
        out.push((generic.name.clone(), TokKind::Var));
        if let Some(default) = &generic.default { out.push((format!("defaults to {}", default.plain()), TokKind::Word)); }
    }
    out
}

/// A record: a bracket whose back is the spine, one rung per field. Names
/// and types at rest; a field's doc only on hover, in the margin.
fn bracket(record: &Record, geo: &Geometry, anchors: &Rc<Anchors>, m: &Measure, palette: &Palette) -> AnyElement {
    let s = geo.scale;
    let mut count = div().flex().flex_wrap().items_baseline().gap(px(5.0 * s)).h(px(16.0 * s)).mb(px(8.0 * s));
    for (n, (text, kind)) in count_words(record).into_iter().enumerate() {
        let (role, color) = match kind {
            TokKind::Prim => (scale::LABEL, palette.ink2),
            TokKind::Named => (scale::LABEL_MONO, palette.ink2),
            TokKind::Var => (scale::LABEL_MONO, palette.ink1),
            _ => (scale::LABEL, palette.ink3),
        };
        count = count.child(said(format!("page-count-{n}"), text, role, color, m));
    }
    let name_w = record.rungs.iter().map(|rung| rung.name.chars().count() + usize::from(rung.optional)).max().unwrap_or(4) as f32 * 7.9 * s + 24.0 * s;
    let mut rows = div().flex().flex_col();
    for (n, rung) in record.rungs.iter().enumerate() {
        let key = format!("page-rung-{n}");
        let group = SharedString::from(key.clone());
        let mut name = div().flex().items_baseline().w(px(name_w)).flex_none()
            .child(said(format!("{key}-name"), rung.name.clone(), scale::MONO_NAME, if rung.optional { palette.ink1 } else { palette.ink0 }, m));
        if rung.optional { name = name.child(said(format!("{key}-optional"), "?", scale::MONO, palette.ink3, m)); }
        if rung.deprecated { name = name.line_through(); }
        let doc = rung.doc.as_ref().filter(|_| geo.margins()).map(|doc| {
            div()
                .absolute()
                .left(geo.col_w + px(24.0 * s))
                .w((geo.reach - px(40.0 * s)).max(px(0.0)))
                .top(px(6.0 * s))
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .child(said(format!("{key}-doc"), plain(doc), scale::LABEL, palette.ink3, m))
        });
        let row = div()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .min_h(px(rhythm::ROW * s))
            .child(anchor(at(SectionId::Spec, Part::Row(n as u16)), anchors, name))
            .child(ty_element(&format!("{key}-type"), &rung.ty, m, palette, m.reveal().xray))
            .children(doc);
        rows = rows.child(row);
    }
    let mut body = div().flex().flex_col().child(anchor(at(SectionId::Spec, Part::Count), anchors, count)).child(rows);
    if record.private > 0 {
        let words = format!("and {} private", record.private);
        body = body.child(anchor(at(SectionId::Spec, Part::Private), anchors,
            div().min_h(px(rhythm::ROW * s)).flex().items_center().child(said("page-private", words, scale::LABEL, palette.ink3, m))));
    }
    body.into_any_element()
}

// ------------------------------------------------------------------ section heads

/// A section's head: its mark sits on the spine (drawn by [`ink`]), then its
/// heading; the rule and the stub are ink, the stub's count is text in the
/// margin.
#[must_use]
pub fn section_head(section: &Section, geo: &Geometry, anchors: &Rc<Anchors>, measure: &Measure, palette: &Palette) -> AnyElement {
    let s = geo.scale;
    let key = format!("page-section-{:?}", section.id);
    let title = anchor(at(section.id, Part::Head), anchors, said(key.clone(), section.title.clone(), scale::SECTION, palette.ink2, measure));
    let mut head = div().relative().h(px(16.0 * s)).mb(px(rhythm::HEAD_GAP * s)).flex().items_center().child(title);
    if let Some(count) = &section.count {
        let label = section.yours.as_ref().map_or_else(|| count.clone(), |yours| format!("{count} · {yours}"));
        let place = if geo.margins() {
            match section.dir {
                Dir::In => div().absolute().right(geo.col_w + (geo.col - geo.spine) + px(16.0 * s)).top(px(-18.0 * s)),
                _ => div().absolute().left(geo.col_w + px(12.0 * s)).top(px(-18.0 * s)),
            }
        } else {
            div().absolute().right_0().top_0()
        };
        head = head.child(place.whitespace_nowrap().child(said(format!("{key}-count"), label, scale::LABEL_MONO, palette.ink3, measure)));
    }
    head.into_any_element()
}

// ------------------------------------------------------------------ ink

/// Every stroke on the page, painted in one pass from this frame's anchors:
/// the spine, the specimen's shape, and each section's mark, rule and stub.
#[must_use]
pub fn ink(plan: &PagePlan, geo: Geometry, anchors: &Rc<Anchors>, palette: &Palette) -> AnyElement {
    let anchors = Rc::clone(anchors);
    let kind = hue(plan.hero.fam, palette).hsla();
    let coral = palette.coral.base.hsla();
    let rule = palette.line2.hsla();
    let ground = palette.g1.hsla();
    let sections = plan.sections.iter().map(|section| (section.id, section.dir, section.tier)).collect::<Vec<_>>();
    let record = match &plan.spec { Spec::Record(record) => Some(record.rungs.iter().map(|rung| rung.optional).collect::<Vec<_>>()), Spec::None => None };
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let s = geo.scale;
            let x_spine = bounds.origin.x + geo.spine + px(0.5);
            let x_col = bounds.origin.x + geo.col;
            let x_end = x_col + geo.col_w;
            let line = |window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, width: f32, color: Hsla, dash: Option<[f32; 2]>| {
                let mut path = PathBuilder::stroke(px(width));
                if let Some([on, off]) = dash { path = path.dash_array(&[px(on * s), px(off * s)]); }
                path.move_to(from);
                path.line_to(to);
                if let Ok(path) = path.build() { window.paint_path(path, color); }
            };
            let poly = |window: &mut Window, points: &[Point<Pixels>], width: f32, color: Hsla, fill: Option<Hsla>| {
                if let Some(fill) = fill {
                    let mut path = PathBuilder::fill();
                    path.add_polygon(points, true);
                    if let Ok(path) = path.build() { window.paint_path(path, fill); }
                }
                let mut path = PathBuilder::stroke(px(width));
                path.add_polygon(points, fill.is_some());
                if let Ok(path) = path.build() { window.paint_path(path, color); }
            };
            let gem_bottom = anchors.get(at(SectionId::Spec, Part::Gem)).map(|gem| gem.bottom() + px(2.0));
            let heads = sections.iter().filter_map(|(id, dir, tier)| anchors.get(at(*id, Part::Head)).map(|b| (*id, *dir, *tier, b))).collect::<Vec<_>>();
            let mid = |b: &Bounds<Pixels>| (b.origin.y + b.size.height / 2.0).round() + px(0.5);
            // The spine: the node's body, from the gem to the last section.
            if let Some(top) = gem_bottom {
                let bottom = heads.last().map_or(top + px(40.0 * s), |(_, _, _, b)| mid(b));
                line(window, point(x_spine, top), point(x_spine, bottom), stroke::HAIR, kind.opacity(0.28), None);
            }
            // The record's bracket: its back is the spine.
            if let Some(optional) = &record {
                let rows = anchors.rows(SectionId::Spec);
                if let (Some((_, first)), Some((_, last))) = (rows.first(), rows.last()) {
                    let top = mid(first) - px(14.0 * s);
                    let mut bottom = mid(last) + px(14.0 * s);
                    if let Some(private) = anchors.get(at(SectionId::Spec, Part::Private)) { bottom = mid(&private) + px(8.0 * s); }
                    let serif = px(7.0 * s);
                    let mut back = PathBuilder::stroke(px(stroke::RELATION));
                    back.move_to(point(x_spine + serif, top));
                    back.line_to(point(x_spine, top));
                    back.line_to(point(x_spine, bottom));
                    back.line_to(point(x_spine + serif, bottom));
                    if let Ok(path) = back.build() { window.paint_path(path, kind); }
                    if let Some(top_of_gem) = gem_bottom { line(window, point(x_spine, top_of_gem), point(x_spine, top), stroke::HAIR, kind.opacity(0.45), None); }
                    for (n, b) in &rows {
                        let y = mid(b);
                        let opt = optional.get(*n as usize).copied().unwrap_or(false);
                        let end = x_col - px(12.0 * s);
                        line(window, point(x_spine, y), point(end, y), stroke::HAIR, kind, opt.then_some(stroke::OPTIONAL));
                        let sq = px(3.0 * s);
                        let square = [point(end - sq, y - sq), point(end + sq, y - sq), point(end + sq, y + sq), point(end - sq, y + sq)];
                        poly(window, &square, stroke::HAIR, kind, Some(if opt { ground } else { kind }));
                    }
                    if let Some(private) = anchors.get(at(SectionId::Spec, Part::Private)) {
                        // Private fields: short hatched rungs, no names.
                        let y = mid(&private);
                        for k in 0..3 {
                            let x = x_spine + px((4.0 + 5.0 * k as f32) * s);
                            line(window, point(x, y + px(3.0 * s)), point(x + px(4.0 * s), y - px(3.0 * s)), stroke::HAIR, kind.opacity(0.6), None);
                        }
                    }
                }
            }
            // Each section: its mark on the spine, the rule after its heading,
            // and its stub into the margin it relates to.
            for (id, dir, tier, b) in &heads {
                let y = mid(b);
                let color = if *id == SectionId::Fails { coral } else { kind };
                let r = px(8.0 * s);
                let knock = [point(x_spine - r, y - r), point(x_spine + r, y - r), point(x_spine + r, y + r), point(x_spine - r, y + r)];
                poly(window, &knock, 0.0, ground, Some(ground));
                mark(window, *id, point(x_spine, y), s, color);
                line(window, point(b.right() + px(14.0 * s), y), point(x_end, y), stroke::HAIR, rule, None);
                let dash = (*tier == Tier::Name).then_some(stroke::INFERRED);
                if geo.margins() {
                    match dir {
                        Dir::In => line(window, point(x_spine - px(10.0 * s) - (geo.reach - px(16.0 * s)).min(geo.col), y), point(x_spine - px(10.0 * s), y), stroke::RELATION, color.opacity(0.55), dash),
                        Dir::Out => line(window, point(x_end + px(8.0 * s), y), point(x_end + geo.reach - px(16.0 * s), y), stroke::RELATION, color.opacity(0.55), dash),
                        Dir::None => {}
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

/// A section's gutter mark: one glyph per section, 12 px.
fn mark(window: &mut Window, id: SectionId, c: Point<Pixels>, s: f32, color: Hsla) {
    let p = |x: f32, y: f32| point(c.x + px(x * s), c.y + px(y * s));
    let mut path = PathBuilder::stroke(px(1.4));
    match id {
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

#[cfg(test)]
mod tests {
    use super::Geometry;
    use gpui::px;

    #[test]
    fn the_column_is_640_with_the_spine_44_left_of_it_in_a_wide_folio() {
        let geo = Geometry::new(px(784.0), px(160.0), 1.0);
        assert_eq!((geo.col, geo.col_w, geo.spine), (px(72.0), px(640.0), px(28.0)));
        assert!(geo.margins(), "a wide reader leaves the margins room for stubs");
        let narrow = Geometry::new(px(640.0), px(20.0), 1.0);
        assert_eq!(narrow.col - narrow.spine, px(36.0), "the narrow spine sits 36 px left of the column");
        assert!(!narrow.margins(), "a narrow reader keeps its counts in the column");
    }
}
