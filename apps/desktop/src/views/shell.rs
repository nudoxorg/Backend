//! Window chrome, shelf, and transient overlays.

use super::primitives::{heading, route_label, short_root};
use crate::core::layout::{PanelMode, RegionId, ResponsiveLayout, SheetKind};
use crate::model::AppSnapshot;
use crate::navigation::{
    Intent, OrbitRoute, Overlay, PackageLane, PackageRoute, Route, SettingsPage,
};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{components, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, WeakEntity, Window, div, px,
};
use gpui_component::{Placement, Root, Selectable as _, WindowExt as _};
use std::sync::Arc;

pub(super) fn header(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let route_label = route_label(snapshot);
    let compact = layout.collapse.is_compact();
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
                .child(if compact { "N" } else { "Nudox" }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::Silver3))
                .child(route_label),
        )
        .child(components::measure(
            theme,
            "back",
            nav_button(theme, "back", "‹", Intent::Back, root, cx),
        ))
        .child(components::measure(
            theme,
            "forward",
            nav_button(theme, "forward", "›", Intent::Forward, root, cx),
        ))
        .child(components::measure(
            theme,
            "command-palette",
            components::button_with_state(
                theme,
                "command-palette",
                if compact {
                    "⌘K"
                } else {
                    "Search all docs…  ⌘K"
                },
                components::Weight::Regular,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenCommandPalette, cx);
            })),
        ))
        .child(components::measure(
            theme,
            "settings",
            components::button_with_state(
                theme,
                "settings",
                if compact { "⚙" } else { "Settings" },
                components::Weight::Quiet,
                false,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue(Intent::OpenSettings(SettingsPage::Appearance), cx);
            })),
        ))
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
            let id = format!("shelf-{index}");
            components::measure(
                theme,
                id.clone(),
                components::button_with_state(
                    theme,
                    id,
                    item.label.to_string(),
                    components::Weight::Quiet,
                    false,
                    true,
                )
                .selected(snapshot.shelf().selected.as_ref() == Some(&identity))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.queue(Intent::Navigate(route.clone()), cx);
                })),
            )
            .into_any_element()
        })
        .collect::<Vec<_>>();
    let add = components::measure(
        theme,
        "add-project",
        components::button_with_state(
            theme,
            "add-project",
            "+ Add project",
            components::Weight::Primary,
            false,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.queue(Intent::OpenSettings(SettingsPage::Index), cx);
        })),
    );
    let _ = root;
    div()
        .id(gpui::ElementId::Name("project-shelf".into()))
        .w(px(
            layout.region(RegionId::ProjectShelf).bounds.width.get() as f32
        ))
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

/// Renders the persistent orbit rail. The semantic identity remains stable
/// when its label switches from text to an icon at compact widths.
pub(super) fn orbit_rail(
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let compact = layout.collapse.is_compact();
    let home = Route::Orbit(OrbitRoute::Home);
    let shelf_sheet = (layout.shelf == PanelMode::Sheet).then(|| {
        responsive_sheet_trigger(
            theme,
            "orbit-shelf-sheet",
            if compact { "▤" } else { "Shelf" },
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
            if compact { "≡" } else { "Context" },
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
            components::button_with_state(
                theme,
                "orbit-home",
                if compact { "⌂" } else { "Home" },
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
            components::button_with_state(
                theme,
                "orbit-overflow",
                if compact { "⋯" } else { "More" },
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
    kind: SheetKind,
    width: crate::core::layout::LogicalPx,
    root: WeakEntity<UiRootEntity>,
) -> impl IntoElement {
    let sheet_theme = theme.clone();
    components::measure(
        theme,
        id,
        components::button_with_state(theme, id, label, components::Weight::Quiet, false, true)
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
                            responsive_sheet(
                                sheet,
                                &sheet_theme,
                                kind,
                                width,
                                root.clone(),
                                snapshot,
                            )
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

/// Synchronizes reducer-owned overlay intent with the CE Root overlay layer.
/// Root owns dialog focus, backdrop dismissal, scrolling, and animation;
/// this adapter only opens the requested typed surface and feeds button
/// intents back to the product reducer.
pub(super) fn sync_overlay(
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    window: &mut Window,
    cx: &mut Context<UiRootEntity>,
) {
    let desired = snapshot.overlay();
    if let Some(overlay) = desired.copied() {
        if window.has_active_dialog(cx) {
            return;
        }
        let (id, title, description) = overlay_metadata(overlay);
        theme.register_action(
            components::ActionMetadata::new(id, title, components::ActionRole::Dialog)
                .description(description),
        );
        let root = cx.weak_entity();
        let dialog_theme = theme.clone();
        let reduced_motion = snapshot.settings().reduced_motion;
        Root::update(window, cx, move |ce_root, window, ce_cx| {
            ce_root.open_dialog(
                move |dialog, _window, _app| {
                    responsive_dialog(
                        dialog,
                        &dialog_theme,
                        overlay,
                        layout,
                        reduced_motion,
                        root.clone(),
                    )
                },
                window,
                ce_cx,
            );
        });
    } else if window.has_active_dialog(cx) {
        Root::update(window, cx, |ce_root, window, ce_cx| {
            ce_root.close_all_dialogs(window, ce_cx);
        });
    }
}

const fn overlay_metadata(overlay: Overlay) -> (&'static str, &'static str, &'static str) {
    match overlay {
        Overlay::Settings(_) => (
            "settings-dialog",
            "Settings",
            "Application settings overlay",
        ),
        Overlay::CommandPalette => (
            "command-palette-dialog",
            "Command palette",
            "Search and run workspace commands",
        ),
    }
}

/// Projects one typed reducer overlay into a CE `Dialog`. The dialog's
/// controls retain stable action IDs and dispatch product intents through the
/// existing root entity; no view-local modal or focus state is introduced.
fn responsive_dialog(
    mut dialog: gpui_component::dialog::Dialog,
    theme: &Theme,
    overlay: Overlay,
    layout: ResponsiveLayout,
    reduced_motion: bool,
    root: WeakEntity<UiRootEntity>,
) -> gpui_component::dialog::Dialog {
    let (id, title, _) = overlay_metadata(overlay);
    let dialog_theme = theme.clone();
    let close_root = root.clone();
    let dialog_width = layout
        .window
        .width()
        .get()
        .saturating_sub(layout.content_padding.get().saturating_mul(2))
        .max(1);
    dialog = dialog
        .title(heading(theme, title))
        .width(px(dialog_width as f32))
        .margin_top(px(layout.region(RegionId::Titlebar).bounds.height.get()
            as f32
            + layout.content_padding.get() as f32))
        .overlay(true)
        .overlay_closable(true)
        .on_close(move |_, _, app| {
            let _ = close_root.update(app, |this, cx| {
                this.queue(Intent::DismissOverlay, cx);
            });
        })
        .content(move |mut content, _window, app| {
            let dialog_theme = dialog_theme.with_action_parent(id);
            match overlay {
                Overlay::Settings(page) => {
                    let toggle_root = root.clone();
                    let done_root = root.clone();
                    let active_reduced_motion = root.upgrade().map_or(reduced_motion, |entity| {
                        entity.read(app).snapshot().settings().reduced_motion
                    });
                    content = content
                        .child(text::single_line(text::faint(&dialog_theme)).child(page.as_str()))
                        .child(components::measure(
                            &dialog_theme,
                            "toggle-motion",
                            components::button_with_state(
                                &dialog_theme,
                                "toggle-motion",
                                if active_reduced_motion {
                                    "Enable motion"
                                } else {
                                    "Reduce motion"
                                },
                                components::Weight::Regular,
                                false,
                                true,
                            )
                            .on_click(move |_, _, app| {
                                let _ = toggle_root.update(app, |this, cx| {
                                    this.queue(Intent::ToggleReducedMotion, cx);
                                });
                            }),
                        ))
                        .child(components::measure(
                            &dialog_theme,
                            "close-settings",
                            components::button_with_state(
                                &dialog_theme,
                                "close-settings",
                                "Done",
                                components::Weight::Primary,
                                false,
                                true,
                            )
                            .on_click(move |_, window, app| {
                                let _ = done_root.update(app, |this, cx| {
                                    this.queue(Intent::DismissOverlay, cx);
                                });
                                window.close_dialog(app);
                            }),
                        ));
                }
                Overlay::CommandPalette => {
                    let actions = crate::navigation::ActionId::ALL.into_iter().map(|action| {
                        let intent = action.intent();
                        let id = format!("palette-action-{}", action.as_str());
                        let mut button = components::button_with_state(
                            &dialog_theme,
                            id.clone(),
                            action.spec().label,
                            components::Weight::Quiet,
                            intent.is_none(),
                            true,
                        );
                        if let Some(intent) = intent {
                            let action_root = root.clone();
                            let closes_dialog = matches!(
                                &intent,
                                Intent::Navigate(_)
                                    | Intent::OpenSource { .. }
                                    | Intent::ZoomOut
                                    | Intent::Back
                                    | Intent::Forward
                                    | Intent::OpenSettings(_)
                                    | Intent::DismissOverlay
                            );
                            button = button.on_click(move |_, window, app| {
                                let _ = action_root.update(app, |this, cx| {
                                    this.queue(intent.clone(), cx);
                                });
                                if closes_dialog {
                                    window.close_dialog(app);
                                }
                            });
                        }
                        components::measure(&dialog_theme, id, button)
                    });
                    content = content.child(components::vertical_scroll(
                        div().flex().flex_col().children(actions),
                    ));
                }
            }
            content
        });
    dialog
}
