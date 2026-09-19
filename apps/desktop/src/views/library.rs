//! The projects panel: every project on the shelf, what is open, and the way in.
//! A row states a name, its provenance, and its language mix as a length.
//! A project that is not ready says so on its own row; a ready one says nothing.
//!
//! Two versions of one package are one row. The shelf publishes each pinned
//! coordinate as its own project, which is true, but a reader who indexed
//! `serde@1.0.200` beside `serde@1.0.210` is reading *serde* and switching
//! between versions — so the panel groups the shelf into families by name and
//! draws a version strip under the family, with the version being read lit.
//!
//! Numbers are not printed beside the bar. The mix is a proportion and the
//! pointer reveals the counts, with the language logos, where a reader who
//! wants them can find them and a reader who does not is not asked to read
//! "C++ 2447 · Rust 163" on every row of a shelf.
//!
//! Below the projects sits the tab tree: every open page, nested under the
//! page it was opened from. Adding a project expands in place rather than
//! opening a dialog, and the field takes a name, not a package URL: the
//! ecosystem is a toggle, the version is optional, and the catalog resolves
//! the rest. Collapsed, the panel becomes a rail rather than nothing.

use super::keys;
use super::workspace::Workspace;
use crate::presentation::project::{self, Standing};
use crate::store::catalog::{Ask, Catalog, Suggestion};
use crate::store::document::{Subject, TabId, TreeRow};
use crate::store::registry::{Spelling, version_rank};
use crate::store::shell::Side;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{PanelWidth, Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::bar::{self, Motion};
use crate::ui::icon::{self, Icon, Logo};
use crate::ui::tip::{Card, Tip, Tipped as _};
use crate::ui::{button, chip, fault as fault_ui, glyph, surface, text};
use backend_library::RegistryEcosystem;
use backend_present::{Affordance, Fault, Readiness, ShelfEntry};
use gpui::Focusable as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_elements::editable_text::text_input;

/// Indent per level of the tab tree, in pixels.
const TREE_INDENT: f32 = 12.0;

/// Every ecosystem the add flow offers, in the order the toggles sit.
pub(super) const ECOSYSTEMS: [RegistryEcosystem; 7] = [
    RegistryEcosystem::Cargo,
    RegistryEcosystem::Npm,
    RegistryEcosystem::Pypi,
    RegistryEcosystem::Maven,
    RegistryEcosystem::Nuget,
    RegistryEcosystem::Golang,
    RegistryEcosystem::Cpp,
];

/// One row of the panel: every version of one package, or one folder.
struct Family {
    name: String,
    local: bool,
    entries: Vec<ShelfEntry>,
}

impl Family {
    /// Groups a merged shelf into families, newest version first in each.
    fn all(entries: &[ShelfEntry]) -> Vec<Self> {
        let mut families: Vec<Self> = Vec::new();
        for entry in entries {
            let coordinate = entry.identity().coordinate().as_str();
            let local = project::is_local(entry.identity());
            let key = if local {
                coordinate.to_owned()
            } else {
                let spelling = Spelling::of(coordinate);
                format!(
                    "{}/{}",
                    spelling.ecosystem().map_or("", RegistryEcosystem::as_str),
                    spelling.name()
                )
            };
            match families.iter_mut().find(|family| family.name == key) {
                Some(family) => family.entries.push(entry.clone()),
                None => families.push(Self {
                    name: key,
                    local,
                    entries: vec![entry.clone()],
                }),
            }
        }
        for family in &mut families {
            family
                .entries
                .sort_by_key(|entry| version_rank(&project::badge(entry.identity())));
        }
        families
    }

    /// Returns the version the reader is on, or the newest.
    fn shown<'a>(&'a self, active_root: Option<&str>) -> &'a ShelfEntry {
        self.entries
            .iter()
            .find(|entry| Some(entry.identity().coordinate().as_str()) == active_root)
            .or_else(|| self.entries.first())
            .unwrap_or_else(|| &self.entries[0])
    }
}

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
            return self.library_rail(theme, width, &entries, cx);
        }
        let tree = self.document.read(cx).tree();
        let families = Family::all(&entries);
        let active = self.active_root(cx);
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
            .child(Self::library_head(theme, families.len(), cx))
            .child(
                div()
                    .id("library-rows")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .py(space(Space::Tight))
                    .children(
                        families
                            .iter()
                            .enumerate()
                            .map(|(at, family)| Self::family_row(theme, at, family, active.as_deref(), cx)),
                    )
                    .when(entries.is_empty(), |rows| rows.child(empty_shelf(theme))),
            )
            .child(self.add_region(theme, cx))
            .when(!tree.is_empty(), |panel| panel.child(self.tab_tree(theme, &tree, cx)))
            .into_any_element()
    }

    /// Returns the root of the project the reader is inside, if any.
    fn active_root(&self, cx: &Context<Self>) -> Option<String> {
        self.document
            .read(cx)
            .tab()
            .and_then(|tab| tab.subject().and_then(Subject::project_root))
    }

    /// Returns the collapsed panel: one readiness mark per project, one dot per tab.
    fn library_rail(
        &self,
        theme: &Theme,
        width: f32,
        entries: &[ShelfEntry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if width < 1.0 {
            return div().into_any_element();
        }
        let tree = self.document.read(cx).tree();
        let dots: Vec<AnyElement> = tree
            .iter()
            .enumerate()
            .map(|(at, row)| rail_dot(theme, at, row, cx).into_any_element())
            .collect();
        let marks: Vec<AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(at, entry)| rail_project(theme, at, entry, cx).into_any_element())
            .collect();
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
                button::icon_button(theme, "expand-library", Icon::ChevronRight)
                    .tip(Tip::new("Show projects").key(keys::TOGGLE_LIBRARY))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
                    })),
            )
            .children(marks)
            .when(!tree.is_empty(), |rail| {
                rail.child(div().w(px(12.0)).h(hairline()).bg(theme.paint(Paint::Hairline)))
                    .children(dots)
            })
            .into_any_element()
    }

    fn library_head(theme: &Theme, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child("PROJECTS"),
            )
            .when(count > 0, |head| head.child(text::faint(theme).child(count.to_string())))
            .child(div().flex_1())
            .child(
                button::icon_button(theme, "add-project-head", Icon::Plus)
                    .tip(Tip::new("Add a project")
                        .detail("A folder on this machine, or a package from a registry.")
                        .key(keys::ADD_PROJECT))
                    .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx))),
            )
            .child(
                button::icon_button(theme, "collapse-library", Icon::ChevronLeft)
                    .tip(Tip::new("Hide projects").key(keys::TOGGLE_LIBRARY))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
                    })),
            )
    }

    fn family_row(
        theme: &Theme,
        at: usize,
        family: &Family,
        active_root: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = family.shown(active_root);
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let active = active_root == Some(coordinate.as_str());
        let standing = project::standing(entry);
        let opened = coordinate.clone();
        div()
            .id(ElementId::Name(SharedString::from(format!("shelf-{at}"))))
            .mx(space(Space::Tight))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(space(Space::Snug))
            .py(px(6.0))
            .rounded(radius(Radius::Small))
            .when(active, |row| row.bg(theme.paint(Paint::Selected)))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.open_project(opened.clone(), cx)))
            .card(project_tip(entry, family))
            .child(shelf_title(theme, entry, family, standing))
            .child(bar::language_bar(
                theme,
                format!("shelf-bar-{at}"),
                entry.languages(),
                entry.declarations().get(),
                Motion::of(standing),
            ))
            .child(shelf_meta(theme, entry, standing))
            .when(family.entries.len() > 1, |row| {
                row.child(Self::version_strip(theme, at, family, &coordinate, cx))
            })
            .when_some(entry.readiness().fault().cloned(), |row, fault| {
                row.child(Self::failed_row(theme, at, &fault, &coordinate, cx))
            })
            .into_any_element()
    }

    /// Returns the strip of versions under a family, the shown one lit.
    fn version_strip(
        theme: &Theme,
        at: usize,
        family: &Family,
        shown: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap(px(3.0))
            .pt(px(3.0))
            .children(family.entries.iter().enumerate().map(|(index, entry)| {
                let coordinate = entry.identity().coordinate().as_str().to_owned();
                let lit = coordinate == shown;
                let standing = project::standing(entry);
                let opened = coordinate.clone();
                div()
                    .id(ElementId::Name(SharedString::from(format!("version-{at}-{index}"))))
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .px(px(5.0))
                    .py(px(1.0))
                    .rounded(radius(Radius::Hair))
                    .bg(theme.paint(if lit { Paint::GiltWash } else { Paint::Hover }))
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(if lit { Paint::Gilt } else { Paint::TextDim }))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.paint(Paint::Selected)))
                    .when(standing != Standing::Readable, |chip| {
                        chip.child(text::faint(theme).child(glyph::standing_glyph(standing)))
                    })
                    .child(project::badge(entry.identity()))
                    .tip(Tip::new(format!("Open {}", project::badge(entry.identity())))
                        .detail(project::summary(entry))
                        .value(coordinate))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_project(opened.clone(), cx);
                    }))
            }))
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

/// The tab tree.
impl Workspace {
    fn tab_tree(&self, theme: &Theme, tree: &[TreeRow], cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.index.clone();
        div()
            .id("tab-tree")
            .flex_none()
            .max_h(gpui::relative(0.42))
            .border_t(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .px(space(Space::Base))
                    .py(space(Space::Snug))
                    .child(
                        text::faint(theme)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("OPEN"),
                    )
                    .child(text::faint(theme).child(tree.len().to_string())),
            )
            .child(
                div()
                    .id("tab-tree-rows")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .pb(space(Space::Tight))
                    .children(
                        tree.iter()
                            .enumerate()
                            .map(|(at, row)| Self::tab_row(theme, at, row, &index, cx)),
                    ),
            )
    }

    fn tab_row(
        theme: &Theme,
        at: usize,
        row: &TreeRow,
        index: &gpui::Entity<crate::store::index::IndexStore>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let group = SharedString::from(format!("tab-row-{at}"));
        let indent = TREE_INDENT * to_f32(row.depth.min(8));
        div()
            .id(ElementId::Name(SharedString::from(format!("tab-{at}"))))
            .group(group.clone())
            .mx(space(Space::Tight))
            .h(px(24.0))
            .pl(px(indent + 6.0))
            .pr(space(Space::Tight))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .rounded(radius(Radius::Small))
            .when(row.active, |tab| tab.bg(theme.paint(Paint::Selected)))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.document.update(cx, |document, cx| document.activate(id, cx));
            }))
            .on_aux_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if event.is_middle_click() {
                    this.document.update(cx, |document, cx| document.close(id, cx));
                }
            }))
            .tip(tab_tip(row))
            .child(tab_glyph(theme, row, index, cx))
            .child(
                text::single_line(if row.active {
                    text::label(theme).font_weight(FontWeight::MEDIUM)
                } else {
                    text::dim(theme).text_size(type_size(TypeScale::Interface))
                })
                .flex_1()
                .min_w(px(0.0))
                .child(row.title.clone()),
            )
            .when(row.loading, |tab| {
                tab.child(text::faint(theme).flex_none().child("◐"))
            })
            .child(
                div()
                    .flex_none()
                    .opacity(if row.active { 0.7 } else { 0.0 })
                    .group_hover(group, |style| style.opacity(1.0))
                    .child(close_button(theme, at, id, cx)),
            )
            .into_any_element()
    }
}

/// Returns one project as a mark on the collapsed rail.
fn rail_project(theme: &Theme, at: usize, entry: &ShelfEntry, cx: &mut Context<Workspace>) -> impl IntoElement {
    let coordinate = entry.identity().coordinate().as_str().to_owned();
    let name = entry.identity().name().to_owned();
    let readiness = project::summary(entry);
    div()
        .id(ElementId::Name(SharedString::from(format!("rail-{at}"))))
        .cursor_pointer()
        .child(rail_mark(theme, entry))
        .tip(Tip::new(name).detail(readiness).value(coordinate.clone()))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_project(coordinate.clone(), cx);
        }))
}

/// Returns one tab as a dot on the collapsed rail.
fn rail_dot(theme: &Theme, at: usize, row: &TreeRow, cx: &mut Context<Workspace>) -> impl IntoElement {
    let id = row.id;
    div()
        .id(ElementId::Name(SharedString::from(format!("rail-tab-{at}"))))
        .w(px(6.0))
        .h(px(6.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if row.active {
            theme.paint(Paint::Gilt)
        } else {
            theme.paint(Paint::TextFaint)
        })
        .tip(tab_tip(row))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.document.update(cx, |document, cx| document.activate(id, cx));
        }))
}

fn close_button(theme: &Theme, at: usize, id: TabId, cx: &mut Context<Workspace>) -> impl IntoElement {
    button::icon_button(theme, format!("tab-close-{at}"), Icon::Close)
        .tip(Tip::new("Close").detail("Its branches move up to the page it was opened from.").key(keys::CLOSE_TAB))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.document.update(cx, |document, cx| document.close(id, cx));
        }))
}

fn tab_glyph(
    theme: &Theme,
    row: &TreeRow,
    index: &gpui::Entity<crate::store::index::IndexStore>,
    cx: &Context<Workspace>,
) -> AnyElement {
    match &row.subject {
        Some(Subject::Home) | None => {
            icon::sized(theme, Icon::Home, 13.0, Paint::TextDim).into_any_element()
        }
        Some(Subject::Project { .. }) => glyph::package_tile(theme, false).into_any_element(),
        Some(Subject::Package { coordinate }) => {
            let language = Spelling::of(coordinate)
                .ecosystem()
                .map_or(backend_present::Language::Unknown, super::home::ecosystem_language);
            glyph::language_tag(theme, language).into_any_element()
        }
        Some(Subject::Declaration { symbol, .. }) => {
            let kind = index
                .read(cx)
                .entry(*symbol)
                .and_then(crate::store::index::Entry::kind);
            glyph::kind_mark(theme, kind, 12.0).into_any_element()
        }
    }
}

fn tab_tip(row: &TreeRow) -> Tip {
    let coordinate = row
        .subject
        .as_ref()
        .map(Subject::coordinate)
        .unwrap_or_default()
        .to_owned();
    Tip::new(row.title.clone())
        .detail("Middle-click closes; branches nest under the page they were opened from.")
        .value(coordinate)
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
            .child(icon::sized(theme, Icon::Plus, 13.0, Paint::TextDim))
            .child(text::label(theme).flex_1().child("Add a project"))
            .child(button::key_hint(theme, &keys::ADD_PROJECT.label()))
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

    /// Chooses the ecosystem the typed name is looked for in.
    pub(super) fn choose_ecosystem(&mut self, ecosystem: RegistryEcosystem, cx: &mut Context<Self>) {
        self.add_ecosystem = ecosystem;
        self.add_fault = None;
        cx.notify();
    }

    fn add_form(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.coordinate.read(cx).as_str().to_owned();
        let hint = hint_for(&Ask::parse(&live));
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .p(space(Space::Base))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        text::faint(theme)
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("ADD A PROJECT"),
                    )
                    .child(
                        button::icon_button(theme, "add-close", Icon::Close)
                            .tip(Tip::new("Cancel").key(keys::DISMISS))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_add(cx))),
                    ),
            )
            .child(Self::folder_button(theme, cx))
            .child(
                text::faint(theme)
                    .text_align(gpui::TextAlign::Center)
                    .child("or a package from a registry"),
            )
            .child(self.ecosystem_toggles(theme, cx))
            .child(self.coordinate_field(theme))
            .when_some(hint.filter(|_| !live.trim().is_empty()), |form, message| {
                form.child(text::faint(theme).child(message))
            })
            .when_some(self.add_fault.clone(), |form, message| {
                form.child(
                    text::dim(theme)
                        .text_color(theme.paint(Paint::Caution))
                        .child(message),
                )
            })
            .child(self.suggestions(theme, cx))
            .child(Self::add_actions(theme, cx))
    }

    /// Returns the row of ecosystem toggles, each drawn with its logo.
    fn ecosystem_toggles(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let chosen = self.add_ecosystem;
        div()
            .flex()
            .flex_wrap()
            .gap(px(3.0))
            .children(ECOSYSTEMS.map(|ecosystem| {
                let lit = ecosystem == chosen;
                let language = super::home::ecosystem_language(ecosystem);
                let hue = crate::theme::language::hue(language);
                let ink = if lit {
                    theme.on_plane(hue)
                } else {
                    theme.paint(Paint::TextDim)
                };
                div()
                    .id(ElementId::Name(SharedString::from(format!("add-eco-{}", ecosystem.as_str()))))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(radius(Radius::Capsule))
                    .border(hairline())
                    .border_color(if lit { theme.on_plane(hue) } else { theme.paint(Paint::Hairline) })
                    .when(lit, |chip| chip.bg(theme.plane_wash(hue, 0.14)))
                    .text_size(type_size(TypeScale::Tiny))
                    .text_color(ink)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.paint(Paint::Hover)))
                    .when_some(Logo::of(language), |chip, logo| chip.child(icon::logo(logo, 11.0, ink)))
                    .child(ecosystem.as_str().to_owned())
                    .tip(Tip::new(format!("Look in {}", registry_name(ecosystem))))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_ecosystem(ecosystem, cx)))
            }))
    }

    fn add_actions(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .child(
                button::button(theme, "add-submit", "Index", button::Weight::Primary)
                    .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
            )
            .child(button::key_hint(theme, &keys::ACCEPT.label()))
            .child(div().flex_1())
            .child(
                button::button(theme, "add-browse", "Browse the registry", button::Weight::Quiet)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_add(cx);
                        this.open_home(cx);
                    })),
            )
    }

    fn folder_button(theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        button::button(theme, "add-folder", "Choose a folder…", button::Weight::Regular)
            .w_full()
            .justify_center()
            .on_click(cx.listener(|_, _, _, cx| Self::choose_folder(cx)))
    }

    fn coordinate_field(&mut self, theme: &Theme) -> impl IntoElement {
        div()
            .w_full()
            .px(space(Space::Snug))
            .py(px(5.0))
            .rounded(radius(Radius::Small))
            .bg(theme.paint(Paint::Ground))
            .border(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(
                text_input("add-coordinate")
                    .state(self.coordinate.downgrade())
                    .placeholder("name, or name@version")
                    .placeholder_color(theme.paint(Paint::TextFaint))
                    .selection_color(theme.paint(Paint::GiltWash))
                    .caret_color(theme.paint(Paint::Gilt))
                    .text_size(type_size(TypeScale::Small))
                    .text_color(theme.paint(Paint::TextStrong)),
            )
    }

    /// Returns the live catalog rows behind the field, in the chosen ecosystem.
    fn suggestions(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let ecosystem = self.add_ecosystem.as_str();
        match self.catalog.read(cx).state().clone() {
            Catalog::Idle => div().into_any_element(),
            Catalog::Looking => text::faint(theme)
                .child("Searching the local catalog…")
                .into_any_element(),
            Catalog::Faulted(fault) => fault_ui::inline(theme, &fault).into_any_element(),
            Catalog::Rows(rows) => {
                let rows: Vec<&Suggestion> = rows
                    .iter()
                    .filter(|row| row.ecosystem() == ecosystem)
                    .take(6)
                    .collect();
                if rows.is_empty() {
                    return text::faint(theme)
                        .child(format!("Nothing in the local {ecosystem} catalog matches this."))
                        .into_any_element();
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .children(
                        rows.iter()
                            .enumerate()
                            .map(|(at, row)| Self::suggestion_row(theme, at, row, cx)),
                    )
                    .into_any_element()
            }
        }
    }

    fn suggestion_row(
        theme: &Theme,
        at: usize,
        row: &Suggestion,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let filled = format!("{}@{}", row.name(), row.version());
        let coordinate = row.coordinate().to_owned();
        div()
            .id(ElementId::Name(SharedString::from(format!("suggestion-{at}"))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .tip(Tip::new("Use this version").value(coordinate))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.fill_coordinate(&filled, cx);
            }))
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

/// Returns the one faint line that says what the field will do.
fn hint_for(ask: &Ask) -> Option<String> {
    match ask {
        Ask::Folder(_) => Some("A folder on this machine.".to_owned()),
        Ask::Pinned(_) => Some("A pinned package URL, taken as written.".to_owned()),
        Ask::Named { name, version: None } if !name.is_empty() => {
            Some("No version: the newest recorded one is used.".to_owned())
        }
        Ask::Named { version: Some(version), .. } => Some(format!(
            "{version}, or the closest version the index records."
        )),
        Ask::Named { .. } => None,
    }
}

/// Validates a folder or pinned package URL before anything is submitted.
///
/// # Errors
/// Returns the sentence shown under the field when the text cannot be indexed.
pub(super) fn validate(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Type a package name, or choose a folder.".to_owned());
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
        return Err("A package URL looks like pkg:cargo/name@version.".to_owned());
    };
    if ecosystem.is_empty() {
        return Err("A package URL needs an ecosystem, such as cargo or pypi.".to_owned());
    }
    if !tail.contains('@') {
        return Err("A package URL needs a pinned @version; type just the name to take the newest.".to_owned());
    }
    match backend_library::PackageReference::parse(full) {
        Ok(_) => Ok(full.to_owned()),
        Err(error) => Err(format!("This coordinate is not canonical: {error}.")),
    }
}

/// Returns the registry a token stands for, spelled for a reader.
pub(super) const fn registry_name(ecosystem: RegistryEcosystem) -> &'static str {
    match ecosystem {
        RegistryEcosystem::Cargo => "crates.io",
        RegistryEcosystem::Npm => "npm",
        RegistryEcosystem::Pypi => "PyPI",
        RegistryEcosystem::Maven => "Maven Central",
        RegistryEcosystem::Nuget => "NuGet",
        RegistryEcosystem::Golang => "the Go module index",
        RegistryEcosystem::Cpp => "the C/C++ registry",
    }
}

fn shelf_title(theme: &Theme, entry: &ShelfEntry, family: &Family, standing: Standing) -> Div {
    let name = if family.local {
        entry.identity().name().to_owned()
    } else {
        Spelling::of(entry.identity().coordinate().as_str()).name().to_owned()
    };
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .when(standing != Standing::Readable, |title| {
            title.child(glyph::standing_mark(theme, standing))
        })
        .child(
            text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                .flex_1()
                .min_w(px(0.0))
                .child(name),
        )
        .when(family.entries.len() == 1, |title| {
            title.child(chip::badge(theme, &project::badge(entry.identity())))
        })
}

/// Returns the one faint line under the bar: where the project is, or what
/// is happening to it.
fn shelf_meta(theme: &Theme, entry: &ShelfEntry, standing: Standing) -> Div {
    let line = match (standing, entry.readiness()) {
        (Standing::Indexing, Readiness::Indexing { rows }) => {
            format!("indexing · {} declarations so far", rows.get())
        }
        (Standing::Requested, _) => "waiting for the first rows".to_owned(),
        (Standing::Empty, _) => "ready · nothing published".to_owned(),
        (Standing::Failed, _) => "failed".to_owned(),
        _ => provenance_line(entry),
    };
    text::single_line(text::faint(theme)).child(line)
}

fn project_tip(entry: &ShelfEntry, family: &Family) -> Card {
    let mut card = Card::new(entry.identity().name(), None)
        .summary(project::summary(entry))
        .site(entry.identity().coordinate().as_str());
    if family.entries.len() > 1 {
        card = card.signature(format!("{} versions on the shelf", family.entries.len()));
    }
    card
}

fn provenance_line(entry: &ShelfEntry) -> String {
    let coordinate = entry.identity().coordinate().as_str();
    if let Some(rest) = coordinate.strip_prefix("pkg:") {
        return rest
            .split_once('/')
            .map_or_else(|| rest.to_owned(), |(ecosystem, tail)| format!("{ecosystem} · {tail}"));
    }
    home_relative(coordinate)
}

/// Shortens an absolute path to its last two components for one line.
fn home_relative(path: &str) -> String {
    let parts: Vec<&str> = path
        .rsplit(['/', '\\'])
        .filter(|part| !part.is_empty())
        .take(2)
        .collect();
    let mut shown: Vec<&str> = parts.into_iter().rev().collect();
    if shown.len() == 2 {
        shown.insert(0, "…");
    }
    crate::presentation::crumb::elide_middle(&shown.join("/"), 36)
}

fn rail_mark(theme: &Theme, entry: &ShelfEntry) -> Div {
    let standing = project::standing(entry);
    if standing == Standing::Readable {
        let hue = entry
            .languages()
            .first()
            .map_or(crate::theme::language::hue(backend_present::Language::Unknown), |count| {
                crate::theme::language::hue(count.language())
            });
        return div()
            .w(px(8.0))
            .h(px(8.0))
            .rounded_full()
            .bg(theme.on_plane(hue));
    }
    glyph::standing_mark(theme, standing)
}

fn empty_shelf(theme: &Theme) -> Div {
    div()
        .px(space(Space::Base))
        .py(space(Space::Base))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(text::dim(theme).child("No projects yet."))
        .child(text::faint(theme).child("Add a folder or a registry package below."))
}

/// Returns a short human size for a verified archive.
fn bytes(count: u64) -> String {
    crate::store::registry::size_label(count)
}

fn to_f32(depth: usize) -> f32 {
    u8::try_from(depth).map_or(8.0, f32::from)
}
