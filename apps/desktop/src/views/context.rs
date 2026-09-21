//! The context panel: where the open declaration sits, and what sits around it.
//! It appears only when a page is open, because with nothing open it has
//! nothing true to say, and a panel that fills itself with a whole project
//! to avoid being empty is answering a question nobody asked.
//!
//! For a declaration the panel answers three questions in order: *where* is
//! this (project, module, containing declarations), *what does it contain*,
//! and *what sits beside it*. Modules are the spine — a file is a module, and
//! it is named as one, `glyph` rather than `glyph.rs` — and the path itself
//! lives on the page header and in the source sheet, where a path belongs.
//! For a project the panel lists its modules, each with what it holds.
//!
//! There is no "on this page" list. The page's own section heads are the
//! table of contents, a screen away at most, and a second copy of them in a
//! side panel was a list nobody used.
//!
//! Every list here is a `uniform_list`, so ten thousand siblings cost one
//! screen of work, and the row for the declaration being read is scrolled
//! into view whenever it changes.

use super::workspace::Workspace;
use crate::store::document::{Subject, Target};
use crate::store::index::{Entry, ProjectIndex, kind_rank};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{
    Chrome, PanelWidth, Radius, Space, TypeScale, hairline, radius, space, type_size,
};
use crate::ui::tip::{Card, Tipped as _};
use crate::ui::{button, glyph, surface, text};
use backend_library::{DeclarationKind, SymbolKey};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
    uniform_list,
};
use std::sync::Arc;

/// What one row of the panel is.
#[derive(Clone, Debug)]
enum Row {
    /// A section head with its count.
    Section {
        title: &'static str,
        count: usize,
    },
    /// The project the declaration lives in.
    Project {
        name: String,
        coordinate: String,
    },
    /// One declaration: a module, an ancestor, a child, a sibling, or the current one.
    Declaration {
        symbol: SymbolKey,
        name: String,
        kind: Option<DeclarationKind>,
        signature: Option<String>,
        held: usize,
        depth: usize,
        current: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContextKey {
    Empty,
    Declaration(SymbolKey),
    Project(String),
}

/// The context projection is built on state changes, never while drawing.
///
/// Keeping the immutable row slice behind an `Arc` lets the uniform list own
/// a stable snapshot for its callback without cloning or re-sorting thousands
/// of symbols on every frame.
pub(super) struct ContextCache {
    key: ContextKey,
    generation: u64,
    index_version: [u8; 32],
    rows: Arc<[Row]>,
}

impl Workspace {
    /// Returns the right panel at its current animated width.
    pub(super) fn context_panel(
        &mut self,
        theme: &Theme,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if width < 1.0 {
            return div().into_any_element();
        }
        if width < PanelWidth::MIN_CONTEXT {
            return Self::context_rail(theme, width, cx);
        }
        let rows = self.context_rows(cx);
        self.reveal_current(&rows);
        surface::panel(theme)
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_l(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .child(self.context_list(theme, rows, cx))
            .into_any_element()
    }

    fn context_rail(theme: &Theme, width: f32, cx: &mut Context<Self>) -> AnyElement {
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
                        this.shell.update(cx, |shell, cx| {
                            shell.toggle_panel(crate::store::shell::Side::Context, cx);
                        });
                    })),
            )
            .into_any_element()
    }

    /// Scrolls the row for the declaration being read into view.
    fn reveal_current(&mut self, rows: &[Row]) {
        let current = rows.iter().position(|row| {
            matches!(
                row,
                Row::Declaration {
                    current: true,
                    ..
                }
            )
        });
        let Some(at) = current else {
            return;
        };
        let symbol = match rows.get(at) {
            Some(Row::Declaration { symbol, .. }) => Some(*symbol),
            _ => None,
        };
        if self.revealed == symbol {
            return;
        }
        self.outline_scroll.scroll_to_item(at, ScrollStrategy::Center);
        self.revealed = symbol;
    }

    fn context_list(
        &mut self,
        theme: &Theme,
        rows: Arc<[Row]>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = theme.clone();
        let entity = cx.entity();
        let count = rows.len();
        div().flex_1().min_h(px(0.0)).child(
            uniform_list("context-rows", count, move |range, _window, _cx| {
                let theme = theme.clone();
                let entity = entity.clone();
                rows.get(range.clone())
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(offset, row)| {
                        let at = range.start.saturating_add(offset);
                        draw_row(&theme, &entity, at, row)
                    })
                    .collect()
            })
            .track_scroll(&self.outline_scroll)
            .h_full(),
        )
    }

    /// Returns whether the open page gives this panel anything to say.
    pub(super) fn context_available(&self, cx: &Context<Self>) -> bool {
        matches!(
            self.document.read(cx).tab().and_then(super::super::store::document::Tab::subject),
            Some(Subject::Declaration { .. } | Subject::Project { .. })
        )
    }

    fn context_rows(&mut self, cx: &Context<Self>) -> Arc<[Row]> {
        let subject = self
            .document
            .read(cx)
            .tab()
            .and_then(|tab| tab.subject().cloned());
        let generation = self.document.read(cx).generation();
        let key = match &subject {
            Some(Subject::Declaration { symbol, .. }) => ContextKey::Declaration(*symbol),
            Some(Subject::Project { coordinate }) => ContextKey::Project(coordinate.clone()),
            Some(Subject::Home | Subject::Package { .. }) | None => ContextKey::Empty,
        };
        let index_version = self.index.read(cx).version();
        if let Some(cache) = self.context_cache.as_ref()
            && cache.key == key
            && cache.generation == generation
            && cache.index_version == index_version
        {
            return Arc::clone(&cache.rows);
        }

        let rows = {
            let index = self.index.read(cx);
            match subject {
                Some(Subject::Declaration { symbol, .. }) => index
                    .project_of(symbol)
                    .map(|project| declaration_rows(project, symbol))
                    .unwrap_or_default(),
                Some(Subject::Project { coordinate }) => index
                    .project(&coordinate)
                    .map(project_rows)
                    .unwrap_or_default(),
                Some(Subject::Home | Subject::Package { .. }) | None => Vec::new(),
            }
        };
        let rows = Arc::<[Row]>::from(rows);
        self.context_cache = Some(ContextCache {
            key,
            generation,
            index_version,
            rows: Arc::clone(&rows),
        });
        rows
    }
}

/// Rows for one open declaration: where, contains, around.
fn declaration_rows(project: &ProjectIndex, symbol: SymbolKey) -> Vec<Row> {
    let Some(entry) = project.entry(symbol) else {
        return Vec::new();
    };
    let ancestors = project.ancestors(symbol);
    let mut rows = Vec::with_capacity(ancestors.len().saturating_add(8));
    rows.push(Row::Section {
        title: "WHERE",
        count: 0,
    });
    rows.push(Row::Project {
        name: project_name(project.root()),
        coordinate: project.root().to_owned(),
    });
    for (depth, ancestor) in ancestors.iter().enumerate() {
        rows.push(declaration_row(project, ancestor, depth.saturating_add(1), false));
    }
    rows.push(declaration_row(project, entry, ancestors.len().saturating_add(1), true));
    push_group(project, &mut rows, "CONTAINS", &sorted(project.children_of(symbol).collect()));
    push_group(project, &mut rows, "AROUND", &sorted(project.around(symbol)));
    rows
}

/// Rows for one open project: its modules, each with what it holds.
fn project_rows(project: &ProjectIndex) -> Vec<Row> {
    let modules: Vec<&Entry> = project.modules().collect();
    let mut rows = Vec::with_capacity(modules.len().saturating_add(1));
    rows.push(Row::Section {
        title: "MODULES",
        count: modules.len(),
    });
    rows.extend(
        modules
            .iter()
            .map(|module| declaration_row(project, module, 0, false)),
    );
    rows
}

fn push_group(project: &ProjectIndex, rows: &mut Vec<Row>, title: &'static str, entries: &[&Entry]) {
    if entries.is_empty() {
        return;
    }
    rows.push(Row::Section {
        title,
        count: entries.len(),
    });
    rows.extend(
        entries
            .iter()
            .map(|entry| declaration_row(project, entry, 0, false)),
    );
}

fn sorted(mut entries: Vec<&Entry>) -> Vec<&Entry> {
    entries.sort_by(|left, right| {
        kind_rank(left.kind())
            .cmp(&kind_rank(right.kind()))
            .then_with(|| left.name().cmp(right.name()))
    });
    entries
}

fn declaration_row(project: &ProjectIndex, entry: &Entry, depth: usize, current: bool) -> Row {
    Row::Declaration {
        symbol: entry.symbol(),
        name: entry.name().to_owned(),
        kind: entry.kind(),
        signature: entry.signature().map(ToOwned::to_owned),
        held: project.children_of(entry.symbol()).count(),
        depth,
        current,
    }
}

fn project_name(root: &str) -> String {
    backend_present::Identity::parse(root).name().to_owned()
}

fn draw_row(theme: &Theme, entity: &gpui::Entity<Workspace>, at: usize, row: &Row) -> AnyElement {
    match row {
        Row::Section { title, count } => div()
            .h(px(Chrome::OUTLINE_ROW))
            .px(space(Space::Snug))
            .pt(space(Space::Tight))
            .child(section_head(theme, title, (*count > 0).then_some(*count)))
            .into_any_element(),
        Row::Project { name, coordinate } => {
            let opened = coordinate.clone();
            let entity = entity.clone();
            base_row(theme, at, 0, false)
                .on_click(move |_, _window: &mut Window, cx| {
                    entity.update(cx, |workspace, cx| workspace.open_project(opened.clone(), cx));
                })
                .card(Card::new(name.clone(), None).site(coordinate.clone()))
                .child(glyph::package_tile(theme, false))
                .child(name_text(theme, name, false))
                .into_any_element()
        }
        Row::Declaration {
            symbol,
            name,
            kind,
            signature,
            held,
            depth,
            current,
        } => {
            let symbol = *symbol;
            let entity = entity.clone();
            let card = Card::new(name.clone(), *kind)
                .signature(signature.clone().unwrap_or_default());
            base_row(theme, at, *depth, *current)
                .when(!current, |row| {
                    row.on_click(move |event: &gpui::ClickEvent, _window: &mut Window, cx| {
                        let target = crate::store::document::modifier_target(event.modifiers());
                        entity.update(cx, |workspace, cx| workspace.open_symbol(symbol, target, cx));
                    })
                    .card(card)
                })
                .child(glyph::kind_mark(theme, *kind, 12.0))
                .child(name_text(theme, name, *current))
                .when(*held > 0 && !current, |row| {
                    row.child(text::faint(theme).flex_none().child(held.to_string()))
                })
                .into_any_element()
        }
    }
}

fn base_row(theme: &Theme, at: usize, depth: usize, current: bool) -> gpui::Stateful<gpui::Div> {
    let indent = px(12.0 * u8::try_from(depth.min(8)).map_or(8.0, f32::from));
    div()
        .id(ElementId::Name(SharedString::from(format!("context-{at}"))))
        .h(px(Chrome::OUTLINE_ROW))
        .mx(space(Space::Tight))
        .pl(space(Space::Snug))
        .pr(space(Space::Snug))
        .ml(indent)
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .rounded(radius(Radius::Small))
        .when(current, |row| row.bg(theme.paint(Paint::Selected)))
        .when(!current, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme.paint(Paint::Hover)))
        })
}

fn name_text(theme: &Theme, name: &str, current: bool) -> gpui::Div {
    text::single_line(if current {
        text::label(theme)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.paint(Paint::TextStrong))
    } else {
        text::dim(theme).text_size(type_size(TypeScale::Interface))
    })
    .flex_1()
    .min_w(px(0.0))
    .child(name.to_owned())
}

fn section_head(theme: &Theme, title: &str, count: Option<usize>) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            text::faint(theme)
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .when_some(count, |head, count| {
            head.child(text::faint(theme).flex_none().child(count.to_string()))
        })
        .child(
            div()
                .flex_1()
                .h(hairline())
                .bg(theme.paint(Paint::Hairline)),
        )
}

/// Returns where a click with these modifiers should open a link.
pub(super) fn click_target(event: &gpui::ClickEvent) -> Target {
    crate::store::document::modifier_target(event.modifiers())
}
