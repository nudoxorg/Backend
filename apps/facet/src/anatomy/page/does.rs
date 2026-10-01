//! What it does: its own operations grouped by what they do to it (reads
//! it, changes it, uses it up), one row each at 24 px (a name and what it
//! gives back, in words), side by side when the margins carry the edges;
//! then its capabilities, a diamond and a verb each, the usual ones one
//! mark. Makers are Getting one's rails; accessors sit on their tines.

use super::fork::more_link;
use crate::anatomy::plan::Fam;
use crate::anatomy::sigil::{Form, Sigil, sigil};
use super::{Doors, FOLD_AT, Geometry, folds, mono_w, named, said, ty_lit, words_w};
use crate::anatomy::plan::Ty;
use crate::hover::{self, Lit};
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, InteractiveElement, IntoElement, ParentElement, Pixels, StatefulInteractiveElement, Styled, div, px};
use std::rc::Rc;


/// One operation.
#[derive(Clone, Debug)]
pub struct DoesRow {
    /// Its name.
    pub name: String,
    /// What it gives back, in words.
    pub gives: Option<Ty>,
    /// It can fail.
    pub fails: bool,
    /// Its address.
    pub link: Option<String>,
    /// How many inputs it takes (its sigil's prongs; the receiver is not one).
    pub ins: u8,
}

/// Operations that do one thing to it.
#[derive(Clone, Debug)]
pub struct DoesGroup {
    /// What they do (`reads it`).
    pub words: &'static str,
    /// Its mark.
    pub mark: crate::icons::Mod,
    /// The fold key the shell keeps its "and N more" under.
    pub fold: &'static str,
    /// The operations.
    pub rows: Vec<DoesRow>,
}

/// A capability: its verb, and whether it is one of the usual ones.
#[derive(Clone, Debug)]
pub struct Capability {
    /// The verb (`prints`, `converts from`).
    pub word: String,
    /// Copy, Clone, Debug and the like: folded into one mark.
    pub usual: bool,
}

/// A group of more than this many operations is a stack at rest (one row
/// is just the row).
pub const STACK_FROM: usize = 1;

/// The body of What it does.
#[must_use]
pub fn does(groups: &[DoesGroup], caps: &[Capability], geo: &Geometry, m: &Measure, palette: &Palette, doors: &dyn Doors) -> Option<AnyElement> {
    if groups.iter().all(|group| group.rows.is_empty()) && caps.is_empty() {
        return None;
    }
    let s = m.scale();
    let groups = groups.iter().filter(|group| !group.rows.is_empty()).collect::<Vec<_>>();
    let columns = geo.margins() && groups.len() > 1;
    let gap = px(24.0 * s);
    let column_w = if columns { (geo.col_w - gap * (groups.len() - 1) as f32) / groups.len() as f32 } else { geo.col_w };
    let kind = palette.f_call.hue.hsla();
    let mut grid = if columns { div().flex().items_start().gap(gap) } else { div().flex().flex_col().gap(px(rhythm::GROUP * s)) };
    for group in groups {
        doors.say(group.words);
        let mut column = div().flex().flex_col().w(column_w).flex_none();
        column = column.child(
            div().h(px(20.0 * s)).flex().items_center().gap(px(6.0 * s))
                .child(crate::icons::mod_mark(group.mark, 11.0 * s, palette))
                .child(said(format!("page-does-{}", group.words), group.words, scale::LABEL, palette.ink3, m)),
        );
        let stacked = group.rows.len() > STACK_FROM;
        let fold = (stacked || folds(group.rows.len())).then(|| doors.fold(group.fold)).flatten();
        let open = fold.as_ref().is_some_and(|fold| fold.open);
        if stacked && !open {
            // A group of more than three is a stack: its marks overlapped and
            // a count; resting on it spreads the names in place, a click
            // opens the whole list.
            grid = grid.child(stack(group, column_w, kind, fold, m, palette, doors));
            continue;
        }
        let shown = if folds(group.rows.len()) && !open { FOLD_AT } else { group.rows.len() };
        for (n, row) in group.rows.iter().take(shown).enumerate() {
            column = column.child(row_element(group.words, n, row, column_w, kind, m, palette, doors));
        }
        if folds(group.rows.len()) && !open {
            let words = format!("and {} more", group.rows.len() - FOLD_AT);
            doors.say(&words);
            let toggle = fold.as_ref().map(|fold| Rc::clone(&fold.toggle));
            column = column.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center()
                .child(more_link(&format!("page-does-{}-more", group.words), words, toggle, kind, *m, *palette, doors)));
        }
        grid = grid.child(column);
    }
    let mut body = div().flex().flex_col().gap(px(rhythm::GROUP * s)).child(grid);
    if !caps.is_empty() {
        let (usual, special): (Vec<_>, Vec<_>) = caps.iter().partition(|cap| cap.usual);
        let mut row = div().flex().flex_wrap().gap_x(px(22.0 * s));
        let diamond = |color| crate::icons::ui(crate::icons::Icon::Diamond, crate::icons::IconSize::S12, color).size(m.icon(11.0));
        for (n, cap) in special.iter().enumerate() {
            doors.say(&cap.word);
            row = row.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(7.0 * s))
                .child(diamond(palette.f_con.hue))
                .child(said(format!("page-cap-{n}"), cap.word.clone(), scale::BODY, palette.ink1, m)));
        }
        if !usual.is_empty() {
            let words = format!("and the usual {}", usual.len());
            doors.say(&words);
            row = row.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(7.0 * s))
                .child(diamond(palette.ink3))
                .child(said("page-cap-usual", words, scale::BODY, palette.ink3, m)));
        }
        body = body.child(row);
    }
    Some(body.into_any_element())
}

/// A group as a stack: the heading, the operations' sigils overlapped (at
/// most five) and their count. Resting on it spreads them into named tokens
/// under the heading, in place (the group grows to hold them); a click opens
/// the whole list.
fn stack(group: &DoesGroup, column_w: Pixels, kind: gpui::Hsla, fold: Option<super::Fold>, m: &Measure, palette: &Palette, doors: &dyn Doors) -> AnyElement {
    use gpui::{InteractiveElement, StatefulInteractiveElement};
    let s = m.scale();
    let name = format!("page-does-{}-stack", group.words);
    let mark_of = |row: &DoesRow| sigil(Sigil { ins: row.ins, gives: row.gives.is_some(), fails: row.fails, recv: true, ..Sigil::new(Form::Method, Fam::Callable) }, 16.0 * s, palette);
    doors.say(group.words);
    let count = group.rows.len().to_string();
    doors.say(&count);
    for row in &group.rows {
        doors.say(&row.name);
    }
    // Everything the open stack needs, decided now (the build closure runs in
    // layout, where the doors are not at hand): a door per operation.
    let tokens: Vec<AnyElement> = group.rows.iter().enumerate().map(|(n, row)| {
        let key = format!("{name}-{n}");
        let (mm, pal, label, name_key) = (*m, *palette, row.name.clone(), format!("{key}-name"));
        let mark = mark_of(row);
        named(format!("{key}-door"), &row.name, row.link.as_deref(), kind, doors, move |lit| {
            div().h(px(rhythm::ROW_PITCH * mm.scale())).flex().items_center().gap(px(5.0 * mm.scale()))
                .child(mark)
                .child(said(name_key, label, scale::MONO, hover::ink(pal.ink1, lit, &pal), &mm))
                .into_any_element()
        })
    }).collect();
    let marks: Vec<AnyElement> = group.rows.iter().take(5).map(|row| mark_of(row)).collect();
    let (words, mark, pal, mm) = (group.words, group.mark, *palette, *m);
    let toggle = fold.map(|fold| Rc::clone(&fold.toggle));
    let key = gpui::SharedString::from(name.clone());
    super::lazy::spreads(key.clone(), move |open| {
        let mut overlapped = div().flex().items_center().pl(px(7.0 * s));
        for (n, mark) in marks.into_iter().enumerate() {
            overlapped = overlapped.child(div().ml(px(if n == 0 { 0.0 } else { -6.0 * s })).child(mark));
        }
        let head = div().h(px(20.0 * s)).flex().items_center().gap(px(6.0 * s))
            .child(crate::icons::mod_mark(mark, 11.0 * s, &pal))
            .child(said(format!("page-does-{words}"), words, scale::LABEL, pal.ink3, &mm))
            .child(overlapped)
            .child(said(format!("{key}-count"), count, scale::LABEL_MONO, pal.ink1, &mm));
        let mut column = div().id(key.clone()).flex().flex_col().w(column_w).flex_none().cursor_pointer().child(head);
        if open {
            let mut spread = div().flex().flex_wrap().items_center().gap_x(px(12.0 * s)).gap_y(px(2.0 * s)).w(column_w).pt(px(4.0 * s));
            for token in tokens {
                spread = spread.child(token);
            }
            column = column.child(spread);
        }
        if let Some(toggle) = toggle {
            column = column.on_click(move |_, window, cx| toggle(window, cx));
        }
        column.into_any_element()
    })
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn row_element(group: &str, n: usize, row: &DoesRow, column_w: Pixels, kind: gpui::Hsla, m: &Measure, palette: &Palette, doors: &dyn Doors) -> AnyElement {
    let s = m.scale();
    let key = format!("page-does-{group}-{n}");
    doors.say(&row.name);
    let (mm, pal, label, name_key) = (*m, *palette, row.name.clone(), format!("{key}-name"));
    let name = named(format!("{key}-door"), &row.name, row.link.as_deref(), kind, doors, move |lit| {
        said(name_key, label, scale::MONO, hover::ink(pal.ink1, lit, &pal), &mm)
    });
    let mut el = div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(10.0 * s)).child(name);
    // What it gives back, when it fits beside the name on one row.
    if let Some(ty) = row.gives.as_ref().filter(|ty| !ty.toks.is_empty()) {
        let need = mono_w(row.name.chars().count(), scale::MONO, s)
            + words_w(ty.plain().chars().count() + if row.fails { 3 } else { 0 }, scale::BODY, s)
            + px(30.0 * s);
        if need <= column_w {
            doors.say(&ty.plain());
            el = el.child(ty_lit(&format!("{key}-gives"), ty, m, palette, m.reveal().xray, Lit::Rest));
            if row.fails {
                el = el.child(said(format!("{key}-fails"), "?", scale::MONO_NAME, palette.coral.base, m));
            }
        }
    }
    el.into_any_element()
}
