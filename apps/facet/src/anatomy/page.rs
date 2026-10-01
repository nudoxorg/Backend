//! The page drawn from its [`plan`](super::plan): one column, a spine in the
//! gutter, the specimen hanging off it, and every section head a mark on the
//! spine with its relations running out to the margins (DIRECTION.md §4).
//!
//! Text is laid out as elements; every stroke is painted by one [`ink`]
//! canvas that reads the bounds the elements recorded as [`anchor`]s in
//! prepaint, in the same frame. Anchor ids come from the plan (a section and
//! an index into it), never from layout order, so they hold at every width.
//!
//! Compact by law (the lead, wave 6): the first 1440×900 screen holds the
//! hero, the specimen and the first in-section; the hero says nothing the
//! chrome says; a section head is one row; a member is one row, 24 px, its
//! doc only in the hover's peek; a group of more than 7 folds; counts ride
//! the strokes; below the wide room the margins fold into the column.

mod badges;
mod does;
mod fails;
mod fork;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
mod history;
mod ink;
mod lazy;
mod pipe;
mod rails;
mod socket;
mod strip;
mod uses;
mod yours;

pub use does::{Capability, DoesGroup, DoesRow, does};
pub use fails::fails;
pub use fork::{CASES, more_link};
pub use ink::ink;
pub use rails::RAILS;
pub use rails::band_possible;
pub use uses::uses;
pub use yours::{scrub_subject, yours};

use super::plan::{
    AnchorId, Dir, Fam, Hero, Mark, PagePlan, Part, Record, Section, SectionId, Spec, Tok, TokKind,
    Ty, Wrap,
};
use crate::hover::{self, Lit, Subject};
use crate::measure::{Measure, Set};
use crate::motion::presence::Presence;
use crate::overlay::float::FloatRequest;
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, Tone, TypeRole, rhythm, scale};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Global, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, ParentElement, PathBuilder,
    Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, point,
    px,
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

/// The reach at which the margins carry the edges: at 100 % text a 1440 or
/// 1280 window with the shelf open has it; a narrower room folds the edges
/// into the column as counts.
const WIDE_REACH: f32 = 180.0;

impl Geometry {
    /// The geometry of a page `width` wide, with `margin` more room on each
    /// side outside it (the reader's gutters), at text `scale`.
    #[must_use]
    pub fn new(width: Pixels, margin: Pixels, scale: f32) -> Self {
        let narrow = f32::from(width) < 720.0 * scale;
        let spine_off = if narrow {
            rhythm::SPINE_NARROW
        } else {
            rhythm::SPINE
        } * scale;
        let gem = if narrow {
            rhythm::GEM_NARROW
        } else {
            rhythm::GEM
        } * scale;
        let lead = spine_off + gem / 2.0;
        let col_w = (f32::from(width) - lead * 2.0).clamp(240.0, rhythm::COLUMN * scale);
        let col = (f32::from(width) - col_w) / 2.0;
        let col = col.max(lead);
        let reach = (f32::from(width) - col - col_w + f32::from(margin)).max(0.0);
        Self {
            spine: px(col - spine_off),
            col: px(col),
            col_w: px(col_w),
            gem,
            reach: px(reach),
            scale,
        }
    }

    /// Whether the margins carry the edges (stubs with their counts, rail
    /// sources); otherwise they fold into the column.
    #[must_use]
    pub fn margins(&self) -> bool {
        f32::from(self.reach) >= WIDE_REACH * self.scale
    }

    fn s(&self, value: f32) -> Pixels {
        px(value * self.scale)
    }
}

/// Rows a group shows before it folds, everywhere on the page (cases,
/// rails, operations, sites).
pub const FOLD_AT: usize = 7;

/// Whether a group of `n` rows folds: only when that hides two rows or
/// more (never fold a single row: 8 shows all 8, 9 shows 7 and "and 2
/// more").
#[must_use]
pub const fn folds(n: usize) -> bool {
    n > FOLD_AT + 1
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

/// The bounds a frame's anchored elements recorded, read by [`ink`] and by
/// the transitions (the page ↔ graph fold reads the gem, the spine, the
/// section rules and the stubs).
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

    /// Every row anchor recorded in `section`, in plan order.
    #[must_use]
    pub fn rows(&self, section: SectionId) -> Vec<(u16, Bounds<Pixels>)> {
        self.parts(section, |part| match part {
            Part::Row(n) => Some(n),
            _ => None,
        })
    }

    /// Every rail anchor (the step plates), in plan order.
    #[must_use]
    pub fn rails(&self) -> Vec<(u16, Bounds<Pixels>)> {
        self.parts(SectionId::Getting, |part| match part {
            Part::Rail(n) => Some(n),
            _ => None,
        })
    }

    /// Every anchor, in id order: for the transitions and for tests.
    #[must_use]
    pub fn all(&self) -> Vec<(AnchorId, Bounds<Pixels>)> {
        self.map
            .borrow()
            .iter()
            .map(|(id, bounds)| (*id, *bounds))
            .collect()
    }

    fn parts(
        &self,
        section: SectionId,
        pick: impl Fn(Part) -> Option<u16>,
    ) -> Vec<(u16, Bounds<Pixels>)> {
        self.map
            .borrow()
            .iter()
            .filter(|(id, _)| id.section == section)
            .filter_map(|(id, bounds)| pick(id.part).map(|n| (n, *bounds)))
            .collect()
    }

    /// Records `bounds` under `id` (elements in prepaint; the ink records
    /// the spine, rules and stubs it paints).
    pub fn record(&self, id: AnchorId, bounds: Bounds<Pixels>) {
        self.map.borrow_mut().insert(id, bounds);
    }
}

/// The anchors of the pages on screen, by the page's address, so the
/// transitions can read a page's gem, spine, rules and stubs on its first
/// frame. Bounded: the last few pages drawn.
#[derive(Default)]
struct Published {
    pages: Vec<(SharedString, Rc<Anchors>)>,
}

impl Global for Published {}

/// Publishes `anchors` as the page `address`'s.
pub fn publish(address: impl Into<SharedString>, anchors: &Rc<Anchors>, cx: &mut App) {
    let address = address.into();
    let published = cx.default_global::<Published>();
    published.pages.retain(|(key, _)| *key != address);
    published.pages.push((address, Rc::clone(anchors)));
    if published.pages.len() > 8 {
        published.pages.remove(0);
    }
}

/// The anchors the page `address` last published.
#[must_use]
pub fn published(address: &str, cx: &App) -> Option<Rc<Anchors>> {
    cx.try_global::<Published>()?
        .pages
        .iter()
        .rev()
        .find(|(key, _)| key.as_ref() == address)
        .map(|(_, anchors)| Rc::clone(anchors))
}

/// Wraps `child` so its bounds are recorded under `id` in prepaint.
pub fn anchor(id: AnchorId, anchors: &Rc<Anchors>, child: impl IntoElement) -> Anchor {
    Anchor {
        id,
        anchors: Rc::clone(anchors),
        child: child.into_any_element(),
    }
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

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.anchors.record(self.id, bounds);
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

pub(crate) fn at(section: SectionId, part: Part) -> AnchorId {
    AnchorId { section, part }
}

// ------------------------------------------------------------------ doors

/// Where a name on the page leads: what lights with it, the peek that
/// unfurls from it, and what a click does.
#[derive(Clone)]
pub struct Door {
    /// What lights together with it.
    pub subject: Subject,
    /// The card that unfurls from it after the hover delay.
    pub peek: Option<Rc<dyn Fn(Bounds<Pixels>) -> FloatRequest>>,
    /// A click.
    pub open: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    /// It is the declaration this page was reached from: ringed, "from".
    pub from: bool,
}

/// A fold's state: open or not, its clip motion, and its toggle.
#[derive(Clone)]
pub struct Fold {
    /// Unrolled.
    pub open: bool,
    /// Its clip motion.
    pub presence: Presence,
    /// Opens or closes it.
    pub toggle: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// What the shell lends the page: doors for its names, its folds, keyboard
/// targets, and the record of what the page says.
pub trait Doors {
    /// The door for the thing at `link` (an address), when it leads anywhere.
    fn door(&self, link: &str) -> Option<Door>;
    /// The fold named `key`.
    fn fold(&self, key: &'static str) -> Option<Fold>;
    /// `element` as a keyboard target labelled `label`, opening `door`.
    fn track(
        &self,
        key: SharedString,
        label: SharedString,
        door: Option<&Door>,
        element: AnyElement,
    ) -> AnyElement;
    /// Records a string the page puts on screen.
    fn say(&self, text: &str);
    /// One level up from this page (its package, folding the page back into
    /// the card it came from): the sibling strip's module chip.
    fn up(&self) -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
        None
    }
    /// The shared-element id of the mark of the declaration at `link`, so a
    /// chip's mark and the page's hero mark are one element in flight.
    fn mark(&self, _link: &str) -> Option<ElementId> {
        None
    }
}

/// No doors: a still page (the gallery, tests).
pub struct Still;

impl Doors for Still {
    fn door(&self, _: &str) -> Option<Door> {
        None
    }
    fn fold(&self, _: &'static str) -> Option<Fold> {
        None
    }
    fn track(
        &self,
        _: SharedString,
        _: SharedString,
        _: Option<&Door>,
        element: AnyElement,
    ) -> AnyElement {
        element
    }
    fn say(&self, _: &str) {}
}

/// One hoverable name: `build` lays it out with this frame's [`Lit`]. With a
/// door it lights its subject, unfurls its peek and opens on a click, and it
/// is a keyboard target; without one it is plain.
pub fn named(
    key: impl Into<SharedString>,
    label: &str,
    link: Option<&str>,
    hue: Hsla,
    doors: &dyn Doors,
    build: impl FnOnce(Lit) -> AnyElement + 'static,
) -> AnyElement {
    named_as(key, label, link, hue, doors, true, build)
}

/// [`named`], the door shared under its title's key (`share`) or not: a chip
/// that is more than a name shares only its name (it wraps the name in
/// [`title_key`] itself), so a flight carries the name, never the plate.
pub fn named_as(
    key: impl Into<SharedString>,
    label: &str,
    link: Option<&str>,
    hue: Hsla,
    doors: &dyn Doors,
    share: bool,
    build: impl FnOnce(Lit) -> AnyElement + 'static,
) -> AnyElement {
    let key = key.into();
    let door = link.and_then(|link| doors.door(link));
    let Some(door) = door else {
        return build(Lit::Rest);
    };
    // A row that opens a declaration carries its title's key: its name
    // becomes that page's title.
    let shared_key = title_key(door.subject.0.as_ref());
    let mut lit = hover::hoverable(
        ElementId::Name(SharedString::from(format!("{key}-hover"))),
        door.subject.clone(),
        hue,
        move |lit| {
            if share {
                crate::motion::shared::shared(shared_key, build(lit)).into_any_element()
            } else {
                build(lit)
            }
        },
    );
    if let Some(peek) = door.peek.clone() {
        lit = lit.peek(move |bounds| peek(bounds));
    }
    let mut hit = div()
        .id(ElementId::Name(key.clone()))
        .relative()
        .cursor_pointer()
        .child(lit);
    if door.from {
        // Where you came from stays ringed (dashed, in the periwinkle: the
        // current chip's ring is solid).
        hit = hit.child(from_ring(hue));
    }
    if let Some(open) = door.open.clone() {
        hit = hit.on_click(move |_, window, cx| open(window, cx));
    }
    doors.track(
        key,
        SharedString::from(label.to_owned()),
        Some(&door),
        hit.into_any_element(),
    )
}

/// A dashed chamfered ring around what it sits in, with the word "from" on
/// its edge: "you came from here".
fn from_ring(hue: Hsla) -> AnyElement {
    let ring = canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let (x, y) = (
                f32::from(bounds.origin.x) - 3.0,
                f32::from(bounds.origin.y) - 2.0,
            );
            let (w, h) = (
                f32::from(bounds.size.width) + 6.0,
                f32::from(bounds.size.height) + 4.0,
            );
            let c = 4.0;
            let pts = [
                point(px(x + c), px(y)),
                point(px(x + w), px(y)),
                point(px(x + w), px(y + h - c)),
                point(px(x + w - c), px(y + h)),
                point(px(x), px(y + h)),
                point(px(x), px(y + c)),
            ];
            let mut path = PathBuilder::stroke(px(1.2)).dash_array(&[px(3.0), px(2.5)]);
            path.add_polygon(&pts, true);
            if let Ok(path) = path.build() {
                window.paint_path(path, hue.opacity(0.9));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    // The word rides the ring's top edge, small, in the ring's own hue.
    let role = scale::LABEL_MONO;
    let tag = probe::text(
        ElementId::Name("page-from-tag".into()),
        SharedString::from("from"),
        role,
        1.0,
        TextOverflow::Clip,
        crate::fonts::Typeset::typeset_at(
            div().whitespace_nowrap().text_color(hue),
            TypeRole {
                size: 9.5,
                line: 11.0,
                ..role
            },
            1.0,
        )
        .child("from"),
    );
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(ring)
        .child(div().absolute().top(px(-9.0)).right(px(6.0)).child(tag))
        .into_any_element()
}

// ------------------------------------------------------------------ text

pub fn said(
    key: impl Into<SharedString>,
    content: impl Into<SharedString>,
    role: TypeRole,
    color: impl Into<Hsla>,
    measure: &Measure,
) -> AnyElement {
    let key = key.into();
    let content = content.into();
    probe::text(
        ElementId::Name(key),
        content.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Wrap,
        div()
            .set(role, measure)
            .text_color(color.into())
            .whitespace_nowrap()
            .child(content),
    )
    .into_any_element()
}

/// A type in plain words: its glyph, then its tokens.
#[must_use]
pub fn ty_element(
    key: &str,
    ty: &Ty,
    measure: &Measure,
    palette: &Palette,
    xray: bool,
) -> AnyElement {
    ty_lit(key, ty, measure, palette, xray, Lit::Rest)
}

pub fn ty_lit(
    key: &str,
    ty: &Ty,
    measure: &Measure,
    palette: &Palette,
    xray: bool,
    lit: Lit,
) -> AnyElement {
    let mut row = div()
        .flex()
        .flex_none()
        .items_baseline()
        .gap(px(5.0 * measure.scale()));
    if let Some(head) = ty.head().filter(|tok| tok.kind != TokKind::Var) {
        row = row.child(div().flex_none().self_center().child(glyph(
            head,
            ty.wrap(),
            palette,
            measure.scale(),
        )));
    }
    for (n, tok) in ty.toks.iter().enumerate() {
        let (role, color) = tok_style(tok, palette);
        row = row.child(said(
            format!("{key}-tok-{n}"),
            tok.text.clone(),
            role,
            hover::ink(color, lit, palette),
            measure,
        ));
    }
    if xray && !ty.exact.is_empty() && ty.exact != ty.plain() {
        row = row.child(said(
            format!("{key}-exact"),
            ty.exact.clone(),
            scale::LABEL_MONO,
            palette.ink3,
            measure,
        ));
    }
    row.into_any_element()
}

fn tok_style(tok: &Tok, palette: &Palette) -> (TypeRole, Tone) {
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
pub(crate) fn glyph(head: &Tok, wrap: Wrap, palette: &Palette, scale: f32) -> AnyElement {
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
                    (TokKind::Prim | TokKind::Lit, _) => (0..12)
                        .map(|k| {
                            let a = k as f32 / 12.0 * std::f32::consts::TAU;
                            p(5.0 + 3.2 * a.cos(), 5.0 + 3.2 * a.sin())
                        })
                        .collect(),
                    (_, Fam::Contract) => vec![p(5.0, 0.8), p(9.2, 5.0), p(5.0, 9.2), p(0.8, 5.0)],
                    _ => vec![
                        p(2.6, 1.2),
                        p(9.2, 1.2),
                        p(9.2, 7.4),
                        p(7.4, 9.2),
                        p(0.8, 9.2),
                        p(0.8, 3.0),
                    ],
                }
            };
            let draw = |window: &mut Window, points: &[Point<Pixels>], filled: bool, alpha: f32| {
                let mut outline = PathBuilder::stroke(px(1.1 * s));
                if wrap == Wrap::Maybe {
                    outline = outline.dash_array(&[px(1.2 * s), px(1.6 * s)]);
                }
                outline.add_polygon(points, true);
                if let Ok(path) = outline.build() {
                    window.paint_path(path, color.opacity(alpha));
                }
                if filled {
                    let mut fill = PathBuilder::fill();
                    fill.add_polygon(points, true);
                    if let Ok(path) = fill.build() {
                        window.paint_path(path, color.opacity(0.35 * alpha));
                    }
                }
            };
            let filled = wrap != Wrap::Maybe;
            if wrap == Wrap::List {
                draw(window, &shape(2.0, -2.0), filled, 0.5);
            }
            draw(window, &shape(0.0, 0.0), filled, 1.0);
        },
    )
    .w(px(size))
    .h(px(size))
    .flex_none()
    .into_any_element()
}

/// A neutral stone: a value's mark (values carry no hue).
pub fn stone(palette: &Palette, scale: f32) -> AnyElement {
    glyph(
        &Tok {
            kind: TokKind::Lit,
            text: String::new(),
            fam: Fam::Value,
        },
        Wrap::Plain,
        palette,
        scale,
    )
}

/// Markup (`code`, [links]) as plain words.
fn plain(markup: &str) -> String {
    crate::overlay::text::parse(markup)
        .iter()
        .map(crate::overlay::text::Piece::text)
        .collect()
}

/// Widths for a column of mono text, from its longest entry: Geist Mono's
/// advance is 0.6 em.
pub fn mono_w(chars: usize, role: TypeRole, scale: f32) -> Pixels {
    px(chars as f32 * role.size * 0.6 * scale)
}

/// Widths for plain words in the UI face (an upper bound: 0.58 em).
pub fn words_w(chars: usize, role: TypeRole, scale: f32) -> Pixels {
    px(chars as f32 * role.size * 0.58 * scale)
}

// ------------------------------------------------------------------ hero

/// The key a declaration's title is shared under, by its address: the
/// title and every row that opens the declaration carry it, so the row's
/// name becomes the title (and Back returns it), driven by the reader.
#[must_use]
pub fn title_key(address: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("title:{address}")))
}

/// The top of the page, in two parts. The title line (the gem on the spine
/// and the name) stands apart so a transition can carry the name along its
/// plate's edge while the rest prints; then the author's lede (only when
/// the docs have one) and the marks the chrome does not already say:
/// since, deprecated, yours. `gem` and `title` come from the shell (shared
/// elements, a name fitted to the room).
#[must_use]
pub fn hero(
    plan: &PagePlan,
    gem: AnyElement,
    title: AnyElement,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    measure: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> (AnyElement, Option<AnyElement>) {
    let m = measure;
    let hero = &plan.hero;
    let gem = anchor(
        at(SectionId::Spec, Part::Gem),
        anchors,
        div().w(px(geo.gem)).h(px(geo.gem)).child(gem),
    );
    // The kind line: what it is and where, quiet, above the name.
    let kind_h = geo.s(16.0);
    let word = badges::kind_word(plan);
    let place = if hero.module.is_empty() {
        word.to_owned()
    } else {
        format!("{word} · {}", hero.module)
    };
    doors.say(&place);
    let kind_line = div().h(kind_h).flex().items_center().child(said(
        "page-kind-line",
        place.to_uppercase(),
        scale::LABEL_MONO,
        hue(hero.fam, palette),
        m,
    ));
    // The gem is centred on the title's first line.
    let line = scale::DISPLAY.line * geo.scale;
    // The sibling strip runs above the name, from the spine.
    let lead = geo.col - geo.spine;
    let strip = strip::strip(plan, geo.col_w + lead, geo, m, palette, doors).map(|strip| {
        div()
            .ml(-lead)
            .mb(geo.s(4.0))
            .child(strip)
            .into_any_element()
    });
    let above = if strip.is_some() {
        geo.s(46.0)
    } else {
        px(0.0)
    };
    let gem = div()
        .absolute()
        .left(geo.spine - geo.col - px(geo.gem / 2.0))
        .top(above + (kind_h + px(line) - px(geo.gem)) / 2.0)
        .child(gem);
    // Its history: in the right margin when the margins carry the edges,
    // else under the badges.
    let history_wide = geo
        .margins()
        .then(|| {
            history::history(
                &plan.history,
                (geo.reach - geo.s(72.0)).min(geo.s(300.0)),
                m,
                palette,
            )
        })
        .flatten();
    let history_wide = history_wide.map(|history| {
        div()
            .absolute()
            .left(geo.col_w + geo.s(28.0))
            .top(above)
            .child(history)
            .into_any_element()
    });
    let title_line = div()
        .relative()
        .child(gem)
        .children(history_wide)
        .child(
            div()
                .flex()
                .flex_col()
                .children(strip)
                .child(kind_line)
                .child(anchor(at(SectionId::Spec, Part::Title), anchors, title)),
        )
        .into_any_element();
    let mut rest = div().flex().flex_col();
    let mut any = false;
    if let Some(lede) = &hero.lede {
        let lede = plain(lede);
        doors.say(&lede);
        any = true;
        rest = rest.child(
            div().mt(geo.s(4.0)).max_w(geo.col_w).child(probe::text(
                ElementId::Name("page-lede".into()),
                SharedString::from(lede.clone()),
                m.role(scale::LEDE),
                1.0,
                TextOverflow::Wrap,
                div()
                    .set(scale::LEDE, m)
                    .text_color(palette.ink2.hsla())
                    .child(lede),
            )),
        );
    }
    if let Some(row) = badges::row(plan, geo, m, palette, doors) {
        any = true;
        rest = rest.child(
            div()
                .mt(geo.s(12.0))
                .max_w(geo.col_w + geo.reach.min(geo.s(120.0)))
                .child(row),
        );
    }
    if !geo.margins()
        && let Some(history) =
            history::history(&plan.history, geo.col_w.min(geo.s(320.0)), m, palette)
    {
        any = true;
        rest = rest.child(div().mt(geo.s(12.0)).child(history));
    }
    if !hero.marks.is_empty() {
        any = true;
        let mut marks = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(geo.s(20.0))
            .gap_y(geo.s(6.0))
            .mt(geo.s(10.0));
        for (n, mark) in hero.marks.iter().enumerate() {
            marks = marks.child(mark_element(n, mark, m, palette, doors));
        }
        rest = rest.child(marks);
    }
    (title_line, any.then(|| rest.into_any_element()))
}

fn mark_element(
    n: usize,
    mark: &Mark,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let (icon, text, struck, color) = match mark {
        Mark::Since(version) => (
            crate::icons::Icon::Clock,
            format!("since {version}"),
            false,
            palette.ink3,
        ),
        Mark::Deprecated(d) => (
            crate::icons::Icon::Alert,
            d.since
                .as_ref()
                .map_or_else(|| "deprecated".to_owned(), |s| format!("deprecated {s}")),
            true,
            palette.ink3,
        ),
        Mark::Yours => (
            crate::icons::Icon::Diamond,
            "yours".to_owned(),
            false,
            palette.mint.base,
        ),
    };
    doors.say(&text);
    let mut words = div().set(scale::LABEL_MONO, m).text_color(color.hsla());
    if struck {
        words = words.line_through();
    }
    div()
        .id(SharedString::from(format!("page-mark-{n}")))
        .flex()
        .items_center()
        .gap(px(7.0 * m.scale()))
        .whitespace_nowrap()
        .child(crate::icons::ui(icon, crate::icons::IconSize::S12, color).size(m.icon(12.0)))
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
pub fn specimen(
    plan: &PagePlan,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    measure: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> Option<AnyElement> {
    match &plan.spec {
        Spec::None => None,
        Spec::Record(record) => Some(bracket(record, geo, anchors, measure, palette)),
        Spec::Choice(choice) => Some(fork::fork(
            choice,
            plan.hero.fam,
            geo,
            anchors,
            measure,
            palette,
            doors,
        )),
        Spec::Callable(callable) => Some(pipe::pipe(
            callable,
            &plan.hero.name,
            geo,
            anchors,
            measure,
            palette,
            doors,
        )),
        Spec::Contract(contract) => Some(socket::socket(
            contract,
            plan.hero.fam,
            geo,
            anchors,
            measure,
            palette,
            doors,
        )),
    }
}

fn count_words(record: &Record) -> Vec<(String, TokKind)> {
    let n = record.rungs.len() + record.private as usize;
    let optional = record.rungs.iter().filter(|rung| rung.optional).count();
    let mut out = vec![
        ("holds".to_owned(), TokKind::Word),
        (n.to_string(), TokKind::Prim),
    ];
    if optional > 0 {
        out.push((format!(", {optional} optional"), TokKind::Word));
    }
    if record.all_readonly {
        out.push(("· all readonly".to_owned(), TokKind::Word));
    }
    for base in &record.extends {
        out.push(("· extends".to_owned(), TokKind::Word));
        out.push((base.clone(), TokKind::Named));
    }
    for generic in &record.generics {
        out.push(("·".to_owned(), TokKind::Word));
        out.push((generic.name.clone(), TokKind::Var));
        if let Some(default) = &generic.default {
            out.push((format!("defaults to {}", default.plain()), TokKind::Word));
        }
    }
    out
}

/// A record: a bracket whose back is the spine, one rung per field. Names
/// and types at rest; a field's doc only on hover, in the margin.
fn bracket(
    record: &Record,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    m: &Measure,
    palette: &Palette,
) -> AnyElement {
    let s = geo.scale;
    let mut count = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap(px(5.0 * s))
        .h(px(16.0 * s))
        .mb(px(8.0 * s));
    for (n, (text, kind)) in count_words(record).into_iter().enumerate() {
        let (role, color) = match kind {
            TokKind::Prim => (scale::LABEL, palette.ink2),
            TokKind::Named => (scale::LABEL_MONO, palette.ink2),
            TokKind::Var => (scale::LABEL_MONO, palette.ink1),
            _ => (scale::LABEL, palette.ink3),
        };
        count = count.child(said(format!("page-count-{n}"), text, role, color, m));
    }
    let name_w = record
        .rungs
        .iter()
        .map(|rung| rung.name.chars().count() + usize::from(rung.optional))
        .max()
        .unwrap_or(4) as f32
        * 7.9
        * s
        + 24.0 * s;
    let mut rows = div().flex().flex_col();
    for (n, rung) in record.rungs.iter().enumerate() {
        let key = format!("page-rung-{n}");
        let group = SharedString::from(key.clone());
        let mut name = div()
            .flex()
            .items_baseline()
            .w(px(name_w))
            .flex_none()
            .child(said(
                format!("{key}-name"),
                rung.name.clone(),
                scale::MONO_NAME,
                if rung.optional {
                    palette.ink1
                } else {
                    palette.ink0
                },
                m,
            ));
        if rung.optional {
            name = name.child(said(
                format!("{key}-optional"),
                "?",
                scale::MONO,
                palette.ink3,
                m,
            ));
        }
        if rung.deprecated {
            name = name.line_through();
        }
        let doc = rung.doc.as_ref().filter(|_| geo.margins()).map(|doc| {
            div()
                .absolute()
                .left(geo.col_w + px(24.0 * s))
                .w((geo.reach - px(40.0 * s)).max(px(0.0)))
                .top(px(6.0 * s))
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .child(said(
                    format!("{key}-doc"),
                    plain(doc),
                    scale::LABEL,
                    palette.ink3,
                    m,
                ))
        });
        let row = div()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .h(px(rhythm::ROW_PITCH * s))
            .child(anchor(
                at(SectionId::Spec, Part::Row(n as u16)),
                anchors,
                name,
            ))
            .child(ty_element(
                &format!("{key}-type"),
                &rung.ty,
                m,
                palette,
                m.reveal().xray,
            ))
            .children(doc);
        rows = rows.child(row);
    }
    let mut body = div()
        .flex()
        .flex_col()
        .child(anchor(at(SectionId::Spec, Part::Count), anchors, count))
        .child(rows);
    if record.private > 0 {
        let words = format!("and {} private", record.private);
        body = body.child(anchor(
            at(SectionId::Spec, Part::Private),
            anchors,
            div()
                .h(px(rhythm::ROW_PITCH * s))
                .flex()
                .items_center()
                .child(said("page-private", words, scale::LABEL, palette.ink3, m)),
        ));
    }
    body.into_any_element()
}

// ------------------------------------------------------------------ sections

/// The subject a section's head and its stub light together under.
#[must_use]
pub fn section_subject(id: SectionId) -> Subject {
    Subject::new(format!("page-section:{id:?}"))
}

/// A section's head: its mark sits on the spine (drawn by [`ink`]), then its
/// heading, one row high; the rule and the stub are ink, the stub's count is
/// text: in the margin above its stub when the margins carry the edges, at
/// the head's right end otherwise.
#[must_use]
pub fn section_head(
    section: &Section,
    fam: Fam,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    measure: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let key = format!("page-section-{:?}", section.id);
    doors.say(&section.title);
    let subject = section_subject(section.id);
    let color = if section.id == SectionId::Fails {
        palette.coral.base
    } else {
        hue(fam, palette)
    };
    let (title, m, pal) = (section.title.clone(), *measure, *palette);
    let head_key = key.clone();
    let title = hover::hoverable(
        ElementId::Name(SharedString::from(format!("{key}-hover"))),
        subject,
        color.hsla(),
        move |lit| {
            said(
                head_key,
                title,
                scale::SECTION,
                hover::ink(pal.ink2, lit, &pal),
                &m,
            )
        },
    );
    let title = anchor(at(section.id, Part::Head), anchors, title);
    let mut head = div()
        .relative()
        .h(geo.s(16.0))
        .flex()
        .items_center()
        .child(title);
    if let Some(count) = &section.count {
        let label = section
            .yours
            .as_ref()
            .map_or_else(|| count.clone(), |yours| format!("{count} · {yours}"));
        doors.say(&label);
        let words = count_label(&key, count, section.yours.as_deref(), measure, palette);
        // A count too long for its margin folds to the head's right end.
        let margin_room = f32::from(geo.reach - (geo.col - geo.spine)) - 30.0 * geo.scale;
        let est = label.chars().count() as f32 * scale::LABEL_MONO.size * 0.6 * geo.scale;
        let place = if geo.margins() && section.dir != Dir::None && est <= margin_room {
            match section.dir {
                Dir::In => div()
                    .absolute()
                    .right(geo.col_w + (geo.col - geo.spine) + geo.s(18.0))
                    .bottom(geo.s(10.0)),
                _ => div()
                    .absolute()
                    .left(geo.col_w + geo.s(14.0))
                    .bottom(geo.s(10.0)),
            }
        } else {
            div().absolute().right_0().top_0()
        };
        head = head.child(place.whitespace_nowrap().child(anchor(
            at(section.id, Part::Count),
            anchors,
            words,
        )));
    }
    head.into_any_element()
}

fn count_label(
    key: &str,
    count: &str,
    yours: Option<&str>,
    m: &Measure,
    palette: &Palette,
) -> AnyElement {
    let mut row = div().flex().items_baseline().gap(px(6.0 * m.scale()));
    // The separator rides the count's own run: a lone `·` is too little ink
    // to read (and to measure) on its own.
    let count = if yours.is_some() {
        format!("{count} ·")
    } else {
        count.to_owned()
    };
    row = row.child(said(
        format!("{key}-count"),
        count,
        scale::LABEL_MONO,
        palette.ink3,
        m,
    ));
    if let Some(yours) = yours {
        row = row.child(said(
            format!("{key}-count-yours"),
            yours.to_owned(),
            scale::LABEL_MONO,
            palette.mint.base,
            m,
        ));
    }
    row.into_any_element()
}

/// Getting one's body: the rails.
#[must_use]
pub fn getting(
    plan: &PagePlan,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    measure: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
    band: Option<AnyElement>,
) -> Option<AnyElement> {
    rails::rails(plan, geo, anchors, measure, palette, doors, band)
}

// ------------------------------------------------------------------ the page

/// The whole page: the hero, the specimen, then each section in reading
/// order (its head, then its body), and the ink over them. `bodies` are the
/// sections the shell draws itself; Getting one is drawn here from the plan
/// unless the shell gives one.
///
/// Every part is its own child, in reading order, keyed
/// `page-part-{n}`, so a transition can reveal them in that order.
#[must_use]
pub fn page(
    plan: &PagePlan,
    gem: AnyElement,
    title: AnyElement,
    mut bodies: Vec<(SectionId, AnyElement)>,
    band: Option<AnyElement>,
    geo: Geometry,
    anchors: &Rc<Anchors>,
    measure: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let (title_line, hero_rest) = hero(plan, gem, title, &geo, anchors, measure, palette, doors);
    let mut parts: Vec<AnyElement> = hero_rest.into_iter().collect();
    // The band: where what makes one runs to a plate and what it does runs
    // on from it (left = where it comes from, right = where it goes), then
    // its cases hang below. What it does is said there, and not again.
    let band_on = band.is_some() && rails::band_possible(plan, &geo);
    if band_on
        && let Some(section) = plan
            .sections
            .iter()
            .find(|section| section.id == SectionId::Getting)
    {
        let head = section_head(
            section,
            plan.hero.fam,
            &geo,
            anchors,
            measure,
            palette,
            doors,
        );
        let mut block = div()
            .flex()
            .flex_col()
            .mt(geo.s(rhythm::SECTION - 24.0))
            .child(head);
        if let Some(body) = getting(plan, &geo, anchors, measure, palette, doors, band) {
            block = block.child(anchor(
                at(SectionId::Getting, Part::Body),
                anchors,
                div().mt(geo.s(rhythm::HEAD_GAP - 4.0)).child(body),
            ));
        }
        parts.push(block.into_any_element());
    }
    if let Some(spec) = specimen(plan, &geo, anchors, measure, palette, doors) {
        parts.push(div().mt(geo.s(24.0)).child(spec).into_any_element());
    }
    for section in &plan.sections {
        if band_on && matches!(section.id, SectionId::Getting | SectionId::Does) {
            continue;
        }
        let body = match bodies.iter().position(|(id, _)| *id == section.id) {
            Some(at) => Some(bodies.remove(at).1),
            None if section.id == SectionId::Getting => {
                getting(plan, &geo, anchors, measure, palette, doors, None)
            }
            None if section.id == SectionId::Fails => {
                fails(&plan.fails, &geo, anchors, measure, palette, doors)
            }
            None if section.id == SectionId::Uses => {
                uses(&plan.uses, &geo, measure, palette, doors)
            }
            None if section.id == SectionId::Yours => {
                yours(&plan.reach, &plan.hero.name, &geo, measure, palette, doors)
            }
            None => None,
        };
        let head = section_head(
            section,
            plan.hero.fam,
            &geo,
            anchors,
            measure,
            palette,
            doors,
        );
        let mut block = div()
            .flex()
            .flex_col()
            .mt(geo.s(rhythm::SECTION - 8.0))
            .child(head);
        if let Some(body) = body {
            block = block.child(anchor(
                at(section.id, Part::Body),
                anchors,
                div().mt(geo.s(rhythm::HEAD_GAP - 4.0)).child(body),
            ));
        }
        parts.push(block.into_any_element());
    }
    // The title line stands outside the parts a transition prints in reading
    // order (`page-part-{n}`): it rides the opening plate's edge instead.
    let mut column = div()
        .relative()
        .w(geo.col + geo.col_w)
        .pl(geo.col)
        .flex()
        .flex_col()
        .child(div().id("page-title-line").child(title_line));
    for (n, part) in parts.into_iter().enumerate() {
        column = column.child(
            div()
                .id(SharedString::from(format!("page-part-{n}")))
                .child(part),
        );
    }
    column
        .child(ink(plan, geo, anchors, palette))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::Geometry;
    use gpui::px;

    #[test]
    fn the_column_is_640_with_the_spine_44_left_of_it_in_a_wide_folio() {
        let geo = Geometry::new(px(784.0), px(196.0), 1.0);
        assert_eq!(
            (geo.col, geo.col_w, geo.spine),
            (px(72.0), px(640.0), px(28.0))
        );
        assert!(
            geo.margins(),
            "a 1440 window leaves the margins room for stubs"
        );
        // 1280 with the shelf open: the reader is 1016 wide.
        assert!(
            Geometry::new(px(784.0), px(116.0), 1.0).margins(),
            "1280 still carries the edges"
        );
        // Below: the edges fold into the column.
        assert!(!Geometry::new(px(784.0), px(60.0), 1.0).margins());
        let narrow = Geometry::new(px(640.0), px(20.0), 1.0);
        assert_eq!(
            narrow.col - narrow.spine,
            px(36.0),
            "the narrow spine sits 36 px left of the column"
        );
        assert!(
            !narrow.margins(),
            "a narrow reader keeps its counts in the column"
        );
    }
}
