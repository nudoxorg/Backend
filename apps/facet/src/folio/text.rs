//! Words on the folio's marks: every string is published to the probe
//! ledger under a stable key, so a test reads what a mark says from the
//! rendered frame (never from a count) and the harness lints its box.

use crate::Set;
use crate::measure::Measure;
use crate::probe::{self, Text, TextOverflow};
use crate::tokens::TypeRole;
use gpui::{ElementId, Hsla, ParentElement, SharedString, Styled, div};
use std::sync::Arc;

/// `id` with `channel` appended: `hero-name`, `card-Sender-doc`.
#[must_use]
pub fn key(id: &ElementId, channel: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), channel.into())
}

/// One line that keeps its natural width.
#[must_use]
pub fn one(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> Text {
    let content: SharedString = content.into();
    probe::text(
        id,
        content.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Clip,
        div().flex_none().set(role, measure).text_color(ink.into()).whitespace_nowrap().child(content),
    )
}

/// One line that gives way with an ellipsis when its box is short.
#[must_use]
pub fn ellipsis(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> Text {
    let content: SharedString = content.into();
    probe::text(
        id,
        content.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Ellipsis,
        div()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .set(role, measure)
            .text_color(ink.into())
            .child(content),
    )
}

/// Wrapping words, cut with an ellipsis after `lines` lines when given.
#[must_use]
pub fn wrap(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
    lines: Option<usize>,
) -> Text {
    let content: SharedString = content.into();
    let mut body = div().min_w_0().set(role, measure).text_color(ink.into());
    let overflow = if let Some(lines) = lines {
        body = body.overflow_hidden().text_ellipsis().line_clamp(lines);
        TextOverflow::Ellipsis
    } else {
        TextOverflow::Wrap
    };
    probe::text(id, content.clone(), measure.role(role), 1.0, overflow, body.child(content))
}
