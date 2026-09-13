//! The library panel: every project on the shelf, and the way to add one.
//! A row states its name, its provenance, its readiness, and its language mix.
//! A project that failed to index says so on its own row, with a way out.
//!
//! Adding a project expands in place rather than opening a dialog, because
//! adding is the one thing a new reader must discover without being told. The
//! two ways in — a folder on this machine, or a registry coordinate — are the
//! two facts the engine can act on, and validation happens before anything is
//! submitted so a typo is answered in the field rather than by a failed job.

use super::workspace::Workspace;
use crate::presentation::shelf::{Readiness, ShelfEntry};
use crate::store::shell::Side;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, fault as fault_ui, glyph, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::Focusable as _;
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
        if width < 1.0 {
            return div().into_any_element();
        }
        let shelf = self
            .jobs
            .read(cx)
            .merge(self.workspace.read(cx).shelf().clone());
        let entries: Vec<ShelfEntry> = shelf.entries().to_vec();
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
            .child(self.library_head(theme, entries.len(), cx))
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

    fn library_head(
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
                text::faint(theme)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("SHELF"),
            )
            .child(div().flex_1())
            .child(chip::count_chip(theme, count, "projects"))
            .child(
                button::icon_button(theme, "collapse-library", crate::ui::icon::Icon::ChevronLeft).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
                    },
                )),
            )
    }

    fn shelf_row(
        &mut self,
        theme: &Theme,
        at: usize,
        entry: &ShelfEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let coordinate = entry.identity().coordinate().to_owned();
        let active = self
            .active_identity(cx)
            .is_some_and(|identity| identity.project() == entry.identity().project());
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
                row.child(self.failed_row(theme, at, &fault, coordinate.clone(), cx))
            })
            .into_any_element()
    }

    fn failed_row(
        &mut self,
        theme: &Theme,
        at: usize,
        fault: &crate::presentation::fault::Fault,
        coordinate: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = self.affordances(theme, &format!("shelf-{at}"), fault, &coordinate, cx);
        div()
            .pt(px(3.0))
            .child(fault_ui::block(theme, fault, actions))
            .into_any_element()
    }

    /// Returns the buttons for one fault's affordances, already wired.
    pub(super) fn affordances(
        &mut self,
        theme: &Theme,
        prefix: &str,
        fault: &crate::presentation::fault::Fault,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        fault
            .affordances()
            .iter()
            .enumerate()
            .map(|(at, affordance)| {
                let id = format!("{prefix}-affordance-{at}");
                let action = affordance.clone();
                let coordinate = coordinate.to_owned();
                fault_ui::affordance_button(theme, id, affordance)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_affordance(&action, &coordinate, window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn run_affordance(
        &mut self,
        affordance: &crate::presentation::fault::Affordance,
        coordinate: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::presentation::fault::Affordance;
        match affordance {
            Affordance::Retry | Affordance::Refresh => {
                self.document.update(cx, |document, cx| document.reload(cx));
                self.workspace
                    .update(cx, |workspace, cx| workspace.refresh_health(cx));
            }
            Affordance::Reindex { coordinate } => self.index_project(coordinate.clone(), cx),
            Affordance::Remove { coordinate } => self.remove_project(coordinate.clone(), cx),
            Affordance::OpenFolder { path } => cx.reveal_path(std::path::Path::new(path)),
            Affordance::Copy { label, text } => self.copy(label, text.clone(), cx),
            Affordance::Reconnect => self.jobs.update(cx, |jobs, cx| jobs.dismiss(coordinate, cx)),
            Affordance::OpenSettings => {
                self.shell.update(cx, |shell, cx| shell.toggle_settings(cx));
            }
            Affordance::Waiting => {}
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
                self.add_button(theme, cx).into_any_element()
            })
    }

    fn add_button(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = true;
        self.add_fault = None;
        let handle = self.coordinate.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
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
            .child(self.folder_button(theme, cx))
            .child(self.coordinate_field(theme, cx))
            .when_some(hint.filter(|_| !live.trim().is_empty()), |form, message| {
                form.child(text::dim(theme).text_color(theme.paint(Paint::Caution)).child(message))
            })
            .when_some(self.add_fault.clone(), |form, message| {
                form.child(text::dim(theme).text_color(theme.paint(Paint::Fault)).child(message))
            })
            .child(self.add_examples(theme, cx))
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding = false;
                                this.add_fault = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn folder_button(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        button::button(
            theme,
            "add-folder",
            "Choose a folder on this Mac…",
            button::Weight::Regular,
        )
        .w_full()
        .justify_center()
        .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx)))
    }

    fn coordinate_field(&mut self, theme: &Theme, cx: &Context<Self>) -> impl IntoElement {
        let _ = cx;
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

    fn add_examples(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Tight))
            .children(EXAMPLES.iter().enumerate().map(|(at, example)| {
                div()
                    .id(ElementId::Name(SharedString::from(format!("example-{at}"))))
                    .px(space(Space::Snug))
                    .py(px(2.0))
                    .rounded(radius(Radius::Hair))
                    .bg(theme.paint(Paint::Hover))
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(Paint::TextDim))
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme.paint(Paint::Gilt)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.coordinate.update(cx, |field, cx| {
                            field.emplace(example, cx);
                            field.move_to(example.len(), cx);
                        });
                        cx.notify();
                    }))
                    .child((*example).to_owned())
            }))
    }

    fn submit_coordinate(&mut self, cx: &mut Context<Self>) {
        let text = self.coordinate.read(cx).as_str().trim().to_owned();
        match validate(&text) {
            Ok(coordinate) => {
                self.add_fault = None;
                self.adding = false;
                self.coordinate.update(cx, |field, cx| field.emplace("", cx));
                self.index_project(coordinate, cx);
            }
            Err(message) => {
                self.add_fault = Some(message);
                cx.notify();
            }
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
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
        .child(glyph::readiness_mark(theme, entry.readiness()))
        .child(
            text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                .flex_1()
                .min_w(px(0.0))
                .child(entry.identity().project_name().to_owned()),
        )
        .child(chip::badge(theme, &entry.badge()))
}

fn shelf_detail(theme: &Theme, entry: &ShelfEntry) -> Div {
    let state = match entry.readiness() {
        Readiness::Indexing { declarations } => format!("indexing · {declarations} so far"),
        Readiness::Requested => "requested".to_owned(),
        Readiness::Failed(_) => "failed".to_owned(),
        Readiness::Ready => format!(
            "{} declarations · {} files",
            entry.declarations(),
            entry.files()
        ),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .pl(px(20.0))
        .child(text::single_line(text::faint(theme)).child(state))
        .child(glyph::language_bar(
            theme,
            entry.languages(),
            entry.declarations(),
        ))
}
