//! Production GPUI shell.
//!
//! The view layer is a projection only. Every label, package fact, route, and
//! loading state comes from `AppSnapshot`; every interaction emits a typed
//! `Intent`. Route families live in separate modules so package facts, reader
//! panes, and shell chrome cannot acquire one another's state.

mod catalog;
mod keys;
mod local_package;
mod onboarding;
mod package;
mod primitives;
mod project_admission;
mod project_shelf;
mod reader;
mod shell;
mod workspace_settings;

use crate::core::layout::{LayoutCache, PanelPreferences, ResponsiveLayout};
use crate::model::AppSnapshot;
use crate::navigation::{OrbitRoute, Overlay, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::ui::surface;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Focusable as _, IntoElement, ParentElement, Styled,
    Window, div, px,
};
use gpui_component::WindowExt as _;

/// Renders the complete production window from one root entity.
pub(crate) fn render_root(
    root: &mut UiRootEntity,
    window: &mut Window,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let mut theme = crate::theme::theme(cx);
    let snapshot = root.snapshot();
    // Start a fresh semantic registration frame before any component is
    // constructed.  The same token is passed through every shared builder,
    // including overlays, so the accessibility tree cannot retain controls
    // from a previous route or silently drift from the rendered shell.
    theme.begin_action_frame_with_modal(
        window,
        primitives::route_label(&snapshot),
        snapshot.overlay().is_some(),
    );
    let layout_input = LayoutCache::input_from_window(
        window,
        theme.layout_text_scale(),
        PanelPreferences {
            shelf_open: snapshot.settings().shelf_open,
            context_open: snapshot.settings().context_open,
        },
    );
    let layout = theme.responsive_layout(layout_input);
    // The CE Root owns the actual dialog/sheet lifetime, focus trap, scrim,
    // resize handle, and animation. The product root keeps only a typed route
    // marker so reducer state remains authoritative without a second overlay
    // stack.
    cx.set_global(theme.clone());
    sync_component_overlay(root, window, cx, &snapshot);
    let header = shell::header(&theme, &snapshot, layout, cx).into_any_element();
    let orbit = shell::orbit_rail(&theme, &snapshot, layout, cx).into_any_element();
    let shelf = shell::shelf_panel(root, &theme, &snapshot, layout, cx).into_any_element();
    let body = content_panel(root, &theme, &snapshot, layout, cx).into_any_element();
    let context = shell::context_panel(&theme, &snapshot, layout, cx).into_any_element();
    let status = primitives::status_bar(&theme, &snapshot, layout).into_any_element();
    let shell = surface::ground(&theme)
        .size_full()
        .font_family(theme.ui_face())
        .child(header)
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(orbit)
                .child(shelf)
                .child(body)
                .child(context),
        )
        .child(status);
    // Register the modal semantic roots after the document controls so the
    // final action frame can mark the background inert and retain the actual
    // launch control for close restoration.
    register_overlay_actions(&theme, snapshot.overlay());
    let sheet_layer = gpui_component::Root::render_sheet_layer(window, cx);
    let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
    theme.publish_action_frame(window);
    // Keep the frame token and its per-window collector available to the next
    // harness probe without making the action tree process-global.
    cx.set_global(theme);
    shell
        .children(sheet_layer)
        .children(dialog_layer)
        .into_any_element()
}

fn register_overlay_actions(theme: &Theme, overlay: Option<Overlay>) {
    match overlay {
        Some(Overlay::AddProject) => {
            theme.register_action(
                crate::ui::components::ActionMetadata::new(
                    "add-project-dialog",
                    "Add a local project",
                    crate::ui::components::ActionRole::Dialog,
                )
                .description("Choose a local source folder to index"),
            );
        }
        Some(Overlay::CommandPalette) => {
            theme.register_action(
                crate::ui::components::ActionMetadata::new(
                    "command-palette-dialog",
                    "Search workspace",
                    crate::ui::components::ActionRole::Dialog,
                )
                .description("Search projects and workspace actions"),
            );
            theme.register_action(
                crate::ui::components::ActionMetadata::new(
                    "command-palette-search",
                    "Search workspace",
                    crate::ui::components::ActionRole::Search,
                )
                .description("Search projects and workspace actions")
                .parent("command-palette-dialog"),
            );
        }
        Some(Overlay::Settings(_)) => {
            theme.register_action(
                crate::ui::components::ActionMetadata::new(
                    "settings-dialog",
                    "Settings",
                    crate::ui::components::ActionRole::Dialog,
                )
                .description("Workspace settings"),
            );
        }
        None => {}
    }
}

fn sync_component_overlay(
    root: &mut UiRootEntity,
    window: &mut Window,
    cx: &mut Context<UiRootEntity>,
    snapshot: &AppSnapshot,
) {
    let desired = snapshot.overlay();
    if root.component_overlay() == desired {
        return;
    }
    if root.component_overlay().is_some() {
        window.close_all_dialogs(cx);
        window.close_sheet(cx);
    }
    root.set_component_overlay(desired);
    let Some(desired) = desired else {
        return;
    };
    let owner = cx.entity();
    match desired {
        Overlay::AddProject => {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder(project_admission::example_path())
            });
            let focus = input.read(cx).focus_handle(cx);
            let dialog_owner = owner.clone();
            let dialog_input = input.clone();
            window.open_dialog(cx, move |dialog, window, app| {
                let theme = crate::theme::theme(app);
                let width = (window.viewport_size().width.as_f32() - 32.0).clamp(320.0, 560.0);
                project_admission::dialog(
                    dialog,
                    &theme,
                    dialog_owner.clone(),
                    dialog_input.clone(),
                    width,
                )
            });
            window.defer(cx, move |window, app| focus.focus(window, app));
        }
        Overlay::CommandPalette => {
            let palette_owner = owner.clone();
            window.open_dialog(cx, move |dialog, window, _| {
                let width = (window.viewport_size().width.as_f32() - 32.0).clamp(320.0, 680.0);
                crate::ui::search_palette::dialog(dialog, palette_owner.clone(), width)
            });
        }
        Overlay::Settings(page) => {
            let settings_owner = owner.clone();
            window.open_sheet_at(
                gpui_component::Placement::Right,
                cx,
                move |sheet, window, app| {
                    let theme = crate::theme::theme(app);
                    let width = (window.viewport_size().width.as_f32() - 24.0).clamp(360.0, 820.0);
                    workspace_settings::sheet(sheet, &theme, settings_owner.clone(), page, width)
                },
            );
        }
    }
}

fn content_panel(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    layout: ResponsiveLayout,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let content = match snapshot.route() {
        Route::Orbit(OrbitRoute::Home) => catalog::home_page(root, theme, snapshot, cx),
        Route::Orbit(OrbitRoute::Project(_)) => catalog::project_page(root, theme, snapshot, cx),
        Route::Package(route) => package::package_page(root, theme, snapshot, route, cx),
        Route::Page(route) => reader::document_page(root, theme, snapshot, route, cx),
        Route::Source(route) => reader::source_page(root, theme, snapshot, route, cx),
    };
    div()
        .flex_1()
        .min_w(px(0.0))
        .min_h(px(0.0))
        .p(px(layout.content_padding.get() as f32))
        .child(content)
}
