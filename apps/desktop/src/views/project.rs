//! One project page: what is on the shelf, and what can be done about it.
//! Counts and language mix come from the engine; nothing is scraped from disk.
//! Registry facts appear only when a surface command actually returns them.
//!
//! Decision, recorded here: this page deliberately does not parse `Cargo.toml`
//! or any other manifest. Reading one manifest format would make a page that
//! is rich for Rust and empty for the other six languages, and a page that is
//! empty for six of seven languages is a page that lies about what the product
//! is. Everything shown here is a fact the compiler produced.

use super::workspace::Workspace;
use crate::presentation::identity::Identity;
use crate::presentation::shelf::{Readiness, ShelfEntry};
use crate::theme::Theme;
use crate::theme::language::Language;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, fault as fault_ui, glyph, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use std::collections::BTreeMap;

/// How many declarations one file group lists before it stops.
const FILE_BUDGET: usize = 60;

impl Workspace {
    /// Returns the project page for one coordinate.
    pub(super) fn project_page(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let shelf = self
            .jobs
            .read(cx)
            .merge(self.workspace.read(cx).shelf().clone());
        let Some(entry) = shelf.find(coordinate).cloned() else {
            return self.missing_project(theme, coordinate, cx).into_any_element();
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.project_header(theme, &entry, cx))
            .when_some(entry.readiness().fault().cloned(), |body, fault| {
                let actions = self.affordances(theme, "project", &fault, coordinate, cx);
                body.child(fault_ui::block(theme, &fault, actions))
            })
            .child(self.project_facts(theme, &entry))
            .child(self.project_files(theme, &entry, cx))
            .into_any_element()
    }

    fn project_header(
        &mut self,
        theme: &Theme,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let coordinate = entry.identity().coordinate().to_owned();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                text::single_line(text::identity_text(theme, TypeScale::Small))
                    .child(entry.identity().coordinate().to_owned()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(glyph::package_tile(theme, true))
                    .child(
                        text::heading(theme, TypeScale::Title)
                            .child(entry.identity().project_name().to_owned()),
                    )
                    .child(chip::badge(theme, &entry.badge()))
                    .child(readiness_chip(theme, entry.readiness()))
                    .child(div().flex_1())
                    .child(self.project_actions(theme, &coordinate, entry.is_local(), cx)),
            )
    }

    fn project_actions(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        local: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reindex = coordinate.to_owned();
        let remove = coordinate.to_owned();
        let folder = coordinate.to_owned();
        div()
            .flex()
            .flex_none()
            .gap(space(Space::Tight))
            .child(
                button::button(theme, "project-reindex", "Re-index", button::Weight::Regular)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.index_project(reindex.clone(), cx);
                    })),
            )
            .when(local, |row| {
                row.child(
                    button::button(theme, "project-folder", "Open folder", button::Weight::Quiet)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.reveal_path(std::path::Path::new(&folder));
                        })),
                )
            })
            .child(
                button::button(theme, "project-remove", "Remove", button::Weight::Quiet).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.remove_project(remove.clone(), cx);
                    }),
                ),
            )
    }

    fn project_facts(&self, theme: &Theme, entry: &ShelfEntry) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Tight))
                    .child(chip::count_chip(theme, entry.declarations(), "declarations"))
                    .child(chip::count_chip(theme, entry.files(), "files"))
                    .child(chip::count_chip(theme, entry.languages().len(), "languages")),
            )
            .child(glyph::language_bar(
                theme,
                entry.languages(),
                entry.declarations(),
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Snug))
                    .children(entry.languages().iter().map(|count| {
                        div()
                            .flex()
                            .items_center()
                            .gap(space(Space::Tight))
                            .child(glyph::language_tag(theme, count.language()))
                            .child(
                                text::dim(theme)
                                    .child(format!(
                                        "{} · {}",
                                        count.language().label(),
                                        count.declarations()
                                    )),
                            )
                    })),
            )
    }

    fn project_files(
        &mut self,
        theme: &Theme,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let groups = self.group_by_file(entry.identity().project(), cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Base))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(
                        text::faint(theme)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("OUTLINE"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(hairline())
                            .bg(theme.paint(Paint::Hairline)),
                    ),
            )
            .children(
                groups
                    .into_iter()
                    .map(|(file, rows)| self.file_group(theme, &file, &rows, cx)),
            )
    }

    fn group_by_file(
        &self,
        project: &str,
        cx: &Context<Self>,
    ) -> BTreeMap<String, Vec<(backend_library::SymbolKey, Identity, Option<backend_library::DeclarationKind>)>>
    {
        let mut groups = BTreeMap::new();
        for row in self.workspace.read(cx).declarations_in(project) {
            let backend_library::RowId::Symbol(symbol) = row.id else {
                continue;
            };
            let identity = Identity::parse(&row.label);
            let file = identity.path().unwrap_or("(no file)").to_owned();
            groups
                .entry(file)
                .or_insert_with(Vec::new)
                .push((symbol, identity, row.kind));
        }
        groups
    }

    fn file_group(
        &mut self,
        theme: &Theme,
        file: &str,
        rows: &[(
            backend_library::SymbolKey,
            Identity,
            Option<backend_library::DeclarationKind>,
        )],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(glyph::language_tag(theme, Language::of_path(file)))
                    .child(
                        text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                            .flex_1()
                            .min_w(px(0.0))
                            .font_family(theme.specimen())
                            .child(file.to_owned()),
                    )
                    .child(text::faint(theme).child(rows.len().to_string())),
            )
            .children(
                rows.iter()
                    .take(FILE_BUDGET)
                    .enumerate()
                    .map(|(at, row)| self.file_row(theme, file, at, row, cx)),
            )
            .into_any_element()
    }

    fn file_row(
        &mut self,
        theme: &Theme,
        file: &str,
        at: usize,
        row: &(
            backend_library::SymbolKey,
            Identity,
            Option<backend_library::DeclarationKind>,
        ),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (symbol, identity, kind) = row;
        let symbol = *symbol;
        let line = identity.line().map_or_else(String::new, |line| line.to_string());
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "file-{file}-{at}"
            ))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .ml(space(Space::Loose))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_symbol(symbol, crate::store::document::Target::Here, cx);
            }))
            .child(glyph::kind_tile(theme, *kind, false))
            .child(
                text::single_line(text::navigable(theme, TypeScale::Small, false))
                    .flex_1()
                    .min_w(px(0.0))
                    .child(identity.name().to_owned()),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(Paint::TextFaint))
                    .child(line),
            )
            .into_any_element()
    }

    fn missing_project(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fault = crate::presentation::fault::Fault::new(
            crate::presentation::fault::Severity::Caution,
            crate::presentation::fault::Operand::Project {
                name: Identity::parse(coordinate).project_name().to_owned(),
                spelling: coordinate.to_owned(),
            },
            "This project is not on the shelf",
            "no row on the current revision answers to this coordinate",
            vec![crate::presentation::fault::Affordance::Reindex {
                coordinate: coordinate.to_owned(),
            }],
        );
        let actions = self.affordances(theme, "missing-project", &fault, coordinate, cx);
        fault_ui::block(theme, &fault, actions)
    }
}

fn readiness_chip(theme: &Theme, readiness: &Readiness) -> Div {
    let role = match readiness {
        Readiness::Ready => Paint::Ok,
        Readiness::Indexing { .. } => Paint::Caution,
        Readiness::Failed(_) => Paint::Fault,
        Readiness::Requested => Paint::Info,
    };
    let mut wash = theme.paint(role);
    wash.a = 0.12;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Hair))
        .bg(wash)
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(role))
        .child(readiness.glyph().to_string())
        .child(readiness.name().to_owned())
}
