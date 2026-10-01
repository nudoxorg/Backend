//! The sibling strip: the names beside this one in its module, as chips you
//! hop along, the current one ringed (the board's top rail). Each chip is a
//! sigil, the name and the kind word; a name is a door like any other on the
//! page (it opens its page, and its name becomes the title). The module leads
//! the strip as a quiet chip with its count.
//!
//! The strip shows the window of chips around the current one that fits its
//! room, and says how many more there are on each side.

use super::{Doors, Geometry, named_as, said, title_key};
use crate::anatomy::plan::{DeclKind, Fam, PagePlan, Sibling};
use crate::anatomy::sigil::{Form, Sigil, sigil};
use crate::hover;
use crate::measure::Measure;
use crate::paint::geom::{Poly, fill_poly};
use crate::tokens::{Palette, scale};
use gpui::{AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, StatefulInteractiveElement, Styled, canvas, div, px};

/// The chip's kind word.
const fn word(kind: DeclKind) -> &'static str {
    match kind {
        DeclKind::Struct => "struct",
        DeclKind::Class => "class",
        DeclKind::Interface => "interface",
        DeclKind::Trait => "trait",
        DeclKind::Enum => "enum",
        DeclKind::Union => "union",
        DeclKind::Alias => "alias",
        DeclKind::Function => "fn",
        DeclKind::Method => "method",
        DeclKind::Constant => "const",
        DeclKind::Module => "module",
        DeclKind::Field => "field",
        DeclKind::Variant => "variant",
        DeclKind::Other => "",
    }
}

const fn form(kind: DeclKind) -> (Form, Fam) {
    match kind {
        DeclKind::Struct | DeclKind::Class | DeclKind::Union => (Form::Struct, Fam::Type),
        DeclKind::Enum => (Form::Enum, Fam::Type),
        DeclKind::Alias => (Form::Alias, Fam::Type),
        DeclKind::Trait | DeclKind::Interface => (Form::Trait, Fam::Contract),
        DeclKind::Function => (Form::Fn, Fam::Callable),
        DeclKind::Method => (Form::Method, Fam::Callable),
        _ => (Form::Value, Fam::Value),
    }
}

/// A chip's width, estimated from its words (the UI face is 0.58 em, the
/// mono 0.6 em).
fn width(chip: &Sibling, s: f32) -> f32 {
    (10.0 + 18.0 + 7.0 + chip.name.chars().count() as f32 * scale::MONO_NAME.size * 0.6 + 8.0 + word(chip.kind).len() as f32 * scale::LABEL.size * 0.58 + 10.0) * s
}

/// The chips that fit `room` around the current one: `(from, to)` indexes
/// into the strip, end exclusive.
fn window(strip: &[Sibling], room: f32, s: f32) -> (usize, usize) {
    let at = strip.iter().position(|chip| chip.current).unwrap_or(0);
    let (mut from, mut to) = (at, at + 1);
    let mut used = strip.get(at).map_or(0.0, |chip| width(chip, s) + 6.0 * s);
    let mut right = true;
    loop {
        let next = if right { strip.get(to) } else { from.checked_sub(1).and_then(|k| strip.get(k)) };
        match next {
            Some(chip) if used + width(chip, s) + 6.0 * s <= room => {
                used += width(chip, s) + 6.0 * s;
                if right { to += 1; } else { from -= 1; }
            }
            _ => {
                // The other side may still fit one more.
                let other = if right { from.checked_sub(1).and_then(|k| strip.get(k)) } else { strip.get(to) };
                match other {
                    Some(chip) if used + width(chip, s) + 6.0 * s <= room => {
                        used += width(chip, s) + 6.0 * s;
                        if right { from -= 1; } else { to += 1; }
                    }
                    _ => break,
                }
            }
        }
        right = !right;
    }
    (from, to)
}

/// A chamfered chip plate: fill and a ring.
fn plate(fill: Hsla, ring: Hsla, width_: f32, thick: f32) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let poly = Poly::chamfer(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 5.0);
            fill_poly(window, &poly, fill);
            for edge in poly.stroke_ring(thick) {
                fill_poly(window, &edge, ring);
            }
            let _ = width_;
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// The strip, when the module has more than the symbol itself.
#[must_use]
pub fn strip(plan: &PagePlan, room: Pixels, geo: &Geometry, m: &Measure, palette: &Palette, doors: &dyn Doors) -> Option<AnyElement> {
    if plan.strip.len() < 2 {
        return None;
    }
    let s = geo.scale;
    // The module chip leads and a "+N" may close each side: they take their room first.
    let lead_text = format!("‹ {}  {}", if plan.scope.is_empty() { "module" } else { plan.scope.as_str() }, plan.strip.len());
    let lead_w = (26.0 + lead_text.chars().count() as f32 * scale::LABEL_MONO.size * 0.6) * s;
    let (from, to) = window(&plan.strip, f32::from(room) - lead_w - 72.0 * s, s);
    // Room above the chips for the word "from" that rides a ringed chip's top edge.
    let mut row = div().flex().items_center().gap(px(6.0 * s)).h(px(42.0 * s)).pt(px(12.0 * s)).w(room);
    // The module leads: a quiet chip with its count.
    let lead = lead_text;
    doors.say(&lead);
    let mut lead_chip = div().id("page-strip-up").relative().h(px(28.0 * s)).flex_none().px(px(10.0 * s)).flex().items_center()
        .child(plate(palette.g1.hsla(), palette.line2.hsla(), 0.0, 1.0))
        .child(said("page-strip-scope", lead, scale::LABEL_MONO, palette.ink3, m));
    if let Some(up) = doors.up() {
        lead_chip = lead_chip.cursor_pointer().on_click(move |_, window, cx| up(window, cx));
    }
    row = row.child(lead_chip);
    if from > 0 {
        row = row.child(said("page-strip-before", format!("+{from}"), scale::LABEL_MONO, palette.ink3, m));
    }
    for (n, chip) in plan.strip[from..to].iter().enumerate() {
        let key = format!("page-strip-{}", from + n);
        doors.say(&chip.name);
        let (form, fam) = form(chip.kind);
        let mark = sigil(Sigil { ins: u8::from(matches!(form, Form::Fn | Form::Method)), gives: matches!(form, Form::Fn | Form::Method), ..Sigil::new(form, fam) }, 18.0 * s, palette);
        // The chip's mark is the shared mark of the declaration it opens: on
        // a hop it is the one the previous hero's mark shrinks into.
        let mark = match chip.link.as_deref().filter(|_| !chip.current).and_then(|link| doors.mark(link)) {
            Some(id) => crate::motion::shared::shared(id, mark).into_any_element(),
            None => mark,
        };
        let (mm, pal, name, kind, current) = (*m, *palette, chip.name.clone(), word(chip.kind), chip.current);
        let width_ = width(chip, s);
        let name_key = format!("{key}-name");
        let door_key = format!("{key}-door");
        // Only the chip's name is the shared title: it, not the plate, flies.
        let title_link = chip.link.clone().filter(|_| !chip.current);
        let body = move |lit: hover::Lit| {
            let ink = if current { pal.ink0 } else { pal.ink1 };
            div().relative().h(px(28.0 * s)).w(px(width_)).flex_none().px(px(10.0 * s)).flex().items_center().gap(px(7.0 * s))
                .child(plate(
                    if current { pal.plate2.hsla() } else { pal.plate.hsla() },
                    if current { pal.peri.base.hsla() } else { pal.line2.hsla() },
                    width_,
                    if current { 1.5 } else { 1.0 },
                ))
                .child(mark)
                .child({
                    let name = said(name_key, name, scale::MONO_NAME, hover::ink(ink, lit, &pal), &mm);
                    match title_link {
                        Some(link) => crate::motion::shared::shared(title_key(&link), name).into_any_element(),
                        None => name,
                    }
                })
                .child(said(format!("{key}-word"), kind, scale::LABEL, pal.ink3, &mm))
                .into_any_element()
        };
        let door = named_as(door_key, &chip.name, chip.link.as_deref().filter(|_| !chip.current), palette.f_call.hue.hsla(), doors, false, body);
        row = row.child(door);
    }
    if to < plan.strip.len() {
        row = row.child(said("page-strip-after", format!("+{}", plan.strip.len() - to), scale::LABEL_MONO, palette.ink3, m));
    }
    Some(row.into_any_element())
}
