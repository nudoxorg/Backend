//! Does: members grouped by what they do to it (`reads it`, `changes it`,
//! `uses it up`, `makes one, or stands alone`), look-alikes folded into one
//! row, trait-provided members under "through its traits".
//!
//! Also the member rows the contract draws: a name (a link to the member),
//! its signature in plain words, its one sentence; a folded row names the
//! shared prefix, the inputs it varies over, the shared result and how many
//! there are.

use super::text::{Line, Links, TypeInk};
use super::{icon_kind, k, roles, row_pad, section_title};
use crate::icons::{self, KindSize};
use crate::measure::{Measure, Set};
use crate::semantics::model::{Does, FoldRow, MemberRow, Row, SigLine};
use crate::semantics::types::Target;
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};
use std::sync::Arc;

/// Below this effective width the rows drop their sentence (the
/// prototype's reader query at 620, less the folio's padding).
pub const SAY_BELOW: f32 = 580.0;

/// Writes `(params) → ret` after whatever the line holds.
pub(crate) fn sig(line: &mut Line, sig: &SigLine, ink: &TypeInk, links: &Links, xray: bool, palette: &Palette) {
    let punct = palette.ink4.hsla();
    if !sig.params.is_empty() {
        line.push("(", ink.base, punct);
        for (n, p) in sig.params.iter().enumerate() {
            if n > 0 {
                line.push(", ", ink.base, punct);
            }
            line.spelled(p, ink, links, xray);
        }
        line.push(")", ink.base, punct);
    }
    if let Some(ret) = &sig.ret {
        line.push(" → ", ink.base, punct);
        line.spelled(ret, ink, links, xray);
    }
}

/// A member's name and signature as one line.
pub(crate) fn member_line(row: &MemberRow, links: &Links, xray: bool, palette: &Palette) -> Line {
    let mut line = Line::new();
    line.link(&row.name, roles::ROW, palette.ink0.hsla(), Target::Node(row.node));
    line.push(" ", roles::ROW, palette.ink2.hsla());
    let mut ink = TypeInk::new(roles::words(roles::ROW), palette);
    ink.base = crate::tokens::TypeRole { weight: 400.0, ..roles::ROW };
    sig(&mut line, &row.sig, &ink, links, xray, palette);
    line
}

/// A folded row's line: `visit_… (one of bool, i8, i16, i32 and 16 more) → its Value…`.
pub(crate) fn fold_line(row: &FoldRow, links: &Links, xray: bool, palette: &Palette) -> Line {
    let mut line = Line::new();
    line.push(&row.prefix, roles::ROW, palette.ink0.hsla());
    line.push("…", roles::ROW, palette.ink3.hsla());
    let ink = TypeInk::new(crate::tokens::TypeRole { weight: 400.0, ..roles::ROW }, palette);
    let punct = palette.ink4.hsla();
    if !row.inputs.is_empty() {
        line.push(" (", ink.base, punct);
        line.push("one of ", roles::words(ink.base), palette.ink3.hsla());
        for (n, input) in row.inputs.iter().take(FoldRow::SHOWN).enumerate() {
            if n > 0 {
                line.push(", ", ink.base, punct);
            }
            line.spelled(input, &ink, links, xray);
        }
        let more = row.inputs.len().saturating_sub(FoldRow::SHOWN);
        if more > 0 {
            line.push(&format!(" and {more} more"), roles::words(ink.base), palette.ink3.hsla());
        }
        line.push(")", ink.base, punct);
    }
    if let Some(ret) = &row.ret {
        line.push(" → ", ink.base, punct);
        line.spelled(ret, &ink, links, xray);
    }
    line
}

/// What a folded row says instead of a sentence: `22 of them: bool, i8, …`.
#[must_use]
pub fn fold_count(row: &FoldRow) -> String {
    let names: Vec<&str> = row.suffixes.iter().take(5).map(AsRef::as_ref).collect();
    let tail = if row.suffixes.len() > 5 { ", …" } else { "" };
    format!("{} of them: {}{tail}", row.members.len(), names.join(", "))
}

/// One member row: mark, name and signature, and the sentence (or the
/// folded count) while there is room.
pub(crate) fn row(
    id: ElementId,
    row: &Row,
    mark: Option<AnyElement>,
    measure: &Measure,
    links: &Links,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    let xray = measure.reveal().xray;
    let room = measure.effective() >= SAY_BELOW;
    let (line, say): (Line, Option<(SharedString, bool)>) = match row {
        Row::One(member) => (member_line(member, links, xray, palette), member.doc.clone().map(|d| (d, true))),
        Row::Fold(fold) => (fold_line(fold, links, xray, palette), Some((SharedString::from(fold_count(fold)), false))),
    };
    let line_id = ElementId::NamedChild(Arc::new(id.clone()), SharedString::new_static("line"));
    let mut el = div()
        .id(id)
        .flex()
        .items_baseline()
        .gap(k(measure, 12.0))
        .py(row_pad(measure, 7.0))
        .px(k(measure, 10.0))
        .mx(-k(measure, 10.0))
        .min_h(row_pad(measure, 34.0))
        .hover(|s| s.bg(palette.tint.hsla()))
        .children(mark.map(|m| div().flex_none().w(k(measure, 18.0)).child(m)))
        .child(div().flex_shrink().min_w_0().child(line.element(line_id, roles::ROW, measure, links, palette)));
    if room && let Some((say, serif)) = say {
        let role = if serif { roles::SAY } else { crate::tokens::TypeRole { size: 12.0, line: 16.0, ..roles::QUIET } };
        el = el.child(div().flex_1().min_w_0().truncate().set(role, measure).text_color(palette.ink3.hsla()).child(say));
    }
    el
}

/// The Does section. Build with [`does`].
#[derive(IntoElement)]
pub struct DoesView {
    id: ElementId,
    does: Does,
    measure: Measure,
    links: Links,
}

/// The Does section for `does` at `measure`.
#[must_use]
pub fn does(id: impl Into<ElementId>, does: Does, measure: &Measure, links: &Links) -> DoesView {
    DoesView { id: id.into(), does, measure: *measure, links: links.clone() }
}

fn group_heading(text: &str, measure: &Measure, palette: &Palette) -> gpui::Div {
    div()
        .set(roles::GROUP, measure)
        .text_color(palette.ink4.hsla())
        .pt(k(measure, 12.0))
        .pb(k(measure, 2.0))
        .child(SharedString::from(text.to_owned()))
}

impl RenderOnce for DoesView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 6.0));
        if self.does.is_empty() {
            return root;
        }
        root = root.child(section_title("Does", &m, palette));
        let sub = |part: String| ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(part));
        for (g, group) in self.does.groups.iter().enumerate() {
            root = root.child(group_heading(group.receiver.heading(), &m, palette));
            for (r, member) in group.rows.iter().enumerate() {
                let kind = match member {
                    Row::One(one) => icon_kind(one.kind),
                    Row::Fold(_) => icons::Kind::Method,
                };
                let mark = icons::kind_mark(kind, KindSize::Sm, palette);
                root = root.child(row(sub(format!("{g}-{r}")), member, Some(mark), &m, &self.links, palette));
            }
        }
        if !self.does.through.is_empty() {
            root = root.child(group_heading("through its traits", &m, palette));
            for (t, through) in self.does.through.iter().enumerate() {
                let mut line = Line::new();
                line.push(&through.trait_name, roles::ROW, palette.ink2.hsla());
                line.push(" · ", roles::ROW, palette.ink4.hsla());
                for (n, (node, name)) in through.members.iter().enumerate() {
                    if n > 0 {
                        line.push(", ", roles::ROW, palette.ink4.hsla());
                    }
                    line.link(name, crate::tokens::TypeRole { weight: 400.0, ..roles::ROW }, palette.ink1.hsla(), Target::Node(*node));
                }
                let id = sub(format!("via-{t}"));
                root = root.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(k(&m, 12.0))
                        .py(row_pad(&m, 7.0))
                        .min_h(row_pad(&m, 34.0))
                        .child(div().flex_none().w(k(&m, 18.0)).child(icons::kind_mark(icons::Kind::Trait, KindSize::Sm, palette)))
                        .child(line.element(id, roles::ROW, &m, &self.links, palette)),
                );
            }
        }
        root
    }
}
