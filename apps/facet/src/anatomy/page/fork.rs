//! The fork: a choice drawn as the spine splitting into tines, one per case.
//!
//! One row per case, 24 px: the case (a name, a literal, a constant with its
//! printed value, or a type in words), what it carries, and the accessors
//! that read it. Its doc is the hover's peek, never a line at rest. More
//! than seven cases fold to seven and "and N more", which unrolls in place.
//! An open choice (Go's named int, a TypeScript union kept open) runs its
//! trunk on, dashed, to what else it admits. The strokes are [`super::ink`]'s.

use super::{Doors, Geometry, at, hue, mono_w, named, said, stone, ty_lit, words_w};
use crate::anatomy::plan::{Case, CaseKind, Choice, Fam, Part, SectionId};
use crate::anatomy::page::{Anchors, anchor};
use crate::hover::{self, Lit};
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, IntoElement, ParentElement, Pixels, Styled, div, px};
use std::rc::Rc;

/// Cases shown before the fold.
pub(crate) const FOLD_AT: usize = 7;

/// The fold key the shell keeps a fork's "and N more" under.
pub const CASES: &str = "page-cases";

fn case_chars(case: &Case) -> (usize, bool) {
    match case.kind {
        CaseKind::Type => (case.carries.first().map_or(case.name.chars().count(), |ty| ty.plain().chars().count()), true),
        _ => (case.name.chars().count(), false),
    }
}

fn carries_w(case: &Case, s: f32) -> Pixels {
    if case.kind == CaseKind::Type {
        return px(0.0);
    }
    let mut w = px(0.0);
    for (n, ty) in case.carries.iter().enumerate() {
        if n > 0 {
            w += words_w(3, scale::BODY, s);
        }
        w += words_w(ty.plain().chars().count(), scale::BODY, s) + px(15.0 * s);
    }
    if let Some(value) = &case.value {
        w += mono_w(value.chars().count(), scale::MONO, s) + px(16.0 * s);
    }
    if !case.fields.is_empty() {
        let chars = case.fields.iter().map(|field| field.name.chars().count() + usize::from(field.optional) + 2).sum::<usize>();
        w += mono_w(chars, scale::MONO, s);
    }
    w
}

fn accessors_w(case: &Case, s: f32) -> Pixels {
    case.accessors.iter().map(|accessor| mono_w(accessor.name.chars().count(), scale::LABEL_MONO, s) + px(if accessor.changes { 26.0 } else { 12.0 } * s)).fold(px(0.0), |a, b| a + b)
}

/// The fork for `choice`.
pub(super) fn fork(choice: &Choice, fam: Fam, geo: &Geometry, anchors: &Rc<Anchors>, m: &Measure, palette: &Palette, doors: &dyn Doors) -> AnyElement {
    let s = geo.scale;
    let pitch = px(rhythm::ROW_PITCH * s);
    let kind = hue(fam, palette).hsla();
    let fold = (choice.cases.len() > FOLD_AT).then(|| doors.fold(CASES)).flatten();
    let open = fold.as_ref().is_some_and(|fold| fold.open);
    let folded = choice.cases.len() > FOLD_AT;

    // The count line: "one of 7", what every case is, what tells them apart.
    let mut count = div().flex().items_baseline().gap(px(5.0 * s)).h(px(16.0 * s)).mb(px(4.0 * s));
    count = count.child(said("page-count-0", "one of", scale::LABEL, palette.ink3, m));
    count = count.child(said("page-count-1", choice.cases.len().to_string(), scale::LABEL, palette.ink2, m));
    doors.say(&format!("one of {}", choice.cases.len()));
    if let Some(each) = &choice.each {
        count = count.child(said("page-count-2", "· each", scale::LABEL, palette.ink3, m));
        count = count.child(ty_lit("page-count-each", each, m, palette, m.reveal().xray, Lit::Rest));
    }
    if let Some(told) = &choice.told_by {
        count = count.child(said("page-count-3", "· told apart by", scale::LABEL, palette.ink3, m));
        count = count.child(said("page-count-4", told.clone(), scale::LABEL_MONO, palette.ink1, m));
    }
    let mut body = div().flex().flex_col().child(anchor(at(SectionId::Spec, Part::Count), anchors, count));

    // Fields every case holds, on the trunk before it splits.
    for (n, rung) in choice.shared.iter().enumerate() {
        let key = format!("page-shared-{n}");
        doors.say(&rung.name);
        let row = div()
            .h(pitch)
            .flex()
            .items_center()
            .gap(px(16.0 * s))
            .child(said(format!("{key}-name"), format!("{}{}", rung.name, if rung.optional { "?" } else { "" }), scale::MONO, palette.ink1, m))
            .child(ty_lit(&format!("{key}-type"), &rung.ty, m, palette, m.reveal().xray, Lit::Rest));
        body = body.child(anchor(at(SectionId::Spec, Part::Shared(n as u16)), anchors, row));
    }

    let shown = if folded && !open { FOLD_AT } else { choice.cases.len() };
    let name_w = choice.cases[..shown]
        .iter()
        .map(|case| match case_chars(case) {
            (chars, true) => words_w(chars, scale::BODY, s) + px(15.0 * s),
            (chars, false) => mono_w(chars, scale::MONO_NAME, s),
        })
        .fold(px(0.0), Pixels::max)
        + px(24.0 * s);
    let payload_w = choice.cases[..shown].iter().map(|case| carries_w(case, s)).fold(px(0.0), Pixels::max);
    let payload_w = if payload_w > px(0.0) { payload_w + px(24.0 * s) } else { payload_w };
    let room = geo.col_w - name_w - payload_w;
    let widest = choice.cases[..shown].iter().map(|case| accessors_w(case, s)).fold(px(0.0), Pixels::max);
    let condensed = widest > room;

    let row = |n: usize, case: &Case| anchor(at(SectionId::Spec, Part::Row(n as u16)), anchors, case_row(n, case, name_w, payload_w, condensed, kind, m, palette, doors));
    let mut rows = div().flex().flex_col();
    for (n, case) in choice.cases.iter().enumerate().take(if folded { FOLD_AT } else { choice.cases.len() }) {
        rows = rows.child(row(n, case));
    }
    if let (Some(fold), true) = (&fold, folded) {
        // The cases past the seventh unroll in place by clip; closed and
        // still, they are not built at all (no hidden targets).
        let tail = if fold.open || !fold.presence.is_empty() {
            choice.cases.iter().enumerate().skip(FOLD_AT).fold(div().flex().flex_col(), |tail, (n, case)| tail.child(row(n, case)))
        } else {
            div()
        };
        rows = rows.child(crate::anatomy::unroll::unroll("page-cases-rest", fold.open, fold.presence.clone(), tail));
    }
    body = body.child(rows);
    if folded && !open {
        let more = choice.cases.len() - FOLD_AT;
        let words = format!("and {more} more");
        doors.say(&words);
        let toggle = fold.as_ref().map(|fold| Rc::clone(&fold.toggle));
        let link = div().h(pitch).flex().items_center().child(more_link("page-cases-more", words, toggle, kind, *m, *palette, doors));
        body = body.child(anchor(at(SectionId::Spec, Part::More), anchors, link));
    }
    if let Some(open) = &choice.open {
        let words = format!("or {open}");
        doors.say(&words);
        body = body.child(anchor(at(SectionId::Spec, Part::Open), anchors,
            div().h(pitch).flex().items_center().child(said("page-open", words, scale::LABEL, palette.ink3, m))));
    }
    body.into_any_element()
}

/// "and N more": a link that unrolls the rest in place.
pub fn more_link(key: &str, words: String, toggle: Option<Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>>, kind: gpui::Hsla, m: Measure, pal: Palette, doors: &dyn Doors) -> AnyElement {
    use gpui::{InteractiveElement, StatefulInteractiveElement};
    let key = gpui::SharedString::from(key.to_owned());
    let (text, words_key) = (words.clone(), format!("{key}-words"));
    let lit = hover::hoverable(gpui::ElementId::Name(format!("{key}-hover").into()), hover::Subject::new(key.clone()), kind, move |lit| {
        said(words_key, text, scale::LABEL, hover::ink(pal.ink2, lit, &pal), &m)
    });
    let Some(toggle) = toggle else { return lit.into_any_element() };
    let door = super::Door { subject: hover::Subject::new(key.clone()), peek: None, open: Some(Rc::clone(&toggle)) };
    let hit = div().id(gpui::ElementId::Name(format!("{key}-hit").into())).cursor_pointer().child(lit)
        .on_click(move |_, window, cx| toggle(window, cx));
    doors.track(key, words.into(), Some(&door), hit.into_any_element())
}

#[allow(clippy::too_many_arguments)]
fn case_row(n: usize, case: &Case, name_w: Pixels, payload_w: Pixels, condensed: bool, kind: gpui::Hsla, m: &Measure, palette: &Palette, doors: &dyn Doors) -> AnyElement {
    let s = m.scale();
    let key = format!("page-case-{n}");
    let (mm, pal) = (*m, *palette);
    doors.say(&case.name);
    // The case itself: its name as a door to its page (the peek carries its
    // doc), or a type in words.
    let name = {
        let (case_owned, key) = (case.clone(), key.clone());
        named(format!("{key}-door"), &case.name, case.link.as_deref(), kind, doors, move |lit| {
            match case_owned.kind {
                CaseKind::Type => match case_owned.carries.first() {
                    Some(ty) => ty_lit(&format!("{key}-name"), ty, &mm, &pal, mm.reveal().xray, lit),
                    None => said(format!("{key}-name"), case_owned.name.clone(), scale::MONO_NAME, hover::ink(pal.ink0, lit, &pal), &mm),
                },
                _ => {
                    let rest = if case_owned.deprecated { pal.ink2 } else { pal.ink0 };
                    let mut el = div().child(said(format!("{key}-name"), case_owned.name.clone(), scale::MONO_NAME, hover::ink(rest, lit, &pal), &mm));
                    if case_owned.deprecated { el = el.line_through(); }
                    el.into_any_element()
                }
            }
        })
    };
    let mut row = div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().child(div().w(name_w).flex_none().flex().items_center().child(name));
    // What it carries: a printed value (a stone), its parts in words, or its
    // named fields.
    let mut payload = div().flex().items_center().gap(px(6.0 * s));
    if let Some(value) = &case.value {
        doors.say(value);
        payload = payload.child(stone(palette, s)).child(said(format!("{key}-value"), value.clone(), scale::MONO, palette.ink1, m));
    }
    if case.kind != CaseKind::Type {
        for (q, ty) in case.carries.iter().enumerate() {
            if q > 0 {
                payload = payload.child(said(format!("{key}-times-{q}"), "×", scale::BODY, palette.ink4, m));
            }
            doors.say(&ty.plain());
            let (ty, tkey, link) = (ty.clone(), format!("{key}-carries-{q}"), ty.head().filter(|tok| tok.kind == crate::anatomy::plan::TokKind::Named).map(|tok| tok.text.clone()));
            payload = payload.child(named(format!("{tkey}-door"), &ty.plain(), link.as_deref(), kind, doors, move |lit| ty_lit(&tkey, &ty, &mm, &pal, mm.reveal().xray, lit)));
        }
    }
    if !case.fields.is_empty() {
        let mut fields = div().flex().items_baseline().gap(px(12.0 * s));
        for (q, field) in case.fields.iter().enumerate() {
            let words = format!("{}{}", field.name, if field.optional { "?" } else { "" });
            doors.say(&words);
            fields = fields.child(said(format!("{key}-field-{q}"), words, scale::MONO, palette.ink2, m));
        }
        payload = payload.child(fields);
    }
    if payload_w > px(0.0) {
        row = row.child(div().w(payload_w).flex_none().child(payload));
    }
    // The accessors that read it.
    if !case.accessors.is_empty() {
        let mut list = div().flex().items_center().gap(px(10.0 * s));
        let take = if condensed { 1 } else { case.accessors.len() };
        for (q, accessor) in case.accessors.iter().take(take).enumerate() {
            doors.say(&accessor.name);
            let (akey, name, changes) = (format!("{key}-acc-{q}"), accessor.name.clone(), accessor.changes);
            let el = named(format!("{akey}-door"), &accessor.name, accessor.link.as_deref(), palette.f_call.hue.hsla(), doors, move |lit| {
                let mut el = div().flex().items_center().gap(px(4.0 * mm.scale()));
                if changes {
                    el = el.child(crate::icons::mod_mark(crate::icons::Mod::Changes, 10.0 * mm.scale(), &pal));
                }
                el.child(said(akey, name, scale::LABEL_MONO, hover::ink(pal.ink3, lit, &pal), &mm)).into_any_element()
            });
            list = list.child(el);
        }
        if condensed && case.accessors.len() > 1 {
            let words = format!("+{}", case.accessors.len() - 1);
            doors.say(&words);
            list = list.child(said(format!("{key}-acc-more"), words, scale::LABEL_MONO, palette.ink3, m));
        }
        row = row.child(list);
    }
    row.into_any_element()
}
