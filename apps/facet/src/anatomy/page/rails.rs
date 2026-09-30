//! Getting one: routes as rails. Each rail runs from what you have (in the
//! left margin when the margins carry the edges, inside the column when
//! they fold), through its step (a plate in the maker's hue, a coral `?`
//! when it can fail), into one terminal bar every route shares: the thing
//! this page is. A route that lands on one case says which; the first
//! route's code is printed past the bar. More than seven fold.

use super::fork::more_link;
use super::{Doors, FOLD_AT, Geometry, at, folds, hue, mono_w, named, said, ty_lit, words_w};
use crate::anatomy::page::{Anchors, anchor};
use crate::anatomy::plan::{PagePlan, Part, SectionId};
use crate::hover::{self, Lit};
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, ParentElement, PathBuilder, Pixels, Refineable, Style, StyleRefinement, Styled, Window, div,
    point, px,
};
use std::rc::Rc;


/// The fold key the shell keeps the rails' "and N more" under.
pub const RAILS: &str = "page-rails";

/// Whether Getting one can carry the band (what it does, to the right of the
/// terminal): every route lands on the same bar, and what you have fits the
/// margin (so the routes hold the left of the column and the band the right).
pub fn band_possible(plan: &PagePlan, geo: &Geometry) -> bool {
    if plan.getting.is_empty() || plan.getting.iter().any(|rail| rail.lands.is_some()) || plan.getting.first().is_some_and(|rail| rail.code.is_some()) {
        return false;
    }
    let s = geo.scale;
    let source_w = plan.getting.iter().map(|rail| words_w(rail.from.plain().chars().count(), scale::BODY, s) + px(15.0 * s)).fold(px(0.0), Pixels::max);
    geo.margins() && source_w + px(40.0 * s) <= geo.reach - (geo.col - geo.spine)
}

/// The rails of `plan`'s Getting one, when it has any.
pub(super) fn rails(plan: &PagePlan, geo: &Geometry, anchors: &Rc<Anchors>, m: &Measure, palette: &Palette, doors: &dyn Doors, band: Option<AnyElement>) -> Option<AnyElement> {
    if plan.getting.is_empty() {
        return None;
    }
    let s = geo.scale;
    let kind = hue(plan.hero.fam, palette).hsla();
    let call = palette.f_call.hue.hsla();
    let folded = folds(plan.getting.len());
    let fold = folded.then(|| doors.fold(RAILS)).flatten();
    let open = fold.as_ref().is_some_and(|fold| fold.open);
    let shown = if folded && !open { FOLD_AT } else { plan.getting.len() };
    let rails = &plan.getting[..shown];
    let plate_w = rails.iter().map(|rail| mono_w(rail.verb.chars().count() + if rail.fails { 2 } else { 0 }, scale::MONO_NAME, s)).fold(px(0.0), Pixels::max) + px(22.0 * s);
    let source_w = rails.iter().map(|rail| words_w(rail.from.plain().chars().count(), scale::BODY, s) + px(15.0 * s)).fold(px(0.0), Pixels::max);
    let margins = geo.margins() && source_w + px(40.0 * s) <= geo.reach - (geo.col - geo.spine);
    let pitch = px(rhythm::ROW_TIGHT * s);
    // With the band the rails hold only the left of the column; a row's
    // margin words hang off its own right edge.
    let has_band = band.is_some() && band_possible(plan, geo);
    let lands_w = mono_w(plan.hero.name.chars().count(), scale::MONO_NAME, s) + px(24.0 * s);
    let band_left = plate_w + px(40.0 * s) + lands_w + px(56.0 * s);
    let row_w = if has_band { band_left } else { geo.col_w };
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
            row = row.child(div().absolute().right(row_w + (geo.col - geo.spine) + px(16.0 * s)).top_0().h(pitch).flex().items_center().child(from));
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
    // The band: what it does, to the right of the thing this page is. Only
    // where every route lands on the same bar and the margins carry the edges.
    let band = band.filter(|_| has_band);
    let mut column = div().flex().flex_col().relative();
    if band.is_some() {
        column = column.w(band_left).flex_none();
    }
    let rows = if folded { FOLD_AT } else { plan.getting.len() };
    for (n, rail) in plan.getting.iter().enumerate().take(rows) {
        column = column.child(row_of(n, rail));
    }
    // Every route lands on the thing this page is: said once, past the
    // terminal bar, level with its middle (unless a route names the case it
    // lands on, or prints its code there).
    if plan.getting.iter().all(|rail| rail.lands.is_none()) && plan.getting.first().is_none_or(|rail| rail.code.is_none()) {
        let name = plan.hero.name.clone();
        doors.say(&name);
        let lead = if margins { px(0.0) } else { source_w + px(20.0 * s) };
        let mark = super::glyph(&crate::anatomy::plan::Tok { kind: crate::anatomy::plan::TokKind::Named, text: String::new(), fam: plan.hero.fam }, crate::anatomy::plan::Wrap::Plain, palette, s);
        column = column.child(
            div()
                .absolute()
                .left(lead + plate_w + px(40.0 * s))
                .top(pitch * (rows as f32 / 2.0) - px(10.0 * s))
                .h(px(20.0 * s))
                .flex()
                .items_center()
                .gap(px(6.0 * s))
                .child(mark)
                .child(said("page-rails-lands", name, scale::MONO_NAME, palette.ink1, m)),
        );
    }
    if band.is_some() {
        // From the plate to what it does: a hairline and a chevron.
        let start = plate_w + px(40.0 * s) + lands_w + px(6.0 * s);
        column = column.child(div().absolute().left(start).top(pitch * (rows as f32 / 2.0) - px(5.0 * s)).w(band_left - start - px(6.0 * s)).h(px(10.0 * s)).child(connector(kind, s)));
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
    match band {
        Some(band) => Some(
            div().flex().items_start().child(column).child(div().flex().flex_col().justify_center().min_h(pitch * (rows as f32)).child(band)).into_any_element(),
        ),
        None => Some(column.into_any_element()),
    }
}

/// A hairline with a chevron at its end, filling its box.
fn connector(color: gpui::Hsla, s: f32) -> AnyElement {
    RailConnector { color: color.opacity(0.7), scale: s, style: StyleRefinement::default() }
        .size_full()
        .into_any_element()
}

struct RailConnector {
    color: Hsla,
    scale: f32,
    style: StyleRefinement,
}

impl Styled for RailConnector {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for RailConnector {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for RailConnector {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Style, _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, style: &mut Style, _: &mut (), window: &mut Window, cx: &mut App) {
        let (scale, color) = (self.scale, self.color);
        style.paint(bounds, window, cx, |window, _| {
            let y = bounds.origin.y + bounds.size.height / 2.0;
            let (x0, x1) = (bounds.origin.x, bounds.origin.x + bounds.size.width);
            let h = px(3.5 * scale);
            let mut path = PathBuilder::stroke(px(1.0));
            path.move_to(point(x0, y));
            path.line_to(point(x1, y));
            path.move_to(point(x1 - h, y - h));
            path.line_to(point(x1, y));
            path.line_to(point(x1 - h, y + h));
            if let Ok(path) = path.build() { window.paint_path(path, color); }
        });
    }
}
