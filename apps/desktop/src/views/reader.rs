//! The reader: whatever the active tab is showing, in one reading column.
//! The previous page stays until the next one has arrived; nothing flashes.
//! A failure renders here, in place, with the same weight a page would have.
//!
//! Tabs live in the projects panel as a tree, not here as a strip. With no
//! tab at all the column shows the home page, so the window never opens on
//! nothing and the first click a reader makes is the same click they will
//! make every day.
//!
//! The column has two measures. A declaration page is prose and a signature,
//! and reads best at a book's width; the home page holds a grid of package
//! cards and a search field that should feel large, and takes the wider one.

use super::workspace::Workspace;
use crate::motion::{Beat, entering_opacity, once};
use crate::store::document::Content;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, radius, space};
use crate::ui::{fault as fault_ui, surface, text};
use backend_present::Identity;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, ElementId, InteractiveElement,
    IntoElement, ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    div, px,
};

/// Widest a declaration page grows, in pixels.
const MEASURE: f32 = 880.0;

/// Widest the home page grows, in pixels: room for three package cards.
const HOME_MEASURE: f32 = 1220.0;

impl Workspace {
    /// Returns the centre column.
    pub(super) fn reader(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .flex()
            .flex_col()
            .child(self.reader_body(theme, cx))
    }

    fn reader_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.document.read(cx).tab();
        let pending = tab.and_then(|tab| tab.pending().cloned());
        let scroll = tab.map_or_else(ScrollHandle::new, |tab| tab.scroll().clone());
        let content = tab.map_or(Content::Home, |tab| tab.content().clone());
        let measure = match content {
            Content::Home => HOME_MEASURE,
            _ => MEASURE,
        };
        let regions = self.content(theme, &content, cx);
        let reduced = theme.reduced_motion();
        let generation = self.document.read(cx).generation();
        div()
            .id("reader-body")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .max_w(px(measure))
            .mx_auto()
            .px(space(Space::Margin))
            .py(space(Space::Gutter))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .when_some(pending, |body, identity| {
                body.child(loading_bar(theme, &identity))
            })
            .children(regions)
            .with_animation(
                ElementId::Name(SharedString::from(format!("page-swap-{generation}"))),
                once(Beat::Reveal, reduced),
                |body, delta| body.opacity(entering_opacity(delta)),
            )
            .into_any_element()
    }

    fn content(
        &mut self,
        theme: &Theme,
        content: &Content,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match content {
            Content::Blank => vec![reserved(theme).into_any_element()],
            Content::Home => vec![self.home_page(theme, cx)],
            Content::Page(page) => self.declaration_page(theme, page, cx),
            Content::Project { coordinate } => {
                vec![self.project_page(theme, coordinate, cx).into_any_element()]
            }
            Content::Outline { coordinate } => {
                vec![self.project_page(theme, coordinate, cx).into_any_element()]
            }
            Content::Package { coordinate } => vec![self.package_page(theme, coordinate, cx)],
            Content::Faulted(fault) => {
                let actions = Self::affordances(theme, "reader", fault, "", cx);
                vec![fault_ui::block(theme, fault, actions).into_any_element()]
            }
        }
    }
}

/// Returns the reserved-geometry block a tab shows before its first page.
fn reserved(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .child(skeleton(theme, 280.0, 22.0))
        .child(skeleton(theme, 520.0, 14.0))
        .child(surface::sunken(theme).w_full().h(px(76.0)).flex_none())
        .child(skeleton(theme, 640.0, 14.0))
        .child(skeleton(theme, 480.0, 14.0))
}

fn skeleton(theme: &Theme, width: f32, height: f32) -> Div {
    div()
        .w(px(width))
        .max_w(gpui::relative(1.0))
        .h(px(height))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
}

/// Returns the thin bar that says which identity is arriving.
fn loading_bar(theme: &Theme, identity: &Identity) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .px(space(Space::Base))
        .py(px(4.0))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
        .child(text::faint(theme).child("reading"))
        .child(
            text::single_line(text::identity_text(theme, TypeScale::Tiny))
                .child(identity.name().to_owned()),
        )
}
