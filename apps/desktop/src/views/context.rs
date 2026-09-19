//! The context panel: the current project's outline and a jump list.
//! The declaration being read is highlighted and revealed, never hunted for.
//! Ten thousand rows cost one screen of work, because the list is virtualized.
//!
//! This panel answers "where am I?" and nothing else. It is the only place in
//! the window that shows the whole project at once, so it is a flat, dense,
//! file-ordered list rather than a tree that must be unfolded — a reader
//! scanning for a name should not have to guess which branch it is under.
//!
//! Two things make it usable at ten thousand rows. The list is a
//! `uniform_list`, so only the visible rows are built. And the row for the
//! declaration being read is scrolled into view whenever it changes, so
//! following a link from a signature moves the panel with the reader instead
//! of leaving them to scroll for their own position.

use super::workspace::Workspace;
use crate::store::document::{Content, Target};
use crate::store::shell::Side;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{
    Chrome, PanelWidth, Radius, Space, TypeScale, hairline, radius, space, type_size,
};
use crate::ui::{button, glyph, surface, text};
use backend_library::{DeclarationKind, RowId, SymbolKey};
use backend_present::{Identity, IdentityKey};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
    uniform_list,
};

/// One row of the outline list.
#[derive(Clone)]
struct OutlineRow {
    symbol: Option<SymbolKey>,
    label: String,
    kind: Option<DeclarationKind>,
    file: bool,
}

impl Workspace {
    /// Returns the right panel at its current animated width.
    pub(super) fn context_panel(
        &mut self,
        theme: &Theme,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if width < PanelWidth::MIN_CONTEXT {
            return Self::context_rail(theme, width, cx);
        }
        let rows = self.outline_rows(cx);
        let current = self.current_symbol(cx);
        self.reveal_current(&rows, current);
        surface::panel(theme)
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_l(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .child(Self::context_head(theme, rows.len(), cx))
            .child(self.outline_list(theme, rows, current, cx))
            .child(self.jump_list(theme, cx))
            .into_any_element()
    }

    fn context_rail(theme: &Theme, width: f32, cx: &mut Context<Self>) -> AnyElement {
        if width < 1.0 {
            return div().into_any_element();
        }
        surface::panel(theme)
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_l(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .items_center()
            .py(space(Space::Snug))
            .child(
                button::icon_button(theme, "expand-context", crate::ui::icon::Icon::ChevronLeft)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Context, cx));
                    })),
            )
            .into_any_element()
    }

    fn context_head(
                theme: &Theme,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(
                button::icon_button(theme, "collapse-context", crate::ui::icon::Icon::ChevronRight)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Context, cx));
                    })),
            )
            .child(
                text::faint(theme)
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("OUTLINE"),
            )
            .child(text::faint(theme).child(count.to_string()))
    }

    /// Scrolls the row for the declaration being read into view.
    fn reveal_current(&mut self, rows: &[OutlineRow], current: Option<SymbolKey>) {
        let Some(current) = current else {
            return;
        };
        if self.revealed == Some(current) {
            return;
        }
        let Some(at) = rows
            .iter()
            .position(|row| !row.file && row.symbol == Some(current))
        else {
            return;
        };
        self.outline_scroll.scroll_to_item(at, ScrollStrategy::Center);
        self.revealed = Some(current);
    }

    fn outline_list(
        &mut self,
        theme: &Theme,
        rows: Vec<OutlineRow>,
        current: Option<SymbolKey>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme.clone();
        let entity = cx.entity();
        let count = rows.len();
        div().flex_1().min_h(px(0.0)).child(
            uniform_list("outline-rows", count, move |range, _window, _cx| {
                let theme = theme.clone();
                let entity = entity.clone();
                rows.get(range.clone())
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(offset, row)| {
                        let at = range.start.saturating_add(offset);
                        outline_row(&theme, &entity, at, row, current == row.symbol && !row.file)
                    })
                    .collect()
            })
            .track_scroll(&self.outline_scroll)
            .h_full(),
        )
    }

    fn jump_list(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = self.page_sections(cx);
        div()
            .flex_none()
            .border_t(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                text::faint(theme)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("ON THIS PAGE"),
            )
            .when(sections.is_empty(), |list| {
                list.child(text::faint(theme).child("Nothing open."))
            })
            .children(sections.into_iter().map(|(label, index)| {
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "jump-{index}"
                    ))))
                    .py(px(1.0))
                    .px(space(Space::Tight))
                    .rounded(radius(Radius::Hair))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.paint(Paint::Hover)))
                    .child(text::single_line(text::dim(theme)).child(label))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(index, cx)))
            }))
    }

    /// Scrolls the reader to one region of the page it is showing.
    fn jump_to(&mut self, index: usize, cx: &mut Context<Self>) {
        let handle = self
            .document
            .read(cx)
            .tab()
            .map(|tab| tab.scroll().clone());
        if let Some(handle) = handle {
            handle.scroll_to_item(index);
            cx.notify();
        }
    }

    /// Returns each listable region of the open page, with its child index.
    ///
    /// The indices come from [`super::page::regions`], which is the same list
    /// the reader renders, so a jump can never land on the wrong section.
    fn page_sections(&self, cx: &Context<Self>) -> Vec<(String, usize)> {
        let Some(Content::Page(page)) = self.document.read(cx).tab().map(super::super::store::document::Tab::content)
        else {
            return Vec::new();
        };
        let offset = usize::from(
            self.document
                .read(cx)
                .tab()
                .is_some_and(|tab| tab.pending().is_some()),
        );
        super::page::regions(page)
            .into_iter()
            .enumerate()
            .filter_map(|(at, region)| {
                super::page::region_label(page, &region)
                    .map(|label| (label, at.saturating_add(offset)))
            })
            .collect()
    }

    fn current_symbol(&self, cx: &Context<Self>) -> Option<SymbolKey> {
        match self.document.read(cx).tab()?.subject()? {
            crate::store::document::Subject::Declaration { symbol, .. } => Some(*symbol),
            crate::store::document::Subject::Project { .. } => None,
        }
    }

    fn outline_rows(&self, cx: &Context<Self>) -> Vec<OutlineRow> {
        let Some(identity) = self.active_identity(cx) else {
            return self.all_rows(cx);
        };
        let Some(project) = identity.project().map(|project| project.root().to_owned()) else {
            return self.all_rows(cx);
        };
        let mut rows = Vec::new();
        let mut file = String::new();
        for row in self.engine.read(cx).declarations_in(&project) {
            let RowId::Symbol(symbol) = row.id else {
                continue;
            };
            let identity = Identity::parse_with_key(&row.label, IdentityKey::Symbol(symbol));
            let path = identity
                .path()
                .map_or_else(String::new, |path| path.as_str().to_owned());
            if path != file {
                file.clone_from(&path);
                rows.push(OutlineRow {
                    symbol: None,
                    label: path.clone(),
                    kind: None,
                    file: true,
                });
            }
            rows.push(OutlineRow {
                symbol: Some(symbol),
                label: identity.name().to_owned(),
                kind: row.kind,
                file: false,
            });
        }
        rows
    }

    fn all_rows(&self, cx: &Context<Self>) -> Vec<OutlineRow> {
        self.engine
            .read(cx)
            .root()
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Symbol(symbol) => Some(OutlineRow {
                    symbol: Some(symbol),
                    label: Identity::parse(&row.label).name().to_owned(),
                    kind: row.kind,
                    file: false,
                }),
                RowId::Package(_) | RowId::Object(_) => None,
            })
            .collect()
    }
}

fn outline_row(
    theme: &Theme,
    entity: &gpui::Entity<Workspace>,
    at: usize,
    row: &OutlineRow,
    current: bool,
) -> AnyElement {
    if row.file {
        return file_header(theme, at, &row.label);
    }
    let Some(symbol) = row.symbol else {
        return div().h(px(Chrome::OUTLINE_ROW)).into_any_element();
    };
    let entity = entity.clone();
    div()
        .id(ElementId::Name(SharedString::from(format!("outline-{at}"))))
        .h(px(Chrome::OUTLINE_ROW))
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .pl(space(Space::Loose))
        .pr(space(Space::Snug))
        .rounded(radius(Radius::Hair))
        .when(current, |row| row.bg(theme.paint(Paint::Selected)))
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
        .cursor_pointer()
        .on_click(move |_, _window: &mut Window, cx| {
            entity.update(cx, |workspace, cx| {
                workspace.open_symbol(symbol, Target::Here, cx);
            });
        })
        .child(glyph::kind_tile(theme, row.kind, false))
        .child(
            text::single_line(if current {
                text::label(theme).font_weight(FontWeight::MEDIUM)
            } else {
                text::dim(theme)
            })
            .flex_1()
            .min_w(px(0.0))
            .child(row.label.clone()),
        )
        .into_any_element()
}

fn file_header(theme: &Theme, at: usize, path: &str) -> AnyElement {
    div()
        .id(ElementId::Name(SharedString::from(format!(
            "outline-file-{at}"
        ))))
        .h(px(Chrome::OUTLINE_ROW))
        .flex()
        .items_center()
        .px(space(Space::Base))
        .child(
            text::single_line(text::faint(theme))
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Micro))
                .child(text::elide(path, 34).to_string()),
        )
        .into_any_element()
}
