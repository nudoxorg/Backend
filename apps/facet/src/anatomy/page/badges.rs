//! The hero's badges: real GUI elements where a code line would have been.
//! Nothing here prints `pub unsafe fn`: the recorded declaration is read
//! into tags (async, unsafe, can fail, takes 2, generic over T, ...), the
//! specimen's own counts become badges ("one of 7", "you write 2"), what
//! its traits let it do is one **can** group of small marks that spread into
//! words when you rest on them, and yours is a mint badge that says how many
//! crates name it. A badge widens in place to say what it means, so a hover
//! never covers a neighbour.

use super::{Doors, Geometry};
use crate::anatomy::plan::{Cap, CapGlyph, DeclKind, Lang, PagePlan, Spec};
use crate::hover::{self, Lit, Subject};
use crate::marks::badges::{self, Glyph, Ink, Item};
use crate::measure::{Measure, Set, Space};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{AnyElement, ElementId, Hsla, IntoElement, ParentElement, PathBuilder, SharedString, Styled, canvas, div, point, px};

/// The words on a badge.
const WORD: TypeRole = TypeRole { weight: 520.0, ..ty::BUTTON };
/// The sentence that opens.
const TIP: TypeRole = TypeRole { weight: 400.0, ..ty::SMALL };

const fn icon_lang(lang: Lang) -> crate::icons::Lang {
    match lang {
        Lang::Rust | Lang::Other => crate::icons::Lang::Rust,
        Lang::TypeScript => crate::icons::Lang::Typescript,
        Lang::Go => crate::icons::Lang::Go,
        Lang::Python => crate::icons::Lang::Python,
        Lang::Java => crate::icons::Lang::Java,
        Lang::CSharp => crate::icons::Lang::Csharp,
        Lang::Cpp => crate::icons::Lang::Cpp,
    }
}

const fn icon_kind(kind: DeclKind) -> Option<crate::icons::Kind> {
    use crate::icons::Kind as K;
    Some(match kind {
        DeclKind::Struct => K::Struct,
        DeclKind::Class => K::Class,
        DeclKind::Interface => K::Interface,
        DeclKind::Trait => K::Trait,
        DeclKind::Enum => K::Enum,
        DeclKind::Union => K::Union,
        DeclKind::Alias => K::Type,
        DeclKind::Function => K::Function,
        DeclKind::Method => K::Method,
        DeclKind::Constant => K::Constant,
        DeclKind::Module => K::Module,
        DeclKind::Field => K::Field,
        DeclKind::Variant => K::Variant,
        DeclKind::Other => return None,
    })
}

/// The kind word the kind line leads with (`enum`, `fn`, `trait`).
#[must_use]
pub fn kind_word(plan: &PagePlan) -> &'static str {
    let hero = &plan.hero;
    let item = Item::new(&hero.name, icon_lang(hero.lang)).kind(icon_kind(hero.kind)).signature(hero.signature.as_deref());
    let word = badges::read(&item).word;
    if hero.kind == DeclKind::Method && word == "fn" { "method" } else { word }
}

/// The badges the page's facts make, in reading order.
fn facts(plan: &PagePlan) -> Vec<badges::Badge> {
    let hero = &plan.hero;
    let mut out = Vec::new();
    let new = |glyph: Glyph, word: String, tip: &str, ink: Ink| badges::Badge::new(glyph, word, tip.to_owned(), ink);
    match &plan.spec {
        Spec::Choice(choice) => {
            let n = choice.cases.len();
            out.push(new(Glyph::Iter, format!("one of {n}"), "An enum: a value is exactly one of these cases.", Ink::Teal));
        }
        Spec::Record(record) => {
            let n = record.rungs.len();
            out.push(new(Glyph::Owner, format!("holds {n}"), "Its public fields.", Ink::Teal));
        }
        Spec::Contract(contract) => {
            out.push(new(Glyph::Marker, format!("you write {}", contract.write.len()), "Members an implementor must provide.", Ink::Peri));
            if !contract.get.is_empty() {
                out.push(new(Glyph::Makes, format!("you get {}", contract.get.len()), "Members it provides for free.", Ink::Plain));
            }
        }
        Spec::Callable(_) | Spec::None => {}
    }
    let item = Item::new(&hero.name, icon_lang(hero.lang)).kind(icon_kind(hero.kind)).signature(hero.signature.as_deref());
    out.extend(badges::read(&item).badges);
    out
}

/// Whether the hero has any badge to draw.
#[must_use]
pub fn any(plan: &PagePlan) -> bool {
    !facts(plan).is_empty() || !plan.hero.caps.is_empty() || plan.reach.read()
}

/// The badge row.
#[must_use]
pub fn row(plan: &PagePlan, geo: &Geometry, m: &Measure, palette: &Palette, doors: &dyn Doors) -> Option<AnyElement> {
    if !any(plan) {
        return None;
    }
    let s = geo.scale;
    let mut row = div().flex().flex_wrap().items_center().gap_x(px(8.0 * s)).gap_y(px(6.0 * s));
    for (n, fact) in facts(plan).iter().enumerate() {
        doors.say(&fact.word);
        row = row.child(badge_el(&format!("page-badge-{n}"), fact, m, palette));
    }
    if !plan.hero.caps.is_empty() {
        for cap in &plan.hero.caps {
            doors.say(&cap.word);
        }
        row = row.child(can_group(&plan.hero.caps, m, palette));
    }
    if plan.reach.read() {
        let crates = plan.reach.yours.len();
        let (word, ink, tip) = if crates == 0 {
            ("not named by your code".to_owned(), Ink::Plain, "Not a single line of your crates names it (a path scan).")
        } else {
            (format!("{} in {} {}", plan.reach.total(), crates, if crates == 1 { "crate" } else { "crates" }), Ink::Mint, "Named by your code: a path scan of your crates.")
        };
        doors.say(&word);
        let fact = badges::Badge::new(Glyph::You, word, tip.to_owned(), ink);
        row = row.child(badge_el("page-badge-yours", &fact, m, palette));
    }
    Some(row.into_any_element())
}

// ------------------------------------------------------------------ a badge

/// One badge: a cut plate wearing a glyph and one word; resting on it opens
/// its sentence inside it (the plate widens, so a neighbour is never covered).
/// It answers through the one hover grammar (`hover::hoverable`), the same as
/// every other name on the page, and is not a click target.
fn badge_el(key: &str, fact: &badges::Badge, m: &Measure, palette: &Palette) -> AnyElement {
    let (fact, mm, pal, key) = (fact.clone(), *m, *palette, key.to_owned());
    let hue = badges::view::ink_of(fact.ink, palette);
    hover::hoverable(ElementId::Name(SharedString::from(key.clone())), Subject::new(format!("{key}:subject")), hue, move |lit| {
        let open = lit == Lit::Target;
        let s = mm.scale();
        let ink = badges::view::ink_of(fact.ink, &pal);
        let text = |suffix: &str, content: SharedString, role: TypeRole, color: Hsla| probe::text(
            ElementId::Name(SharedString::from(format!("{key}-{suffix}"))),
            content.clone(),
            mm.role(role),
            1.0,
            TextOverflow::Clip,
            div().set(role, &mm).text_color(color).whitespace_nowrap().child(content),
        );
        let mut body = cut()
            .chamfer(Chamfer::Px(3.0 * s))
            .edge(badges::view::edge_of(fact.ink, &pal, if open { 1.0 } else { 0.0 }))
            .plate(Plate::Flat)
            .fill(mix(pal.plate.into(), pal.plate2.into(), if open { 1.0 } else { 0.0 }))
            .flex()
            .flex_none()
            .items_center()
            .gap(mm.space(Space::Snug))
            .h(px(21.0 * s))
            .pl(mm.space(Space::Snug))
            .pr(mm.space(Space::Snug) + px(1.0))
            .child(badges::glyph(fact.glyph, 12.0 * s, ink))
            .child(text("word", fact.word.clone(), WORD, ink));
        if open {
            body = body.child(text("tip", fact.tip.clone(), TIP, pal.ink2.hsla()));
        }
        body.into_any_element()
    })
    .shape(hover::Shape::Chamfer(3.0 * m.scale()))
    .into_any_element()
}

// ------------------------------------------------------------------ can

/// The **can** group: a plate wearing "can" and one small mark per
/// capability; resting on it spreads the marks into words, in place.
fn can_group(caps: &[Cap], m: &Measure, palette: &Palette) -> AnyElement {
    let (caps, mm, pal) = (caps.to_vec(), *m, *palette);
    let subject = Subject::new("page-can");
    hover::hoverable(ElementId::Name("page-can".into()), subject, palette.f_con.hue.hsla(), move |lit| {
        let open = lit == Lit::Target;
        let s = mm.scale();
        let ink = pal.ink1.hsla();
        let word = |text: &'static str| probe::text(
            ElementId::Name(SharedString::from(format!("page-can-{text}"))),
            SharedString::from(text),
            mm.role(WORD),
            1.0,
            TextOverflow::Clip,
            div().set(WORD, &mm).text_color(ink).whitespace_nowrap().child(text),
        );
        let mut body = cut()
            .chamfer(Chamfer::Px(3.0 * s))
            .edge(badges::view::edge_of(Ink::Plain, &pal, if open { 1.0 } else { 0.0 }))
            .plate(Plate::Flat)
            .fill(mix(pal.plate.into(), pal.plate2.into(), if open { 1.0 } else { 0.0 }))
            .flex()
            .flex_none()
            .items_center()
            .gap(mm.space(Space::Snug))
            .h(px(21.0 * s))
            .px(mm.space(Space::Snug))
            .child(word("can"));
        for (n, cap) in caps.iter().enumerate() {
            let mark = cap_glyph(cap.glyph, 12.0 * s, if open { ink } else { pal.ink2.hsla() });
            if open {
                let label = SharedString::from(cap.word.clone());
                body = body.child(div().flex().items_center().gap(px(4.0 * s)).child(mark).child(probe::text(
                    ElementId::Name(SharedString::from(format!("page-can-word-{n}"))),
                    label.clone(),
                    mm.role(WORD),
                    1.0,
                    TextOverflow::Clip,
                    div().set(WORD, &mm).text_color(pal.ink2.hsla()).whitespace_nowrap().child(label),
                )));
            } else {
                body = body.child(mark);
            }
        }
        body.into_any_element()
    })
    .shape(hover::Shape::Chamfer(3.0 * m.scale()))
    .into_any_element()
}

/// A capability's mark: twelve units, square caps (a circle is a polygon).
fn cap_glyph(glyph: CapGlyph, size: f32, color: Hsla) -> AnyElement {
    let ring = |cx: f32, cy: f32, r: f32| -> Vec<(f32, f32)> {
        (0..12).map(|i| {
            let a = i as f32 / 12.0 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        }).collect()
    };
    let rect = |x: f32, y: f32, w: f32, h: f32| vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    // (closed, points)
    let prims: Vec<(bool, Vec<(f32, f32)>)> = match glyph {
        CapGlyph::Copy => vec![(true, rect(1.5, 3.5, 6.0, 6.0)), (true, rect(4.5, 1.5, 6.0, 6.0))],
        CapGlyph::Eq => vec![(false, vec![(2.0, 4.5), (10.0, 4.5)]), (false, vec![(2.0, 7.5), (10.0, 7.5)])],
        CapGlyph::Ord => vec![(false, vec![(8.5, 2.5), (3.5, 6.0), (8.5, 9.5)])],
        CapGlyph::Hash => vec![
            (false, vec![(4.5, 1.5), (3.5, 10.5)]), (false, vec![(8.5, 1.5), (7.5, 10.5)]),
            (false, vec![(1.5, 4.5), (10.5, 4.5)]), (false, vec![(1.5, 7.5), (10.5, 7.5)]),
        ],
        CapGlyph::Default => vec![(true, ring(6.0, 6.0, 4.0)), (true, ring(6.0, 6.0, 1.0))],
        CapGlyph::Print => vec![(false, vec![(2.0, 3.0), (10.0, 3.0)]), (false, vec![(2.0, 6.0), (10.0, 6.0)]), (false, vec![(2.0, 9.0), (7.0, 9.0)])],
        CapGlyph::Debug => vec![
            (false, vec![(4.0, 1.5), (2.6, 3.0), (2.4, 4.8), (1.5, 6.0), (2.4, 7.2), (2.6, 9.0), (4.0, 10.5)]),
            (false, vec![(8.0, 1.5), (9.4, 3.0), (9.6, 4.8), (10.5, 6.0), (9.6, 7.2), (9.4, 9.0), (8.0, 10.5)]),
        ],
        CapGlyph::Ser => vec![(true, rect(1.5, 3.0, 6.0, 6.0)), (false, vec![(5.0, 6.0), (10.5, 6.0)]), (false, vec![(8.5, 4.0), (10.5, 6.0), (8.5, 8.0)])],
        CapGlyph::De => vec![(true, rect(4.5, 3.0, 6.0, 6.0)), (false, vec![(1.0, 6.0), (7.0, 6.0)]), (false, vec![(5.0, 4.0), (7.0, 6.0), (5.0, 8.0)])],
        CapGlyph::Iter => vec![
            (false, vec![(2.0, 3.5), (8.0, 3.5)]), (false, vec![(2.0, 6.0), (8.0, 6.0)]), (false, vec![(2.0, 8.5), (8.0, 8.5)]),
            (false, vec![(9.5, 7.0), (11.0, 8.5), (9.5, 10.0)]),
        ],
        CapGlyph::Error => vec![(true, rect(2.0, 2.0, 8.0, 8.0)), (false, vec![(4.3, 4.3), (7.7, 7.7)]), (false, vec![(7.7, 4.3), (4.3, 7.7)])],
        CapGlyph::Send => vec![(false, vec![(1.5, 6.0), (10.5, 6.0)]), (false, vec![(7.5, 3.0), (10.5, 6.0), (7.5, 9.0)]), (false, vec![(1.5, 3.0), (1.5, 9.0)])],
        CapGlyph::Index => vec![
            (false, vec![(3.5, 2.0), (2.0, 2.0), (2.0, 10.0), (3.5, 10.0)]),
            (false, vec![(8.5, 2.0), (10.0, 2.0), (10.0, 10.0), (8.5, 10.0)]),
            (false, vec![(6.0, 4.0), (6.0, 8.0)]),
        ],
    };
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let k = size / 12.0;
            let at = |(x, y): (f32, f32)| point(bounds.origin.x + px(x * k), bounds.origin.y + px(y * k));
            for (closed, points) in &prims {
                let pts: Vec<_> = points.iter().copied().map(at).collect();
                let mut path = PathBuilder::stroke(px(1.15 * k));
                if *closed {
                    path.add_polygon(&pts, true);
                } else {
                    for (n, p) in pts.iter().enumerate() {
                        if n == 0 { path.move_to(*p); } else { path.line_to(*p); }
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .flex_none()
    .size(px(size))
    .into_any_element()
}

/// The palette's bevel edge for a plain plate, exposed so the can group and
/// the badges beside it wear the same rim.
#[allow(dead_code)]
fn plain_edge(palette: &Palette) -> Edge {
    let mut edge = Edge::of(Bevel::Rest, palette);
    edge.hi = palette.line3.into();
    edge.lo = palette.line2.into();
    edge
}
