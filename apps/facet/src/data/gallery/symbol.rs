//! The symbol page's head and rose band (calm `v4/shots/SymbolPage.png`,
//! `SymbolPage-rose.png`, `flow-*.png`): hero, facts line, lens bar, the
//! signature (a stand-in for the page's own), and the rose — drawn at the
//! reader widths of a 1440, 1100, 760 and 480 window and at 200 % text.

use super::super::{Dir, Member, Run, Tab, facts, lens_bar, rose, spell};
use crate::icons::Kind;
use crate::overlay::lens::{self, Lens, LensItem};
use crate::paint::gem;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use crate::{Set, Typeset};
use gpui::{AnyElement, App, Hsla, IntoElement, ParentElement, Styled, div, px};

const HERO_NAME: TypeRole = TypeRole {
    face: Face::Display,
    weight: 640.0,
    size: 44.0,
    line: 46.0,
    tracking: -0.035,
    italic: false,
};
const CODE: TypeRole = TypeRole {
    size: 13.0,
    line: 22.0,
    ..ty::CODE
};

/// The rose's data: RelationLabel and what it touches.
pub(crate) fn members(dir: Dir) -> Vec<Member> {
    match dir {
        Dir::Is => vec![
            Member::new("Display", Kind::Trait),
            Member::new("ToString", Kind::Trait).via(),
        ],
        Dir::MadeOf => vec![
            Member::new("Typed", Kind::Variant),
            Member::new("Neighbourhood", Kind::Variant),
            Member::new("Related", Kind::Variant),
        ],
        Dir::From => vec![
            Member::new("relation_label", Kind::Function),
            Member::new("From<LinkKind>", Kind::Trait),
            Member::new("Page::relations", Kind::Method),
        ],
        Dir::To => vec![
            Member::new("group_head", Kind::Function),
            Member::new("typed_heading", Kind::Function),
            Member::new("as_str", Kind::Method),
            Member::new("into_heading", Kind::Method),
        ],
    }
}

fn direction_lens(d: usize) -> Option<Lens> {
    let dir = Dir::of(d)?;
    let items: Vec<LensItem> = members(dir)
        .into_iter()
        .map(|m| LensItem::new(m.name).kind(m.kind))
        .collect();
    let (unit, yours) = match dir {
        Dir::Is => ("traits", Vec::new()),
        Dir::MadeOf => ("variants", Vec::new()),
        Dir::From => (
            "ways in",
            vec![("your code builds it in ".into(), false), ("6".into(), true), (" places".into(), false)],
        ),
        Dir::To => (
            "ways out",
            vec![("your code reads it through ".into(), false), ("as_str".into(), true)],
        ),
    };
    Some(Lens::direction(dir, unit, items, yours))
}

/// The folio for a reader `width` px wide; `rested` shows a rose part rested.
pub(crate) fn folio(width: f32, rested: Option<usize>, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let scale = facet.text_scale;
    // .cfol: max-width 880, padding clamp(22, 5cqi, 64) clamp(16, 4cqi, 48),
    // resolved on the reader's effective width.
    let eff = width / scale;
    let cqi = eff / 100.0;
    let pad_x = (4.0 * cqi).clamp(16.0, 48.0) * scale;
    let pad_y = (5.0 * cqi).clamp(22.0, 64.0) * scale;
    let column = (width.min(880.0 * scale) - pad_x * 2.0).max(100.0);
    let m = facet.measure(px(column));

    let hero = div()
        .flex()
        .items_center()
        .gap(px((2.0 * cqi).clamp(14.0, 22.0) * scale))
        .child(gem(Kind::Enum).size(64.0 * scale).state(crate::paint::GemState::Normal))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().set(HERO_NAME, &m).text_color(palette.ink0.hsla()).child("RelationLabel"))
                .child(
                    div()
                        .mt(px(6.0 * scale))
                        .set(ty::LEDE, &m)
                        .text_color(palette.ink2.hsla())
                        .child("The readable label of one relation group."),
                ),
        );
    let line = facts(&m, palette)
        .fact([Run::Words("enum in ".into()), Run::Mono("present::glyph".into())])
        .fact([Run::Words("since ".into()), Run::Mono("0.3.0".into())])
        .fact([Run::Yours("9".into()), Run::Words(" uses in your code".into())]);
    let tabs = lens_bar(
        "tabs",
        vec![
            Tab::new("Reference"),
            Tab::new("Relations").count("19"),
            Tab::new("Usage").count("9"),
            Tab::new("History").count("3"),
            Tab::new("Source"),
        ],
        0,
        &m,
    );
    let syn = palette.syntax;
    let code_lines: Vec<Vec<(&str, Hsla)>> = vec![
        vec![("#[derive(Clone, Copy, Debug, Eq, Hash)]", palette.ink3.into())],
        vec![
            ("pub enum ", syn.keyword.into()),
            ("RelationLabel", syn.type_name.into()),
            (" {", palette.ink3.into()),
        ],
        vec![
            ("    Typed", syn.constant.into()),
            ("(", palette.ink3.into()),
            ("SemanticLinkKind", syn.type_name.into()),
            (", ", palette.ink3.into()),
            ("RelationDirection", syn.type_name.into()),
            ("),", palette.ink3.into()),
        ],
        vec![("    Neighbourhood", syn.constant.into()), (",", palette.ink3.into())],
        vec![("    Related", syn.constant.into()), (",", palette.ink3.into())],
        vec![("}", palette.ink3.into())],
    ];
    let mut code = div()
        .flex()
        .flex_col()
        .px(px(18.0 * scale))
        .py(px(16.0 * scale))
        .bg(palette.inset);
    for runs in code_lines {
        let mut l = spell(&m).nowrap();
        for (text, color) in runs {
            l = l.text(text, CODE, color);
        }
        code = code.child(l);
    }

    let mut r = rose("rose", Kind::Enum, &m)
        .rest(rested)
        .door(lens::door(direction_lens))
        .member_door(crate::data::Door::peek(|part, m, w, cx| {
            let all: [std::rc::Rc<[Member]>; 4] = Dir::ALL.map(|d| std::rc::Rc::from(members(d)));
            let Some((dir, Some(i))) = crate::data::Rose::part(&all, part) else {
                return div().into_any_element();
            };
            let member = all[dir.index()][i].clone();
            let peek = crate::overlay::peek::Peek::Symbol(crate::overlay::peek::SymbolPeek {
                kind: Some(member.kind),
                name: member.name.clone(),
                place: "method of `RelationLabel`".into(),
                path: format!("present::glyph::{}", member.name).into(),
                sentence: Some("Where a relation label goes next.".into()),
                uses: Some(3),
                ..Default::default()
            });
            crate::overlay::peek::card(&peek, m, w, cx)
        }));
    for dir in Dir::ALL {
        r = r.members(dir, members(dir));
    }

    let gap = 30.0 * facet.density.space() * scale;
    div()
        .size_full()
        .flex()
        .justify_center()
        .child(
            div()
                .w(px(column + pad_x * 2.0))
                .px(px(pad_x))
                .pt(px(pad_y))
                .flex()
                .flex_col()
                .child(hero)
                .child(div().mt(px(gap - 12.0 * scale)).child(line))
                .child(div().mt(px(gap)).child(tabs))
                .child(div().mt(px(gap)).child(code))
                .child(div().mt(px(gap)).child(r)),
        )
        .typeset(ty::BODY, &facet)
        .into_any_element()
}

