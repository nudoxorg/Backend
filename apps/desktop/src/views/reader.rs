//! Version-pinned declaration and source reading projections.

use super::primitives::{crumb, heading};
use crate::model::AppSnapshot;
use crate::navigation::{Intent, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};

pub(super) fn document_page(
    root: &mut UiRootEntity,
    theme: &Theme,
    _snapshot: &AppSnapshot,
    route: &crate::navigation::PageRoute,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let coordinate = route.coordinate.as_str().to_owned();
    let source_route = Route::Source(crate::navigation::SourceRoute {
        project: route.project.clone(),
        package: route.package.clone(),
        page: route.coordinate.clone(),
        line: 1,
        selected: route.selected,
    });
    let _ = root;
    surface::panel(theme)
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(crumb(theme, &format!("{} / docs", route.package)))
        .child(heading(theme, &coordinate))
        .child(
            div()
                .flex()
                .gap(space(Space::Tight))
                .child(
                    components::measure(
                        theme,
                        "docs-source",
                        components::button_with_state(
                            theme,
                            "docs-source",
                            "View source",
                            components::Weight::Regular,
                            false,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.queue(Intent::Navigate(source_route.clone()), cx);
                        })),
                    ),
                )
                .child(
                    components::measure(
                        theme,
                        "docs-graph",
                        components::button_with_state(
                            theme,
                            "docs-graph",
                            "Graph",
                            components::Weight::Quiet,
                            false,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.queue(Intent::OpenCommandPalette, cx);
                        })),
                    ),
                ),
        )
        .child(
            surface::sunken(theme)
                .p(px(22.0))
                .child(text::single_line(text::body(theme)).child(
                    "Documentation is read from the versioned declaration projection. Source, code search, and graph use the same package and object identity.",
                )),
        )
        .into_any_element()
}

pub(super) fn source_page(
    _root: &mut UiRootEntity,
    theme: &Theme,
    _snapshot: &AppSnapshot,
    route: &crate::navigation::SourceRoute,
    _cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    surface::sunken(theme)
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(crumb(theme, &format!("{} / source", route.package)))
        .child(heading(theme, route.page.as_str()))
        .child(
            div()
                .mt(space(Space::Gutter))
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::Silver1))
                .child(format!(
                    "{:>4}  // source is pinned to line {}",
                    route.line, route.line
                ))
                .child("\n   1  // awaiting the live source projection")
                .child("\n   2  // the object identity remains stable across deltas"),
        )
        .into_any_element()
}
