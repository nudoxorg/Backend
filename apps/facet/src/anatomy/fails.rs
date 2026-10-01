//! Real failure kinds, with the callables that construct each kind. A kind
//! and its makers share a branch; absent maker evidence stays explicit.

use super::text::{Line, Links};
use super::{k, roles};
use crate::graph::World;
use crate::measure::{Measure, Set};
use crate::semantics::fails::Section;
use crate::semantics::types::Target;
use crate::theme::ActiveFacet;
use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};
use std::sync::Arc;

#[derive(IntoElement)]
pub struct Failures {
    id: ElementId,
    section: Section,
    world: Arc<World>,
    measure: Measure,
    links: Links,
}

#[must_use]
pub fn fails(
    id: impl Into<ElementId>,
    section: Section,
    world: Arc<World>,
    measure: &Measure,
    links: &Links,
) -> Failures {
    Failures {
        id: id.into(),
        section,
        world,
        measure: *measure,
        links: links.clone(),
    }
}

impl RenderOnce for Failures {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let mut root = div().flex().flex_col().gap(k(&measure, 10.0));
        for (n, row) in self.section.rows.iter().enumerate() {
            let mut kind = Line::new();
            kind.link(
                &self.world.node(row.kind).name,
                roles::ROW,
                palette.ink0.hsla(),
                Target::Node(row.kind),
            );
            let mut makers = Line::new();
            for (at, &maker) in row.makers.iter().enumerate() {
                if at > 0 {
                    makers.push(" · ", roles::QUIET, palette.ink3.hsla());
                }
                makers.link(
                    &self.world.name_of(maker),
                    roles::ROW,
                    palette.ink1.hsla(),
                    Target::Node(maker),
                );
            }
            if row.makers.is_empty() {
                makers.push(
                    "No constructor recorded here",
                    roles::QUIET,
                    palette.ink3.hsla(),
                );
            }
            if row.more > 0 {
                makers.push(
                    &format!(" and {} more constructors", row.more),
                    roles::QUIET,
                    palette.ink3.hsla(),
                );
            }
            let key = |part: &str| {
                ElementId::NamedChild(
                    Arc::new(self.id.clone()),
                    SharedString::from(format!("{n}-{part}")),
                )
            };
            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(k(&measure, 4.0))
                    .pl(k(&measure, 12.0))
                    .border_l_1()
                    .border_color(palette.coral.base.hsla())
                    .child(kind.element(key("kind"), roles::ROW, &measure, &self.links, palette))
                    .child(makers.element(
                        key("makers"),
                        roles::ROW,
                        &measure,
                        &self.links,
                        palette,
                    )),
            );
        }
        root.child(
            div()
                .set(roles::QUIET, &measure)
                .text_color(palette.ink3.hsla())
                .child(self.section.foot()),
        )
    }
}
