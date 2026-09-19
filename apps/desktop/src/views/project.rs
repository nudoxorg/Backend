//! One project page: what it holds, organised by what things are.
//! Counts and language mix come from the engine; nothing is scraped from disk.
//! Registry facts appear only when a surface command actually returns them.
//!
//! The spine of the page is the *structure* — the top-level declarations
//! grouped by kind, modules before types before functions — because that is
//! how a reader asks what a library offers. Files are one section near the
//! end: supplemental, true, and not what anyone came for.
//!
//! Decision, recorded here: this page deliberately does not parse `Cargo.toml`
//! or any other manifest. Reading one manifest format would make a page that
//! is rich for Rust and empty for the other six languages, and a page that is
//! empty for six of seven languages is a page that lies about what the product
//! is. Everything shown here is a fact the compiler produced.

use super::workspace::Workspace;
use crate::presentation::project::{self, Standing};
use crate::store::index::{Entry, ProjectIndex, kind_rank};
use crate::theme::Theme;
use crate::theme::kind::group_title;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::bar::{self, Motion};
use crate::ui::icon::{self, Icon};
use crate::ui::tip::{Tip, Tipped as _};
use crate::ui::{button, chip, fault as fault_ui, glyph, specimen, text};
use backend_library::DeclarationKind;
use backend_present::{ProductView, ShelfEntry, Signature};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use std::collections::BTreeMap;

/// How many declarations one kind group lists before it offers the rest.
const GROUP_BUDGET: usize = 30;

/// How many files are listed before the section offers the rest.
const FILE_BUDGET: usize = 40;

/// Character budget for a row's signature preview.
const PREVIEW: usize = 90;

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
        let structure = self.structure_section(theme, coordinate, cx);
        let files = self.files_section(theme, coordinate, cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.project_header(theme, &entry, cx))
            .when_some(entry.readiness().fault().cloned(), |body, fault| {
                let actions = Self::affordances(theme, "project", &fault, coordinate, cx);
                body.child(fault_ui::block(theme, &fault, actions))
            })
            .child(structure)
            .child(self.registry_facts(theme, coordinate, &entry, cx))
            .child(files)
            .into_any_element()
    }

    fn project_header(&self, theme: &Theme, entry: &ShelfEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let local = project::is_local(entry.identity());
        let standing = project::standing(entry);
        let pinned = self.shell.read(cx).marks().is_pinned(&coordinate);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
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
                    .when(standing != Standing::Readable, |head| {
                        head.child(standing_chip(theme, entry))
                    })
                    .child(div().flex_1())
                    .child(Self::project_actions(theme, &coordinate, local, pinned, cx)),
            )
            .child(bar::language_bar(
                theme,
                "project-bar",
                entry.languages(),
                entry.declarations().get(),
                Motion::of(standing),
            ))
            .child(Self::coordinate_row(theme, entry, cx))
    }

    fn coordinate_row(theme: &Theme, entry: &ShelfEntry, cx: &mut Context<Self>) -> Div {
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let copy = coordinate.clone();
        div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(
                        div()
                            .id("project-coordinate")
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_center()
                            .gap(space(Space::Tight))
                            .cursor_pointer()
                            .rounded(radius(Radius::Hair))
                            .px(px(2.0))
                            .hover(|style| style.bg(theme.paint(Paint::GiltWash)))
                            .child(
                                text::single_line(text::identity_text(theme, TypeScale::Small))
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(coordinate.clone()),
                            )
                            .child(icon::sized(theme, Icon::Copy, 11.0, Paint::GiltDim))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy("Coordinate copied", copy.clone(), cx);
                            })),
                    )
                    .child(text::faint(theme).flex_none().child(format!(
                        "{} declarations · {} languages",
                        entry.declarations().get(),
                        entry.languages().len()
                    )))
    }

    fn project_actions(
        theme: &Theme,
        coordinate: &str,
        local: bool,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reindex = coordinate.to_owned();
        let remove = coordinate.to_owned();
        let folder = coordinate.to_owned();
        let pin = coordinate.to_owned();
        div()
            .flex()
            .flex_none()
            .gap(space(Space::Tight))
            .child(
                button::button(theme, "project-pin", if pinned { "Unpin" } else { "Pin" }, button::Weight::Quiet)
                    .tip(Tip::new(if pinned { "Unpin from home" } else { "Pin to home" })
                        .detail("Pinned projects sit at the top of the browse page."))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_pin(&pin, cx);
                    })),
            )
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

    /// Returns the structure: top-level declarations grouped by kind.
    fn structure_section(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let groups = {
            let index = self.index.read(cx);
            index.project(coordinate).map(top_level_groups)
        };
        let Some(groups) = groups.filter(|groups| !groups.is_empty()) else {
            return head_and_body(
                theme,
                "Structure",
                text::dim(theme)
                    .child("No declarations are published for this project yet.")
                    .into_any_element(),
            )
            .into_any_element();
        };
        let sections: Vec<AnyElement> = groups
            .into_iter()
            .map(|(kind, rows)| self.kind_group(theme, kind, rows, cx))
            .collect();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Base))
            .child(section_head(theme, "Structure"))
            .children(sections)
            .into_any_element()
    }

    fn kind_group(
        &mut self,
        theme: &Theme,
        kind: Option<DeclarationKind>,
        rows: Vec<GroupRow>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("structure-{}", kind.map_or(255, DeclarationKind::wire_tag));
        let folded = self.is_folded(&key);
        let shown = if self.is_unfurled(&key) {
            rows.len()
        } else {
            rows.len().min(GROUP_BUDGET)
        };
        let title = kind.map_or_else(|| "Declarations".to_owned(), group_title);
        let hidden = rows.len().saturating_sub(shown);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(Self::kind_head(theme, &key, kind, &title, rows.len(), folded, cx))
            .when(!folded, |group| {
                group
                    .children(
                        rows.into_iter()
                            .take(shown)
                            .enumerate()
                            .map(|(at, row)| Self::structure_row(theme, &key, at, row, cx)),
                    )
                    .when(hidden > 0, |group| {
                        let more = key.clone();
                        group.child(
                            button::button(
                                theme,
                                format!("more-{key}"),
                                &format!("Show {hidden} more"),
                                button::Weight::Quiet,
                            )
                            .ml(space(Space::Loose))
                            .on_click(cx.listener(move |this, _, _, cx| this.unfurl(&more, cx))),
                        )
                    })
            })
            .into_any_element()
    }

    fn kind_head(
        theme: &Theme,
        key: &str,
        kind: Option<DeclarationKind>,
        title: &str,
        count: usize,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owned = key.to_owned();
        div()
            .id(ElementId::Name(SharedString::from(format!("fold-{key}"))))
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .py(px(2.0))
            .cursor_pointer()
            .child(icon::sized(
                theme,
                if folded { Icon::ChevronRight } else { Icon::ChevronDown },
                11.0,
                Paint::TextFaint,
            ))
            .child(glyph::kind_tile(theme, kind, false))
            .child(
                text::label(theme)
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_owned()),
            )
            .child(text::faint(theme).child(count.to_string()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&owned, cx)))
    }

    fn structure_row(
        theme: &Theme,
        group: &str,
        at: usize,
        row: GroupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let symbol = row.symbol;
        let preview = row
            .signature
            .as_ref()
            .map(|signature| specimen::signature_line(theme, signature, PREVIEW));
        div()
            .id(ElementId::Name(SharedString::from(format!("{group}-{at}"))))
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .ml(space(Space::Loose))
            .px(space(Space::Snug))
            .py(px(3.0))
            .rounded(radius(Radius::Small))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.open_symbol(symbol, super::context::click_target(event), cx);
            }))
            .on_hover(cx.listener(move |this, entering: &bool, window, cx| {
                this.hover_member(symbol, *entering, window, cx);
            }))
            .child(
                text::single_line(text::navigable(theme, TypeScale::Interface, false))
                    .flex_none()
                    .max_w(px(260.0))
                    .child(row.name),
            )
            .when_some(preview, |line, preview| {
                line.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .font_family(theme.specimen())
                        .text_size(type_size(TypeScale::Small))
                        .text_color(theme.paint(Paint::TextDim))
                        .child(preview),
                )
            })
            .when(row.children > 0, |line| {
                line.child(text::faint(theme).flex_none().child(format!("{} inside", row.children)))
            })
            .into_any_element()
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
                    .child("This project is a local folder. No registry records it.")
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

    /// Returns the file list, folded by default.
    fn files_section(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let files: Vec<(String, usize)> = self
            .index
            .read(cx)
            .project(coordinate)
            .map(|project| {
                project
                    .files()
                    .iter()
                    .map(|group| (group.path().to_owned(), group.len()))
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            return div().into_any_element();
        }
        let open = self.is_unfurled("files");
        let shown = if self.is_unfurled("files-all") {
            files.len()
        } else {
            files.len().min(FILE_BUDGET)
        };
        let project = backend_present::Identity::parse(coordinate).name().to_owned();
        let hidden = files.len().saturating_sub(shown);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(Self::files_head(theme, open, files.len(), cx))
            .when(open, |section| {
                section
                    .children(
                        files
                            .into_iter()
                            .take(shown)
                            .enumerate()
                            .map(|(at, (path, count))| Self::file_row(theme, &project, at, &path, count, cx)),
                    )
                    .when(hidden > 0, |section| {
                        section.child(
                            button::button(theme, "more-files", &format!("Show {hidden} more"), button::Weight::Quiet)
                                .ml(space(Space::Loose))
                                .on_click(cx.listener(|this, _, _, cx| this.unfurl("files-all", cx))),
                        )
                    })
            })
            .into_any_element()
    }

    fn files_head(theme: &Theme, open: bool, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("fold-files")
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .cursor_pointer()
            .child(icon::sized(
                theme,
                if open { Icon::ChevronDown } else { Icon::ChevronRight },
                11.0,
                Paint::TextFaint,
            ))
            .child(
                text::faint(theme)
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("FILES"),
            )
            .child(text::faint(theme).child(count.to_string()))
            .child(div().flex_1().h(hairline()).bg(theme.paint(Paint::Hairline)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_unfurl("files", cx)))
    }

    fn file_row(
        theme: &Theme,
        project: &str,
        at: usize,
        path: &str,
        count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scoped = format!("@{project} {path}");
        div()
            .id(ElementId::Name(SharedString::from(format!("file-{at}"))))
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .ml(space(Space::Loose))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.set_field(scoped.clone(), cx);
                this.focus_field(window, cx);
            }))
            .child(glyph::language_tag(theme, crate::theme::language::of_path(path)))
            .child(
                text::single_line(text::dim(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme.specimen())
                    .child(path.to_owned()),
            )
            .child(text::faint(theme).flex_none().child(count.to_string()))
            .into_any_element()
    }

    fn missing_project(theme: &Theme, coordinate: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let fault = crate::presentation::fault::project_missing(coordinate);
        let actions = Self::affordances(theme, "missing-project", &fault, coordinate, cx);
        fault_ui::block(theme, &fault, actions)
    }
}

/// One row of a kind group, owned so the render closure borrows nothing.
struct GroupRow {
    symbol: backend_library::SymbolKey,
    name: String,
    signature: Option<Signature>,
    children: usize,
}

/// One kind group: the kind and its rows.
type KindGroup = (Option<DeclarationKind>, Vec<GroupRow>);

/// Groups a project's top-level declarations by kind, in reading order.
fn top_level_groups(project: &ProjectIndex) -> Vec<KindGroup> {
    let mut groups: BTreeMap<u8, KindGroup> = BTreeMap::new();
    for entry in project.top_level() {
        let rank = kind_rank(entry.kind());
        groups
            .entry(rank)
            .or_insert_with(|| (entry.kind(), Vec::new()))
            .1
            .push(group_row(project, entry));
    }
    groups.into_values().collect()
}

fn group_row(project: &ProjectIndex, entry: &Entry) -> GroupRow {
    let language = entry.identity().language();
    GroupRow {
        symbol: entry.symbol(),
        name: entry.name().to_owned(),
        signature: entry
            .signature()
            .map(|text| Signature::tokenize(text, language))
            .filter(|signature| !signature.is_empty()),
        children: project.children_of(entry.symbol()).count(),
    }
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
