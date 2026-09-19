//! One project page: what is on the shelf, and what can be done about it.
//! Counts and language mix come from the engine; nothing is scraped from disk.
//! Registry facts appear only when a surface command actually returns them.
//!
//! Decision, recorded here: this page deliberately does not parse `Cargo.toml`
//! or any other manifest. Reading one manifest format would make a page that
//! is rich for Rust and empty for the other six languages, and a page that is
//! empty for six of seven languages is a page that lies about what the product
//! is. Everything shown here is a fact the compiler produced.
//!
//! The Versions, Dependents, and Registry sections follow the same rule one
//! step further. They are drawn from real `package-versions`, `dependents`,
//! and `package` replies through [`backend_present::product_view`], which
//! carries the engine's own `Unconfigured` and `NotRecorded` states — so a
//! section that has nothing to say says which kind of nothing it is, and a
//! local folder is told it is not in any registry rather than being shown
//! three empty boxes.

use super::workspace::Workspace;
use crate::presentation::project;
use crate::theme::Theme;
use crate::theme::language::{label as language_label, of_path};
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, fault as fault_ui, glyph, text};
use backend_present::{Identity, IdentityKey, ProductView, ShelfEntry};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use std::collections::BTreeMap;

/// How many declarations one file group lists before it stops.
const FILE_BUDGET: usize = 60;

/// One row of a project's dense outline.
type FileRow = (
    backend_library::SymbolKey,
    Identity,
    Option<backend_library::DeclarationKind>,
);

impl Workspace {
    /// Returns the project page for one coordinate.
    pub(super) fn project_page(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let merged = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        let Some(entry) = project::find(&merged, coordinate).cloned() else {
            return Self::missing_project(theme, coordinate, cx).into_any_element();
        };
        let owned = coordinate.to_owned();
        self.catalog
            .update(cx, |catalog, cx| catalog.read_facts(&owned, cx));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(Self::project_header(theme, &entry, cx))
            .when_some(entry.readiness().fault().cloned(), |body, fault| {
                let actions = Self::affordances(theme, "project", &fault, coordinate, cx);
                body.child(fault_ui::block(theme, &fault, actions))
            })
            .child(project_facts(theme, &entry))
            .child(self.registry_facts(theme, coordinate, &entry, cx))
            .child(self.project_files(theme, &entry, cx))
            .into_any_element()
    }

    fn project_header(
                theme: &Theme,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let local = project::is_local(entry.identity());
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                text::single_line(text::identity_text(theme, TypeScale::Small))
                    .child(coordinate.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(glyph::package_tile(theme, true))
                    .child(
                        text::heading(theme, TypeScale::Title)
                            .child(entry.identity().name().to_owned()),
                    )
                    .child(chip::badge(theme, &project::badge(entry.identity())))
                    .child(standing_chip(theme, entry))
                    .child(div().flex_1())
                    .child(Self::project_actions(theme, &coordinate, local, cx)),
            )
    }

    fn project_actions(
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

    /// Returns the registry sections, or the one sentence that says there are none.
    fn registry_facts(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if project::is_local(entry.identity()) {
            return head_and_body(
                theme,
                "Registry",
                text::dim(theme)
                    .child(format!(
                        "This project is a folder on {}. No registry records it.",
                        project::this_machine()
                    ))
                    .into_any_element(),
            )
            .into_any_element();
        }
        let Some(facts) = self.catalog.read(cx).facts_for(coordinate) else {
            return head_and_body(
                theme,
                "Registry",
                text::faint(theme)
                    .child("Reading the local registry…")
                    .into_any_element(),
            )
            .into_any_element();
        };
        if facts.sections().is_empty() {
            return head_and_body(
                theme,
                "Registry",
                text::dim(theme)
                    .child("The local registry answered no section for this package.")
                    .into_any_element(),
            )
            .into_any_element();
        }
        let sections: Vec<AnyElement> = facts
            .sections()
            .iter()
            .map(|(title, view)| product_section(theme, title, view))
            .collect();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Loose))
            .children(sections)
            .into_any_element()
    }

    fn project_files(
        &mut self,
        theme: &Theme,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let root = entry
            .identity()
            .project()
            .map(|project| project.root().to_owned())
            .unwrap_or_default();
        let groups = self.group_by_file(&root, cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Base))
            .child(section_head(theme, "Outline"))
            .when(groups.is_empty(), |body| {
                body.child(
                    text::dim(theme)
                        .child("No declarations are published for this project yet."),
                )
            })
            .children(
                groups
                    .into_iter()
                    .map(|(file, rows)| Self::file_group(theme, &file, &rows, cx)),
            )
    }

    fn group_by_file(&self, project: &str, cx: &Context<Self>) -> BTreeMap<String, Vec<FileRow>> {
        let mut groups: BTreeMap<String, Vec<FileRow>> = BTreeMap::new();
        for row in self.engine.read(cx).declarations_in(project) {
            let backend_library::RowId::Symbol(symbol) = row.id else {
                continue;
            };
            let identity = Identity::parse_with_key(&row.label, IdentityKey::Symbol(symbol));
            let file = identity
                .path()
                .map_or_else(|| "(no file)".to_owned(), |path| path.as_str().to_owned());
            groups
                .entry(file)
                .or_default()
                .push((symbol, identity, row.kind));
        }
        groups
    }

    fn file_group(
                theme: &Theme,
        file: &str,
        rows: &[FileRow],
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
                    .child(glyph::language_tag(theme, of_path(file)))
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
                    .map(|(at, row)| Self::file_row(theme, file, at, row, cx)),
            )
            .when(rows.len() > FILE_BUDGET, |group| {
                group.child(
                    text::faint(theme)
                        .pl(space(Space::Loose))
                        .child(format!("{} more in this file", rows.len() - FILE_BUDGET)),
                )
            })
            .into_any_element()
    }

    fn file_row(
                theme: &Theme,
        file: &str,
        at: usize,
        row: &FileRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (symbol, identity, kind) = row;
        let symbol = *symbol;
        let line = identity
            .line()
            .map_or_else(String::new, |line| line.get().to_string());
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
                theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fault = crate::presentation::fault::project_missing(coordinate);
        let actions = Self::affordances(theme, "missing-project", &fault, coordinate, cx);
        fault_ui::block(theme, &fault, actions)
    }
}

fn project_facts(theme: &Theme, entry: &ShelfEntry) -> Div {
    let declarations = entry.declarations().get();
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
                .child(chip::count_chip(theme, declarations, "declarations"))
                .child(chip::count_chip(
                    theme,
                    entry.languages().len() as u64,
                    "languages",
                )),
        )
        .child(glyph::language_bar(theme, entry.languages(), declarations))
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
                        .child(text::dim(theme).child(format!(
                            "{} · {}",
                            language_label(count.language()),
                            count.declarations().get()
                        )))
                })),
        )
}

/// Returns one registry section exactly as the engine answered it.
fn product_section(theme: &Theme, title: &str, view: &ProductView) -> AnyElement {
    let body = if let Some(fault) = view.fault() {
        fault_ui::block(theme, fault, Vec::new()).into_any_element()
    } else if view.records().is_empty() {
        text::dim(theme)
            .child(
                view.note()
                    .unwrap_or("The engine recorded nothing for this section.")
                    .to_owned(),
            )
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(view.records().iter().map(|record| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(
                        text::single_line(text::label(theme))
                            .flex_none()
                            .child(record.title().to_owned()),
                    )
                    .when_some(record.operand().map(ToOwned::to_owned), |row, operand| {
                        row.child(
                            text::single_line(text::faint(theme))
                                .flex_1()
                                .min_w(px(0.0))
                                .font_family(theme.specimen())
                                .child(operand),
                        )
                    })
                    .children(
                        record
                            .tags()
                            .iter()
                            .map(|tag| chip::badge(theme, tag).into_any_element()),
                    )
            }))
            .into_any_element()
    };
    head_and_body(theme, title, body).into_any_element()
}

fn head_and_body(theme: &Theme, title: &str, body: AnyElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(section_head(theme, title))
        .child(body)
}

fn section_head(theme: &Theme, title: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            text::faint(theme)
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_ascii_uppercase()),
        )
        .child(
            div()
                .flex_1()
                .h(hairline())
                .bg(theme.paint(Paint::Hairline)),
        )
}

fn standing_chip(theme: &Theme, entry: &ShelfEntry) -> Div {
    let standing = project::standing(entry);
    let role = glyph::standing_paint(standing);
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
        .child(glyph::standing_glyph(standing))
        .child(entry.readiness().name())
}
