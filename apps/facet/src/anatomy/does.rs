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
    Styled, StatefulInteractiveElement, Window, div,
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
    let (data, say) = match row {
        Row::One(member) => (super::Operation {
            name: member.name.clone(), target: Some(Target::Node(member.node)),
            inputs: member.sig.params.clone(), result: member.sig.success.clone(), failure: member.sig.fails.clone(), signature_known: true,
        }, member.doc.clone()),
        Row::Fold(fold) => (super::Operation {
            name: SharedString::from(format!("{}…", fold.prefix)), target: None,
            inputs: fold.inputs.iter().take(FoldRow::SHOWN).cloned().collect(), result: fold.success.clone(), failure: fold.fails.clone(), signature_known: true,
        }, Some(SharedString::from(fold_count(fold)))),
    };
    let operation = super::operation(ElementId::NamedChild(Arc::new(id.clone()), "path".into()), data, measure, links, palette);
    div().id(id).flex().items_start().gap(k(measure, 10.0))
        .py(row_pad(measure, 8.0)).px(k(measure, 10.0)).mx(-k(measure, 10.0))
        .hover(|s| s.bg(palette.tint.hsla()))
        .children(mark.map(|mark| div().pt(k(measure, 8.0)).flex_none().child(mark)))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(k(measure, 3.0))
            .child(operation).children(say.map(|say| div().set(roles::SAY, measure).text_color(palette.ink3.hsla()).child(say))))
}

/// The Does section. Build with [`does`].
#[derive(IntoElement)]
pub struct DoesView {
    id: ElementId,
    does: Does,
    measure: Measure,
    links: Links,
    expanded: Vec<crate::semantics::members::Receiver>,
    toggle: Option<std::rc::Rc<dyn Fn(crate::semantics::members::Receiver, &mut App)>>,
    title: bool,
}

/// The Does section for `does` at `measure`.
#[must_use]
pub fn does(id: impl Into<ElementId>, does: Does, measure: &Measure, links: &Links) -> DoesView {
    DoesView { id: id.into(), does, measure: *measure, links: links.clone(), expanded: Vec::new(), toggle: None, title: true }
}

impl DoesView {
    /// Keep controls in the shell's bounded per-symbol state.
    #[must_use]
    pub fn disclosure(mut self, expanded: Vec<crate::semantics::members::Receiver>, toggle: impl Fn(crate::semantics::members::Receiver, &mut App) + 'static) -> Self {
        self.expanded = expanded; self.toggle = Some(std::rc::Rc::new(toggle)); self
    }
    /// The enclosing page supplies its own tracked section heading.
    #[must_use]
    pub fn without_title(mut self) -> Self { self.title = false; self }
}

fn mode(receiver: crate::semantics::members::Receiver) -> (&'static str, crate::icons::Mod) {
    use crate::semantics::members::Receiver as R;
    match receiver {
        R::Reads => ("Inspect", crate::icons::Mod::Reads),
        R::Changes => ("Edit", crate::icons::Mod::Changes),
        R::UsesUp => ("Take ownership", crate::icons::Mod::Consumes),
        R::Makes => ("Associated operations", crate::icons::Mod::Makes),
    }
}

fn group_heading(id: ElementId, text: &str, measure: &Measure, palette: &Palette) -> crate::probe::Text {
    let text = SharedString::from(text.to_owned());
    let words = div()
        .set(roles::GROUP, measure)
        .text_color(palette.ink3.hsla())
        .pt(k(measure, 12.0))
        .pb(k(measure, 2.0))
        .child(text.clone());
    crate::probe::text(id, text, measure.role(roles::GROUP), 1.0, crate::probe::TextOverflow::Wrap, words)
}

impl RenderOnce for DoesView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 6.0));
        if self.does.is_empty() {
            return root;
        }
        if self.title { root = root.child(section_title("What it does", &m, palette)); }
        let sub = |part: String| ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(part));
        for (g, group) in self.does.groups.iter().enumerate() {
            let (label, glyph) = mode(group.receiver);
            root = root.child(div().flex().items_center().gap(k(&m, 8.0))
                .child(icons::mod_mark(glyph, 14.0 * m.scale(), palette))
                .child(group_heading(sub(format!("group-{g}-heading")), label, &m, palette)));
            let expanded = self.expanded.contains(&group.receiver);
            let limit = if expanded { group.rows.len() } else { 6 };
            for (r, member) in group.rows.iter().take(limit).enumerate() {
                let kind = match member {
                    Row::One(one) => icon_kind(one.kind),
                    Row::Fold(_) => icons::Kind::Method,
                };
                let mark = icons::kind_mark(kind, KindSize::Sm, palette);
                root = root.child(row(sub(format!("{g}-{r}")), member, Some(mark), &m, &self.links, palette));
            }
            if group.rows.len() > 6 {
                let label = if expanded { "Show fewer operations".to_owned() } else { format!("Explore {} more operations", group.rows.len() - 6) };
                let receiver = group.receiver;
                let toggle = self.toggle.clone();
                root = root.child(div().id(sub(format!("group-{g}-fold"))).py(k(&m, 8.0))
                    .set(roles::QUIET, &m).text_color(palette.peri.base.hsla()).child(label)
                    .on_click(move |_, _, cx| { if let Some(toggle) = &toggle { toggle(receiver, cx); } }));
            }
        }
        if !self.does.through.is_empty() {
            root = root.child(group_heading(sub("traits-heading".to_owned()), "through its traits", &m, palette));
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
