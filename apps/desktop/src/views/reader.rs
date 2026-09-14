//! The reader: a tab strip and whatever the active tab is showing.
//! The previous page stays until the next one has arrived; nothing flashes.
//! A failure renders here, in place, with the same weight a page would have.
//!
//! The tab strip disappears when there is one tab, because a single tab is not
//! a choice and a control that offers no choice is noise. Middle-click closes,
//! option-click opens behind — the two gestures a reader brings from every
//! other document surface they use.
//!
//! The scrolling element is the reading column itself rather than a full-width
//! box wrapped around one, and that is load-bearing rather than cosmetic: it
//! makes each region of the page a direct child of the scroll container, which
//! is what lets the outline panel's jump list scroll to a section by index
//! instead of guessing at an offset.

use super::workspace::Workspace;
use crate::motion::{Beat, entering_opacity, once};
use crate::store::document::{Content, Tab, Target};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Radius, Space, TypeScale, hairline, radius, space};
use crate::ui::{button, fault as fault_ui, surface, text};
use backend_present::Identity;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement,
    IntoElement, ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    div, px,
};

/// Widest the reading column ever grows, in pixels.
const MEASURE: f32 = 880.0;

impl Workspace {
    /// Returns the centre column.
    pub(super) fn reader(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.document.read(cx).tabs().len();
        div()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .flex()
            .flex_col()
            .when(tabs > 1, |column| column.child(self.tab_strip(theme, cx)))
            .child(self.reader_body(theme, cx))
    }

    fn tab_strip(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.document.read(cx).active();
        let titles: Vec<String> = self
            .document
            .read(cx)
            .tabs()
            .iter()
            .map(Tab::title)
            .collect();
        div()
            .id("tab-strip")
            .flex_none()
            .h(px(Chrome::TABS))
            .w_full()
            .overflow_x_scroll()
            .flex()
            .items_stretch()
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .children(
                titles
                    .into_iter()
                    .enumerate()
                    .map(|(at, title)| Self::tab(theme, at, &title, at == active, cx)),
            )
    }

    fn tab(
                theme: &Theme,
        at: usize,
        title: &str,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(SharedString::from(format!("tab-{at}"))))
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Tight))
            .px(space(Space::Base))
            .max_w(px(200.0))
            .when(active, |tab| {
                tab.bg(theme.paint(Paint::Panel))
                    .border_b(px(2.0))
                    .border_color(theme.paint(Paint::Gilt))
            })
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select_tab(at, cx)))
            .on_aux_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if event.is_middle_click() {
                    this.document
                        .update(cx, |document, cx| document.close(at, cx));
                }
            }))
            .child(
                text::single_line(if active {
                    text::label(theme).font_weight(FontWeight::MEDIUM)
                } else {
                    text::dim(theme)
                })
                .child(title.to_owned()),
            )
            .child(
                button::icon_button(theme, format!("tab-close-{at}"), crate::ui::icon::Icon::Close)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.document
                            .update(cx, |document, cx| document.close(at, cx));
                    })),
            )
            .into_any_element()
    }

    fn reader_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        if self.document.read(cx).is_empty() {
            return self.first_run(theme, cx).into_any_element();
        }
        let tab = self.document.read(cx).tab();
        let pending = tab.and_then(|tab| tab.pending().cloned());
        let scroll = tab.map_or_else(ScrollHandle::new, |tab| tab.scroll().clone());
        let content = tab
            .map_or(Content::Blank, |tab| tab.content().clone());
        let regions = self.content(theme, &content, cx);
        let reduced = theme.reduced_motion();
        let generation = self.document.read(cx).generation();
        div()
            .id("reader-body")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .max_w(px(MEASURE))
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
            Content::Page(page) => self.declaration_page(theme, page, cx),
            Content::Project { coordinate } => {
                vec![self.project_page(theme, coordinate, cx).into_any_element()]
            }
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

/// Returns where an option-click should open a link.
pub(super) const fn modifier_target(alternate: bool) -> Target {
    if alternate {
        Target::Background
    } else {
        Target::Here
    }
}
