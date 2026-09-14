//! The one component that renders every typed failure, in place.
//! Slug, operand, cause, affordance — always all four, never a modal, never a toast.
//! If this component is on screen, the reader knows what failed and what to do.
//!
//! It has two sizes and no other variation. Inline is a single row for a shelf
//! entry or a status line; block is a bordered region for a reader page or an
//! empty result set. Both draw the same parts in the same order, so a reader
//! learns to read failures once.
//!
//! The slug is drawn, not hidden. `not-found`, `endpoint`, `lane-unavailable`
//! are the same words `backend` prints and an MCP tool returns, so a reader who
//! searches for one finds the other.

use crate::presentation::fault::{
    Severity, affordance_label, headline, is_actionable, operand_role, operand_spelling, severity,
};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use backend_present::{Affordance, Fault};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Div, ElementId, FontWeight, InteractiveElement, ParentElement, SharedString,
    Stateful, Styled, div, px,
};

/// Returns the paint role for one severity.
pub(crate) const fn severity_paint(level: Severity) -> Paint {
    match level {
        Severity::Note => Paint::Info,
        Severity::Caution => Paint::Caution,
        Severity::Fault => Paint::Fault,
    }
}

/// Returns the glyph that opens a fault.
pub(crate) const fn severity_glyph(level: Severity) -> &'static str {
    match level {
        Severity::Note => "○",
        Severity::Caution => "◐",
        Severity::Fault => "✗",
    }
}

/// Returns a full fault block: headline, slug, operand, cause, and actions.
pub(crate) fn block(theme: &Theme, fault: &Fault, actions: Vec<AnyElement>) -> Div {
    let level = severity(fault);
    let role = severity_paint(level);
    let mut wash = theme.paint(role);
    wash.a = 0.08;
    div()
        .w_full()
        .p(space(Space::Room))
        .rounded(radius(Radius::Medium))
        .bg(wash)
        .border(hairline())
        .border_color(edge(theme, role))
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(head(theme, fault, level, role))
        .child(operand_line(theme, fault))
        .child(
            div()
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::TextDim))
                .child(fault.cause().sentence().to_owned()),
        )
        .when_not_empty(actions)
}

/// Returns a one-line fault for a dense row or a status line.
pub(crate) fn inline(theme: &Theme, fault: &Fault) -> Div {
    let level = severity(fault);
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(severity_paint(level)))
                .child(severity_glyph(level)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(type_size(TypeScale::Tiny))
                .text_color(theme.paint(Paint::TextDim))
                .child(headline(fault)),
        )
}

/// Returns the button shell for one affordance; the view attaches the action.
pub(crate) fn affordance_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    affordance: &Affordance,
) -> Stateful<Div> {
    let enabled = is_actionable(affordance);
    let ink = if enabled {
        theme.paint(Paint::Text)
    } else {
        theme.paint(Paint::TextFaint)
    };
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .items_center()
        .px(space(Space::Snug))
        .py(px(3.0))
        .rounded(radius(Radius::Hair))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .bg(theme.paint(Paint::Panel))
        .text_size(type_size(TypeScale::Tiny))
        .text_color(ink)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme.paint(Paint::Hover)))
        })
        .child(affordance_label(affordance))
}

fn head(theme: &Theme, fault: &Fault, level: Severity, role: Paint) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(role))
                .child(severity_glyph(level)),
        )
        .child(
            div()
                .flex_1()
                .text_size(type_size(TypeScale::Interface))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::TextStrong))
                .child(headline(fault)),
        )
        .child(
            div()
                .flex_none()
                .px(px(4.0))
                .py(px(1.0))
                .rounded(radius(Radius::Hair))
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(role))
                .child(fault.slug().as_str()),
        )
}

fn operand_line(theme: &Theme, fault: &Fault) -> Div {
    let spelling = operand_spelling(fault.operand());
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .min_w(px(0.0))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(Paint::TextFaint))
                .child(operand_role(fault.operand())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Tiny))
                .text_color(theme.paint(Paint::Gilt))
                .child(spelling),
        )
}

fn edge(theme: &Theme, role: Paint) -> gpui::Hsla {
    let mut ink = theme.paint(role);
    ink.a = 0.32;
    ink
}

/// Adds the action row only when there is at least one action.
trait Actions {
    fn when_not_empty(self, actions: Vec<AnyElement>) -> Self;
}

impl Actions for Div {
    fn when_not_empty(self, actions: Vec<AnyElement>) -> Self {
        if actions.is_empty() {
            return self;
        }
        self.child(
            div()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .pt(space(Space::Tight))
                .children(actions),
        )
    }
}
