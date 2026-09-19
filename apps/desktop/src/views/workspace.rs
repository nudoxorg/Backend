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
    GoForward, GoHome, GrowInterface, MoveDown, MoveUp, NextTab, OpenEditor, OpenPalette,
    OpenSettings, OpenSource, PageDown, PageUp, PreviousTab, Reload, ResetInterface, SelectFirst, SelectLast,
    ShrinkInterface, Tab1, Tab2, Tab3, Tab4, Tab5, Tab6, Tab7, Tab8, Tab9, ToggleAppearance,
    ToggleContext, ToggleLibrary, ToggleMotion, WINDOW_CONTEXT, tab_index,
};
use crate::host::lease::HostMode;
use backend_present::Identity;
use crate::reducer::model::Model;
use crate::store::document::{DocumentStore, Subject, Target};
use crate::store::catalog::{Ask, CatalogStore};
use crate::store::events::CatalogEvent;
use crate::store::marks::Recent;
use crate::store::index::IndexStore;
use crate::store::registry::RegistryStore;
use crate::store::jobs::{JobKind, JobsStore};
use crate::store::prefs::Preferences;
use crate::store::search::{Mode, SearchStore};
use crate::store::service::Endpoint;
use crate::store::shell::{Focus, ShellStore, Side};
use crate::store::workspace::WorkspaceStore;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::transport::unix::UnixSubscriptionTransport;
use crate::ui::surface;
use backend_library::{SymbolKey, ViewRoot};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage, TextChanged};
use std::path::PathBuf;

/// The window's root entity.
pub(crate) struct Workspace {
    pub(super) engine: Entity<WorkspaceStore>,
    pub(super) search: Entity<SearchStore>,
    pub(super) document: Entity<DocumentStore>,
    pub(super) index: Entity<IndexStore>,
    pub(super) jobs: Entity<JobsStore>,
    pub(super) catalog: Entity<CatalogStore>,
    pub(super) registry: Entity<RegistryStore>,
    pub(super) shell: Entity<ShellStore>,
    pub(super) field: Entity<EditableTextState>,
    pub(super) coordinate: Entity<EditableTextState>,
    pub(super) adding: bool,
    pub(super) add_fault: Option<String>,
    pub(super) add_ecosystem: backend_library::RegistryEcosystem,
    pub(super) source_open: bool,
    pub(super) source_cache: Option<super::source::SourceCache>,
    folded: Vec<String>,
    unfurled: Vec<String>,
    #[cfg(feature = "preview")]
    scene: Option<crate::preview::Scene>,
    pub(super) outline_scroll: gpui::UniformListScrollHandle,
    pub(super) revealed: Option<SymbolKey>,
    pub(super) sheet_scroll: gpui::ScrollHandle,
    pub(super) revealed_row: Option<usize>,
    focus: FocusHandle,
    /// Held, not read: a `Subscription` unsubscribes the moment it is dropped.
    _subscriptions: Vec<Subscription>,
}

/// Everything one window opens with, gathered before the platform starts.
pub(crate) struct Bootstrap {
    /// Where the local service is listening.
    pub(crate) endpoint: Endpoint,
    /// The project this window discovered.
    pub(crate) project: PathBuf,
    /// The durable workspace directory preferences live beside.
    pub(crate) data: PathBuf,
    /// Whether this process owns the service or attached to it.
    pub(crate) mode: HostMode,
    /// The first admitted view root.
    pub(crate) root: ViewRoot,
    /// The reducer holding that root and its cursor.
    pub(crate) model: Model,
    /// The certified subscription transport.
    pub(crate) transport: UnixSubscriptionTransport,
    /// The preferences read before the first frame.
    pub(crate) prefs: Preferences,
}

impl Workspace {
    /// Builds the whole window around one admitted root and one live feed.
    pub(crate) fn new(opened: Bootstrap, cx: &mut Context<Self>) -> Self {
        let Bootstrap {
            endpoint,
            project,
            data,
            mode,
            root,
            model,
            transport,
            prefs,
        } = opened;
        let engine = cx.new(|cx| {
            WorkspaceStore::new(
                crate::store::workspace::EngineLink {
                    endpoint: endpoint.clone(),
                    project,
                    mode,
                    root,
                    model,
                    transport,
                },
                cx,
            )
        });
        let stores = Self::stores(&endpoint, &engine, data, prefs, cx);
        let subscriptions = Self::wire(
            &Wiring {
                engine: &engine,
                search: &stores.search,
                document: &stores.document,
                index: &stores.index,
                jobs: &stores.jobs,
                catalog: &stores.catalog,
                registry: &stores.registry,
                shell: &stores.shell,
                field: &stores.field,
                coordinate: &stores.coordinate,
            },
            cx,
        );
        Self::assemble(engine, stores, subscriptions, cx)
    }

    /// Returns the window with every store installed and nothing yet open.
    fn assemble(
        engine: Entity<WorkspaceStore>,
        stores: Stores,
        subscriptions: Vec<Subscription>,
        cx: &Context<Self>,
    ) -> Self {
        let Stores {
            search,
            document,
            index,
            jobs,
            catalog,
            registry,
            shell,
            field,
            coordinate,
        } = stores;
        Self {
            engine,
            search,
            document,
            index,
            jobs,
            catalog,
            registry,
            shell,
            field,
            coordinate,
            adding: false,
            add_fault: None,
            add_ecosystem: backend_library::RegistryEcosystem::Cargo,
            source_open: false,
            source_cache: None,
            folded: Vec::new(),
            unfurled: Vec::new(),
            #[cfg(feature = "preview")]
            scene: crate::preview::Scene::from_env(),
            outline_scroll: gpui::UniformListScrollHandle::new(),
            revealed: None,
            sheet_scroll: gpui::ScrollHandle::new(),
            revealed_row: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Builds every store that hangs off one endpoint and the engine's feed.
    fn stores(
        endpoint: &Endpoint,
        engine: &Entity<WorkspaceStore>,
        data: PathBuf,
        prefs: Preferences,
        cx: &mut Context<Self>,
    ) -> Stores {
        let index = cx.new(|cx| IndexStore::new(engine, cx));
        Stores {
            search: cx.new(|_| SearchStore::new(endpoint.clone())),
            document: cx.new(|_| {
                DocumentStore::new(endpoint.clone(), engine.clone(), index.clone())
            }),
            index,
            jobs: cx.new(|_| JobsStore::new(endpoint.clone())),
            catalog: cx.new(|_| CatalogStore::new(endpoint.clone())),
            registry: cx.new(|_| RegistryStore::new(endpoint.clone())),
            shell: cx.new(|_| ShellStore::new(data, prefs)),
            field: cx.new(|cx| EditableTextState::new(StringStorage::default(), cx)),
            coordinate: cx.new(|cx| EditableTextState::new(StringStorage::default(), cx)),
        }
    }

    fn wire(parts: &Wiring<'_>, cx: &mut Context<Self>) -> Vec<Subscription> {
        vec![
            cx.observe(parts.engine, |this, store, cx| this.on_engine(&store, cx)),
            cx.observe(parts.search, |_, _, cx| cx.notify()),
            cx.observe(parts.document, |_, _, cx| cx.notify()),
            cx.observe(parts.index, |_, _, cx| cx.notify()),
            cx.observe(parts.jobs, |_, _, cx| cx.notify()),
            cx.observe(parts.catalog, |_, _, cx| cx.notify()),
            cx.subscribe(parts.catalog, |this, catalog, event, cx| {
                this.on_catalog(&catalog, *event, cx);
            }),
            cx.observe(parts.registry, |_, _, cx| cx.notify()),
            cx.observe(parts.shell, |_, _, cx| cx.notify()),
            cx.subscribe(parts.field, |this, field, event, cx| this.on_typed(&field, event, cx)),
            cx.subscribe(parts.coordinate, |this, field, event, cx| {
                this.on_coordinate_typed(&field, event, cx);
            }),
        ]
    }

    fn on_engine(&mut self, store: &Entity<WorkspaceStore>, cx: &mut Context<Self>) {
        let published = store.read(cx).shelf().clone();
        self.jobs.update(cx, |jobs, cx| jobs.reconcile(&published, cx));
        cx.notify();
    }

    /// Opens whatever scene the preview build was asked for, once it can.
    ///
    /// A scene is applied on the first render where the live root can actually
    /// satisfy it, because a scene applied before the first revision arrived
    /// would only ever photograph the loading state.
    #[cfg(feature = "preview")]
    fn stage_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scene) = self.scene else {
            return;
        };
        if crate::preview::stage(self, scene, window, cx) {
            eprintln!("backend-desktop: opened in the {} scene", scene.name());
            self.scene = None;
        }
    }

    /// Acts on a resolved add: index what was decided, or say why nothing was.
    fn on_catalog(&mut self, catalog: &Entity<CatalogStore>, event: CatalogEvent, cx: &mut Context<Self>) {
        if event != CatalogEvent::Resolved {
            return;
        }
        let Some(resolution) = catalog.update(cx, |catalog, _| catalog.take_resolved()) else {
            return;
        };
        match resolution.coordinate() {
            Some(coordinate) => {
                let coordinate = coordinate.to_owned();
                self.add_fault = None;
                self.adding = false;
                self.coordinate.update(cx, |field, cx| field.emplace("", cx));
                self.catalog.update(cx, CatalogStore::clear);
                if let Some(notice) = resolution.notice() {
                    self.shell.update(cx, |shell, cx| shell.notify(notice, cx));
                }
                self.index_project(coordinate, cx);
            }
            None => self.add_fault = resolution.notice(),
        }
        cx.notify();
    }

    fn on_typed(
        &mut self,
        field: &Entity<EditableTextState>,
        _: &TextChanged,
        cx: &mut Context<Self>,
    ) {
        let text = field.read(cx).as_str().to_owned();
        self.search.update(cx, |search, cx| search.set_text(text, cx));
    }

    fn on_coordinate_typed(
        &mut self,
        field: &Entity<EditableTextState>,
        _: &TextChanged,
        cx: &mut Context<Self>,
    ) {
        let text = field.read(cx).as_str().to_owned();
        self.add_fault = None;
        self.catalog
            .update(cx, |catalog, cx| catalog.look_up(&text, cx));
        cx.notify();
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
    /// Opens one declaration, resolving its coordinate from the index.
    pub(super) fn open_symbol(&mut self, symbol: SymbolKey, target: Target, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.open_symbol(symbol, target, cx));
        if let Some(coordinate) = self.index.read(cx).coordinate_of(symbol).map(ToOwned::to_owned) {
            self.remember(Recent::Declaration { coordinate }, cx);
        }
        self.dismiss_sheet(cx);
    }

    /// Opens one registry package's page.
    pub(crate) fn open_package(&mut self, coordinate: String, target: Target, cx: &mut Context<Self>) {
        self.remember(Recent::Package { coordinate: coordinate.clone() }, cx);
        self.document.update(cx, |document, cx| {
            document.open(Subject::Package { coordinate }, target, cx);
        });
        self.dismiss_sheet(cx);
    }

    /// Records one subject in the reader's recents.
    fn remember(&mut self, entry: Recent, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.remember(entry, cx));
    }

    /// Opens one remembered subject again.
    pub(super) fn open_recent(&mut self, entry: &Recent, cx: &mut Context<Self>) {
        match entry {
            Recent::Project { coordinate } => self.open_project(coordinate.clone(), cx),
            Recent::Package { coordinate } => self.open_package(coordinate.clone(), Target::Child, cx),
            Recent::Declaration { coordinate } => {
                match self.index.read(cx).symbol_for(coordinate) {
                    Some(symbol) => self.open_symbol(symbol, Target::Child, cx),
                    None => self.set_field(Identity::parse(coordinate).name().to_owned(), cx),
                }
            }
        }
    }

    /// Pins or unpins one project or package for the browse page.
    pub(super) fn toggle_pin(&mut self, coordinate: &str, cx: &mut Context<Self>) {
        let pinned = self.shell.update(cx, |shell, cx| shell.toggle_pin(coordinate, cx));
        let notice = if pinned { "Pinned" } else { "Unpinned" };
        self.shell.update(cx, |shell, cx| shell.notify(notice, cx));
    }

    /// Moves focus into the omnibar field and drops the sheet.
    pub(super) fn focus_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, SearchStore::open);
    }

    /// Opens the browse page.
    pub(crate) fn open_home(&mut self, cx: &mut Context<Self>) {
        self.document.update(cx, DocumentStore::open_home);
        self.dismiss_sheet(cx);
    }

    /// Opens one project's page.
    pub(crate) fn open_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.remember(Recent::Project { coordinate: coordinate.clone() }, cx);
        self.document.update(cx, |document, cx| {
            document.open(Subject::Project { coordinate }, Target::Child, cx);
        });
        self.dismiss_sheet(cx);
    }

    /// Submits an index intent for one project path or package coordinate.
    pub(super) fn index_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.jobs.update(cx, |jobs, cx| {
            jobs.submit(JobKind::Index, coordinate, cx);
        });
    }

    /// Submits a remove intent for one project and closes its pages.
    pub(super) fn remove_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.close_project(&coordinate, cx));
        self.shell
            .update(cx, |shell, cx| shell.forget_project(&coordinate, cx));
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

    /// Closes the omnibar sheet when the reader clicks the page behind it.
    pub(super) fn dismiss_sheet_from_scrim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_field(String::new(), cx);
        self.dismiss_sheet(cx);
        window.focus(&self.focus, cx);
    }

    fn dismiss_sheet(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, SearchStore::dismiss);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Reader, cx));
    }

    /// Returns the identity of whatever the reader is showing.
    pub(super) fn active_identity(&self, cx: &Context<Self>) -> Option<Identity> {
        self.document
            .read(cx)
            .tab()
            .and_then(|tab| tab.subject().and_then(Subject::identity))
    }

    /// Follows one crumb of the trail above the page.
    ///
    /// A project crumb opens that project. A file crumb cannot open a page —
    /// the engine publishes declarations, not files — so it scopes the omnibar
    /// to that file's project and searches for the file, which is the closest
    /// honest thing to "show me what is in here".
    pub(super) fn open_crumb(&mut self, destination: &super::page::Destination, cx: &mut Context<Self>) {
        match destination {
            super::page::Destination::Project(root) => self.open_project(root.clone(), cx),
            super::page::Destination::File(path) => {
                let root = self.active_identity(cx).and_then(|identity| {
                    identity.project().map(|project| project.root().to_owned())
                });
                let module = root.and_then(|root| self.module_symbol(&root, path, cx));
                if let Some(symbol) = module {
                    self.open_symbol(symbol, Target::Child, cx);
                } else {
                    self.set_field(path.clone(), cx);
                    self.search.update(cx, SearchStore::open);
                }
            }
            super::page::Destination::Symbol(coordinate) => {
                let symbol = self.index.read(cx).symbol_for(coordinate);
                if let Some(symbol) = symbol {
                    self.open_symbol(symbol, Target::Child, cx);
                }
            }
            super::page::Destination::Key(symbol) => self.open_symbol(*symbol, Target::Child, cx),
        }
    }

    /// Returns the symbol of the module one source file is, when published.
    pub(super) fn module_symbol(&self, root: &str, path: &str, cx: &Context<Self>) -> Option<SymbolKey> {
        self.index
            .read(cx)
            .project(root)?
            .modules()
            .find(|module| module.path() == Some(path))
            .map(crate::store::index::Entry::symbol)
    }

    /// Opens one already-resolved subject, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn open_subject(
        &mut self,
        subject: Subject,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        self.document
            .update(cx, |document, cx| document.open(subject, target, cx));
    }

    /// Raises a hover card at an explicit anchor, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_hover(
        &mut self,
        symbol: SymbolKey,
        coordinate: &str,
        anchor: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let _ = coordinate;
        self.document.update(cx, |document, cx| {
            document.hover_over(symbol, anchor, cx);
        });
    }

    /// Opens the settings sheet on its agents page, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_settings(&mut self, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| {
            shell.show_settings_page(crate::store::shell::SettingsPage::Agents, cx);
        });
    }

    /// Returns the merged shelf, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_shelf(&self, cx: &Context<Self>) -> backend_present::Shelf {
        self.jobs.read(cx).merge(self.engine.read(cx).shelf())
    }

    /// Returns the live view root, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_root(&self, cx: &Context<Self>) -> ViewRoot {
        self.engine.read(cx).root().clone()
    }

    /// Submits whatever is in the add field, for the preview scenes.
    #[cfg(feature = "preview")]
    pub(crate) fn submit_preview_coordinate(&mut self, cx: &mut Context<Self>) {
        self.submit_add(cx);
    }

    /// Returns whether one page section is folded away.
    pub(super) fn is_folded(&self, key: &str) -> bool {
        self.folded.iter().any(|held| held == key)
    }

    /// Folds or unfolds one page section.
    pub(super) fn toggle_fold(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.is_folded(key) {
            self.folded.retain(|held| held != key);
        } else {
            self.folded.push(key.to_owned());
        }
        cx.notify();
    }

    /// Returns whether one page section is showing everything past its budget.
    pub(super) fn is_unfurled(&self, key: &str) -> bool {
        self.unfurled.iter().any(|held| held == key)
    }

    /// Shows everything one page section was holding back.
    pub(super) fn unfurl(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.is_unfurled(key) {
            self.unfurled.push(key.to_owned());
        }
        cx.notify();
    }

    /// Shows or hides everything one page section was holding back.
    pub(super) fn toggle_unfurl(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.is_unfurled(key) {
            self.unfurled.retain(|held| held != key);
        } else {
            self.unfurled.push(key.to_owned());
        }
        cx.notify();
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
        #[cfg(feature = "preview")]
        self.stage_preview(window, cx);
        surface::ground(&theme)
            .id("nudox-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.paint(Paint::Text))
            .child(self.titlebar(&theme, window, cx))
            .child(self.body(&theme, cx))
            .child(self.status_bar(&theme, cx))
            .child(self.overlays(&theme, window, cx))
            .with_key_handlers(cx)
    }
}

impl Workspace {
    fn fit(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let available = self.context_available(cx);
        self.shell.update(cx, |shell, cx| {
            shell.set_context_available(available, cx);
            shell.fit_to(width, cx);
        });
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
            .when_some(self.source_sheet(theme, cx), ParentElement::child)
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
            .on_action(cx.listener(Workspace::go_home))
            .on_action(cx.listener(Workspace::open_source))
            .on_action(cx.listener(Workspace::open_editor))
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
        self.field.update(cx, EditableTextState::select_document);
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, SearchStore::open);
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.set_field(">".to_owned(), cx);
        self.focus_omnibar(&FocusOmnibar, window, cx);
    }

    fn start_add(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_add(window, cx);
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
        self.shell.update(cx, ShellStore::toggle_settings);
    }

    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, DocumentStore::back);
    }

    fn go_forward(&mut self, _: &GoForward, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, DocumentStore::forward);
    }

    fn go_home(&mut self, _: &GoHome, _: &mut Window, cx: &mut Context<Self>) {
        self.open_home(cx);
    }

    fn open_source(&mut self, _: &OpenSource, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_source(cx);
    }

    fn open_editor(&mut self, _: &OpenEditor, _: &mut Window, cx: &mut Context<Self>) {
        let site = match self.document.read(cx).tab().map(super::super::store::document::Tab::content) {
            Some(super::super::store::document::Content::Page(page)) => super::page::site_of(page),
            _ => None,
        };
        if let Some((path, line)) = site {
            self.open_in_editor(&path, line, cx);
        }
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document.update(cx, DocumentStore::close_active);
    }

    fn previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.activate_neighbour(false, cx));
    }

    fn next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.activate_neighbour(true, cx));
    }

    pub(super) fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.document
            .update(cx, |document, cx| document.activate_nth(index, cx));
    }

    fn copy_identity(&mut self, _: &CopyIdentity, _: &mut Window, cx: &mut Context<Self>) {
        let Some(identity) = self.active_identity(cx) else {
            return;
        };
        self.copy("Identity copied", identity.coordinate().as_str().to_owned(), cx);
    }

    fn copy_key(&mut self, _: &CopyKey, _: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.active_key(cx) else {
            return;
        };
        self.copy("Key copied", key, cx);
    }

    fn active_key(&self, cx: &Context<Self>) -> Option<String> {
        match self.document.read(cx).tab()?.subject()? {
            Subject::Home => None,
            Subject::Declaration { symbol, .. } => {
                Some(backend_library::encode_id(symbol.as_bytes()))
            }
            Subject::Project { coordinate } | Subject::Package { coordinate } => {
                Some(coordinate.clone())
            }
        }
    }

    /// Returns the omnibar text one command affordance would run.
    pub(super) fn palette_for(name: &str, args: &[String]) -> String {
        crate::presentation::fault::affordance_palette_text(name, args)
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
        self.document.update(cx, DocumentStore::reload);
        self.engine
            .update(cx, WorkspaceStore::refresh_health);
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
            self.shell.update(cx, ShellStore::toggle_settings);
            return;
        }
        if self.source_open {
            self.close_source(cx);
            return;
        }
        if self.adding {
            self.cancel_add(cx);
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
        self.step_selection(-page_step(), cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(page_step(), cx);
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
            let arguments = self.search.read(cx).parsed().arguments().to_owned();
            if self.run_palette(command, &arguments, window, cx) {
                return;
            }
            self.search
                .update(cx, |search, cx| search.run(command, cx));
            return;
        }
        let Some(symbol) = self
            .search
            .read(cx)
            .selected_result()
            .and_then(crate::store::search::ResultRow::symbol)
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

    pub(crate) fn set_field(&mut self, text: String, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| {
            field.emplace(&text, cx);
            let end = field.as_str().len();
            field.move_to(end, cx);
        });
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
    }

    /// Submits the add field: a folder or pinned URL at once, a name via the catalog.
    pub(super) fn submit_add(&mut self, cx: &mut Context<Self>) {
        let text = self.coordinate.read(cx).as_str().trim().to_owned();
        let ask = Ask::parse(&text);
        if let Ask::Named { name, version } = &ask {
            if name.is_empty() {
                self.add_fault = Some("Type a package name, or choose a folder.".to_owned());
                cx.notify();
                return;
            }
            let ecosystem = self.add_ecosystem.as_str().to_owned();
            let (name, version) = (name.clone(), version.clone());
            self.add_fault = None;
            self.catalog.update(cx, |catalog, cx| {
                catalog.resolve_named(&ecosystem, &name, version.as_deref(), cx);
            });
            cx.notify();
            return;
        }
        match super::library::validate(&text) {
            Err(message) => {
                self.add_fault = Some(message);
                cx.notify();
            }
            Ok(coordinate) => {
                self.add_fault = None;
                self.adding = false;
                self.coordinate.update(cx, |field, cx| field.emplace("", cx));
                self.catalog.update(cx, CatalogStore::clear);
                self.index_project(coordinate, cx);
            }
        }
    }
}

/// Every store one window owns besides the engine's own feed.
struct Stores {
    search: Entity<SearchStore>,
    document: Entity<DocumentStore>,
    index: Entity<IndexStore>,
    jobs: Entity<JobsStore>,
    catalog: Entity<CatalogStore>,
    registry: Entity<RegistryStore>,
    shell: Entity<ShellStore>,
    field: Entity<EditableTextState>,
    coordinate: Entity<EditableTextState>,
}

/// Every entity the window observes, named so wiring stays one call.
struct Wiring<'a> {
    engine: &'a Entity<WorkspaceStore>,
    search: &'a Entity<SearchStore>,
    document: &'a Entity<DocumentStore>,
    index: &'a Entity<IndexStore>,
    jobs: &'a Entity<JobsStore>,
    catalog: &'a Entity<CatalogStore>,
    registry: &'a Entity<RegistryStore>,
    shell: &'a Entity<ShellStore>,
    field: &'a Entity<EditableTextState>,
    coordinate: &'a Entity<EditableTextState>,
}

/// Returns how many rows one page-up or page-down step moves.
fn page_step() -> isize {
    isize::try_from(crate::store::search::PAGE).unwrap_or(8)
}
