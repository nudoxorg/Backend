//! Window chrome, shelf, and transient overlays.

use super::primitives::{heading, route_label, short_root};
use crate::model::AppSnapshot;
use crate::navigation::{
    Intent, OrbitRoute, Overlay, PackageLane, PackageRoute, Route, SettingsPage,
};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, px};
use gpui_component::Selectable as _;

pub(super) fn header(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let route_label = route_label(snapshot);
    div()
        .h(px(64.0))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(space(Space::Gutter))
        .px(space(Space::Gutter))
        .border_b(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(
            div()
                .text_size(type_size(TypeScale::Title))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Silver0))
                .child("Nudox"),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::Silver3))
                .child(route_label),
        )
        .child(nav_button(theme, "back", "‹", Intent::Back, root, cx))
        .child(nav_button(theme, "forward", "›", Intent::Forward, root, cx))
        .child(
            components::button_with_state(
                theme,
                "command-palette",
                "Search all docs…  ⌘K",
                components::Weight::Regular,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenCommandPalette, cx);
            })),
        )
        .child(
            components::button_with_state(
                theme,
                "settings",
                "Settings",
                components::Weight::Quiet,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenSettings(SettingsPage::Appearance), cx);
            })),
        )
}

fn nav_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    intent: Intent,
    root: &mut UiRootEntity,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let _ = root;
    components::button_with_state(theme, id, label, components::Weight::Quiet, false, true)
        .on_click(cx.listener(move |this, _, _, cx| this.queue(intent.clone(), cx)))
}

pub(super) fn shelf_panel(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let rows = snapshot
        .shelf()
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let identity = item.identity.clone();
            let route = match identity.clone() {
                crate::core::ResourceIdentity::Project(project) => {
                    Route::Orbit(OrbitRoute::Project(project))
                }
                crate::core::ResourceIdentity::Local(_) => Route::Orbit(OrbitRoute::Home),
                crate::core::ResourceIdentity::Package(package) => Route::Package(PackageRoute {
                    project: None,
                    package,
                    lane: PackageLane::Overview,
                    selected: Some(item.object),
                }),
            };
            components::button_with_state(
                theme,
                format!("shelf-{index}"),
                item.label.to_string(),
                components::Weight::Quiet,
                false,
                true,
            )
            .selected(snapshot.shelf().selected.as_ref() == Some(&identity))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.queue(Intent::Navigate(route.clone()), cx);
            }))
            .into_any_element()
        })
        .collect::<Vec<_>>();
    let add = components::button_with_state(
        theme,
        "add-project",
        "+ Add project",
        components::Weight::Primary,
        false,
        true,
    )
    .on_click(cx.listener(|this, _, _, cx| {
        this.queue(Intent::OpenSettings(SettingsPage::Index), cx);
    }));
    let _ = root;
    div()
        .w(px(248.0))
        .flex_none()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .p(space(Space::Gutter))
        .border_r(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(
            div()
                .text_size(type_size(TypeScale::Small))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Silver2))
                .child("SHELF"),
        )
        .children(rows)
        .child(div().flex_1())
        .child(add)
}
pub(super) fn overlay(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    cx: &mut Context<UiRootEntity>,
) -> Option<AnyElement> {
    match snapshot.overlay()? {
        Overlay::Settings(page) => Some(
            surface::raised(theme)
                .absolute()
                .top(px(92.0))
                .right(px(28.0))
                .w(px(440.0))
                .p(px(24.0))
                .flex()
                .flex_col()
                .gap(space(Space::Base))
                .child(heading(theme, "Settings"))
                .child(text::single_line(text::faint(theme)).child(page.as_str()))
                .child(
                    components::button_with_state(
                        theme,
                        "toggle-motion",
                        if snapshot.settings().reduced_motion {
                            "Enable motion"
                        } else {
                            "Reduce motion"
                        },
                        components::Weight::Regular,
                        false,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.queue(Intent::ToggleReducedMotion, cx);
                    })),
                )
                .child(
                    components::button_with_state(
                        theme,
                        "close-settings",
                        "Done",
                        components::Weight::Primary,
                        false,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.queue(Intent::DismissOverlay, cx);
                    })),
                )
                .into_any_element(),
        ),
        Overlay::CommandPalette => Some(
            surface::raised(theme)
                .absolute()
                .top(px(84.0))
                .left(px(320.0))
                .w(px(620.0))
                .p(px(24.0))
                .flex()
                .flex_col()
                .gap(space(Space::Base))
                .child(heading(theme, "Command palette"))
                .children(crate::navigation::ActionId::ALL.into_iter().map(|action| {
                    let intent = action.intent();
                    components::button_with_state(
                        theme,
                        action.as_str(),
                        action.spec().label,
                        components::Weight::Quiet,
                        intent.is_none(),
                        true,
                    )
                    .when_some(intent, |button, intent| {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            this.queue(intent.clone(), cx);
                        }))
                    })
                    .into_any_element()
                }))
                .child(
                    components::button_with_state(
                        theme,
                        "close-palette",
                        "Close",
                        components::Weight::Quiet,
                        false,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.queue(Intent::DismissOverlay, cx);
                    })),
                )
                .into_any_element(),
        ),
    }
}
