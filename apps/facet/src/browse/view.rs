//! Small shared drawing vocabulary for the browsing folios.

use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::TypeRole;
use gpui::{
    AnyElement, ElementId, Hsla, IntoElement, ParentElement, ScrollHandle, SharedString, Styled,
    Window, div, point, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn child(parent: &ElementId, part: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(parent.clone()), part.into())
}

pub(super) fn words(
    id: ElementId,
    text: impl Into<SharedString>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> AnyElement {
    let text = text.into();
    probe::text(
        id,
        text.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Wrap,
        div().set(role, measure).text_color(ink.into()).child(text),
    )
    .into_any_element()
}

pub(super) fn words_ellipsis(
    id: ElementId,
    text: impl Into<SharedString>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> AnyElement {
    let text = text.into();
    probe::text(
        id,
        text.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Ellipsis,
        div()
            .set(role, measure)
            .text_color(ink.into())
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(text),
    )
    .into_any_element()
}

/// A keyboard walk may select a row below the Reader's viewport. Only an
/// explicit arrow walk requests reveal; data refreshes and pointer selection
/// leave the person's scroll position alone. The selected row's own prepaint
/// supplies actual bounds, so variable text height needs no estimate.
#[derive(Clone)]
pub(super) struct KeyboardReveal {
    pending: Rc<Cell<bool>>,
    scroll: ScrollHandle,
}

impl KeyboardReveal {
    pub(super) fn new(scroll: ScrollHandle) -> Self {
        Self {
            pending: Rc::new(Cell::new(false)),
            scroll,
        }
    }
    pub(super) fn request(&self) {
        self.pending.set(true);
    }

    pub(super) fn selected(&self, row: impl IntoElement) -> AnyElement {
        let pending = Rc::clone(&self.pending);
        let scroll = self.scroll.clone();
        div()
            .w_full()
            .on_children_prepainted(move |children, window: &mut Window, _| {
                if !pending.replace(false) {
                    return;
                }
                let Some(target) = children.first() else {
                    return;
                };
                let viewport = scroll.bounds();
                let offset = scroll.offset();
                let margin = px(24.0).min(viewport.size.height / 4.0);
                // This callback runs inside the scroll subtree. GPUI's child
                // layout bounds already include the ancestor scroll offset.
                let top = target.origin.y;
                let bottom = top + target.size.height;
                let lowest = viewport.origin.y + viewport.size.height - margin;
                let highest = viewport.origin.y + margin;
                let y = if bottom > lowest {
                    offset.y - (bottom - lowest)
                } else if top < highest {
                    (offset.y + (highest - top)).min(px(0.0))
                } else {
                    offset.y
                };
                if y != offset.y {
                    scroll.set_offset(point(offset.x, y));
                    window.request_animation_frame();
                }
            })
            .child(row)
            .into_any_element()
    }
}
