//! The window root: titlebar, panels, reader, status bar, and overlays.
//! It owns the stores, the focus, and every action; the panels only draw.
//! Layout is reserved on the first frame, so nothing arrives as a late overlay.
//!
//! Everything a reader can do enters through this entity. Keeping the action
//! handlers in one place — rather than scattered through the panels that show
//! their effects — is what lets the keyboard map be complete and checkable: a
//! key reaches an action, an action calls a store, and every panel re-renders
//! from the store it observes.

use super::actions::{
    Accept, AddProject, CloseTab, Complete, CopyIdentity, CopyKey, Dismiss, FocusOmnibar, GoBack,
    GoForward, GrowInterface, MoveDown, MoveUp, NextTab, OpenPalette, OpenSettings, PageDown,
    PageUp, PreviousTab, Reload, ResetInterface, SelectFirst, SelectLast, ShrinkInterface, Tab1,
    Tab2, Tab3, Tab4, Tab5, Tab6, Tab7, Tab8, Tab9, ToggleAppearance, ToggleContext, ToggleLibrary,
    ToggleMotion, WINDOW_CONTEXT, tab_index,
};
use crate::host::lease::HostMode;
use crate::presentation::identity::Identity;
use crate::reducer::model::Model;
use crate::store::document::{DocumentStore, Subject, Target};
use crate::store::jobs::{JobKind, JobsStore};
use crate::store::prefs::Preferences;
use crate::store::search::{Mode, SearchStore};
use crate::store::service::Endpoint;
use crate::store::shell::{Focus, ShellStore, Side};
use crate::store::workspace::WorkspaceStore;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::Chrome;
use crate::transport::unix::UnixSubscriptionTransport;
use crate::ui::surface;
use backend_library::{RowId, SymbolKey, ViewRoot};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage, TextChanged};
use std::path::PathBuf;

/// The window's root entity.
pub(crate) struct Workspace {
    pub(super) workspace: Entity<WorkspaceStore>,
    pub(super) search: Entity<SearchStore>,
    pub(super) document: Entity<DocumentStore>,
    pub(super) jobs: Entity<JobsStore>,
    pub(super) shell: Entity<ShellStore>,
    pub(super) field: Entity<EditableTextState>,
    pub(super) coordinate: Entity<EditableTextState>,
    pub(super) adding: bool,
    pub(super) add_fault: Option<String>,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// Builds the whole window around one admitted root and one live feed.
    pub(crate) fn new(
        endpoint: Endpoint,
        project: PathBuf,
        data: PathBuf,
        mode: HostMode,
        root: ViewRoot,
        model: Model,
        transport: UnixSubscriptionTransport,
        prefs: Preferences,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = cx.new(|cx| {
            WorkspaceStore::new(
                endpoint.clone(),
                project,
                data.clone(),
                mode,
                root,
                model,
                transport,
                cx,
            )
        });
        let search = cx.new(|_| SearchStore::new(endpoint.clone()));
        let document = cx.new(|_| DocumentStore::new(endpoint.clone(), workspace.clone()));
        let jobs = cx.new(|_| JobsStore::new(endpoint));
        let shell = cx.new(|_| ShellStore::new(data, prefs));
        let field = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let coordinate = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let subscriptions = Self::wire(&workspace, &search, &document, &jobs, &shell, &field, cx);
        Self {
            workspace,
            search,
            document,
            jobs,
            shell,
            field,
            coordinate,
            adding: false,
            add_fault: None,
            focus: cx.focus_handle(),
            subscriptions,
        }
    }

    fn wire(
        workspace: &Entity<WorkspaceStore>,
        search: &Entity<SearchStore>,
        document: &Entity<DocumentStore>,
        jobs: &Entity<JobsStore>,
        shell: &Entity<ShellStore>,
        field: &Entity<EditableTextState>,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        vec![
            cx.observe(workspace, Self::on_workspace),
            cx.observe(search, |_, _, cx| cx.notify()),
            cx.observe(document, |_, _, cx| cx.notify()),
            cx.observe(jobs, |_, _, cx| cx.notify()),
            cx.observe(shell, |_, _, cx| cx.notify()),
            cx.subscribe(field, Self::on_typed),
        ]
    }

    fn on_workspace(&mut self, store: Entity<WorkspaceStore>, cx: &mut Context<Self>) {
        let shelf = store.read(cx).shelf().clone();
        self.jobs.update(cx, |jobs, cx| jobs.reconcile(&shelf, cx));
        cx.notify();
    }

    fn on_typed(
        &mut self,
        field: Entity<EditableTextState>,
        _: &TextChanged,
        cx: &mut Context<Self>,
    ) {
        let text = field.read(cx).as_str().to_owned();
        self.search.update(cx, |search, cx| search.set_text(text, cx));
    }

    /// Returns the lit theme, kept in step with the shell's preferences.
    pub(super) fn theme(&self, cx: &Context<Self>) -> Theme {
        let prefs = self.shell.read(cx).prefs();
        Theme::new(
            prefs.appearance(),
            prefs.interface(),
            prefs.reduced_motion(),
        )
    }
}

/// Navigation.
impl Workspace {
    /// Opens one declaration, resolving its coordinate from the shelf.
    pub(super) fn open_symbol(&mut self, symbol: SymbolKey, target: Target, cx: &mut Context<Self>) {
        let coordinate = self
            .workspace
            .read(cx)
            .row(RowId::Symbol(symbol))
            .map_or_else(
                || backend_library::encode_id(symbol.as_bytes()),
                |row| row.label.clone(),
            );
        self.document.update(cx, |document, cx| {
            document.open(Subject::Declaration { symbol, coordinate }, target, cx);
        });
        self.dismiss_sheet(cx);
    }

    /// Opens one project's page.
    pub(super) fn open_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| {
            document.open(Subject::Project { coordinate }, Target::Here, cx);
        });
        self.dismiss_sheet(cx);
    }

    /// Submits an index intent for one project path or package coordinate.
    pub(super) fn index_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.jobs.update(cx, |jobs, cx| {
            jobs.submit(JobKind::Index, coordinate, cx);
        });
    }

    /// Submits a remove intent for one project.
    pub(super) fn remove_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.jobs.update(cx, |jobs, cx| {
            jobs.submit(JobKind::Remove, coordinate, cx);
        });
    }

    /// Copies exact text and raises a short confirmation.
    pub(super) fn copy(&mut self, label: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.shell.update(cx, |shell, cx| {
            shell.notify_copied(label.to_owned(), Some(text), cx);
        });
    }

    fn dismiss_sheet(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.dismiss(cx));
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Reader, cx));
    }

    /// Returns the identity of whatever the reader is showing.
    pub(super) fn active_identity(&self, cx: &Context<Self>) -> Option<Identity> {
        self.document
            .read(cx)
            .tab()
            .and_then(|tab| tab.subject().map(Subject::identity))
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme(cx);
        window.set_rem_size(theme.root_pixels());
        self.fit(window, cx);
        surface::ground(&theme)
            .id("nudox-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.paint(Paint::Text))
            .child(self.titlebar(&theme, cx))
            .child(self.body(&theme, cx))
            .child(self.status_bar(&theme, cx))
            .child(self.overlays(&theme, window, cx))
            .with_key_handlers(cx)
    }
}

impl Workspace {
    fn fit(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        self.shell.update(cx, |shell, cx| shell.fit_to(width, cx));
    }

    fn body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = self.shell.read(cx);
        let (library, context) = (shell.library_width(), shell.context_width());
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .child(self.library_panel(theme, library, cx))
            .child(self.reader(theme, cx))
            .child(self.context_panel(theme, context, cx))
    }

    fn overlays(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let settings = self.shell.read(cx).settings_open();
        div()
            .absolute()
            .inset_0()
            .when(settings, |layer| {
                layer.child(self.settings_sheet(theme, cx))
            })
            .when_some(self.hover_card(theme, cx), ParentElement::child)
            .when_some(self.notice_bar(theme, cx), ParentElement::child)
            .child(self.omnibar_sheet(theme, window, cx))
    }
}

/// Action handling.
trait KeyHandlers: Sized {
    fn with_key_handlers(self, cx: &mut Context<Workspace>) -> Self;
}

impl<E: InteractiveElement> KeyHandlers for E {
    fn with_key_handlers(self, cx: &mut Context<Workspace>) -> Self {
        let element = self
            .on_action(cx.listener(Workspace::focus_omnibar))
            .on_action(cx.listener(Workspace::open_palette))
            .on_action(cx.listener(Workspace::start_add))
            .on_action(cx.listener(Workspace::toggle_library))
            .on_action(cx.listener(Workspace::toggle_context))
            .on_action(cx.listener(Workspace::open_settings))
            .on_action(cx.listener(Workspace::go_back))
            .on_action(cx.listener(Workspace::go_forward))
            .on_action(cx.listener(Workspace::close_tab))
            .on_action(cx.listener(Workspace::previous_tab))
            .on_action(cx.listener(Workspace::next_tab))
            .on_action(cx.listener(Workspace::copy_identity))
            .on_action(cx.listener(Workspace::copy_key))
            .on_action(cx.listener(Workspace::reset_interface))
            .on_action(cx.listener(Workspace::grow_interface))
            .on_action(cx.listener(Workspace::shrink_interface))
            .on_action(cx.listener(Workspace::reload))
            .on_action(cx.listener(Workspace::toggle_appearance))
            .on_action(cx.listener(Workspace::toggle_motion));
        with_sheet_handlers(with_tab_handlers(element, cx), cx)
    }
}

fn with_tab_handlers<E: InteractiveElement>(element: E, cx: &mut Context<Workspace>) -> E {
    element
        .on_action(cx.listener(|this, _: &Tab1, _, cx| this.select_tab(tab_index(1), cx)))
        .on_action(cx.listener(|this, _: &Tab2, _, cx| this.select_tab(tab_index(2), cx)))
        .on_action(cx.listener(|this, _: &Tab3, _, cx| this.select_tab(tab_index(3), cx)))
        .on_action(cx.listener(|this, _: &Tab4, _, cx| this.select_tab(tab_index(4), cx)))
        .on_action(cx.listener(|this, _: &Tab5, _, cx| this.select_tab(tab_index(5), cx)))
        .on_action(cx.listener(|this, _: &Tab6, _, cx| this.select_tab(tab_index(6), cx)))
        .on_action(cx.listener(|this, _: &Tab7, _, cx| this.select_tab(tab_index(7), cx)))
        .on_action(cx.listener(|this, _: &Tab8, _, cx| this.select_tab(tab_index(8), cx)))
        .on_action(cx.listener(|this, _: &Tab9, _, cx| this.select_tab(tab_index(9), cx)))
}

fn with_sheet_handlers<E: InteractiveElement>(element: E, cx: &mut Context<Workspace>) -> E {
    element
        .on_action(cx.listener(Workspace::dismiss))
        .on_action(cx.listener(Workspace::move_up))
        .on_action(cx.listener(Workspace::move_down))
        .on_action(cx.listener(Workspace::page_up))
        .on_action(cx.listener(Workspace::page_down))
        .on_action(cx.listener(Workspace::select_first))
        .on_action(cx.listener(Workspace::select_last))
        .on_action(cx.listener(Workspace::accept))
        .on_action(cx.listener(Workspace::complete))
}

impl Workspace {
    fn focus_omnibar(&mut self, _: &FocusOmnibar, window: &mut Window, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| field.select_document(cx));
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, |search, cx| search.open(cx));
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.set_field(">".to_owned(), cx);
        self.focus_omnibar(&FocusOmnibar, window, cx);
    }

    fn start_add(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = true;
        self.add_fault = None;
        self.shell.update(cx, |shell, cx| {
            if !shell.library_open() {
                shell.toggle_panel(Side::Library, cx);
            }
            shell.focus_on(Focus::Library, cx);
        });
        let handle = self.coordinate.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn toggle_library(&mut self, _: &ToggleLibrary, _: &mut Window, cx: &mut Context<Self>) {
        self.shell
            .update(cx, |shell, cx| shell.toggle_panel(Side::Library, cx));
    }

    fn toggle_context(&mut self, _: &ToggleContext, _: &mut Window, cx: &mut Context<Self>) {
        self.shell
            .update(cx, |shell, cx| shell.toggle_panel(Side::Context, cx));
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.toggle_settings(cx));
    }

    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| document.back(cx));
    }

    fn go_forward(&mut self, _: &GoForward, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| document.forward(cx));
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| {
            let active = document.active();
            document.close(active, cx);
        });
    }

    fn previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| {
            let index = document.active().saturating_sub(1);
            document.activate(index, cx);
        });
    }

    fn next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| {
            let index = document.active().saturating_add(1);
            document.activate(index, cx);
        });
    }

    pub(super) fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.activate(index, cx));
    }

    fn copy_identity(&mut self, _: &CopyIdentity, _: &mut Window, cx: &mut Context<Self>) {
        let Some(identity) = self.active_identity(cx) else {
            return;
        };
        self.copy("Identity copied", identity.coordinate().to_owned(), cx);
    }

    fn copy_key(&mut self, _: &CopyKey, _: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.active_key(cx) else {
            return;
        };
        self.copy("Key copied", key, cx);
    }

    fn active_key(&self, cx: &Context<Self>) -> Option<String> {
        match self.document.read(cx).tab()?.subject()? {
            Subject::Declaration { symbol, .. } => {
                Some(backend_library::encode_id(symbol.as_bytes()))
            }
            Subject::Project { coordinate } => Some(coordinate.clone()),
        }
    }

    fn reset_interface(&mut self, _: &ResetInterface, _: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| {
            shell.set_interface(crate::theme::tokens::InterfaceSize::DEFAULT, cx);
        });
    }

    fn grow_interface(&mut self, _: &GrowInterface, _: &mut Window, cx: &mut Context<Self>) {
        self.step_interface(true, cx);
    }

    fn shrink_interface(&mut self, _: &ShrinkInterface, _: &mut Window, cx: &mut Context<Self>) {
        self.step_interface(false, cx);
    }

    fn step_interface(&mut self, up: bool, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| {
            let next = shell.prefs().interface().stepped(up);
            shell.set_interface(next, cx);
        });
    }

    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, |document, cx| document.reload(cx));
        self.workspace
            .update(cx, |workspace, cx| workspace.refresh_health(cx));
    }

    fn toggle_appearance(&mut self, _: &ToggleAppearance, _: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| {
            let next = shell.prefs().appearance().flipped();
            shell.set_appearance(next, cx);
        });
    }

    fn toggle_motion(&mut self, _: &ToggleMotion, _: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| {
            let next = !shell.reduced_motion();
            shell.set_reduced_motion(next, cx);
        });
    }
}

/// Sheet navigation.
impl Workspace {
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.read(cx).settings_open() {
            self.shell.update(cx, |shell, cx| shell.toggle_settings(cx));
            return;
        }
        if self.adding {
            self.adding = false;
            self.add_fault = None;
            cx.notify();
            return;
        }
        if self.search.read(cx).is_open() {
            self.set_field(String::new(), cx);
            self.dismiss_sheet(cx);
            window.focus(&self.focus, cx);
        }
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-1, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(1, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-8, cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(8, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).is_open() {
            self.search.update(cx, |search, cx| search.select(0, cx));
        }
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).is_open() {
            self.search.update(cx, |search, cx| {
                let last = search.len().saturating_sub(1);
                search.select(last, cx);
            });
        }
    }

    fn step_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.search.read(cx).is_open() {
            return;
        }
        self.search
            .update(cx, |search, cx| search.step(delta, cx));
    }

    fn accept(&mut self, _: &Accept, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding {
            self.submit_add(cx);
            return;
        }
        if !self.search.read(cx).is_open() {
            return;
        }
        if let Some(command) = self.search.read(cx).selected_command() {
            self.search
                .update(cx, |search, cx| search.run(command.spec(), cx));
            return;
        }
        let Some(symbol) = self
            .search
            .read(cx)
            .selected_result()
            .and_then(super::super::store::search::ResultRow::symbol)
        else {
            return;
        };
        self.open_symbol(symbol, Target::Here, cx);
        self.set_field(String::new(), cx);
        window.focus(&self.focus, cx);
    }

    fn complete(&mut self, _: &Complete, _: &mut Window, cx: &mut Context<Self>) {
        if !self.search.read(cx).is_open() {
            return;
        }
        let completion = self.completion(cx);
        if let Some(text) = completion {
            self.set_field(text, cx);
        }
    }

    fn completion(&self, cx: &Context<Self>) -> Option<String> {
        let search = self.search.read(cx);
        match search.parsed().mode() {
            Mode::Palette => search
                .selected_command()
                .map(|command| format!("> {}", command.spec().name)),
            _ => search
                .selected_result()
                .map(|row| row.identity().name().to_owned()),
        }
    }

    pub(super) fn set_field(&mut self, text: String, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| {
            field.emplace(&text, cx);
            let end = field.as_str().len();
            field.move_to(end, cx);
        });
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let text = self.coordinate.read(cx).as_str().trim().to_owned();
        match super::library::validate(&text) {
            Err(message) => {
                self.add_fault = Some(message);
                cx.notify();
            }
            Ok(coordinate) => {
                self.add_fault = None;
                self.adding = false;
                self.coordinate.update(cx, |field, cx| field.emplace("", cx));
                self.index_project(coordinate, cx);
            }
        }
    }
}

/// Returns the fixed height of the titlebar row.
pub(super) fn titlebar_height() -> gpui::Pixels {
    px(Chrome::TITLEBAR)
}
