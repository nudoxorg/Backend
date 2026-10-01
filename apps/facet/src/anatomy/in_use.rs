//! In use: up to three real statements that use the symbol, each captioned
//! with its caller (a link) and `package · file:line`, the symbol underlined
//! in periwinkle. The statement is code, so it keeps its own lines and wraps
//! only when the room is narrower than a line; it never clips.

use super::text::{Line, Links};
use super::{k, roles, section_title};
use crate::measure::{Measure, Set};
use crate::semantics::model::Use;
use crate::semantics::types::Target;
use crate::theme::ActiveFacet;
use gpui::{
    InteractiveElement,App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div};
use std::sync::Arc;

/// In use. Build with [`in_use`].
#[derive(IntoElement)]
pub struct InUse {
    id: ElementId,
    uses: Vec<(Use, SharedString)>,
    measure: Measure,
    links: Links,
}

/// The In use section for `uses` (each with its caller's display name,
/// `Page::new`) at `measure`. Nothing when there are none.
#[must_use]
pub fn in_use(id: impl Into<ElementId>, uses: Vec<(Use, SharedString)>, measure: &Measure, links: &Links) -> InUse {
    InUse { id: id.into(), uses, measure: *measure, links: links.clone() }
}

impl RenderOnce for InUse {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 14.0));
        if self.uses.is_empty() {
            return root;
        }
        root = root.child(section_title("In use", &m, palette));
        let key = |part: String| ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(part));
        let pad_x = k(&m, 14.0);
        let code_measure = m.within(m.width() - pad_x * 2.0);
        for (n, (u, caller)) in self.uses.iter().enumerate() {
            let mut caption = Line::new();
            caption.link(caller, roles::CAPTION, palette.ink1.hsla(), Target::Node(u.caller));
            let caption = caption.element(key(format!("caller-{n}")), roles::CAPTION, &m, &self.links, palette);
            let place = div()
                .set(roles::NOTE, &m)
                .text_color(palette.ink4.hsla())
                .child(SharedString::from(format!("{} · {}:{}", u.package, u.file, u.excerpt.line)));
            let mut code = div().flex().flex_col().px(pad_x).py(k(&m, 10.0)).bg(palette.well.hsla());
            for (q, text) in u.excerpt.lines.iter().enumerate() {
                let hot = q == u.excerpt.hit;
                let mut line = Line::new();
                let ink = if hot { palette.ink1 } else { palette.ink3 };
                if hot {
                    let mark = u.excerpt.mark.clone();
                    line.push(&text[..mark.start], roles::CODE, ink.hsla());
                    let name_role = crate::tokens::TypeRole { weight: 600.0, ..roles::CODE };
                    let range = line.push(&text[mark.clone()], name_role, palette.ink0.hsla());
                    line.mark(range);
                    line.push(&text[mark.end..], roles::CODE, ink.hsla());
                } else {
                    line.push(text, roles::CODE, ink.hsla());
                }
                code = code.child(line.element(key(format!("code-{n}-{q}")), roles::CODE, &code_measure, &self.links, palette));
            }
            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(k(&m, 6.0))
                    .child(div().flex().flex_wrap().items_baseline().gap(k(&m, 10.0)).child(caption).child(place))
                    .child(code),
            );
        }
        root
    }
}
