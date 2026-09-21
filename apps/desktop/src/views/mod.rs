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
    let theme = crate::theme::theme(cx);
    let snapshot = root.snapshot();
    let shelf = shell::shelf_panel(root, &theme, &snapshot, cx).into_any_element();
    let body = content_panel(root, &theme, &snapshot, cx).into_any_element();
    let overlay = shell::overlay(root, &theme, &snapshot, cx);
    let header = shell::header(root, &theme, &snapshot, cx).into_any_element();
    let shell = surface::ground(&theme)
        .size_full()
        .font_family(theme.ui_face())
        .child(header)
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(shelf)
                .child(body),
        )
        .child(primitives::status_bar(&theme, &snapshot))
        .when_some(overlay, ParentElement::child);
    let _ = window;
    shell.into_any_element()
}

fn content_panel(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
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
        .p(px(40.0))
        .child(content)
}
