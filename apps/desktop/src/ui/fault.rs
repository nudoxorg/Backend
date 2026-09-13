//! The one component that renders every typed failure, in place.
//! Operand, cause, affordances — always all three, never a modal, never a toast.
//! If this component is on screen, the reader knows what failed and what to do.
//!
//! It has two sizes and no other variation. Inline is a single row for a shelf
//! entry or a status line; block is a bordered region for a reader page or an
//! empty result set. Both draw the same three parts in the same order, so a
//! reader learns to read failures once.

use crate::presentation::fault::{Affordance, Fault, Severity};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Div, ElementId, FontWeight, InteractiveElement, ParentElement, SharedString,
    Stateful, Styled, div, px,
};

/// Returns the paint role for one severity.
pub(crate) const fn severity_paint(severity: Severity) -> Paint {
    match severity {
        Severity::Note => Paint::Info,
        Severity::Caution => Paint::Caution,
        Severity::Fault => Paint::Fault,
    }
}

/// Returns the glyph that opens a fault.
pub(crate) const fn severity_glyph(severity: Severity) -> &'static str {
    match severity {
        Severity::Note => "○",
        Severity::Caution => "◐",
        Severity::Fault => "✗",
    }
}

/// Returns a full fault block: headline, operand, cause, and affordances.
pub(crate) fn block(theme: &Theme, fault: &Fault, actions: Vec<AnyElement>) -> Div {
    let role = severity_paint(fault.severity());
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
        .child(headline(theme, fault, role))
        .child(operand_line(theme, fault))
        .child(
            div()
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::TextDim))
                .child(fault.cause().to_owned()),
        )
        .when_not_empty(actions)
}

/// Returns a one-line fault for a dense row or a status line.
pub(crate) fn inline(theme: &Theme, fault: &Fault) -> Div {
    let role = severity_paint(fault.severity());
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(role))
                .child(severity_glyph(fault.severity())),
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
                .child(fault.headline().to_owned()),
        )
}

/// Returns the button shell for one affordance; the view attaches the action.
pub(crate) fn affordance_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    affordance: &Affordance,
) -> Stateful<Div> {
    let enabled = affordance.is_actionable();
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
        .child(affordance.label())
}

fn headline(theme: &Theme, fault: &Fault, role: Paint) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(role))
                .child(severity_glyph(fault.severity())),
        )
        .child(
            div()
                .text_size(type_size(TypeScale::Interface))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::TextStrong))
                .child(fault.headline().to_owned()),
        )
}

fn operand_line(theme: &Theme, fault: &Fault) -> Div {
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
                .child(fault.operand().role().to_owned()),
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
                .child(fault.operand().spelling()),
        )
}

fn edge(theme: &Theme, role: Paint) -> gpui::Hsla {
    let mut ink = theme.paint(role);
    ink.a = 0.32;
    ink
}

/// Adds the affordance row only when there is at least one affordance.
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
