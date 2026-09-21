//! Live package catalog and project landing projections.

use super::primitives::{heading, loading_card};
use crate::model::{AppSnapshot, PackageSummary};
use crate::navigation::{Intent, PackageLane, PackageRoute, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, div, px,
};

pub(super) fn home_page(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let title = if snapshot.shelf().items.is_empty() {
        "Add a project to start reading"
    } else {
        "Your documentation shelf"
    };
    let cards = package_cards(root, theme, snapshot, cx);
    surface::cut(theme, Paint::MintLine)
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(heading(theme, title))
        .child(text::single_line(text::body(theme)).child(
            "Local-first documentation, declarations, source, graph, and registry facts from the live index.",
        ))
        .child(cards)
        .into_any_element()
}

pub(super) fn project_page(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let label = snapshot
        .project()
        .loaded_value()
        .map_or("Project", |project| project.label.as_ref());
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(heading(theme, label))
        .child(text::single_line(text::faint(theme)).child("Live indexed packages"))
        .child(package_cards(root, theme, snapshot, cx))
        .into_any_element()
}

fn package_cards(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let cards = snapshot
        .catalog()
        .loaded_value()
        .map(|catalog| {
            catalog
                .packages
                .iter()
                .take(24)
                .map(|package| package_card(theme, package, root, cx))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if cards.is_empty() {
        return loading_card(theme, snapshot.catalog().terminal()).into_any_element();
    }
    div()
        .grid_cols(2)
        .gap(space(Space::Snug))
        .children(cards)
        .into_any_element()
}

fn package_card(
    theme: &Theme,
    package: &PackageSummary,
    root: &mut UiRootEntity,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let coordinate = package.coordinate.clone();
    let title = package.name.clone();
    let version = package.version.clone();
    surface::panel(theme)
        .id(ElementId::Name(
            format!("package-card-{}", package.coordinate).into(),
        ))
        .p(px(20.0))
        .border(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .cursor_pointer()
        .hover(|style| style.bg(theme.paint(Paint::Tint)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.queue(
                Intent::Navigate(Route::Package(PackageRoute {
                    project: None,
                    package: coordinate.clone(),
                    lane: PackageLane::Overview,
                    selected: None,
                })),
                cx,
            );
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space(Space::Tight))
                .child(
                    text::single_line(text::heading(theme, TypeScale::Interface))
                        .child(title.to_string()),
                )
                .child(text::single_line(text::faint(theme)).child(version.to_string())),
        )
        .child(text::single_line(text::faint(theme)).child(package.ecosystem.to_string()))
        .child(
            div()
                .flex()
                .gap(space(Space::Gutter))
                .child(text::single_line(text::faint(theme)).child(package.downloads.to_string()))
                .child(text::single_line(text::faint(theme)).child(package.standing.to_string())),
        )
        .into_any_element()
}
