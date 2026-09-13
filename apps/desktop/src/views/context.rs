//! The context panel: the current project's outline and a jump list.
//! The declaration being read is highlighted and revealed, never hunted for.
//! Ten thousand rows cost one screen of work, because the list is virtualized.
//!
//! This panel answers "where am I?" and nothing else. It is the only place in
//! the window that shows the whole project at once, so it is a flat, dense,
//! file-ordered list rather than a tree that must be unfolded — a reader
//! scanning for a name should not have to guess which branch it is under.

use super::workspace::Workspace;
use crate::presentation::identity::Identity;
use crate::store::document::{Content, Target};
use crate::store::shell::Side;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, glyph, surface, text};
use backend_library::{DeclarationKind, RowId, SymbolKey};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, uniform_list,
};

/// One row of the outline list.
#[derive(Clone)]
struct OutlineRow {
    symbol: SymbolKey,
    identity: Identity,
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
    ) -> impl IntoElement {
        if width < 1.0 {
            return div();
        }
        let rows = self.outline_rows(cx);
        let current = self.current_symbol(cx);
        surface::panel(theme)
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_l(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .child(self.context_head(theme, rows.len(), cx))
            .child(self.outline_list(theme, rows, current, cx))
            .child(self.jump_list(theme, cx))
    }

    fn context_head(
        &mut self,
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
                button::icon_button(theme, "collapse-context", crate::ui::icon::Icon::ChevronRight).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Context, cx));
                    },
                )),
            )
            .child(
                text::faint(theme)
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("OUTLINE"),
            )
            .child(text::faint(theme).child(count.to_string()))
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
            uniform_list("outline-rows", count, move |range, _window, cx| {
                let theme = theme.clone();
                let entity = entity.clone();
                rows.get(range.clone())
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(offset, row)| {
                        let at = range.start.saturating_add(offset);
                        outline_row(&theme, &entity, at, row, current == Some(row.symbol), cx)
                    })
                    .collect()
            })
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
            .children(sections.into_iter().map(|section| {
                text::single_line(text::dim(theme))
                    .py(px(1.0))
                    .child(section)
            }))
    }

    fn page_sections(&self, cx: &Context<Self>) -> Vec<String> {
        let Some(Content::Page(page)) = self.document.read(cx).tab().map(|tab| tab.content().clone())
        else {
            return Vec::new();
        };
        let mut sections = Vec::new();
        if !page.signature().is_empty() {
            sections.push("Signature".to_owned());
        }
        if !page.prose().is_empty() {
            sections.push("Documentation".to_owned());
        }
        for group in page.members() {
            sections.push(format!(
                "{} · {}",
                crate::theme::kind::group_title(group.kind()),
                group.members().len()
            ));
        }
        for group in page.relations() {
            sections.push(format!("{} · {}", group.label(), group.entries().len()));
        }
        sections.push("Source".to_owned());
        sections
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
        let project = identity.project().to_owned();
        let mut rows = Vec::new();
        let mut file = String::new();
        for row in self.workspace.read(cx).declarations_in(&project) {
            let RowId::Symbol(symbol) = row.id else {
                continue;
            };
            let identity = Identity::parse(&row.label);
            let path = identity.path().unwrap_or_default().to_owned();
            if path != file {
                file.clone_from(&path);
                rows.push(OutlineRow {
                    symbol,
                    identity: Identity::parse(&path),
                    kind: None,
                    file: true,
                });
            }
            rows.push(OutlineRow {
                symbol,
                identity,
                kind: row.kind,
                file: false,
            });
        }
        rows
    }

    fn all_rows(&self, cx: &Context<Self>) -> Vec<OutlineRow> {
        self.workspace
            .read(cx)
            .root()
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Symbol(symbol) => Some(OutlineRow {
                    symbol,
                    identity: Identity::parse(&row.label),
                    kind: row.kind,
                    file: false,
                }),
                _ => None,
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
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let _ = cx;
    if row.file {
        return file_header(theme, at, &row.identity);
    }
    let symbol = row.symbol;
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
            .child(row.identity.name().to_owned()),
        )
        .into_any_element()
}

fn file_header(theme: &Theme, at: usize, identity: &Identity) -> gpui::AnyElement {
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
                .child(identity.coordinate().to_owned()),
        )
        .into_any_element()
}
