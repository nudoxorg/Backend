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
    GoForward, GoHome, GraphNext, GraphPrevious, GrowInterface, MoveDown, MoveUp, NextTab,
    OpenEditor, OpenPalette, FindInSource, OpenSettings, OpenSource, PageDown, PageUp,
    PreviousSourceMatch, PreviousTab, Reload, ResetInterface, SelectFirst, SelectLast,
    ShrinkInterface, Tab1, Tab2, Tab3, Tab4, Tab5, Tab6, Tab7, Tab8, Tab9, ToggleAppearance,
    ToggleContext, ToggleLibrary, ToggleMotion, WINDOW_CONTEXT, tab_index,
};
use crate::host::lease::HostMode;
use crate::reducer::model::Model;
use crate::store::catalog::{Ask, CatalogStore};
use crate::store::document::{
    DocumentStore, ReaderEntry, ReaderEntryFault, ReaderRoute, Subject, Target,
};
use crate::store::events::{CatalogEvent, DocumentEvent};
use crate::store::index::IndexStore;
use crate::store::jobs::{JobKind, JobsStore};
use crate::store::marks::Recent;
use crate::store::prefs::Preferences;
use crate::store::registry::RegistryStore;
use crate::store::search::{Mode, SearchStore};
use crate::store::service::Endpoint;
use crate::store::shell::{Focus, ShellStore, Side, Transient, TransientStack};
use crate::store::workspace::WorkspaceStore;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::transport::unix::UnixSubscriptionTransport;
use crate::ui::components::ActionFrames;
use crate::ui::surface;
#[cfg(feature = "visual-harness")]
use backend_gui_harness::{
    ActionDescriptor, FocusState, GuiState, InputStep, OverlayState, PageState,
};
use backend_library::{SymbolKey, ViewRoot};
use backend_present::{Identity, IdentityKey, Source};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tree::TreeState;
#[cfg(feature = "visual-harness")]
use serde::Serialize;
use std::path::PathBuf;
use std::rc::Rc;

/// Product-owned semantic facts emitted by the visual harness.
///
/// This is intentionally derived from the same stores that render the window;
/// a screenshot cannot pass a scenario assertion merely by carrying a matching
/// state id in its manifest.
#[cfg(feature = "visual-harness")]
#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceSemanticProbe {
    /// Coarse content page currently active.
    pub page: String,
    /// Topmost transient surface currently active.
    pub overlay: String,
    /// Shell focus owner.
    pub focus: String,
    /// Active settings page, if settings is open.
    pub settings_page: Option<String>,
    /// Number of tabs in the document tree.
    pub tab_count: usize,
    /// Whether the active tab is waiting for a live page.
    pub pending: bool,
    /// Short admitted projection revision.
    pub data_revision: String,
    /// Locale selected by the in-process harness adapter.
    pub locale: String,
    /// Text direction selected by the deterministic environment.
    pub text_direction: String,
    /// Active design-system appearance.
    pub theme: String,
    /// Motion preference read from the shell store.
    pub reduced_motion: bool,
    /// Stable focus identity used by keyboard and focus-trap assertions.
    pub focus_id: String,
    /// Route identity observed from the product content tree.
    pub route: String,
    /// Action tree installed by the rendered desktop adapter.
    pub actions: Vec<ActionDescriptor>,
    /// Animation phase that produced this semantic observation.
    pub animation_phase: String,
    /// Virtual animation time at this observation.
    pub virtual_time_ms: u64,
    /// Input event that most recently preceded this frame.
    pub input_index: Option<usize>,
    /// Exact production input event, when a frame followed one.
    pub input: Option<InputStep>,
}

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
    pub(super) field: Entity<InputState>,
    pub(super) coordinate: Entity<InputState>,
    /// Dedicated to the source sheet so in-page find never opens the omnibar.
    pub(super) source_field: Entity<InputState>,
    pending_field: Option<String>,
    pending_coordinate: Option<String>,
    pub(super) adding: bool,
    pub(super) add_fault: Option<String>,
    pub(super) add_ecosystem: backend_library::RegistryEcosystem,
    pub(super) transients: TransientStack,
    pub(super) source_open: bool,
    /// Focus target to restore when the source reader sheet is dismissed.
    pub(super) source_restore_focus: Option<FocusHandle>,
    pub(super) source_cache: Option<super::source::SourceCache>,
    /// Graph source navigation waits for the internal document reader to
    /// settle before opening its captured-source sheet.
    pending_graph_source: Option<SymbolKey>,
    pub(super) source_query_cache: Option<super::source::SourceQueryCache>,
    pub(super) source_scroll: gpui::UniformListScrollHandle,
    pub(super) source_active_match: usize,
    pub(super) context_cache: Option<super::context::ContextCache>,
    folded: Vec<String>,
    unfurled: Vec<String>,
    capture_time: Option<std::time::Duration>,
    #[cfg(feature = "preview")]
    scene: Option<crate::preview::Scene>,
    pub(super) outline_scroll: gpui::UniformListScrollHandle,
    pub(super) revealed: Option<SymbolKey>,
    pub(super) sheet_scroll: gpui::ScrollHandle,
    pub(super) tab_tree_state: Entity<TreeState>,
    pub(super) revealed_row: Option<usize>,
    /// Revision-pinned graph projection and navigation state for the reader.
    pub(super) graph: crate::graph::ExplorerState,
    focus: FocusHandle,
    pub(super) library_focus: FocusHandle,
    pub(super) source_focus: FocusHandle,
    pub(super) settings_focus: FocusHandle,
    /// Held, not read: a `Subscription` unsubscribes the moment it is dropped.
    _subscriptions: Vec<Subscription>,
    action_frames: Rc<ActionFrames>,
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
    pub(crate) fn new(opened: Bootstrap, window: &mut Window, cx: &mut Context<Self>) -> Self {
        #[cfg(feature = "preview")]
        let capture_time = crate::preview::capture_time();
        #[cfg(not(feature = "preview"))]
        let capture_time = None;
        Self::new_with_capture(opened, capture_time, window, cx)
    }

    /// Builds a window with an optional deterministic animation timestamp.
    ///
    /// This seam is part of the production workspace rather than the preview
    /// module, so an offscreen or visible harness can inject exact frame times
    /// without relying on an environment variable or the wall clock.
    pub(crate) fn new_with_capture(
        opened: Bootstrap,
        capture_time: Option<std::time::Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let stores = Self::stores(&endpoint, &engine, data, prefs, window, cx);
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
                source_field: &stores.source_field,
            },
            cx,
        );
        Self::assemble(engine, stores, subscriptions, capture_time, cx)
    }

    /// Returns the window with every store installed and nothing yet open.
    fn assemble(
        engine: Entity<WorkspaceStore>,
        stores: Stores,
        subscriptions: Vec<Subscription>,
        capture_time: Option<std::time::Duration>,
        cx: &mut Context<Self>,
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
            source_field,
            pending_field: None,
            pending_coordinate: None,
        } = stores;
        if capture_time.is_some() {
            shell.update(cx, |shell, cx| shell.set_capture_mode(true, cx));
        }
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
            source_field,
            adding: false,
            add_fault: None,
            add_ecosystem: backend_library::RegistryEcosystem::Cargo,
            transients: TransientStack::default(),
            source_open: false,
            source_restore_focus: None,
            source_cache: None,
            pending_graph_source: None,
            source_query_cache: None,
            source_scroll: gpui::UniformListScrollHandle::new(),
            source_active_match: 0,
            context_cache: None,
            folded: Vec::new(),
            unfurled: Vec::new(),
            capture_time,
            #[cfg(feature = "preview")]
            scene: crate::preview::Scene::from_env(),
            outline_scroll: gpui::UniformListScrollHandle::new(),
            revealed: None,
            sheet_scroll: gpui::ScrollHandle::new(),
            tab_tree_state: cx.new(|cx| TreeState::new(cx)),
            revealed_row: None,
            graph: crate::graph::ExplorerState::new(),
            focus: cx.focus_handle(),
            library_focus: cx.focus_handle(),
            source_focus: cx.focus_handle(),
            settings_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
            action_frames: Rc::new(ActionFrames::default()),
        }
    }

    /// Builds every store that hangs off one endpoint and the engine's feed.
    fn stores(
        endpoint: &Endpoint,
        engine: &Entity<WorkspaceStore>,
        data: PathBuf,
        prefs: Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stores {
        let index = cx.new(|cx| IndexStore::new(engine, cx));
        Stores {
            search: cx.new(|_| SearchStore::new(endpoint.clone())),
            document: cx
                .new(|_| DocumentStore::new(endpoint.clone(), engine.clone(), index.clone())),
            index,
            jobs: cx.new(|_| JobsStore::new(endpoint.clone())),
            catalog: cx.new(|_| CatalogStore::new(endpoint.clone())),
            registry: cx.new(|cx| RegistryStore::new(endpoint.clone(), window, cx)),
            shell: cx.new(|_| ShellStore::new(data, prefs)),
            field: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search packages, files, or commands")
            }),
            coordinate: cx
                .new(|cx| InputState::new(window, cx).placeholder("name, or name@version")),
            source_field: cx.new(|cx| InputState::new(window, cx).placeholder("Find in source…")),
        }
    }

    fn wire(parts: &Wiring<'_>, cx: &mut Context<Self>) -> Vec<Subscription> {
        vec![
            cx.observe(parts.engine, |this, store, cx| this.on_engine(&store, cx)),
            cx.observe(parts.search, |_, _, cx| cx.notify()),
            cx.observe(parts.document, |_, _, cx| cx.notify()),
            cx.subscribe(parts.document, |this, _, event, cx| {
                this.on_document(*event, cx);
            }),
            cx.observe(parts.index, |_, _, cx| cx.notify()),
            cx.observe(parts.jobs, |_, _, cx| cx.notify()),
            cx.observe(parts.catalog, |_, _, cx| cx.notify()),
            cx.subscribe(parts.catalog, |this, catalog, event, cx| {
                this.on_catalog(&catalog, *event, cx);
            }),
            cx.observe(parts.registry, |_, _, cx| cx.notify()),
            cx.observe(parts.shell, |_, _, cx| cx.notify()),
            cx.subscribe(parts.field, |this, field, event: &InputEvent, cx| {
                this.on_typed(&field, event, cx)
            }),
            cx.subscribe(parts.coordinate, |this, field, event: &InputEvent, cx| {
                this.on_coordinate_typed(&field, event, cx);
            }),
            cx.subscribe(parts.source_field, |_, _, _: &InputEvent, cx| cx.notify()),
        ]
    }

    fn on_engine(&mut self, store: &Entity<WorkspaceStore>, cx: &mut Context<Self>) {
        let published = store.read(cx).shelf().clone();
        self.jobs
            .update(cx, |jobs, cx| jobs.reconcile(&published, cx));
        cx.notify();
    }

    fn on_document(&mut self, event: DocumentEvent, cx: &mut Context<Self>) {
        if event == DocumentEvent::Navigated {
            self.finish_graph_source_navigation(cx);
        }
        cx.notify();
    }

    /// Opens whatever scene the preview build was asked for, once it can.
    ///
    /// A scene is applied on the first render where the live root can actually
    /// satisfy it, because a scene applied before the first revision arrived
    /// would only ever photograph the loading state.
    #[cfg(feature = "preview")]
    fn stage_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(scene) = self.scene {
            if crate::preview::stage(self, scene, window, cx) {
                eprintln!("backend-desktop: opened in the {} scene", scene.name());
                self.scene = None;
            }
        }
    }

    /// Advances the production animation clock when a harness supplied a
    /// deterministic timestamp. Live windows leave this as a no-op.
    fn advance_capture_frame(&mut self, cx: &mut Context<Self>) {
        if let Some(timestamp) = self.capture_time {
            self.shell.update(cx, |shell, cx| {
                shell.advance_capture(timestamp, cx);
            });
        }
    }

    /// Replaces the deterministic timestamp used by subsequent renders.
    ///
    /// Passing `None` returns ownership to the normal wall-clock animation
    /// task. The shell clock is monotonic, making repeated renders of one
    /// capture frame idempotent.
    pub(crate) fn set_capture_time(
        &mut self,
        timestamp: Option<std::time::Duration>,
        cx: &mut Context<Self>,
    ) {
        self.capture_time = timestamp;
        self.shell.update(cx, |shell, cx| {
            shell.set_capture_mode(timestamp.is_some(), cx);
        });
        cx.notify();
    }

    /// Acts on a resolved add: index what was decided, or say why nothing was.
    fn on_catalog(
        &mut self,
        catalog: &Entity<CatalogStore>,
        event: CatalogEvent,
        cx: &mut Context<Self>,
    ) {
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
                self.remove_transient(Transient::Add, cx);
                self.pending_coordinate = Some(String::new());
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

    fn on_typed(&mut self, field: &Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let text = field.read(cx).value().to_string();
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
        self.sync_search_transient(cx);
    }

    fn on_coordinate_typed(
        &mut self,
        field: &Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let text = field.read(cx).value().to_string();
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
        .with_action_frames(self.action_frames.clone())
    }

    /// Returns the semantic controls from the most recently rendered window
    /// frame. Screenshot and journey harnesses use this instead of rebuilding
    /// a parallel manifest, so the tree always describes the same component
    /// construction path that produced the pixels.
    pub(crate) fn action_tree(
        &self,
        window: &gpui::Window,
        cx: &Context<Self>,
    ) -> crate::ui::components::ActionTree {
        self.theme(cx).action_tree(window)
    }

    /// Clears a completed window frame after a harness scenario has consumed
    /// its semantic snapshot.
    pub(crate) fn reset_action_tree(&self, window: &gpui::Window) {
        self.action_frames.reset(window);
    }

    /// Keeps Escape ownership in step with the typed omnibar route.
    pub(super) fn sync_search_transient(&mut self, cx: &mut Context<Self>) {
        if !self.search.read(cx).is_open() {
            self.remove_transient(Transient::Search, cx);
            self.remove_transient(Transient::Palette, cx);
            return;
        }
        let surface = if self.search.read(cx).parsed().mode() == &Mode::Palette {
            Transient::Palette
        } else {
            Transient::Search
        };
        let restore = self
            .transients
            .restore_for(Transient::Search)
            .or_else(|| self.transients.restore_for(Transient::Palette))
            .unwrap_or_else(|| self.shell.read(cx).focus());
        self.transients.replace_omnibar(surface, restore);
    }

    /// Removes a transient and restores its semantic route when it was topmost.
    pub(super) fn remove_transient(
        &mut self,
        surface: Transient,
        cx: &mut Context<Self>,
    ) -> Option<Focus> {
        let was_top = self.transients.top() == Some(surface);
        let frame = if was_top {
            self.transients.pop_if(surface)
        } else {
            self.transients.remove(surface)
        }?;
        if was_top {
            self.shell
                .update(cx, |shell, cx| shell.focus_on(frame.restore, cx));
        }
        Some(frame.restore)
    }

    /// Removes the active omnibar route and restores the route behind it.
    pub(super) fn remove_omnibar(&mut self, cx: &mut Context<Self>) -> Option<Focus> {
        let restore = self
            .transients
            .restore_for(Transient::Search)
            .or_else(|| self.transients.restore_for(Transient::Palette));
        let was_top = matches!(
            self.transients.top(),
            Some(Transient::Search | Transient::Palette)
        );
        self.transients.remove(Transient::Search);
        self.transients.remove(Transient::Palette);
        if was_top {
            if let Some(restore) = restore {
                self.shell
                    .update(cx, |shell, cx| shell.focus_on(restore, cx));
            }
        }
        restore
    }

    /// Applies one visual harness scenario to the real stores.
    ///
    /// Every successful branch calls the same navigation methods as a user
    /// action. Conditions that the admitted live projection cannot satisfy are
    /// rejected so the harness never turns a missing capability into fixture
    /// pixels.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn apply_harness_state(
        &mut self,
        state: &GuiState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match state.page {
            Some(PageState::Browse) | None => self.open_home(cx),
            Some(PageState::Project) => {
                let coordinate = self
                    .engine
                    .read(cx)
                    .shelf()
                    .entries()
                    .iter()
                    .find_map(|entry| {
                        entry
                            .identity()
                            .project()
                            .map(|project| project.root().to_owned())
                    })
                    .ok_or_else(|| {
                        "live admitted projection has no project page anchor".to_owned()
                    })?;
                self.open_project(coordinate, cx);
            }
            Some(PageState::Package) => {
                let coordinate = self
                    .engine
                    .read(cx)
                    .shelf()
                    .entries()
                    .iter()
                    .map(|entry| entry.identity().coordinate().as_str().to_owned())
                    .find(|coordinate| coordinate.starts_with("pkg:"))
                    .ok_or_else(|| {
                        "live admitted projection has no package page anchor".to_owned()
                    })?;
                self.open_package(coordinate, Target::Here, cx);
            }
            Some(PageState::Declaration) | Some(PageState::Source) => {
                let symbol = self
                    .index
                    .read(cx)
                    .first_entry()
                    .map(|entry| entry.symbol())
                    .ok_or_else(|| {
                        "live admitted projection has no declaration page anchor".to_owned()
                    })?;
                self.open_symbol(symbol, Target::Here, cx);
            }
            Some(unsupported) => {
                return Err(format!(
                    "live Workspace has no registered route for page {}",
                    unsupported.as_str()
                ));
            }
        }

        match state.overlay {
            None => {}
            Some(OverlayState::Omnibar) => self.focus_field(window, cx),
            Some(OverlayState::Palette) => {
                self.open_palette(&super::actions::OpenPalette, window, cx)
            }
            Some(OverlayState::SettingsAppearance) => {
                self.show_harness_settings(crate::store::shell::SettingsPage::Appearance, cx)
            }
            Some(OverlayState::SettingsEditor) => {
                self.show_harness_settings(crate::store::shell::SettingsPage::Editor, cx)
            }
            Some(OverlayState::SettingsAgents) => {
                self.show_harness_settings(crate::store::shell::SettingsPage::Agents, cx)
            }
            Some(OverlayState::SettingsDiagnostics) => {
                self.show_harness_settings(crate::store::shell::SettingsPage::Diagnostics, cx)
            }
            Some(OverlayState::SettingsLegend) => {
                self.show_harness_settings(crate::store::shell::SettingsPage::Legend, cx)
            }
            Some(OverlayState::SettingsIndex | OverlayState::SettingsRegistry) => {
                return Err("live Workspace has no registered indexing/registry settings route".to_owned());
            }
            Some(OverlayState::Fault | OverlayState::Notice) => {
                return Err("failure and notice overlays require an admitted live condition".to_owned());
            }
            Some(unsupported) => {
                return Err(format!(
                    "live adapter cannot realize overlay {} without an admitted live condition",
                    unsupported.as_str()
                ));
            }
        }

        if state.overlay.is_none() && state.page == Some(PageState::Source) {
            self.toggle_source(window, cx);
        }

        match state.focus {
            FocusState::Omnibar => self.focus_field(window, cx),
            FocusState::Library => self
                .shell
                .update(cx, |shell, cx| shell.focus_on(Focus::Library, cx)),
            FocusState::Reader => {
                self.shell
                    .update(cx, |shell, cx| shell.focus_on(Focus::Reader, cx));
                window.focus(&self.focus, cx);
            }
        }
        if state.reduced_motion {
            self.shell
                .update(cx, |shell, cx| shell.set_reduced_motion(true, cx));
        }
        cx.notify();
        Ok(())
    }

    #[cfg(feature = "visual-harness")]
    fn show_harness_settings(
        &mut self,
        page: crate::store::shell::SettingsPage,
        cx: &mut Context<Self>,
    ) {
        self.shell
            .update(cx, |shell, cx| shell.show_settings_page(page, cx));
    }

    /// Applies an appearance through the same preference owner used by the
    /// visible settings sheet.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_set_appearance(
        &mut self,
        appearance: crate::theme::palette::Appearance,
        cx: &mut Context<Self>,
    ) {
        self.shell
            .update(cx, |shell, cx| shell.set_appearance(appearance, cx));
    }

    /// Reads the actual semantic/accessibility-facing state used by scenario assertions.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_semantic_probe(&self, cx: &Context<Self>) -> WorkspaceSemanticProbe {
        let document = self.document.read(cx);
        let active_pending = document.tab().is_some_and(|tab| tab.pending().is_some());
        let (page, pending) = match document.tab().map(|tab| tab.content()) {
            Some(crate::store::document::Content::Home) => ("browse", false),
            Some(crate::store::document::Content::Project { .. }) => ("project", false),
            Some(crate::store::document::Content::Package { .. }) => ("package", false),
            Some(crate::store::document::Content::Page(_)) => ("declaration", false),
            Some(crate::store::document::Content::Faulted(_)) => ("fault", false),
            Some(crate::store::document::Content::Blank) if active_pending => ("declaration", true),
            Some(crate::store::document::Content::Blank) | None => ("blank", false),
        };
        let shell = self.shell.read(cx);
        let overlay = if shell.settings_open() {
            "settings"
        } else if self.source_open {
            "source"
        } else if self.search.read(cx).is_open() {
            if self.field.read(cx).value().starts_with('>') {
                "palette"
            } else {
                "omnibar"
            }
        } else if self.adding {
            "add"
        } else if shell.notice().is_some() {
            "notice"
        } else {
            "none"
        };
        let focus = format!("{:?}", shell.focus()).to_ascii_lowercase();
        let focus_id = match focus.as_str() {
            "omnibar" => "input.omnibar",
            "library" => "panel.library",
            _ => "reader.workspace",
        };
        let frame = cx.try_global::<crate::harness::HarnessFrame>();
        let input = cx.try_global::<crate::harness::HarnessInput>();
        let actions = cx
            .try_global::<crate::harness::HarnessActions>()
            .map_or_else(Vec::new, |actions| actions.0.clone());
        WorkspaceSemanticProbe {
            page: page.to_owned(),
            overlay: overlay.to_owned(),
            focus,
            focus_id: focus_id.to_owned(),
            route: format!("{page}:{overlay}"),
            actions,
            animation_phase: frame
                .map_or_else(|| "unknown".to_owned(), |frame| frame.label.clone()),
            virtual_time_ms: frame.map_or(0, |frame| frame.time_ms),
            input_index: input.map(|input| input.index),
            input: input.map(|input| input.step.clone()),
            settings_page: shell
                .settings_open()
                .then(|| shell.settings_page().label().to_owned()),
            tab_count: document.tree().len(),
            pending: active_pending || pending,
            data_revision: self.engine.read(cx).revision().to_owned(),
            locale: cx
                .try_global::<crate::harness::HarnessLocale>()
                .map_or_else(|| "undetermined".to_owned(), |locale| locale.0.clone()),
            text_direction: cx
                .try_global::<crate::harness::HarnessDirection>()
                .map_or_else(|| "ltr".to_owned(), |direction| direction.0.clone()),
            theme: crate::theme::theme(cx).appearance().name().to_owned(),
            reduced_motion: shell.reduced_motion(),
        }
    }

    /// Verifies that the real stores reached the requested semantic endpoint.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn assert_harness_state(
        &self,
        requested: &GuiState,
        cx: &Context<Self>,
    ) -> Result<WorkspaceSemanticProbe, String> {
        let probe = self.harness_semantic_probe(cx);
        let expected_page = match requested.page {
            None | Some(PageState::Browse) => "browse",
            Some(PageState::Project) => "project",
            Some(PageState::Package) => "package",
            Some(PageState::Declaration) | Some(PageState::Source) => "declaration",
            Some(unsupported) => {
                return Err(format!(
                    "page {} has no registered semantic probe",
                    unsupported.as_str()
                ));
            }
        };
        if probe.page != expected_page {
            return Err(format!(
                "requested page {expected_page}, live Workspace reached {}",
                probe.page
            ));
        }
        let expected_overlay = match requested.overlay {
            None => {
                if matches!(requested.page, Some(PageState::Source)) {
                    "source"
                } else {
                    "none"
                }
            }
            Some(OverlayState::Omnibar) => "omnibar",
            Some(OverlayState::Palette) => "palette",
            Some(OverlayState::SettingsAppearance)
            | Some(OverlayState::SettingsEditor)
            | Some(OverlayState::SettingsAgents)
            | Some(OverlayState::SettingsDiagnostics)
            | Some(OverlayState::SettingsLegend) => "settings",
            Some(OverlayState::Fault | OverlayState::Notice) => {
                return Err("failure and notice overlays require an admitted live condition".to_owned());
            }
            Some(OverlayState::SettingsIndex | OverlayState::SettingsRegistry) => {
                return Err("indexing/registry settings routes are not registered".to_owned());
            }
            Some(unsupported) => {
                return Err(format!(
                    "overlay {} has no admitted live semantic condition",
                    unsupported.as_str()
                ));
            }
        };
        if probe.overlay != expected_overlay {
            return Err(format!(
                "requested overlay {expected_overlay}, live Workspace reached {}",
                probe.overlay
            ));
        }
        let expected_focus = match requested.focus {
            FocusState::Omnibar => "omnibar",
            FocusState::Library => "library",
            FocusState::Reader => "reader",
        };
        if probe.focus != expected_focus {
            return Err(format!(
                "requested focus {expected_focus}, live Workspace reached {}",
                probe.focus
            ));
        }
        if probe.reduced_motion != requested.reduced_motion {
            return Err(format!(
                "requested reduced_motion={}, live Workspace reports {}",
                requested.reduced_motion, probe.reduced_motion
            ));
        }
        if probe.theme != requested.theme.as_str() {
            return Err(format!(
                "requested theme={}, live Workspace reports {}",
                requested.theme.as_str(),
                probe.theme
            ));
        }
        Ok(probe)
    }

    /// Drives the same marked-text callback used by the native IME path.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_ime_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.field.update(cx, |field, cx| {
            field.replace_and_mark_text_in_range(
                None,
                text,
                Some(0..text.encode_utf16().count()),
                window,
                cx,
            );
        });
    }

    /// Commits marked text using the same GPUI input handler as a platform
    /// IME commit event.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_ime_commit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.field.update(cx, |field, cx| {
            field.replace_text_in_range(None, text, window, cx);
            field.unmark_text(window, cx);
        });
    }

    /// Cancels marked text using the same GPUI input handler as a platform IME
    /// cancellation event.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_ime_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.field
            .update(cx, |field, cx| field.unmark_text(window, cx));
    }

    /// Cancels the live subscription task before the harness tears down the
    /// headless window. This keeps embedded local-service connection workers
    /// from outliving the production workspace during matrix capture.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_stop_live_feed(&mut self, cx: &mut Context<Self>) {
        self.engine
            .update(cx, |engine, _cx| engine.harness_stop_live_feed());
    }

    /// Restores the visible workspace focus owner before keyboard journeys.
    #[cfg(feature = "visual-harness")]
    pub(crate) fn harness_focus_window(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }
}

/// Navigation.
impl Workspace {
    /// Navigates through the internal DocumentStore and opens the source
    /// reader after the captured page arrives. The external editor remains a
    /// separate, explicit action in the graph inspector.
    pub(super) fn open_graph_source(&mut self, symbol: SymbolKey, cx: &mut Context<Self>) {
        self.pending_graph_source = Some(symbol);
        self.source_open = false;
        self.source_cache = None;
        self.open_symbol(symbol, Target::Here, cx);
        self.finish_graph_source_navigation(cx);
    }

    fn finish_graph_source_navigation(&mut self, cx: &mut Context<Self>) {
        let Some(symbol) = self.pending_graph_source else {
            return;
        };
        let page = self.document.read(cx).tab().and_then(|tab| match tab.content() {
            super::super::store::document::Content::Page(page)
                if page.identity().key() == IdentityKey::Symbol(symbol) => Some(page.clone()),
            _ => None,
        });
        let Some(page) = page else {
            return;
        };
        self.pending_graph_source = None;
        if matches!(page.source(), Source::Captured { .. }) {
            self.source_open = true;
            self.source_cache = None;
        }
    }

    /// Opens one declaration, resolving its coordinate from the index.
    pub(super) fn open_symbol(
        &mut self,
        symbol: SymbolKey,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        self.document
            .update(cx, |document, cx| document.open_symbol(symbol, target, cx));
        if let Some(coordinate) = self
            .index
            .read(cx)
            .coordinate_of(symbol)
            .map(ToOwned::to_owned)
        {
            self.remember(Recent::Declaration { coordinate }, cx);
        }
        self.dismiss_sheet(cx);
    }

    /// Opens one registry package's page.
    pub(crate) fn open_package(
        &mut self,
        coordinate: String,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        self.remember(
            Recent::Package {
                coordinate: coordinate.clone(),
            },
            cx,
        );
        self.document.update(cx, |document, cx| {
            document.open(Subject::Package { coordinate }, target, cx);
        });
        self.dismiss_sheet(cx);
    }

    /// Completes a typed package-to-reader handoff without inventing a page.
    /// The document store validates its tab and index generations before it
    /// creates or activates the real package tab.
    pub(crate) fn open_reader_entry(
        &mut self,
        entry: &ReaderEntry,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<ReaderRoute, ReaderEntryFault> {
        let route = self.document.update(cx, |document, cx| {
            document.open_reader_entry(entry, target, cx)
        });
        if route.is_ok() {
            self.remember(
                Recent::Package {
                    coordinate: entry.coordinate().to_owned(),
                },
                cx,
            );
            if let Ok(ReaderRoute::Search { scope }) = &route {
                // Close any previous sheet first; setting the scoped query
                // reopens the real search store and owns focus for the route.
                self.dismiss_sheet(cx);
                self.search.update(cx, |search, cx| {
                    search.set_text(format!("@{scope} "), cx);
                });
                self.shell.update(cx, |shell, cx| {
                    shell.focus_on(Focus::Omnibar, cx);
                });
                let handle = self.field.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            } else {
                if matches!(&route, Ok(ReaderRoute::Source { .. })) {
                    self.source_restore_focus = Some(self.focus.clone());
                    self.source_open = true;
                }
                self.dismiss_sheet(cx);
            }
        }
        route
    }

    /// Records one subject in the reader's recents.
    fn remember(&mut self, entry: Recent, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.remember(entry, cx));
    }

    /// Opens one remembered subject again.
    pub(super) fn open_recent(&mut self, entry: &Recent, cx: &mut Context<Self>) {
        match entry {
            Recent::Project { coordinate } => self.open_project(coordinate.clone(), cx),
            Recent::Package { coordinate } => {
                self.open_package(coordinate.clone(), Target::Child, cx)
            }
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
        let pinned = self
            .shell
            .update(cx, |shell, cx| shell.toggle_pin(coordinate, cx));
        let notice = if pinned { "Pinned" } else { "Unpinned" };
        self.shell.update(cx, |shell, cx| shell.notify(notice, cx));
    }

    /// Moves focus into the omnibar field and drops the sheet.
    pub(super) fn focus_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = self.shell.read(cx).focus();
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, SearchStore::open);
        self.transients.replace_omnibar(Transient::Search, restore);
        self.sync_search_transient(cx);
    }

    /// Opens the browse page.
    pub(crate) fn open_home(&mut self, cx: &mut Context<Self>) {
        self.document.update(cx, DocumentStore::open_home);
        self.dismiss_sheet(cx);
    }

    /// Opens one project's page.
    pub(crate) fn open_project(&mut self, coordinate: String, cx: &mut Context<Self>) {
        self.remember(
            Recent::Project {
                coordinate: coordinate.clone(),
            },
            cx,
        );
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
        self.restore_focus(window, cx);
    }

    fn dismiss_sheet(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, SearchStore::dismiss);
        self.remove_omnibar(cx);
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
    pub(super) fn open_crumb(
        &mut self,
        destination: &super::page::Destination,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn module_symbol(
        &self,
        root: &str,
        path: &str,
        cx: &Context<Self>,
    ) -> Option<SymbolKey> {
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
        let restore = self.shell.read(cx).focus_before_settings();
        self.transients
            .push_with_restore(Transient::Settings, restore);
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
        self.flush_input_values(window, cx);
        let mut theme = self.theme(cx);
        theme.begin_action_frame(window, "workspace");
        window.set_rem_size(theme.root_pixels());
        let viewport = window.viewport_size();
        self.graph
            .set_viewport_size(f32::from(viewport.width), f32::from(viewport.height));
        self.fit(window, cx);
        #[cfg(feature = "preview")]
        self.stage_preview(window, cx);
        self.advance_capture_frame(cx);
        let frame = surface::ground(&theme)
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
            .with_key_handlers(cx);
        theme.publish_action_frame(window);
        frame
    }
}

impl Workspace {
    /// Applies programmatic changes requested by actions that do not receive a
    /// live window. User edits are already owned and published by CE's
    /// `InputState`; queued writes are committed at the next frame boundary so
    /// all callers share the same IME, selection, and undo semantics.
    fn flush_input_values(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(value) = self.pending_field.take() {
            self.field
                .update(cx, |field, cx| field.set_value(value, window, cx));
        }
        if let Some(value) = self.pending_coordinate.take() {
            self.coordinate
                .update(cx, |field, cx| field.set_value(value, window, cx));
        }
    }

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
            .on_action(cx.listener(Workspace::find_in_source))
            .on_action(cx.listener(Workspace::previous_source_match))
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
            .on_action(cx.listener(Workspace::toggle_motion))
            .on_action(cx.listener(Workspace::graph_next))
            .on_action(cx.listener(Workspace::graph_previous));
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
        let restore = self.shell.read(cx).focus();
        self.field
            .update(cx, |field, cx| field.select_all(window, cx));
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, SearchStore::open);
        self.transients.replace_omnibar(Transient::Search, restore);
        self.sync_search_transient(cx);
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

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let was_open = self.shell.read(cx).settings_open();
        self.shell.update(cx, ShellStore::toggle_settings);
        if was_open {
            self.remove_transient(Transient::Settings, cx);
            self.restore_focus(window, cx);
        } else {
            let restore = self.shell.read(cx).focus_before_settings();
            self.transients
                .push_with_restore(Transient::Settings, restore);
            window.focus(&self.settings_focus, cx);
        }
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

    fn open_source(&mut self, _: &OpenSource, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_source(window, cx);
    }

    fn find_in_source(&mut self, _: &FindInSource, window: &mut Window, cx: &mut Context<Self>) {
        if !self.source_open {
            self.toggle_source(window, cx);
        }
        if !self.source_open {
            return;
        }
        self.source_field
            .update(cx, |field, cx| field.select_all(window, cx));
        let handle = self.source_field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn previous_source_match(
        &mut self,
        _: &PreviousSourceMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_source_match(-1, cx);
    }

    fn open_editor(&mut self, _: &OpenEditor, _: &mut Window, cx: &mut Context<Self>) {
        let site = match self
            .document
            .read(cx)
            .tab()
            .map(super::super::store::document::Tab::content)
        {
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
        self.copy(
            "Identity copied",
            identity.coordinate().as_str().to_owned(),
            cx,
        );
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
            Subject::Project { coordinate }
            | Subject::Outline { coordinate }
            | Subject::Package { coordinate } => Some(coordinate.clone()),
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
        self.engine.update(cx, WorkspaceStore::refresh_health);
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

    fn graph_next(&mut self, _: &GraphNext, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.focus_next();
        cx.notify();
    }

    fn graph_previous(&mut self, _: &GraphPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.focus_previous();
        cx.notify();
    }
}

/// Sheet navigation.
impl Workspace {
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.transients.top() == Some(Transient::Settings) || self.shell.read(cx).settings_open()
        {
            self.shell.update(cx, ShellStore::close_settings);
            self.remove_transient(Transient::Settings, cx);
            self.restore_focus(window, cx);
            return;
        }
        if self.source_open {
            self.close_source(window, cx);
            return;
        }
        if self.transients.top() == Some(Transient::Add) || self.adding {
            self.cancel_add(cx);
            self.restore_focus(window, cx);
            return;
        }
        if matches!(
            self.transients.top(),
            Some(Transient::Search | Transient::Palette)
        ) || self.search.read(cx).is_open()
        {
            self.set_field(String::new(), cx);
            self.dismiss_sheet(cx);
            self.restore_focus(window, cx);
        }
    }

    /// Restores the concrete GPUI handle for the shell region remembered by
    /// the modal focus state. This keeps Escape reversible even when settings
    /// was opened from the omnibar rather than the reader.
    pub(super) fn restore_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.shell.read(cx).focus() {
            Focus::Omnibar => {
                let handle = self.field.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            Focus::Settings | Focus::Reader => window.focus(&self.focus, cx),
            Focus::Library => window.focus(&self.library_focus, cx),
            Focus::Add => {
                let handle = self.coordinate.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            Focus::Source => window.focus(&self.source_focus, cx),
        }
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.shell.read(cx).settings_open() {
            self.shell
                .update(cx, |shell, cx| shell.step_settings(-1, cx));
            return;
        }
        self.step_selection(-1, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.shell.read(cx).settings_open() {
            self.shell
                .update(cx, |shell, cx| shell.step_settings(1, cx));
            return;
        }
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
        self.search.update(cx, |search, cx| search.step(delta, cx));
    }

    fn accept(&mut self, _: &Accept, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_open
            && self
                .source_field
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.move_source_match(1, cx);
            return;
        }
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
            self.search.update(cx, |search, cx| search.run(command, cx));
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
        self.pending_field = Some(text.clone());
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
        self.sync_search_transient(cx);
    }

    pub(crate) fn set_coordinate(&mut self, text: String) {
        self.pending_coordinate = Some(text);
    }

    /// Submits the add field: a folder or pinned URL at once, a name via the catalog.
    pub(super) fn submit_add(&mut self, cx: &mut Context<Self>) {
        let text = self
            .coordinate
            .read(cx)
            .value()
            .to_string()
            .trim()
            .to_owned();
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
                self.remove_transient(Transient::Add, cx);
                self.pending_coordinate = Some(String::new());
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
    field: Entity<InputState>,
    coordinate: Entity<InputState>,
    source_field: Entity<InputState>,
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
    field: &'a Entity<InputState>,
    coordinate: &'a Entity<InputState>,
    source_field: &'a Entity<InputState>,
}

/// Returns how many rows one page-up or page-down step moves.
fn page_step() -> isize {
    isize::try_from(crate::store::search::PAGE).unwrap_or(8)
}
