//! The bodies of the sections below Getting one, held to the page's laws:
//! one row per member at 24 px, a doc only in the hover's peek, a group of
//! more than seven folded, counts on the stubs and never in prose.
//!
//! CP1 keeps these plain (DESIGN §11 S8 draws the failure tree, the
//! call-site strip and the comb); what is here already replaces the tabs,
//! the ledgers and the relation lists.

use super::presentation::{WorldPacket, failure_words, is_failure};
use super::{Ctx, Reader};
use crate::model::pages::{Receiver, SymbolPage};
use crate::runtime::fixture_world::Anatomy;
use crate::shell::reader::SymbolFold;
use facet::Set as _;
use facet::anatomy::page::{Doors, Geometry, more_link, named, said, ty_lit};
use facet::anatomy::plan::{self, PagePlan, Spec};
use facet::hover::Lit;
use facet::tokens::{rhythm, scale};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use std::collections::BTreeSet;
use std::rc::Rc;

/// Rows shown before a group folds.
const FOLD_AT: usize = 7;

const fn group_words(receiver: Receiver) -> Option<(&'static str, facet::icons::Mod)> {
    match receiver {
        Receiver::Reads => Some(("reads it", facet::icons::Mod::Reads)),
        Receiver::Changes => Some(("changes it", facet::icons::Mod::Changes)),
        Receiver::Consumes => Some(("uses it up", facet::icons::Mod::Consumes)),
        Receiver::Makes | Receiver::Unknown => None,
    }
}

/// What it does: its own operations by what they do to it (makers are in
/// Getting one, accessors on their tines), then its capabilities as marks.
pub(super) fn does(page: &SymbolPage, plan: &PagePlan, anatomy: Option<&Anatomy>, geo: &Geometry, ctx: &Ctx<'_>, doors: &dyn Doors, cx: &mut Context<Reader>) -> Option<AnyElement> {
    let m = ctx.measure;
    let palette = ctx.palette;
    let s = m.scale();
    let on_tines = match &plan.spec {
        Spec::Choice(choice) => choice.cases.iter().flat_map(|case| case.accessors.iter().map(|a| a.name.clone())).collect::<BTreeSet<_>>(),
        _ => BTreeSet::new(),
    };
    let lang = facet::semantics::recorded::Language::from_name(page.identity.language.name());
    let members = page.members.known()?;
    let groups = members
        .does
        .iter()
        .filter_map(|group| {
            let (words, mark) = group_words(group.receiver)?;
            let rows = group.members.iter().filter(|member| !on_tines.contains(member.decl.name.as_ref())).collect::<Vec<_>>();
            (!rows.is_empty()).then_some((group.receiver, words, mark, rows))
        })
        .collect::<Vec<_>>();
    let columns = geo.margins() && groups.len() > 1;
    let column_w = if columns { (geo.col_w - px(24.0 * s) * (groups.len() - 1) as f32) / groups.len() as f32 } else { geo.col_w };
    let mut grid = if columns { div().flex().items_start().gap(px(24.0 * s)) } else { div().flex().flex_col().gap(px(rhythm::GROUP * s)) };
    let kind = palette.f_call.hue.hsla();
    for (receiver, words, mark, rows) in groups {
        doors.say(words);
        let mut column = div().flex().flex_col().w(column_w).flex_none();
        column = column.child(
            div().h(px(20.0 * s)).flex().items_center().gap(px(6.0 * s))
                .child(facet::icons::mod_mark(mark, 11.0 * s, palette))
                .child(said(format!("page-does-{words}"), words, scale::LABEL, palette.ink3, &m)),
        );
        let fold_key = SymbolFold::Methods(receiver);
        let open = ctx.symbol_disclosure.is_open(&fold_key);
        let shown = if rows.len() > FOLD_AT && !open { FOLD_AT } else { rows.len() };
        for (n, member) in rows.iter().take(shown).enumerate() {
            let name = member.decl.name.to_string();
            doors.say(&name);
            // What it gives back, in words, when the signature says.
            let gives = member.signature.known().and_then(|sig| facet::semantics::recorded::callable(&sig.text, &name, lang)).and_then(|pipe| {
                let output = pipe.output.as_ref().map(|out| plan::spelled(&out.pieces, &out.source));
                output.map(|ty| (ty, pipe.fails.is_some()))
            });
            let key = format!("page-does-{words}-{n}");
            let (mm, pal, label) = (m, *palette, name.clone());
            let name_el = named(format!("{key}-door"), &name, Some(member.decl.coordinate.as_str()), kind, doors, move |lit| {
                said(format!("{key}-name"), label, scale::MONO, facet::hover::ink(pal.ink1, lit, &pal), &mm)
            });
            let mut row = div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(10.0 * s)).overflow_hidden().child(name_el);
            let name_w = facet::anatomy::page::mono_w(name.chars().count(), scale::MONO, s);
            if let Some((ty, fails)) = gives
                && !ty.toks.is_empty()
            {
                let words_w = facet::anatomy::page::words_w(ty.plain().chars().count() + if fails { 3 } else { 0 }, scale::BODY, s) + px(15.0 * s);
                if name_w + words_w + px(10.0 * s) <= column_w {
                    doors.say(&ty.plain());
                    row = row.child(ty_lit(&format!("page-does-{words}-{n}-gives"), &ty, &m, palette, m.reveal().xray, Lit::Rest));
                    if fails {
                        row = row.child(said(format!("page-does-{words}-{n}-fails"), "?", scale::MONO_NAME, palette.coral.base, &m));
                    }
                }
            }
            column = column.child(row);
        }
        if rows.len() > FOLD_AT && !open {
            let words_more = format!("and {} more", rows.len() - FOLD_AT);
            doors.say(&words_more);
            let weak = cx.weak_entity();
            let symbol = page.identity.coordinate.clone();
            let toggle: Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)> = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |reader, cx| reader.toggle_symbol(symbol.clone(), SymbolFold::Methods(receiver), cx));
            });
            column = column.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center()
                .child(more_link(&format!("page-does-{words}-more"), words_more, Some(toggle), kind, m, *palette, doors)));
        }
        grid = grid.child(column);
    }
    // Capabilities: a diamond and a verb each; the usual ones are one mark.
    let mut body = div().flex().flex_col().gap(px(rhythm::GROUP * s)).child(grid);
    if let Some(anatomy) = anatomy.filter(|anatomy| !anatomy.page.caps.is_empty()) {
        const USUAL: [&str; 13] = ["Copy", "Clone", "Debug", "Display", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "Default", "ToString", "ToOwned", "Borrow"];
        let (usual, special): (Vec<_>, Vec<_>) = anatomy.page.caps.iter().partition(|cap| USUAL.contains(&cap.trait_name.as_ref()));
        let mut caps = div().flex().flex_wrap().gap_x(px(22.0 * s)).gap_y(px(4.0 * s));
        for (n, cap) in special.iter().enumerate() {
            doors.say(&cap.word);
            caps = caps.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(7.0 * s))
                .child(facet::icons::ui(facet::icons::Icon::Diamond, facet::icons::IconSize::S12, palette.f_con.hue).size(m.icon(11.0)))
                .child(said(format!("page-cap-{n}"), cap.word.to_string(), scale::BODY, palette.ink1, &m)));
        }
        if !usual.is_empty() {
            let words = format!("and the usual {}", usual.len());
            doors.say(&words);
            caps = caps.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(7.0 * s))
                .child(facet::icons::ui(facet::icons::Icon::Diamond, facet::icons::IconSize::S12, palette.ink3).size(m.icon(11.0)))
                .child(said("page-cap-usual", words, scale::BODY, palette.ink3, &m)));
        }
        body = body.child(caps);
    }
    Some(body.into_any_element())
}

/// What can go wrong: what the docs say fails or panics, and the failure
/// the world traced.
pub(super) fn fails(page: &SymbolPage, anatomy: Option<&Anatomy>, packet: Option<&WorldPacket>, ctx: &Ctx<'_>, doors: &dyn Doors) -> Option<AnyElement> {
    let m = ctx.measure;
    let palette = ctx.palette;
    let s = m.scale();
    let mut rows = page.sections.sections.iter().filter(|section| is_failure(section.kind)).map(|section| (None, section)).collect::<Vec<_>>();
    if let Some(members) = page.members.known() {
        rows.extend(members.all().flat_map(|member| member.sections.sections.iter().filter(|s| is_failure(s.kind)).map(move |s| (Some(member.decl.name.to_string()), s))));
    }
    let mut body = div().flex().flex_col().gap(px(8.0 * s));
    for (n, (member, section)) in rows.iter().take(FOLD_AT).enumerate() {
        let verb = match section.kind {
            crate::model::pages::SectionKind::Errors => "fails",
            crate::model::pages::SectionKind::Panics => "panics",
            _ => "you promise",
        };
        let words = super::plain_markup(&failure_words(section));
        doors.say(verb);
        doors.say(&words);
        let mut row = div().flex().items_baseline().gap(px(12.0 * s));
        if let Some(member) = member {
            doors.say(member);
            row = row.child(said(format!("page-fails-{n}-member"), member.clone(), scale::MONO, palette.ink1, &m));
        }
        row = row
            .child(said(format!("page-fails-{n}-verb"), verb, scale::LABEL, palette.coral.base, &m))
            .child(div().flex_1().min_w_0().child(facet::probe::text(
                gpui::ElementId::Name(format!("page-fails-{n}-words").into()),
                gpui::SharedString::from(words.clone()),
                m.role(scale::BODY),
                1.0,
                facet::probe::TextOverflow::Wrap,
                div().set(scale::BODY, &m).text_color(palette.ink2.hsla()).child(words),
            )));
        body = body.child(row);
    }
    if let (Some(anatomy), Some(packet)) = (anatomy, packet) {
        let links = crate::runtime::fixture_world::links(&anatomy.world);
        if let Some(section) = &packet.failure {
            body = body.child(facet::anatomy::fails::fails("symbol-traced-failures", section.clone(), anatomy.world.clone(), &m, &links));
        }
    }
    Some(body.into_any_element())
}

/// Who uses it: the real statements callers write (the world's), then the
/// index's use sites, three at most; the count is the stub's.
pub(super) fn uses(page: &SymbolPage, anatomy: Option<&Anatomy>, ctx: &Ctx<'_>, doors: &dyn Doors) -> Option<AnyElement> {
    let m = ctx.measure;
    let palette = ctx.palette;
    let s = m.scale();
    if let Some(anatomy) = anatomy.filter(|anatomy| !anatomy.uses.is_empty()) {
        let links = crate::runtime::fixture_world::links(&anatomy.world);
        for (_, caller) in &anatomy.uses {
            doors.say(caller);
        }
        return Some(facet::anatomy::in_use("anatomy-in-use", anatomy.uses.clone(), &m, &links).into_any_element());
    }
    let sites = page.references.known().filter(|sites| !sites.is_empty())?;
    let mut body = div().flex().flex_col();
    let kind = palette.f_call.hue.hsla();
    for (n, site) in sites.iter().take(3).enumerate() {
        let name = site.site.name.to_string();
        doors.say(&name);
        let key = format!("page-use-{n}");
        let (mm, pal, label) = (m, *palette, name.clone());
        let door = named(format!("{key}-door"), &name, Some(site.site.coordinate.as_str()), kind, doors, move |lit| {
            said(format!("{key}-name"), label, scale::MONO, facet::hover::ink(pal.ink1, lit, &pal), &mm)
        });
        let place = site.span.known().map(|span| span.file.to_string()).unwrap_or_default();
        doors.say(&place);
        body = body.child(div().h(px(rhythm::ROW_PITCH * s)).flex().items_center().gap(px(12.0 * s)).child(door)
            .child(said(format!("page-use-{n}-place"), place, scale::LABEL_MONO, palette.ink3, &m)));
    }
    Some(body.into_any_element())
}
