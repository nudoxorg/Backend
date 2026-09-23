//! Live package catalog and project landing projections.

use super::onboarding;
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
    onboarding::home_page(root, theme, snapshot, cx)
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

pub(super) fn package_cards(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    theme.register_action(
        components::ActionMetadata::new("package-list", "Packages", components::ActionRole::List)
            .description("Packages available from the live index"),
    );
    let mut cards = local_package_cards(theme, snapshot, cx);
    cards.extend(
        snapshot
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
            .unwrap_or_default(),
    );
    if cards.is_empty() {
        return loading_card(theme, snapshot.catalog().terminal()).into_any_element();
    }
    div()
        .id(ElementId::Name("package-list".into()))
        .role(gpui::Role::List)
        .aria_label("Packages")
        .aria_description("Packages available from the live index")
        .grid_cols(2)
        .gap(space(Space::Snug))
        .children(cards)
        .into_any_element()
}

/// Cards for the served local project's own packages that the registry
/// catalog does not list; their dossier is read from the local manifest.
fn local_package_cards(
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> Vec<AnyElement> {
    let Some(project) = snapshot.project().loaded_value() else {
        return Vec::new();
    };
    let registry = snapshot.catalog().loaded_value();
    project
        .packages
        .iter()
        .filter(|package| {
            registry.is_none_or(|catalog| {
                catalog
                    .packages
                    .iter()
                    .all(|row| row.coordinate != **package)
            })
        })
        .take(24)
        .map(|package| {
            let action_id = format!("package-card-{package}");
            let description = format!("Local project {}, facts from its manifest", project.label);
            theme.register_action(
                components::ActionMetadata::new(
                    action_id.clone(),
                    package.to_string(),
                    components::ActionRole::ListItem,
                )
                .description(description.clone())
                .parent("package-list")
                .relation(components::ActionRelation::FlowTo, "package-list"),
            );
            let route = Route::Package(PackageRoute {
                project: None,
                package: package.clone(),
                lane: PackageLane::Overview,
                selected: None,
            });
            let card =
                components::card_button(theme, action_id.clone(), package.to_string(), description)
                    .border_color(theme.paint(Paint::Rule2))
                    // `Button::render` already installs its own hover style
                    // for every non-disabled, non-selected, interactive
                    // button (gpui_ce_components' button.rs). A second
                    // `.hover(...)` here re-set the same interactivity slot
                    // and tripped GPUI's `debug_assert!("hover style already
                    // set")` the first time this card ever actually painted.
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue(Intent::Navigate(route.clone()), cx);
                    }))
                    .child(
                        text::single_line(text::heading(theme, TypeScale::Interface))
                            .child(package.to_string()),
                    )
                    .child(text::single_line(text::faint(theme)).child("Local project"));
            components::measure(theme, action_id, card).into_any_element()
        })
        .collect()
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
    let action_id = format!("package-card-{}", package.coordinate);
    theme.register_action(
        components::ActionMetadata::new(
            action_id.clone(),
            format!("{} {}", package.name, package.version),
            components::ActionRole::ListItem,
        )
        .description(format!(
            "{} package, {} downloads, {} standing",
            package.ecosystem, package.downloads, package.standing
        ))
        .parent("package-list")
        .relation(components::ActionRelation::FlowTo, "package-list"),
    );
    let click_route = Route::Package(PackageRoute {
        project: None,
        package: coordinate.clone(),
        lane: PackageLane::Overview,
        selected: None,
    });
    let card = components::card_button(
        theme,
        action_id.clone(),
        format!("{} {}", package.name, package.version),
        format!(
            "{} package, {} downloads, {} standing",
            package.ecosystem, package.downloads, package.standing
        ),
    )
    .border_color(theme.paint(Paint::Rule2))
    // See the matching comment in `local_package_cards`: `Button::render`
    // already installs a hover style for an interactive, non-disabled,
    // non-selected button, so a second `.hover(...)` here panics the first
    // time this card actually paints.
    .on_click(cx.listener(move |this, _, _, cx| {
        this.queue(Intent::Navigate(click_route.clone()), cx);
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
    );
    components::measure(theme, action_id, card).into_any_element()
}
