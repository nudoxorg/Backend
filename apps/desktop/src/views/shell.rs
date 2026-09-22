//! Window chrome, shelf, and transient overlays.

use super::primitives::{heading, route_label};
use super::project_shelf;
use crate::core::layout::{PanelMode, RegionId, ResponsiveLayout, SheetKind};
use crate::model::AppSnapshot;
use crate::navigation::{Intent, OrbitRoute, PackageLane, PackageRoute, Route, SettingsPage};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::components;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, WeakEntity, div, px,
};
use gpui_component::{Placement, Root, Selectable as _, WindowExt as _};
use std::sync::Arc;

pub(super) fn header(
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let route_label = route_label(snapshot);
    let density = layout.titlebar_density;
    let expanded = density.is_expanded();
    let mut actions = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(space(Space::Snug));
    if density.shows_history() {
        actions = actions
            .child(nav_button(theme, "back", "‹", Intent::Back, cx))
            .child(nav_button(theme, "forward", "›", Intent::Forward, cx));
    }
    actions = actions.child(crate::ui::search_palette::header_trigger(
        theme, cx, !expanded,
    ));
    if density.shows_settings() {
        actions = actions.child(
            components::button_with_state(
                theme,
                "settings",
                if expanded { "Settings" } else { "⚙" },
                components::Weight::Quiet,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenSettings(SettingsPage::Appearance), cx);
            })),
        );
    }
    theme.register_action(
        components::ActionMetadata::new(
            "window-navigation",
            "Window navigation",
            components::ActionRole::Navigation,
        )
        .description("Route history, search, settings, and workspace navigation"),
    );
    div()
        .id(gpui::ElementId::Name("window-navigation".into()))
        .role(gpui::Role::Navigation)
        .aria_label("Window navigation")
        .aria_description("Route history, search, settings, and workspace navigation")
        .h(px(
            layout.region(RegionId::Titlebar).bounds.height.get() as f32
        ))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(space(if expanded { Space::Gutter } else { Space::Snug }))
        .px(space(if expanded { Space::Gutter } else { Space::Snug }))
        .border_b(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(
            div()
                .text_size(type_size(TypeScale::Title))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Silver0))
                .child(if expanded { "Nudox" } else { "N" }),
        )
        .when(expanded, |header| {
            header.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(type_size(TypeScale::Small))
                    .text_color(theme.paint(Paint::Silver3))
                    .child(route_label),
            )
        })
        .child(actions.ml_auto())
}

fn nav_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    intent: Intent,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    components::button_with_state(theme, id, label, components::Weight::Quiet, false, true)
        .on_click(cx.listener(move |this, _, _, cx| this.queue(intent.clone(), cx)))
}

pub(super) fn shelf_panel(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    if layout.shelf != PanelMode::Full {
        return div()
            .id(gpui::ElementId::Name("project-shelf-flow-hidden".into()))
            .w(px(0.0))
            .h_full()
            .flex_none();
    }
    let _ = root;
    div()
        .id(gpui::ElementId::Name("project-shelf".into()))
        .w(px(
            layout.region(RegionId::ProjectShelf).bounds.width.get() as f32
        ))
        .flex_none()
        .h_full()
        .child(project_shelf::panel(theme, snapshot, true, cx))
}

/// Renders the persistent orbit rail. The semantic identity remains stable
/// when its label switches from text to an icon at compact widths.
pub(super) fn orbit_rail(
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let home = Route::Orbit(OrbitRoute::Home);
    let shelf_sheet = (layout.shelf == PanelMode::Sheet).then(|| {
        responsive_sheet_trigger(
            theme,
            "orbit-shelf-sheet",
            "▤",
            "Shelf",
            SheetKind::Shelf,
            layout.sheet_width(SheetKind::Shelf),
            cx.weak_entity(),
        )
        .into_any_element()
    });
    let context_sheet = (layout.context == PanelMode::Sheet).then(|| {
        responsive_sheet_trigger(
            theme,
            "orbit-context-sheet",
            "≡",
            "Context",
            SheetKind::Context,
            layout.sheet_width(SheetKind::Context),
            cx.weak_entity(),
        )
        .into_any_element()
    });
    div()
        .id(gpui::ElementId::Name("orbit-rail".into()))
        .w(px(
            layout.region(RegionId::OrbitRail).bounds.width.get() as f32
        ))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(space(Space::Snug))
        .p(space(Space::Snug))
        .border_r(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(components::measure(
            theme,
            "orbit-home",
            components::button_with_state_and_accessible(
                theme,
                "orbit-home",
                "⌂",
                "Home",
                components::Weight::Quiet,
                snapshot.route() == &home,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.queue(Intent::Navigate(home.clone()), cx);
            })),
        ))
        .when_some(shelf_sheet, ParentElement::child)
        .when_some(context_sheet, ParentElement::child)
        .child(div().flex_1())
        .child(components::measure(
            theme,
            "orbit-overflow",
            components::button_with_state_and_accessible(
                theme,
                "orbit-overflow",
                "⋯",
                "More",
                components::Weight::Quiet,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenCommandPalette, cx);
            })),
        ))
}

/// Opens a collapsed pane through the CE Root-owned Sheet layer. The
/// resolver supplies only the semantic kind and measured width; Root owns
/// focus capture, backdrop dismissal, scrolling, and the native close path.
fn responsive_sheet_trigger(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    accessible_label: &'static str,
    kind: SheetKind,
    width: crate::core::layout::LogicalPx,
    root: WeakEntity<UiRootEntity>,
) -> impl IntoElement {
    let sheet_theme = theme.clone();
    components::measure(
        theme,
        id,
        components::button_with_state_and_accessible(
            theme,
            id,
            label,
            accessible_label,
            components::Weight::Quiet,
            false,
            true,
        )
        .on_click(move |_, window, app| {
            let sheet_theme = sheet_theme.clone();
            let root = root.clone();
            let placement = match kind {
                SheetKind::Shelf => Placement::Left,
                SheetKind::Context | SheetKind::Details => Placement::Right,
            };
            Root::update(window, app, move |ce_root, window, ce_cx| {
                ce_root.open_sheet_at(
                    placement,
                    move |sheet, _window, app| {
                        let Some(root_entity) = root.upgrade() else {
                            return sheet;
                        };
                        let snapshot = root_entity.read(app).snapshot();
                        responsive_sheet(sheet, &sheet_theme, kind, width, root.clone(), snapshot)
                    },
                    window,
                    ce_cx,
                );
            });
        }),
    )
}

/// Projects one resolver sheet into the CE `Sheet` component. Route changes
/// still enter the product reducer; this adapter only closes the CE-owned
/// presentation after dispatching the route intent.
fn responsive_sheet(
    mut sheet: gpui_component::sheet::Sheet,
    theme: &Theme,
    kind: SheetKind,
    width: crate::core::layout::LogicalPx,
    root: WeakEntity<UiRootEntity>,
    snapshot: Arc<AppSnapshot>,
) -> gpui_component::sheet::Sheet {
    let title = match kind {
        SheetKind::Shelf => "Shelf",
        SheetKind::Context => "Context",
        SheetKind::Details => "Details",
    };
    sheet = sheet
        .title(heading(theme, title))
        .size(px(width.get() as f32))
        .resizable(false);
    match kind {
        SheetKind::Shelf => {
            for (index, item) in snapshot.shelf().items.iter().enumerate() {
                let identity = item.identity.clone();
                let route = match identity.clone() {
                    crate::core::ResourceIdentity::Project(project) => {
                        Route::Orbit(OrbitRoute::Project(project))
                    }
                    crate::core::ResourceIdentity::Local(_) => Route::Orbit(OrbitRoute::Home),
                    crate::core::ResourceIdentity::Package(package) => {
                        Route::Package(PackageRoute {
                            project: None,
                            package,
                            lane: PackageLane::Overview,
                            selected: Some(item.object),
                        })
                    }
                };
                let id = format!("responsive-shelf-{index}");
                let row_root = root.clone();
                let button = components::button_with_state(
                    theme,
                    id.clone(),
                    item.label.to_string(),
                    components::Weight::Quiet,
                    false,
                    true,
                )
                .selected(snapshot.shelf().selected.as_ref() == Some(&identity))
                .on_click(move |_, window, app| {
                    let _ = row_root.update(app, |this, cx| {
                        this.queue(Intent::Navigate(route.clone()), cx);
                    });
                    window.close_sheet(app);
                });
                sheet = sheet.child(components::measure(theme, id, button));
            }
        }
        SheetKind::Context | SheetKind::Details => {
            let entries: [(&'static str, Option<Route>); 3] = match snapshot.route() {
                Route::Package(route) => [
                    (
                        "Overview",
                        Some(Route::Package(PackageRoute {
                            project: route.project.clone(),
                            package: route.package.clone(),
                            lane: PackageLane::Overview,
                            selected: route.selected,
                        })),
                    ),
                    (
                        "Dependencies",
                        Some(Route::Package(PackageRoute {
                            project: route.project.clone(),
                            package: route.package.clone(),
                            lane: PackageLane::Dependencies,
                            selected: route.selected,
                        })),
                    ),
                    (
                        "Security",
                        Some(Route::Package(PackageRoute {
                            project: route.project.clone(),
                            package: route.package.clone(),
                            lane: PackageLane::Security,
                            selected: route.selected,
                        })),
                    ),
                ],
                _ => [("Outline", None), ("History", None), ("Details", None)],
            };
            for (index, (label, route)) in entries.into_iter().enumerate() {
                let id = format!("responsive-context-{index}");
                let mut button = components::button_with_state(
                    theme,
                    id.clone(),
                    label,
                    components::Weight::Quiet,
                    route.is_none(),
                    true,
                );
                if let Some(route) = route {
                    let row_root = root.clone();
                    button = button.on_click(move |_, window, app| {
                        let _ = row_root.update(app, |this, cx| {
                            this.queue(Intent::Navigate(route.clone()), cx);
                        });
                        window.close_sheet(app);
                    });
                }
                sheet = sheet.child(components::measure(theme, id, button));
            }
        }
    }
    sheet
}

/// Renders the contextual rail when it fits in flow. At narrower widths the
/// canonical layout advertises its sheet presentation and keeps the reader
/// column free of an unreachable fixed-width pane.
pub(super) fn context_panel(
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    if layout.context != PanelMode::Full {
        return div()
            .id(gpui::ElementId::Name("context-rail-flow-hidden".into()))
            .w(px(0.0))
            .h_full()
            .flex_none();
    }
    let entries: [(&'static str, Option<Route>); 3] = match snapshot.route() {
        Route::Package(route) => [
            (
                "Overview",
                Some(Route::Package(PackageRoute {
                    project: route.project.clone(),
                    package: route.package.clone(),
                    lane: PackageLane::Overview,
                    selected: route.selected,
                })),
            ),
            (
                "Dependencies",
                Some(Route::Package(PackageRoute {
                    project: route.project.clone(),
                    package: route.package.clone(),
                    lane: PackageLane::Dependencies,
                    selected: route.selected,
                })),
            ),
            (
                "Security",
                Some(Route::Package(PackageRoute {
                    project: route.project.clone(),
                    package: route.package.clone(),
                    lane: PackageLane::Security,
                    selected: route.selected,
                })),
            ),
        ],
        _ => [("Outline", None), ("History", None), ("Details", None)],
    };
    let rows = entries
        .into_iter()
        .enumerate()
        .map(|(index, (label, route))| {
            let id = format!("context-{index}");
            components::measure(
                theme,
                id.clone(),
                components::button_with_state(
                    theme,
                    id,
                    label,
                    components::Weight::Quiet,
                    false,
                    route.is_some(),
                )
                .when_some(route, |button, route| {
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        this.queue(Intent::Navigate(route.clone()), cx);
                    }))
                }),
            )
            .into_any_element()
        });
    div()
        .id(gpui::ElementId::Name("context-rail".into()))
        .w(px(
            layout.region(RegionId::ContextRail).bounds.width.get() as f32
        ))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .p(space(Space::Gutter))
        .border_l(px(1.0))
        .border_color(theme.paint(Paint::Rule2))
        .bg(theme.paint(Paint::Abyss1))
        .child(
            div()
                .text_size(type_size(TypeScale::Small))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.paint(Paint::Silver2))
                .child("CONTEXT"),
        )
        .children(rows)
}
