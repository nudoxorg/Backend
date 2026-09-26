//! The contract: what an implementor writes (a dashed bracket) and what it
//! gets for free (a solid one), look-alikes folded, then how many types in
//! the world keep the contract.

use super::does::row;
use super::fork::gutter;
use super::holds::bracket;
use super::text::Links;
use super::{heading, k, roles};
use crate::measure::{Measure, Set};
use crate::semantics::model::{Contract, Row};
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div};
use std::sync::Arc;

/// A contract. Build with [`contract`].
#[derive(IntoElement)]
pub struct ContractView {
    id: ElementId,
    contract: Contract,
    measure: Measure,
    links: Links,
}

/// The contract for `contract` at `measure`.
#[must_use]
pub fn contract(id: impl Into<ElementId>, contract: Contract, measure: &Measure, links: &Links) -> ContractView {
    ContractView { id: id.into(), contract, measure: *measure, links: links.clone() }
}

fn rows(
    id: &ElementId,
    part: &str,
    list: &[Row],
    dashed: bool,
    measure: &Measure,
    links: &Links,
    palette: &Palette,
) -> gpui::Div {
    let gutter = gutter(measure);
    let inner = measure.within(measure.width() - gutter);
    let color = if dashed { palette.line3 } else { palette.line2 };
    div()
        .relative()
        .pl(gutter)
        .mb(k(measure, 10.0))
        .flex()
        .flex_col()
        .child(bracket(measure, color.hsla(), dashed))
        .children(list.iter().enumerate().map(|(n, r)| {
            let key = ElementId::NamedChild(Arc::new(id.clone()), SharedString::from(format!("{part}-{n}")));
            row(key, r, None, &inner, links, palette)
        }))
}

impl RenderOnce for ContractView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().id(self.id.clone()).flex().flex_col();
        if !self.contract.write.is_empty() {
            root = root
                .child(heading("you write", &m, palette))
                .child(rows(&self.id, "write", &self.contract.write, true, &m, &self.links, palette));
        }
        if !self.contract.get.is_empty() {
            root = root
                .child(heading("you get", &m, palette))
                .child(rows(&self.id, "get", &self.contract.get, false, &m, &self.links, palette));
        }
        let n = self.contract.implementors;
        if n > 0 {
            root = root.child(
                div()
                    .set(roles::QUIET, &m)
                    .text_color(palette.ink3.hsla())
                    .child(SharedString::from(format!("{n} type{} in this world do it", if n == 1 { "" } else { "s" }))),
            );
        }
        root
    }
}
