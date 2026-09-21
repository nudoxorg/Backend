//! Chips: lane coverage, capability state, counts, and scope.
//! A coverage chip is the smallest honest statement this product can make.
//! It shows a lane, a standing glyph, and — when it is not complete — why.
//!
//! Chips are washes rather than filled pills so that a row of four reads as
//! one instrument panel instead of four competing badges. Every chip carries
//! a tooltip with the full sentence, because the glyph is a summary and a
//! summary is only honest when the long form is one hover away.

use crate::presentation::chips::{CapabilityChip, LaneChip, Standing};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyView, App, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use std::time::Duration;

/// How long the pointer rests before a chip explains itself.
const TOOLTIP_DELAY: Duration = Duration::from_millis(300);

/// Returns the paint role for one standing.
pub(crate) const fn standing_paint(standing: Standing) -> Paint {
    match standing {
        Standing::Complete => Paint::Ok,
        Standing::Partial => Paint::Caution,
        Standing::Absent => Paint::Info,
        Standing::Unobserved => Paint::TextFaint,
    }
}

/// Returns one lane coverage chip with its reason on hover.
pub(crate) fn coverage_chip(theme: &Theme, chip: &LaneChip) -> impl IntoElement {
    let role = standing_paint(chip.standing());
    let explanation = chip.explanation().to_owned();
    let title = format!("{} lane", chip.name());
    shell(theme, role)
        .id(ElementId::Name(SharedString::from(format!("coverage-{}", chip.name()))))
        .child(glyph(theme, chip.standing().glyph(), role))
        .child(word(theme, chip.name(), Paint::TextDim))
        .when_some(
            Some(chip.detail()).filter(|detail| !detail.is_empty()),
            |chip, detail| chip.child(word(theme, detail, role)),
        )
        .tooltip_show_delay(TOOLTIP_DELAY)
        .tooltip(move |window, cx| explain(title.clone(), explanation.clone(), window, cx))
}

/// Returns one capability chip with its lifecycle reason on hover.
pub(crate) fn capability_chip(theme: &Theme, chip: &CapabilityChip) -> impl IntoElement {
    let role = standing_paint(chip.standing());
    let explanation = chip.explanation().to_owned();
    let title = format!("{} · {}", chip.name(), chip.role());
    let ink = chip
        .language()
        .map_or_else(|| theme.paint(role), |language| {
            theme.on_plane(crate::theme::language::hue(language))
        });
    shell(theme, role)
        .id(ElementId::Name(SharedString::from(format!(
            "capability-{}-{}",
            chip.name(),
            chip.role()
        ))))
        .child(glyph(theme, chip.standing().glyph(), role))
        .child(
            div()
                .text_size(type_size(TypeScale::Micro))
                .text_color(ink)
                .child(chip.name().to_owned()),
        )
        .tooltip_show_delay(TOOLTIP_DELAY)
        .tooltip(move |window, cx| explain(title.clone(), explanation.clone(), window, cx))
}

/// Returns a plain count chip: a number with a noun.
pub(crate) fn count_chip(theme: &Theme, count: u64, noun: &str) -> Div {
    shell(theme, Paint::TextDim)
        .child(
            div()
                .text_size(type_size(TypeScale::Micro))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Text))
                .child(count.to_string()),
        )
        .child(word(theme, noun, Paint::TextFaint))
}

/// Returns the removable chip that shows an active `@project` scope.
pub(crate) fn scope_chip(theme: &Theme, project: &str) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Capsule))
        .bg(theme.paint(Paint::GiltWash))
        .border(hairline())
        .border_color(theme.paint(Paint::GiltDim))
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::Gilt))
        .child(format!("@{project}"))
}

/// Returns a badge stating a project's provenance: local, or a version.
pub(crate) fn badge(theme: &Theme, text: &str) -> Div {
    div()
        .flex_none()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::TextDim))
        .child(text.to_owned())
}

fn shell(theme: &Theme, role: Paint) -> Div {
    let mut wash = theme.paint(role);
    wash.alpha = 0.10;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Hair))
        .bg(wash)
}

fn glyph(theme: &Theme, mark: char, role: Paint) -> Div {
    div()
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(role))
        .child(mark.to_string())
}

fn word(theme: &Theme, text: &str, role: Paint) -> Div {
    div()
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(role))
        .child(text.to_owned())
}

fn explain(title: String, body: String, window: &mut Window, cx: &mut App) -> AnyView {
    super::tip::Tip::new(title).detail(body).build()(window, cx)
}
