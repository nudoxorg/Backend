//! Production GPUI shell.
//!
//! The view layer is a projection only. Every label, package fact, route, and
//! loading state comes from `AppSnapshot`; every interaction emits a typed
//! `Intent`. Route families live in separate modules so package facts, reader
//! panes, and shell chrome cannot acquire one another's state.

mod catalog;
mod keys;
mod package;
mod primitives;
mod reader;
mod shell;

use crate::core::layout::{LayoutCache, PanelPreferences, ResponsiveLayout};
use crate::model::AppSnapshot;
use crate::navigation::{OrbitRoute, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::space;
use crate::ui::surface;
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, px};

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
    shell::sync_overlay(&theme, &snapshot, layout, window, cx);
    let header = shell::header(root, &theme, &snapshot, layout, cx).into_any_element();
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
    theme.publish_action_frame(window);
    // Keep the frame token and its per-window collector available to the next
    // harness probe without making the action tree process-global.
    cx.set_global(theme);
    let _ = window;
    shell.into_any_element()
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
