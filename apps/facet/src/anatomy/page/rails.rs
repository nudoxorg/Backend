//! Getting one: routes as rails. Each rail runs from what you have (in the
//! left margin when the margins carry the edges, inside the column when
//! they fold), through its step (a plate in the maker's hue, a coral `?`
//! when it can fail), into one terminal bar every route shares: the thing
//! this page is. A route that lands on one case says which; the first
//! route's code is printed past the bar. More than seven fold.

use super::fork::more_link;
use super::{Doors, Geometry, at, hue, mono_w, named, said, ty_lit, words_w};
use crate::anatomy::page::{Anchors, anchor};
use crate::anatomy::plan::{PagePlan, Part, SectionId};
use crate::hover::{self, Lit};
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, IntoElement, ParentElement, Pixels, Styled, div, px};
use std::rc::Rc;

/// Rails shown before the fold.
const FOLD_AT: usize = 7;

/// The fold key the shell keeps the rails' "and N more" under.
pub const RAILS: &str = "page-rails";

/// The rails of `plan`'s Getting one, when it has any.
pub(super) fn rails(plan: &PagePlan, geo: &Geometry, anchors: &Rc<Anchors>, m: &Measure, palette: &Palette, doors: &dyn Doors) -> Option<AnyElement> {
    if plan.getting.is_empty() {
        return None;
    }
    let s = geo.scale;
    let kind = hue(plan.hero.fam, palette).hsla();
    let call = palette.f_call.hue.hsla();
    let folded = plan.getting.len() > FOLD_AT;
    let fold = folded.then(|| doors.fold(RAILS)).flatten();
    let open = fold.as_ref().is_some_and(|fold| fold.open);
    let shown = if folded && !open { FOLD_AT } else { plan.getting.len() };
    let rails = &plan.getting[..shown];
    let plate_w = rails.iter().map(|rail| mono_w(rail.verb.chars().count() + if rail.fails { 2 } else { 0 }, scale::MONO_NAME, s)).fold(px(0.0), Pixels::max) + px(22.0 * s);
    let source_w = rails.iter().map(|rail| words_w(rail.from.plain().chars().count(), scale::BODY, s) + px(15.0 * s)).fold(px(0.0), Pixels::max);
    let margins = geo.margins() && source_w + px(40.0 * s) <= geo.reach + (geo.col - geo.spine);
    let pitch = px(rhythm::ROW_TIGHT * s);
    let row_of = |n: usize, rail: &crate::anatomy::plan::Rail| -> AnyElement {
        let key = format!("page-rail-{n}");
        doors.say(&rail.from.plain());
        doors.say(&rail.verb);
        let (mm, pal) = (*m, *palette);
        // What you have: in the margin, right-aligned to the spine, or at
        // the head of the row.
        let from = {
            let (ty, tkey) = (rail.from.clone(), format!("{key}-from"));
            ty_lit(&tkey, &ty, &mm, &pal, mm.reveal().xray, Lit::Rest)
        };
        let from = anchor(at(SectionId::Getting, Part::Row(n as u16)), anchors, from);
        let mut row = div().relative().h(pitch).flex().items_center();
        if margins {
            row = row.child(div().absolute().right(geo.col_w + (geo.col - geo.spine) + px(16.0 * s)).top_0().h(pitch).flex().items_center().child(from));
        } else {
            row = row.child(div().w(source_w + px(20.0 * s)).flex_none().flex().items_center().child(from));
        }
        // The step: a plate in the maker's hue, a door to the maker.
        let (verb, fails, vkey) = (rail.verb.clone(), rail.fails, format!("{key}-verb"));
        let step = named(format!("{key}-door"), &rail.verb, rail.link.as_deref(), call, doors, move |lit| {
            let mut plate = div().h(px(22.0 * mm.scale())).px(px(9.0 * mm.scale())).flex().items_center().gap(px(3.0 * mm.scale()))
                .child(said(vkey.clone(), verb, scale::MONO_NAME, hover::ink(pal.ink1, lit, &pal), &mm));
            if fails {
                plate = plate.child(said(format!("{vkey}-fails"), "?", scale::MONO_NAME, pal.coral.base, &mm));
            }
            plate.into_any_element()
        });
        row = row.child(div().w(plate_w).flex_none().flex().items_center().child(anchor(at(SectionId::Getting, Part::Rail(n as u16)), anchors, step)));
        // Past the terminal bar: the case it lands on, and the first route's
        // code.
        let mut past = div().flex().items_center().gap(px(14.0 * s)).pl(px(40.0 * s));
        if let Some(lands) = &rail.lands {
            doors.say(lands);
            past = past.child(said(format!("{key}-lands"), lands.clone(), scale::MONO_NAME, palette.ink0, m));
        }
        if n == 0 && let Some(code) = &rail.code {
            doors.say(code);
            past = past.child(said(format!("{key}-code"), code.clone(), scale::MONO, palette.ink3, m));
        }
        let terminal = if n == 0 { Some(anchor(at(SectionId::Getting, Part::Terminal), anchors, div().w(px(0.0)).h(pitch))) } else { None };
        row.children(terminal).child(past).into_any_element()
    };
    let mut column = div().flex().flex_col().relative();
    for (n, rail) in plan.getting.iter().enumerate().take(if folded { FOLD_AT } else { plan.getting.len() }) {
        column = column.child(row_of(n, rail));
    }
    if let (Some(fold), true) = (&fold, folded) {
        let tail = if fold.open || !fold.presence.is_empty() {
            plan.getting.iter().enumerate().skip(FOLD_AT).fold(div().flex().flex_col(), |tail, (n, rail)| tail.child(row_of(n, rail)))
        } else {
            div()
        };
        column = column.child(crate::anatomy::unroll::unroll("page-rails-rest", fold.open, fold.presence.clone(), tail));
    }
    if let (true, false) = (folded, open) {
        let words = format!("and {} more", plan.getting.len() - FOLD_AT);
        doors.say(&words);
        let toggle = fold.as_ref().map(|fold| Rc::clone(&fold.toggle));
        let mut more = div().h(pitch).flex().items_center();
        if !margins {
            more = more.pl(source_w + px(20.0 * s));
        }
        column = column.child(anchor(at(SectionId::Getting, Part::More), anchors, more.child(more_link("page-rails-more", words, toggle, kind, *m, *palette, doors))));
    }
    Some(column.into_any_element())
}
