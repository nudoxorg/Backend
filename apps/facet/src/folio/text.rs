//! Words on the folio's marks: every string is published to the probe
//! ledger under a stable key, so a test reads what a mark says from the
//! rendered frame (never from a count) and the harness lints its box.

use crate::Set;
use crate::measure::Measure;
use crate::probe::{self, Text, TextOverflow};
use crate::reading::{self, Intent, ReadingRole};
use crate::tokens::TypeRole;
use gpui::{ElementId, Hsla, ParentElement, Pixels, SharedString, Styled, Window, div};
use std::sync::Arc;

/// `id` with `channel` appended: `hero-name`, `card-Sender-doc`.
#[must_use]
pub fn key(id: &ElementId, channel: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), channel.into())
}

/// The shaped natural width for a role already resolved by [`Measure`].
#[must_use]
pub fn natural_width(content: &SharedString, role: TypeRole, window: &Window) -> Pixels {
    probe::natural_width(content, role, 1.0, window)
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
    one_as(
        id,
        content,
        Intent::Reading(ReadingRole::Fact),
        role,
        ink,
        measure,
    )
}

/// A natural-width line with an explicit reading or parent-name intent.
#[must_use]
pub fn one_as(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    intent: Intent,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> Text {
    let id = id.into();
    let content: SharedString = content.into();
    probe::text(
        id.clone(),
        content.clone(),
        measure.role(role),
        1.0,
        TextOverflow::Clip,
        div()
            .flex_none()
            .set(role, measure)
            .text_color(ink.into())
            .whitespace_nowrap()
            .child(reading::text(id, content, intent)),
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
    ellipsis_as(
        id,
        content,
        Intent::Reading(ReadingRole::Fact),
        role,
        ink,
        measure,
    )
}

/// An ellipsised line whose native words follow the same measured cut.
#[must_use]
pub fn ellipsis_as(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    intent: Intent,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
) -> Text {
    let id = id.into();
    let content: SharedString = content.into();
    probe::text(
        id.clone(),
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
            .child(reading::text(id, content, intent)),
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
    wrap_as(
        id,
        content,
        Intent::Reading(ReadingRole::Paragraph),
        role,
        ink,
        measure,
        lines,
    )
}

/// Wrapping words with an explicit semantic intent. The actual GPUI clamp
/// limits both the painted text and the native reading node.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn wrap_as(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    intent: Intent,
    role: TypeRole,
    ink: impl Into<Hsla>,
    measure: &Measure,
    lines: Option<usize>,
) -> Text {
    let id = id.into();
    let content: SharedString = content.into();
    let mut body = div().min_w_0().set(role, measure).text_color(ink.into());
    let overflow = if let Some(lines) = lines {
        body = body.overflow_hidden().text_ellipsis().line_clamp(lines);
        TextOverflow::Ellipsis
    } else {
        TextOverflow::Wrap
    };
    probe::text(
        id.clone(),
        content.clone(),
        measure.role(role),
        1.0,
        overflow,
        body.child(reading::text(id, content, intent)),
    )
}
