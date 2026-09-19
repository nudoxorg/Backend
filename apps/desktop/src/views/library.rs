//! The library panel: every project on the shelf, and the way to add one.
//! A row states its name, its provenance, its readiness, and its language mix.
//! A project that failed to index says so on its own row, with a way out.
//!
//! Adding a project expands in place rather than opening a dialog, because
//! adding is the one thing a new reader must discover without being told. The
//! two ways in — a folder on this machine, or a registry coordinate — are the
//! two facts the engine can act on, and validation happens before anything is
//! submitted so a typo is answered in the field rather than by a failed job.
//!
//! Collapsed, the panel becomes a rail rather than nothing. A shelf that
//! vanishes takes its state with it: a reader who folds the panel to read a
//! wide signature still wants to see that a project is indexing, and a rail of
//! readiness marks costs twenty-six pixels to say so.

use super::workspace::Workspace;
use crate::presentation::project;
use crate::store::catalog::{Catalog, Suggestion};
use crate::store::shell::Side;
use crate::theme::Theme;
use crate::theme::language::label as language_label;
use crate::theme::palette::Paint;
use crate::theme::tokens::{PanelWidth, Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, fault as fault_ui, glyph, surface, text};
use backend_present::{Affordance, Fault, ShelfEntry};
use gpui::Focusable as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_elements::editable_text::text_input;

impl Workspace {
    /// Returns the left panel at its current animated width.
    pub(super) fn library_panel(
        &mut self,
        theme: &Theme,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let merged = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        let entries: Vec<ShelfEntry> = merged.entries().to_vec();
        if width < PanelWidth::MIN_LIBRARY {
            return Self::library_rail(theme, width, &entries, cx);
        }
        surface::panel(theme)
            .id("library-panel")
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_r(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .child(Self::library_head(theme, entries.len(), cx))
            .child(
                div()
                    .id("library-rows")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(
                        entries
                            .iter()
                            .enumerate()
                            .map(|(at, entry)| self.shelf_row(theme, at, entry, cx)),
                    ),
            )
            .child(self.add_region(theme, cx))
            .into_any_element()
    }

    /// Returns the collapsed panel: one readiness mark per project.
    fn library_rail(
                theme: &Theme,
        width: f32,
        entries: &[ShelfEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if width < 1.0 {
            return div().into_any_element();
        }
        surface::panel(theme)
            .id("library-rail")
            .flex_none()
            .w(px(width))
            .h_full()
            .overflow_hidden()
            .border_r(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .items_center()
            .py(space(Space::Snug))
            .gap(space(Space::Snug))
            .child(
                button::icon_button(theme, "expand-library", crate::ui::icon::Icon::ChevronRight)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
                    })),
            )
            .children(entries.iter().enumerate().map(|(at, entry)| {
                let coordinate = entry.identity().coordinate().as_str().to_owned();
                let name = entry.identity().name().to_owned();
                let readiness = project::summary(entry);
                div()
                    .id(ElementId::Name(SharedString::from(format!("rail-{at}"))))
                    .cursor_pointer()
                    .child(glyph::standing_mark(theme, project::standing(entry)))
                    .tooltip_show_delay(std::time::Duration::from_millis(250))
                    .tooltip(move |_window, cx| {
                        chip::mono_tip(format!("{name} · {readiness}"), cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_project(coordinate.clone(), cx);
                    }))
            }))
            .into_any_element()
    }

    fn library_head(
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
                text::faint(theme)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("SHELF"),
            )
            .child(div().flex_1())
            .child(chip::count_chip(theme, count as u64, "projects"))
            .child(
                button::icon_button(theme, "collapse-library", crate::ui::icon::Icon::ChevronLeft)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
                    })),
            )
    }

    fn shelf_row(
        &mut self,
        theme: &Theme,
        at: usize,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let active = self.active_identity(cx).is_some_and(|identity| {
            identity.project().map(backend_present::ProjectRef::root)
                == entry
                    .identity()
                    .project()
                    .map(backend_present::ProjectRef::root)
        });
        div()
            .id(ElementId::Name(SharedString::from(format!("shelf-{at}"))))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .when(active, |row| row.bg(theme.paint(Paint::Selected)))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener({
                let coordinate = coordinate.clone();
                move |this, _, _, cx| this.open_project(coordinate.clone(), cx)
            }))
            .child(shelf_title(theme, entry))
            .child(shelf_detail(theme, entry))
            .when_some(entry.readiness().fault().cloned(), |row, fault| {
                row.child(Self::failed_row(theme, at, &fault, &coordinate, cx))
            })
            .into_any_element()
    }

    fn failed_row(
                theme: &Theme,
        at: usize,
        fault: &Fault,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = Self::affordances(theme, &format!("shelf-{at}"), fault, coordinate, cx);
        div()
            .pt(px(3.0))
            .child(fault_ui::block(theme, fault, actions))
            .into_any_element()
    }

    /// Returns the buttons for one fault, already wired.
    ///
    /// A fault carries exactly one typed affordance — the engine's own answer
    /// to "what next?" — and this window draws it as a button. It adds one of
    /// its own beside it: the operand, copyable, because a failure a reader
    /// cannot quote is a failure they cannot report.
    pub(super) fn affordances(
                theme: &Theme,
        prefix: &str,
        fault: &Fault,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut actions = Vec::with_capacity(2);
        let affordance = fault.affordance().clone();
        if crate::presentation::fault::is_actionable(&affordance) {
            let coordinate = coordinate.to_owned();
            actions.push(
                fault_ui::affordance_button(theme, format!("{prefix}-affordance"), &affordance)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_affordance(&affordance, &coordinate, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let spelling = crate::presentation::fault::operand_spelling(fault.operand());
        if !spelling.is_empty() {
            actions.push(
                button::button(
                    theme,
                    format!("{prefix}-copy-operand"),
                    "Copy operand",
                    button::Weight::Quiet,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy("Operand copied", spelling.clone(), cx);
                }))
                .into_any_element(),
            );
        }
        actions
    }

    fn run_affordance(
        &mut self,
        affordance: &Affordance,
        coordinate: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match affordance {
            Affordance::Retry => {
                self.document.update(cx, super::super::store::document::DocumentStore::reload);
                self.engine
                    .update(cx, super::super::store::workspace::WorkspaceStore::refresh_health);
            }
            Affordance::Reindex { path } => {
                let target = if path.is_empty() { coordinate } else { path };
                self.index_project(target.to_owned(), cx);
            }
            Affordance::OpenFolder { path } => cx.reveal_path(std::path::Path::new(path)),
            Affordance::UseCommand { name, args } => {
                let text = Self::palette_for(name, args);
                self.set_field(text, cx);
                self.focus_field(window, cx);
            }
            Affordance::WaitForReadiness | Affordance::None => {}
        }
    }
}

/// The inline add flow.
impl Workspace {
    fn add_region(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .border_t(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(if self.adding {
                self.add_form(theme, cx).into_any_element()
            } else {
                Self::add_button(theme, cx).into_any_element()
            })
    }

    fn add_button(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-project")
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Base))
            .py(space(Space::Snug))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.begin_add(window, cx);
            }))
            .child(
                text::identity_text(theme, TypeScale::Interface)
                    .flex_none()
                    .child("+"),
            )
            .child(text::label(theme).flex_1().child("Add a project"))
            .child(button::key_hint(theme, "⌘⇧A"))
    }

    /// Opens the inline add flow and puts the caret in its field.
    pub(crate) fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = true;
        self.add_fault = None;
        self.shell.update(cx, |shell, cx| {
            if !shell.library_open() {
                shell.toggle_panel(Side::Library, cx);
            }
            shell.focus_on(crate::store::shell::Focus::Library, cx);
        });
        let text = self.coordinate.read(cx).as_str().to_owned();
        self.catalog
            .update(cx, |catalog, cx| catalog.look_up(&text, cx));
        let handle = self.coordinate.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Closes the inline add flow and forgets its catalog lookup.
    pub(super) fn cancel_add(&mut self, cx: &mut Context<Self>) {
        self.adding = false;
        self.add_fault = None;
        self.catalog.update(cx, super::super::store::catalog::CatalogStore::clear);
        cx.notify();
    }

    fn add_form(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.coordinate.read(cx).as_str().to_owned();
        let hint = validate(live.trim()).err();
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .p(space(Space::Base))
            .child(
                text::faint(theme)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("ADD A PROJECT"),
            )
            .child(Self::folder_button(theme, cx))
            .child(self.coordinate_field(theme))
            .when_some(hint.filter(|_| !live.trim().is_empty()), |form, message| {
                form.child(
                    text::dim(theme)
                        .text_color(theme.paint(Paint::Caution))
                        .child(message),
                )
            })
            .when_some(self.add_fault.clone(), |form, message| {
                form.child(
                    text::dim(theme)
                        .text_color(theme.paint(Paint::Fault))
                        .child(message),
                )
            })
            .child(self.suggestions(theme, cx))
            .child(
                div()
                    .flex()
                    .gap(space(Space::Snug))
                    .child(
                        button::button(theme, "add-submit", "Index", button::Weight::Primary)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_coordinate(cx))),
                    )
                    .child(
                        button::button(theme, "add-cancel", "Cancel", button::Weight::Quiet)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_add(cx))),
                    ),
            )
    }

    fn folder_button(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        button::button(
            theme,
            "add-folder",
            "Choose a folder on this Mac…",
            button::Weight::Regular,
        )
        .w_full()
        .justify_center()
        .on_click(cx.listener(|_, _, _, cx| Self::choose_folder(cx)))
    }

    fn coordinate_field(&mut self, theme: &Theme) -> impl IntoElement {
        div()
            .w_full()
            .px(space(Space::Snug))
            .py(px(4.0))
            .rounded(radius(Radius::Small))
            .bg(theme.paint(Paint::Ground))
            .border(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(
                text_input("add-coordinate")
                    .state(self.coordinate.downgrade())
                    .placeholder("pkg:cargo/serde@1.0.0  or  /path/to/project")
                    .placeholder_color(theme.paint(Paint::TextFaint))
                    .selection_color(theme.paint(Paint::GiltWash))
                    .caret_color(theme.paint(Paint::Gilt))
                    .text_size(type_size(TypeScale::Small))
                    .text_color(theme.paint(Paint::TextStrong)),
            )
    }

    /// Returns the live catalog rows behind the coordinate field.
    fn suggestions(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match self.catalog.read(cx).state().clone() {
            Catalog::Idle => div().into_any_element(),
            Catalog::Looking => text::faint(theme)
                .child("Searching the local catalog…")
                .into_any_element(),
            Catalog::Faulted(fault) => fault_ui::inline(theme, &fault).into_any_element(),
            Catalog::Rows(rows) if rows.is_empty() => text::faint(theme)
                .child("The local catalog holds no package matching this text.")
                .into_any_element(),
            Catalog::Rows(rows) => div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(at, row)| Self::suggestion_row(theme, at, row, cx)),
                )
                .into_any_element(),
        }
    }

    fn suggestion_row(
                theme: &Theme,
        at: usize,
        row: &Suggestion,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let coordinate = row.coordinate().to_owned();
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "suggestion-{at}"
            ))))
            .flex()
            .items_center()
            .gap(space(Space::Tight))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.fill_coordinate(&coordinate, cx);
            }))
            .child(chip::badge(theme, row.ecosystem()))
            .child(
                text::single_line(text::label(theme).text_size(type_size(TypeScale::Small)))
                    .flex_1()
                    .min_w(px(0.0))
                    .child(row.name().to_owned()),
            )
            .child(
                text::faint(theme)
                    .flex_none()
                    .font_family(theme.specimen())
                    .child(row.version().to_owned()),
            )
            .child(text::faint(theme).flex_none().child(bytes(row.bytes())))
            .into_any_element()
    }

    /// Replaces the coordinate field's text and re-runs the catalog lookup.
    pub(crate) fn fill_coordinate(&mut self, text: &str, cx: &mut Context<Self>) {
        self.coordinate.update(cx, |field, cx| {
            field.emplace(text, cx);
            field.move_to(text.len(), cx);
        });
        self.catalog
            .update(cx, |catalog, cx| catalog.look_up(text, cx));
        cx.notify();
    }

    fn submit_coordinate(&mut self, cx: &mut Context<Self>) {
        let text = self.coordinate.read(cx).as_str().trim().to_owned();
        match validate(&text) {
            Ok(coordinate) => {
                self.add_fault = None;
                self.adding = false;
                self.coordinate.update(cx, |field, cx| field.emplace("", cx));
                self.catalog.update(cx, super::super::store::catalog::CatalogStore::clear);
                self.index_project(coordinate, cx);
            }
            Err(message) => {
                self.add_fault = Some(message);
                cx.notify();
            }
        }
    }

    fn choose_folder(cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(chosen))) = paths.await else {
                return;
            };
            let Some(folder) = chosen.first().cloned() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.adding = false;
                this.index_project(folder.to_string_lossy().into_owned(), cx);
            });
        })
        .detach();
    }
}

/// Example coordinates offered on the first run and in the add flow.
pub(super) const EXAMPLES: [&str; 3] = [
    "pkg:cargo/memchr@2.7.4",
    "pkg:pypi/attrs@24.2.0",
    "pkg:npm/zod@3.23.8",
];

/// Validates a project path or package coordinate before anything is submitted.
///
/// # Errors
/// Returns the sentence shown under the field when the text cannot be indexed.
pub(super) fn validate(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Type a package coordinate, or choose a folder.".to_owned());
    }
    if let Some(rest) = trimmed.strip_prefix("pkg:") {
        return validate_coordinate(trimmed, rest);
    }
    let path = std::path::Path::new(trimmed);
    if !path.is_absolute() {
        return Err("A local project needs an absolute path.".to_owned());
    }
    if !path.is_dir() {
        return Err("No folder exists at that path.".to_owned());
    }
    Ok(trimmed.to_owned())
}

fn validate_coordinate(full: &str, rest: &str) -> Result<String, String> {
    let Some((ecosystem, tail)) = rest.split_once('/') else {
        return Err("A package coordinate looks like pkg:cargo/name@version.".to_owned());
    };
    if ecosystem.is_empty() {
        return Err("A package coordinate needs an ecosystem, such as cargo or pypi.".to_owned());
    }
    if !tail.contains('@') {
        return Err("A package coordinate needs a pinned @version.".to_owned());
    }
    match backend_library::PackageReference::parse(full) {
        Ok(_) => Ok(full.to_owned()),
        Err(error) => Err(format!("This coordinate is not canonical: {error}.")),
    }
}

fn shelf_title(theme: &Theme, entry: &ShelfEntry) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .child(glyph::standing_mark(theme, project::standing(entry)))
        .child(
            text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                .flex_1()
                .min_w(px(0.0))
                .child(entry.identity().name().to_owned()),
        )
        .child(chip::badge(theme, &project::badge(entry.identity())))
}

fn shelf_detail(theme: &Theme, entry: &ShelfEntry) -> Div {
    let declarations = entry.declarations().get();
    let state = project::summary(entry);
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .pl(px(20.0))
        .child(text::single_line(text::faint(theme)).child(state))
        .child(glyph::language_bar(theme, entry.languages(), declarations))
        .when(!entry.languages().is_empty(), |detail| {
            detail.child(
                text::single_line(text::faint(theme)).child(
                    entry
                        .languages()
                        .iter()
                        .map(|count| {
                            format!(
                                "{} {}",
                                language_label(count.language()),
                                count.declarations().get()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
            )
        })
}

/// Returns a short human size for a verified archive.
fn bytes(count: u64) -> String {
    const UNIT: u64 = 1024;
    if count < UNIT {
        return format!("{count} B");
    }
    let kilobytes = count / UNIT;
    if kilobytes < UNIT {
        return format!("{kilobytes} KB");
    }
    format!("{} MB", kilobytes / UNIT)
}
