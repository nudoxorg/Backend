//! The source sheet: a declaration's captured text, over the page it belongs to.
//! Every identifier the shelf knows is a door; the sheet closes behind a click.
//! It is opened by one small control, one key, and nothing else on the page.
//!
//! The page is about meaning; the sheet is about evidence. Keeping the text
//! off the page is what lets a reader compare a struct's fields with its
//! documentation without scrolling past forty lines of the struct itself —
//! and drawing the text as a sheet rather than a fold means it can be wide,
//! which source needs and a reading column does not.
//!
//! The highlighted view is built once per page and appearance and cached on
//! the window, so opening the sheet twice costs one resolution of every
//! identifier, not two, and re-rendering while it is open costs none.

use super::keys;
use super::workspace::Workspace;
use crate::store::document::{Content, Target};
use crate::theme::Theme;
use crate::theme::palette::{Appearance, Paint};
use crate::theme::tokens::{Space, hairline, space};
use crate::ui::icon::Icon;
use crate::ui::source::{self, SourceView};
use crate::ui::tip::{Tip, Tipped as _};
use crate::ui::{button, fault as fault_ui, glyph, surface, text};
use backend_present::{Page, Source};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

/// Widest the sheet grows, in pixels.
const SHEET_WIDTH: f32 = 960.0;

/// One built view, and the page and appearance it was built for.
pub(crate) struct SourceCache {
    coordinate: String,
    appearance: Appearance,
    view: SourceView,
}

impl Workspace {
    /// Opens or closes the source sheet for the page being read.
    pub(crate) fn toggle_source(&mut self, cx: &mut Context<Self>) {
        if !self.source_open && self.open_page(cx).is_none() {
            return;
        }
        self.source_open = !self.source_open;
        cx.notify();
    }

    /// Opens the source sheet before the page has arrived, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_source(&mut self, cx: &mut Context<Self>) {
        self.source_open = true;
        cx.notify();
    }

    /// Closes the source sheet.
    pub(super) fn close_source(&mut self, cx: &mut Context<Self>) {
        if self.source_open {
            self.source_open = false;
            cx.notify();
        }
    }

    /// Returns the source sheet, when it is open over a page with a site.
    pub(super) fn source_sheet(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.source_open {
            return None;
        }
        let page = self.open_page(cx)?;
        let (path, line) = super::page::site_of(&page)?;
        let body = self.source_body(theme, &page, cx);
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    surface::scrim(theme)
                        .id("source-scrim")
                        .on_click(cx.listener(|this, _, _, cx| this.close_source(cx))),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(space(Space::Gutter))
                        .child(
                            surface::raised(theme)
                                .id("source-sheet")
                                .occlude()
                                .w(px(SHEET_WIDTH))
                                .max_w(gpui::relative(1.0))
                                .h(gpui::relative(0.86))
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .child(self.source_head(theme, &page, &path, line, cx))
                                .child(body),
                        ),
                )
                .into_any_element(),
        )
    }

    fn source_head(
        &mut self,
        theme: &Theme,
        page: &Page,
        path: &str,
        line: u32,
        cx: &mut Context<Self>,
    ) -> Div {
        let spelling = format!("{path}:{line}");
        let copied = spelling.clone();
        let target = path.to_owned();
        let facts = self.source_facts(page, cx);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Base))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(glyph::kind_tile(theme, page.kind(), false))
            .child(
                text::label(theme)
                    .flex_none()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(page.identity().name().to_owned()),
            )
            .child(
                text::single_line(text::dim(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme.specimen())
                    .child(spelling),
            )
            .when_some(facts, |head, facts| head.child(text::faint(theme).flex_none().child(facts)))
            .child(
                button::button(theme, "source-editor", "Open in editor", button::Weight::Regular)
                    .tip(Tip::new("Open in editor").key(keys::OPEN_EDITOR))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_in_editor(&target, line, cx);
                    })),
            )
            .child(
                button::icon_button(theme, "source-copy", Icon::Copy)
                    .tip(Tip::new("Copy path and line").value(copied.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy("Path copied", copied.clone(), cx);
                    })),
            )
            .child(
                button::icon_button(theme, "source-close", Icon::Close)
                    .tip(Tip::new("Close").key(keys::DISMISS))
                    .on_click(cx.listener(|this, _, _, cx| this.close_source(cx))),
            )
    }

    /// Returns the "N lines · N linked" statement for the head.
    fn source_facts(&mut self, page: &Page, cx: &mut Context<Self>) -> Option<String> {
        let Source::Captured { .. } = page.source() else {
            return None;
        };
        let theme = self.theme(cx);
        let view = self.source_view(&theme, page, cx)?;
        Some(match view.doors() {
            0 => format!("{} lines", view.len()),
            doors => format!("{} lines · {doors} linked", view.len()),
        })
    }

    fn source_body(&mut self, theme: &Theme, page: &Page, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match page.source() {
            Source::Captured { .. } => match self.source_view(theme, page, cx) {
                Some(view) => {
                    let entity = cx.entity();
                    source::block(theme, &view, "source", move |symbol, _, cx| {
                        entity.update(cx, |workspace, cx| {
                            workspace.close_source(cx);
                            workspace.open_symbol(symbol, Target::Child, cx);
                        });
                    })
                    .into_any_element()
                }
                None => div().into_any_element(),
            },
            Source::Sited { fault, .. } | Source::Absent { fault } => {
                let actions = Self::affordances(theme, "source", fault, "", cx);
                fault_ui::block(theme, fault, actions).into_any_element()
            }
        };
        div()
            .id("source-body")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .px(space(Space::Base))
            .py(space(Space::Base))
            .child(body)
            .into_any_element()
    }

    /// Returns the highlighted view for one page, building it once.
    fn source_view(&mut self, theme: &Theme, page: &Page, cx: &Context<Self>) -> Option<SourceView> {
        let coordinate = page.identity().coordinate().as_str();
        let appearance = self.shell.read(cx).prefs().appearance();
        let fresh = self
            .source_cache
            .as_ref()
            .filter(|cache| cache.coordinate == coordinate && cache.appearance == appearance);
        if let Some(cache) = fresh {
            return Some(cache.view.clone());
        }
        let Source::Captured {
            site,
            lines,
            truncation,
        } = page.source()
        else {
            return None;
        };
        let own = page.identity().name().to_owned();
        let (symbol, near) = match page.identity().key() {
            backend_present::IdentityKey::Symbol(symbol) => (
                Some(symbol),
                self.index.read(cx).project_of(symbol).map(|project| project.root().to_owned()),
            ),
            _ => (None, None),
        };
        let index = self.index.read(cx);
        let project = near.as_deref().and_then(|root| index.project(root));
        let resolve = |name: &str| {
            if name == own {
                return None;
            }
            project
                .and_then(|project| project.by_name(name))
                .map(crate::store::index::Entry::symbol)
                .filter(|found| Some(*found) != symbol)
        };
        let view = SourceView::build(theme, page.language(), lines, site.line().get(), *truncation, &resolve);
        self.source_cache = Some(SourceCache {
            coordinate: coordinate.to_owned(),
            appearance,
            view: view.clone(),
        });
        Some(view)
    }

    /// Returns the declaration page the reader is showing, if any.
    fn open_page(&self, cx: &Context<Self>) -> Option<Page> {
        match self.document.read(cx).tab()?.content() {
            Content::Page(page) => Some((**page).clone()),
            _ => None,
        }
    }

}
