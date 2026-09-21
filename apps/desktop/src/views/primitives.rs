//! Shared visual vocabulary for route projections.

use crate::core::ResourceTerminal;
use crate::model::AppSnapshot;
use crate::navigation::{OrbitRoute, PackageLane, Route};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{surface, text};
use gpui::{FontWeight, IntoElement, ParentElement, Styled, div, px};

pub(super) fn status_bar(theme: &Theme, snapshot: &AppSnapshot) -> impl IntoElement {
    div()
        .h(px(28.0))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(space(Space::Gutter))
        .px(space(Space::Gutter))
        .border_t(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .text_size(type_size(TypeScale::Tiny))
        .text_color(theme.paint(Paint::Silver3))
        .child(format!("root {}", short_root(snapshot)))
        .child(if snapshot.catalog().loaded_value().is_some() {
            "live index ready"
        } else {
            "waiting for live index"
        })
}

pub(super) fn heading(theme: &Theme, value: &str) -> impl IntoElement {
    div()
        .text_size(type_size(TypeScale::Section))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.paint(Paint::Silver0))
        .child(value.to_owned())
}

pub(super) fn crumb(theme: &Theme, value: &str) -> impl IntoElement {
    div()
        .text_size(type_size(TypeScale::Small))
        .font_family(theme.specimen())
        .text_color(theme.paint(Paint::Silver3))
        .child(value.to_owned())
}

pub(super) fn fact(theme: &Theme, label: &str, value: &str) -> impl IntoElement {
    surface::panel(theme)
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(text::single_line(text::faint(theme)).child(label.to_owned()))
        .child(text::single_line(text::label(theme)).child(value.to_owned()))
}

pub(super) fn loading_card(theme: &Theme, terminal: &ResourceTerminal) -> impl IntoElement {
    let status = match terminal {
        ResourceTerminal::Complete => "Loading live package catalog…",
        ResourceTerminal::Unavailable(_) => "The live registry has no catalog for this workspace.",
        ResourceTerminal::Fault(error) => error.message(),
    };
    surface::sunken(theme)
        .p(px(24.0))
        .child(text::single_line(text::body(theme)).child(status.to_owned()))
}

pub(super) fn route_label(snapshot: &AppSnapshot) -> String {
    match snapshot.route() {
        Route::Orbit(OrbitRoute::Home) => "Shelf / Home".to_owned(),
        Route::Orbit(OrbitRoute::Project(project)) => format!("Project / {project}"),
        Route::Package(package) => format!("{} / {}", package.package, lane_label(package.lane)),
        Route::Page(page) => format!("{} / {}", page.package, page.coordinate.as_str()),
        Route::Source(source) => format!(
            "{} / {}:{}",
            source.package,
            source.page.as_str(),
            source.line
        ),
    }
}

const fn lane_label(lane: PackageLane) -> &'static str {
    match lane {
        PackageLane::Overview => "overview",
        PackageLane::Dependencies => "dependencies",
        PackageLane::Dependents => "dependents",
        PackageLane::Releases => "releases",
        PackageLane::Security => "security",
    }
}

pub(super) fn short_root(snapshot: &AppSnapshot) -> String {
    snapshot
        .root()
        .as_bytes()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
