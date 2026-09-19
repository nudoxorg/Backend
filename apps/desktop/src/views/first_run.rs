//! The empty states, designed rather than defaulted.
//! Zero projects, a shelf with projects but nothing open, and a failing feed.
//! Each one states the situation and offers the two ways forward.
//!
//! An empty window is the first thing a new reader sees and the last thing a
//! product usually designs. This one says what the application is for in one
//! sentence, gives the two real ways to put something on the shelf, and offers
//! three coordinates that work — so the first successful action costs a click.

use super::workspace::Workspace;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, fault as fault_ui, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::Focusable as _;
use gpui::{
    Context, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};

impl Workspace {
    /// Returns the centred empty state for a reader with nothing open.
    pub(super) fn first_run(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let empty_shelf = self
            .jobs
            .read(cx)
            .merge(self.engine.read(cx).shelf())
            .is_empty();
        let fault = self.engine.read(cx).fault().cloned();
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .p(space(Space::Bay))
            .child(
                surface::raised(theme)
                    .w(px(560.0))
                    .p(space(Space::Gutter))
                    .flex()
                    .flex_col()
                    .gap(space(Space::Loose))
                    .child(Self::welcome(theme, empty_shelf, cx))
                    .when_some(fault, |card, fault| {
                        let actions = Self::affordances(theme, "first-run", &fault, "", cx);
                        card.child(fault_ui::block(theme, &fault, actions))
                    }),
            )
    }

    fn welcome(
                theme: &Theme,
        empty_shelf: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Base))
            .child(
                text::heading(theme, TypeScale::Display).child(if empty_shelf {
                    "Nothing is on the shelf yet."
                } else {
                    "Pick something to read."
                }),
            )
            .child(text::body(theme).child(if empty_shelf {
                "Add a project and Nudox compiles it, indexes every declaration in it, and makes all of them readable and linked."
            } else {
                "Search with ⌘K, or choose a project from the shelf on the left."
            }))
            .when(empty_shelf, |card| {
                card.child(Self::welcome_actions(theme, cx))
                    .child(Self::example_chips(theme, cx))
            })
            .when(!empty_shelf, |card| card.child(Self::shortcut_grid(theme)))
    }

    fn welcome_actions(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap(space(Space::Snug))
            .child(
                button::button(
                    theme,
                    "welcome-folder",
                    "Choose a folder…",
                    button::Weight::Primary,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_add_from_welcome(window, cx);
                })),
            )
            .child(
                button::button(
                    theme,
                    "welcome-command",
                    "Open the command palette",
                    button::Weight::Regular,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_field(">".to_owned(), cx);
                    this.focus_field(window, cx);
                })),
            )
    }

    fn example_chips(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(text::faint(theme).child("Or index one of these:"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Tight))
                    .children(super::library::EXAMPLES.iter().enumerate().map(
                        |(at, example)| {
                            div()
                                .id(ElementId::Name(SharedString::from(format!(
                                    "welcome-example-{at}"
                                ))))
                                .px(space(Space::Snug))
                                .py(px(3.0))
                                .rounded(radius(Radius::Hair))
                                .border(hairline())
                                .border_color(theme.paint(Paint::Hairline))
                                .font_family(theme.specimen())
                                .text_size(type_size(TypeScale::Small))
                                .text_color(theme.paint(Paint::TextDim))
                                .cursor_pointer()
                                .hover(|style| {
                                    style
                                        .bg(theme.paint(Paint::Hover))
                                        .text_color(theme.paint(Paint::Gilt))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.index_project((*example).to_owned(), cx);
                                }))
                                .child((*example).to_owned())
                        },
                    )),
            )
    }

    fn shortcut_grid(theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .children(SHORTCUTS.map(|(keys, what)| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(button::key_hint(theme, keys))
                    .child(text::dim(theme).child(what))
            }))
    }

    fn start_add_from_welcome(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.begin_add(window, cx);
    }

    /// Moves focus into the omnibar field.
    pub(super) fn focus_field(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell.update(cx, |shell, cx| {
            shell.focus_on(crate::store::shell::Focus::Omnibar, cx);
        });
        self.search.update(cx, super::super::store::search::SearchStore::open);
    }
}

/// The shortcuts worth learning first.
const SHORTCUTS: [(&str, &str); 6] = [
    ("⌘K", "Search every declaration"),
    ("⌘L", "Run a product command"),
    ("⌘⇧A", "Add a project"),
    ("⌘B", "Show or hide the shelf"),
    ("⌘\\", "Show or hide the outline"),
    ("⌘,", "Settings and capabilities"),
];
