//! Defines the shell application for `interface-gui`.
//! This module owns the window, the keymap, the action set, and the state DAG every view reads.
//! Its narrow surface is one entity whose render is a pure projection of the stores.
//!
//! The window is the resident surface of the local library: it opens the production compiler host,
//! keeps it alive for as long as it runs, and watches the epoch file so packages added by the CLI
//! or the MCP server appear here without a restart. Nothing else in this crate touches the engine;
//! every keystroke, click, and hover becomes one closed [`interface_library::Command`] dispatched
//! through the resident engine, and every reply is folded into exactly one store.

mod engine;
mod input;

use std::{
    fmt,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_channel::Sender;
use compiler_ir_vocabulary::EntityKind;
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, FocusHandle, Focusable, IntoElement, KeyBinding,
    Render, SharedString, TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};
use interface_core::PackageUrl;
use interface_documents::Outline;
use interface_identity::{Address, PackageCoordinate};
use interface_library::{
    Command, CommandId, CommandSpec, CompilerAttachment, ExploreLimit, ExplorePackageName,
    ExploreQuery, Library, LibraryOpenError, OpenOptions, Reply, WorkspaceRoot, WorkspaceRootError,
    COMMANDS,
};
use interface_search::SearchTerminal;
use interface_library::render::common::{Affordance, Fault};

use crate::{
    motion::Motion,
    prefs::{Decoded, LineNumber, Preferences, PreferencesError},
    store::{document::PageKey, explore::ExploreStore, search::OmnibarMode},
    theme::Theme,
};

pub use engine::{EngineEvent, EngineRequest, Errand};
pub use input::FieldTarget;

use self::input::{NativeInputState, SharedFieldGeometry};

actions!(
    nudox,
    [
        FocusOmnibar,
        Submit,
        Cancel,
        StepDown,
        StepUp,
        PageDown,
        PageUp,
        SelectFirst,
        SelectLast,
        SubmitAdd,
        CancelAdd,
        QuickAdd,
        ToggleLibraryPanel,
        ToggleOutlinePanel,
        NavigateBack,
        NavigateForward,
        CloseTab,
        NextTab,
        PreviousTab,
        Refresh,
        CycleAppearance,
        GrowInterface,
        ShrinkInterface,
        ToggleMotion,
        CancelCompile
    ]
);

/// The window title.
const WINDOW_TITLE: &str = "Nudox";

/// The platform application identifier.
const APPLICATION_ID: &str = "dev.nudox.reader";

/// The default window size, large enough for three panels and a reading measure.
const DEFAULT_WINDOW: (f32, f32) = (1280.0, 820.0);

/// The smallest window the layout can honour.
const MINIMUM_WINDOW: (f32, f32) = (940.0, 650.0);

/// How long the omnibar waits after the last keystroke before it searches on its own.
///
/// Enter never waits: submitting stays immediate. The debounce only decides when typing alone
/// runs the search, so the reader sees results while typing without one engine round trip per
/// character.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// Why the process could not open a window at all.
pub enum LaunchRefusal {
    /// No workspace root could be resolved.
    Root(WorkspaceRootError),
    /// The library refused to open, even read-only.
    Library(LibraryOpenError),
    /// The preference file refused in a way defaults cannot absorb.
    Preferences(PreferencesError),
}

impl fmt::Display for LaunchRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(WorkspaceRootError::Missing { variable }) => {
                write!(formatter, "no workspace root: {variable} is not set")
            }
            Self::Root(WorkspaceRootError::Relative { variable, path }) => write!(
                formatter,
                "the root from {variable} is relative: {}",
                path.display()
            ),
            Self::Library(LibraryOpenError::CreateDirectory { path, source }) => write!(
                formatter,
                "{} could not be created: {source}",
                path.display()
            ),
            Self::Library(LibraryOpenError::Compiler { detail }) => {
                write!(formatter, "the compiler host refused: {detail}")
            }
            Self::Library(LibraryOpenError::Shelf(_)) => {
                write!(formatter, "the shelf store could not be opened")
            }
            Self::Library(LibraryOpenError::Epoch(_)) => {
                write!(formatter, "the library epoch file could not be read")
            }
            Self::Preferences(PreferencesError::Io { phase, path, source }) => write!(
                formatter,
                "the preference file refused while {phase:?} at {}: {source}",
                path.display()
            ),
        }
    }
}

/// What the startup path produced before the first frame.
pub struct Startup {
    pub(crate) library: Library,
    pub(crate) root: WorkspaceRoot,
    pub(crate) decoded: Decoded,
    pub(crate) fault: Option<String>,
}

impl Startup {
    /// Opens a workspace read-only: no compiler host, first-run defaults allowed.
    ///
    /// This is the headless embedding path for tests and future hosts; the window path is
    /// [`gather_startup`], which degrades a refusing compiler host instead of failing.
    ///
    /// # Errors
    ///
    /// Returns the exact library or preference refusal.
    pub fn detached(root: WorkspaceRoot) -> Result<Self, LaunchRefusal> {
        let decoded = Preferences::load(&root).map_err(LaunchRefusal::Preferences)?;
        let library = Library::open(OpenOptions {
            root: root.clone(),
            compiler: CompilerAttachment::Detached,
        })
        .map_err(LaunchRefusal::Library)?;
        Ok(Self {
            library,
            root,
            decoded,
            fault: None,
        })
    }
}

/// What the context panel draws.
#[derive(Clone, Debug)]
pub enum ContextSlot {
    /// No tree has been asked for yet.
    Idle,
    /// A tree is on its way for this package.
    Loading {
        /// The package whose tree was requested.
        package: PackageCoordinate,
    },
    /// The tree is on screen.
    Ready {
        /// The engine's own containment tree.
        outline: Outline,
    },
    /// The engine refused, with its own cause retained.
    Failed {
        /// The fault drawn in the panel.
        fault: Fault,
    },
}

impl Default for ContextSlot {
    fn default() -> Self {
        Self::Idle
    }
}

/// The one entity behind the window.
pub struct Workspace {
    library: Library,
    root: WorkspaceRoot,
    theme: Theme,
    preferences: Preferences,
    preferences_fault: Option<String>,
    unreadable_pref_lines: Box<[LineNumber]>,
    startup_fault: Option<String>,
    action_fault: Option<String>,
    store: crate::store::LibraryStore,
    search: crate::store::SearchStore,
    documents: crate::store::DocumentStore,
    explore: ExploreStore,
    context: ContextSlot,
    motion: Motion,
    shelf_cursor: usize,
    command_cursor: usize,
    submitted_query: Option<Box<str>>,
    search_generation: u64,
    search_dispatches: u32,
    hovered: Option<PageKey>,
    requests: Sender<EngineRequest>,
    field_bounds: SharedFieldGeometry,
    native_input: NativeInputState,
    omnibar_focus: FocusHandle,
    add_focus: FocusHandle,
    reader_focus: FocusHandle,
    root_focus: FocusHandle,
    frame_clock: Option<Instant>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl Workspace {
    /// Builds the workspace, starts the resident engine, and performs the first reads.
    pub fn new(startup: Startup, _window: &mut gpui::Window, cx: &Context<Self>) -> Self {
        let (requests, events) = engine::start(startup.library.clone());
        let preferences = startup.decoded.preferences;
        let motion = Motion::restored(
            preferences.motion,
            preferences.library_panel.is_open(),
            preferences.outline_panel.is_open(),
        );
        let mut workspace = Self {
            library: startup.library,
            root: startup.root,
            theme: Theme::resolve(preferences.appearance, preferences.size),
            preferences,
            preferences_fault: startup
                .decoded
                .unreadable
                .first()
                .map(|line| format!("gui.prefs line {} was ignored", line.get())),
            unreadable_pref_lines: startup.decoded.unreadable,
            startup_fault: startup.fault,
            action_fault: None,
            store: crate::store::LibraryStore::default(),
            search: crate::store::SearchStore::default(),
            documents: crate::store::DocumentStore::default(),
            explore: ExploreStore::default(),
            context: ContextSlot::Idle,
            motion,
            shelf_cursor: 0,
            command_cursor: 0,
            submitted_query: None,
            search_generation: 0,
            search_dispatches: 0,
            hovered: None,
            requests,
            field_bounds: Default::default(),
            native_input: NativeInputState::default(),
            omnibar_focus: cx.focus_handle(),
            add_focus: cx.focus_handle(),
            reader_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            frame_clock: None,
        };
        workspace.spawn_event_pump(events, cx);
        workspace.refresh();
        workspace
    }

    fn spawn_event_pump(&self, events: async_channel::Receiver<EngineEvent>, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |workspace, cx| workspace.fold(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The resolved theme every view draws from.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The preferences in force.
    #[must_use]
    pub const fn preferences(&self) -> &Preferences {
        &self.preferences
    }

    /// The library store every panel row comes from.
    #[must_use]
    pub const fn store(&self) -> &crate::store::LibraryStore {
        &self.store
    }

    /// The omnibar and results sheet state.
    #[must_use]
    pub const fn search(&self) -> &crate::store::SearchStore {
        &self.search
    }

    /// The reader state: tabs, pages, hover cards.
    #[must_use]
    pub const fn documents(&self) -> &crate::store::DocumentStore {
        &self.documents
    }

    /// The exploration state: index pages, version rows, and profiles, keyed by what asked.
    #[must_use]
    pub const fn explore(&self) -> &ExploreStore {
        &self.explore
    }

    /// The context panel state.
    #[must_use]
    pub const fn context(&self) -> &ContextSlot {
        &self.context
    }

    /// The motion set, for views that draw animated widths and reveals.
    #[must_use]
    pub const fn motion(&self) -> &Motion {
        &self.motion
    }

    /// Which shelf row is selected.
    #[must_use]
    pub const fn shelf_cursor(&self) -> usize {
        self.shelf_cursor
    }

    /// Which registry row the palette has selected.
    #[must_use]
    pub const fn command_cursor(&self) -> usize {
        self.command_cursor
    }

    /// The target whose hover card was asked for last.
    #[must_use]
    pub fn hovered(&self) -> Option<&PageKey> {
        self.hovered.as_ref()
    }

    /// The fault from the last action that could not proceed, in this window's own words.
    #[must_use]
    pub fn action_fault(&self) -> Option<&str> {
        self.action_fault.as_deref()
    }

    /// The fault from startup that left this window degraded but usable.
    #[must_use]
    pub fn startup_fault(&self) -> Option<&str> {
        self.startup_fault.as_deref()
    }

    /// The fault from the last preference write, when one refused.
    #[must_use]
    pub fn preferences_fault(&self) -> Option<&str> {
        self.preferences_fault.as_deref()
    }

    /// The preference lines this build did not understand, in file order.
    #[must_use]
    pub fn unreadable_pref_lines(&self) -> &[LineNumber] {
        &self.unreadable_pref_lines
    }

    /// The focus handle of the omnibar field.
    #[must_use]
    pub const fn omnibar_focus(&self) -> &FocusHandle {
        &self.omnibar_focus
    }

    /// The focus handle of the add-flow coordinate field.
    #[must_use]
    pub const fn add_focus(&self) -> &FocusHandle {
        &self.add_focus
    }

    /// The focus handle of the reader column.
    #[must_use]
    pub const fn reader_focus(&self) -> &FocusHandle {
        &self.reader_focus
    }

    /// The focus handle of the shell itself.
    #[must_use]
    pub const fn root_focus(&self) -> &FocusHandle {
        &self.root_focus
    }

    /// The field geometry cell the input bridge records into.
    pub(crate) fn field_bounds(&self) -> &SharedFieldGeometry {
        &self.field_bounds
    }

    /// The palette rows: the shared registry filtered by the omnibar's command query.
    #[must_use]
    pub fn registry_rows(&self) -> Vec<CommandSpec> {
        let needle = self.search.mode().query().to_lowercase();
        COMMANDS
            .into_iter()
            .filter(|row| {
                needle.is_empty()
                    || row.name.contains(&needle)
                    || row.title.to_lowercase().contains(&needle)
                    || row.description.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Sends one command to the resident engine.
    fn dispatch(&mut self, errand: Errand, command: Command) {
        let _ = self.requests.send_blocking(EngineRequest { errand, command });
    }

    /// Searches the index catalogue for packages matching the query.
    pub fn search_index(&mut self, query: ExploreQuery) {
        self.explore.begin_index_search();
        let limit = ExploreLimit::default();
        self.dispatch(
            Errand::IndexSearch { query: query.clone() },
            Command::IndexSearch { query, limit },
        );
    }

    /// Asks for one package's published versions.
    pub fn package_versions(&mut self, name: ExplorePackageName) {
        self.explore.begin_versions(&name);
        self.dispatch(
            Errand::Versions { name: name.clone() },
            Command::PackageVersions { name },
        );
    }

    /// Asks for one package's descriptive profile.
    pub fn package_profile(&mut self, name: ExplorePackageName) {
        self.explore.begin_profile(&name);
        self.dispatch(
            Errand::Profile { name: name.clone() },
            Command::PackageProfile { name },
        );
    }

    /// Re-reads the shelf and the capability report.
    pub fn refresh(&mut self) {
        self.dispatch(Errand::Shelf, Command::Packages);
        self.dispatch(Errand::Health, Command::Health);
    }

    /// Opens one page in the reader, keeping whatever is on screen until it lands.
    pub fn open_page(&mut self, key: &PageKey) {
        if self.documents.slot().is_loading() {
            return;
        }
        if Address::parse(key.as_str()).is_ok() {
            self.fetch(key);
        } else {
            self.action_fault = Some(format!("{} cannot be opened as an address", key.as_str()));
        }
    }

    /// Asks for one hover card, at most once per target.
    pub fn open_card(&mut self, key: &PageKey) {
        let Ok(address) = Address::parse(key.as_str()) else {
            return;
        };
        if self.documents.request_card(key) {
            self.hovered = Some(key.clone());
            self.dispatch(
                Errand::Card { key: key.clone() },
                Command::Show {
                    locator: interface_library::PageLocator::Address(address),
                    limits: interface_documents::ProjectionLimits::default(),
                },
            );
        }
    }

    /// Reads one package's containment tree into the context panel.
    pub fn open_outline(&mut self, coordinate: &PackageCoordinate) {
        self.context = ContextSlot::Loading {
            package: coordinate.clone(),
        };
        self.dispatch(
            Errand::Outline {
                coordinate: coordinate.clone(),
            },
            Command::Outline {
                coordinate: coordinate.clone(),
            },
        );
    }

    /// Submits the omnibar: a query that already ran opens its selected hit, and a fresh query
    /// runs as a search. Command rows are executed by the Submit action, which owns the window.
    pub fn submit(&mut self) {
        self.supersede_search_debounce();
        let already_ran = self
            .submitted_query
            .as_deref()
            .is_some_and(|submitted| submitted == self.search.mode().query())
            && !self.search.hits().is_empty();
        if already_ran {
            let key = self
                .search
                .selection()
                .map(|hit| PageKey::of(&hit.symbol.address));
            if let Some(key) = key {
                self.open_page(&key);
            }
            return;
        }
        self.run_search();
    }

    /// Runs the omnibar's current search against the shelf, recording the query as submitted.
    fn run_search(&mut self) {
        let coordinates: Vec<PackageCoordinate> = self
            .store
            .rows()
            .iter()
            .map(|row| row.coordinate.clone())
            .collect();
        if let Some(request) = self.search.request(&coordinates) {
            self.submitted_query = Some(request.text.as_str().into());
            self.search_dispatches = self.search_dispatches.saturating_add(1);
            self.dispatch(Errand::Search, Command::Search(request));
        }
    }

    /// How many searches this window has dispatched, so the debounce and continuation laws are
    /// observable without stepping the scheduler.
    #[must_use]
    pub const fn search_dispatches(&self) -> u32 {
        self.search_dispatches
    }

    /// The current search generation, so a test can fire one debounce arm and prove a superseded
    /// arm does nothing.
    #[must_use]
    pub const fn search_generation(&self) -> u64 {
        self.search_generation
    }

    /// Fires one debounce arm exactly as its timer would, ignoring superseded generations.
    pub fn fire_debounced_search(&mut self, generation: u64) {
        if self.search_generation == generation {
            self.run_search();
        }
    }

    /// Supersedes any pending debounced search, so only the newest arm can fire.
    const fn supersede_search_debounce(&mut self) {
        self.search_generation = self.search_generation.saturating_add(1);
    }

    /// Cancels the pending debounced search, as clearing or escaping the field does.
    pub const fn cancel_pending_search(&mut self) {
        self.supersede_search_debounce();
    }

    /// Arms the debounced search for the omnibar's current text, superseding any pending arm.
    ///
    /// The timer runs on the background executor and dispatches [`Command::Search`] only if no
    /// newer retype, submit, or cancel happened while it waited, so a fast typist costs one
    /// engine round trip rather than one per keystroke. It requests no frame: an idle window
    /// stays idle.
    fn arm_search_debounce(&mut self, cx: &mut Context<Self>) {
        self.supersede_search_debounce();
        let OmnibarMode::Search { query, .. } = self.search.mode() else {
            return;
        };
        if query.is_empty() {
            return;
        }
        let generation = self.search_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |workspace, _cx| {
                workspace.fire_debounced_search(generation);
            });
        })
        .detach();
    }

    /// Pages the results sheet, continuing the search when the reader steps past the loaded rows.
    ///
    /// A page step past the last loaded row of a truncated sheet asks for the rows that were
    /// cut, so the truncation line stops being the end of the story.
    pub fn page_results(&mut self, forward: bool) {
        let count = self.search.hits().len();
        let selected = self.search.selected();
        let stepping_past = forward && count > 0 && selected.saturating_add(1) >= count;
        self.search.page(forward);
        if stepping_past && self.search.is_truncated() {
            self.continue_search();
        }
    }

    /// Fetches the next page of a truncated search, appending it past the loaded rows.
    pub fn continue_search(&mut self) {
        let Some(request) = self.search.continuation_request() else {
            return;
        };
        self.search_dispatches = self.search_dispatches.saturating_add(1);
        self.dispatch(Errand::Search, Command::Search(request));
    }

    /// Flips one kind chip and re-runs the search on the debounce, exactly as retyping does.
    pub fn toggle_search_kind(&mut self, kind: EntityKind, cx: &mut Context<Self>) {
        self.search.toggle_kind(kind);
        self.arm_search_debounce(cx);
        cx.notify();
    }

    /// Adds whatever the seed names, or opens the add flow stating why the seed was refused.
    ///
    /// A seed that parses as a pinned coordinate seeds the flow exactly as offered; a canonical
    /// package URL seeds it with the URL's derived coordinate, which is the spelling the flow's
    /// draft grammar admits. Anything else opens the flow with a fault line carrying the refused
    /// text, so the reader sees and edits exactly what was offered.
    pub fn quick_add(&mut self, seed: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let trimmed = seed.trim();
        let coordinate = match PackageCoordinate::parse(trimmed) {
            Ok(coordinate) => Some(coordinate),
            Err(_) => {
                PackageUrl::try_from(trimmed.to_owned())
                    .ok()
                    .and_then(|url| PackageCoordinate::from_package_url(&url))
            }
        };
        let derived = coordinate.as_ref().map(ToString::to_string);
        let seeded = derived.as_deref().unwrap_or(trimmed);
        self.open_add_flow(seeded, window, cx);
        if coordinate.is_none() {
            self.store.add.fault = Some(
                Fault::new(
                    "clipboard-not-a-package",
                    trimmed.to_owned(),
                    Affordance::Add {
                        package: trimmed.to_owned(),
                    },
                )
                .detailed("the clipboard held no pinned coordinate or package URL".to_owned()),
            );
        }
    }

    /// The `QuickAdd` action: adds whatever the platform clipboard names.
    pub fn quick_add_from_clipboard(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        self.quick_add(&text, window, cx);
    }

    /// Submits the add flow's valid draft as one compile.
    pub fn submit_add(&mut self) {
        let drafted = self
            .store
            .add
            .package_url()
            .and_then(|url| PackageCoordinate::from_package_url(&url).map(|coordinate| (url, coordinate)));
        let Some((url, coordinate)) = drafted else {
            return;
        };
        self.store.begin_job(coordinate.clone());
        self.store.add.close();
        self.motion.set_add_flow_open(false);
        self.dispatch(Errand::Add { coordinate }, Command::Add { url });
    }

    /// Removes the selected shelf row's package.
    pub fn remove_selected(&mut self) {
        let Some(row) = self.store.rows().get(self.shelf_cursor) else {
            self.action_fault = Some("select a package row first".to_owned());
            return;
        };
        let coordinate = row.coordinate.clone();
        self.dispatch(
            Errand::Remove {
                coordinate: coordinate.clone(),
            },
            Command::Remove { coordinate },
        );
    }

    /// Performs one registry row the way this surface can: by acting, or by pointing the reader at
    /// the omnibar, which is where an operand is collected on this surface.
    pub fn execute_registry(
        &mut self,
        id: CommandId,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            CommandId::Packages | CommandId::Health => self.refresh(),
            CommandId::Add => self.open_add_flow("", window, cx),
            CommandId::Remove => self.remove_selected(),
            CommandId::Outline => {
                let Some(row) = self.store.rows().get(self.shelf_cursor) else {
                    self.action_fault = Some("select a package row first".to_owned());
                    return;
                };
                let coordinate = row.coordinate.clone();
                self.open_outline(&coordinate);
            }
            CommandId::Search | CommandId::Resolve | CommandId::Show | CommandId::Graph
            | CommandId::IndexSearch | CommandId::PackageVersions | CommandId::PackageProfile => {
                self.omnibar_focus.focus(window, cx);
            }
            // The registry, home, session, and source rows belong to the rewritten surface; this
            // window keeps the palette honest by focusing the field rather than pretending.
            CommandId::Source
            | CommandId::Related
            | CommandId::Explore
            | CommandId::Package
            | CommandId::Dependents
            | CommandId::Owner
            | CommandId::Subscribe
            | CommandId::Unsubscribe
            | CommandId::Subscriptions
            | CommandId::Releases
            | CommandId::Projects
            | CommandId::ProjectCreate
            | CommandId::ProjectDelete
            | CommandId::ProjectAdd
            | CommandId::ProjectRemove
            | CommandId::ProjectSync
            | CommandId::Tree
            | CommandId::TreeOpen
            | CommandId::TreeClose => {
                self.omnibar_focus.focus(window, cx);
            }
        }
    }

    /// Opens the inline add flow, seeded with optional text such as an example chip.
    pub fn open_add_flow(
        &mut self,
        seed: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.store.add.open_with(seed);
        self.motion.set_add_flow_open(true);
        self.add_focus.focus(window, cx);
    }

    /// Records the reader's selection of one shelf row.
    pub fn select_shelf_row(&mut self, index: usize) {
        if index < self.store.rows().len() {
            self.shelf_cursor = index;
        }
    }

    /// Steps the palette selection, bounded by the filtered registry.
    pub fn step_registry(&mut self, forward: bool) {
        let count = self.registry_rows().len();
        if count == 0 {
            self.command_cursor = 0;
            return;
        }
        let last = count.saturating_sub(1);
        self.command_cursor = if forward {
            (self.command_cursor.saturating_add(1)).min(last)
        } else {
            self.command_cursor.saturating_sub(1)
        };
    }

    /// Selects the palette's first row.
    pub fn step_registry_first(&mut self) {
        self.command_cursor = 0;
    }

    /// Selects the palette's last row.
    pub fn step_registry_last(&mut self) {
        self.command_cursor = self.registry_rows().len().saturating_sub(1);
    }

    /// The omnibar store, mutably, for the shell's field actions.
    pub fn search_mut(&mut self) -> &mut crate::store::SearchStore {
        &mut self.search
    }

    /// The library store, mutably, for the panel's inline actions.
    pub fn store_mut(&mut self) -> &mut crate::store::LibraryStore {
        &mut self.store
    }

    /// The reader store, mutably, for the shell's tab and history actions.
    pub fn documents_mut(&mut self) -> &mut crate::store::DocumentStore {
        &mut self.documents
    }

    /// The exploration store, mutably, for views that fold or clear its slots.
    pub const fn explore_mut(&mut self) -> &mut ExploreStore {
        &mut self.explore
    }

    /// Fetches one page without the double-open guard, for internal navigation.
    pub fn fetch(&mut self, key: &PageKey) {
        if let Ok(address) = Address::parse(key.as_str()) {
            self.documents.begin(key);
            self.dispatch(
                Errand::Page { key: key.clone() },
                Command::Show {
                    locator: interface_library::PageLocator::Address(address),
                    limits: interface_documents::ProjectionLimits::default(),
                },
            );
        }
    }

    /// Re-fetches whatever tab came to the front, for tab cycling.
    pub fn refetch_active(&mut self) {
        if let Some(key) = self.documents.active().map(|tab| tab.key().clone()) {
            if self.documents.cached(&key).is_none() {
                self.fetch(&key);
            }
        }
    }

    /// Forgets one hover target, when the pointer leaves it.
    pub fn forget_hover(&mut self, key: &PageKey) {
        if self.hovered.as_ref() == Some(key) {
            self.hovered = None;
        }
    }

    /// Collapses the add flow and returns focus to the shell.
    pub fn close_add_flow(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.store.add.close();
        self.motion.set_add_flow_open(false);
        self.omnibar_focus.focus(window, cx);
    }

    /// Shows or hides the library panel, persisting the choice.
    pub fn toggle_library_panel(&mut self, cx: &mut Context<Self>) {
        let open = !self.preferences.library_panel.is_open();
        self.preferences.library_panel = crate::prefs::PanelState::from_open(open);
        self.motion.set_library_open(open);
        self.persist_preferences(&*cx);
        cx.notify();
    }

    /// Shows or hides the context panel, persisting the choice.
    pub fn toggle_outline_panel(&mut self, cx: &mut Context<Self>) {
        let open = !self.preferences.outline_panel.is_open();
        self.preferences.outline_panel = crate::prefs::PanelState::from_open(open);
        self.motion.set_outline_open(open);
        self.persist_preferences(&*cx);
        cx.notify();
    }

    /// Flips the appearance, persisting the choice.
    pub fn cycle_appearance(&mut self, cx: &mut Context<Self>) {
        self.preferences.appearance = self.preferences.appearance.flipped();
        self.theme = Theme::resolve(self.preferences.appearance, self.preferences.size);
        self.persist_preferences(&*cx);
        cx.notify();
    }

    /// Steps the interface size, persisting the choice.
    pub fn step_interface(&mut self, larger: bool, cx: &mut Context<Self>) {
        self.preferences.size = if larger {
            self.preferences.size.larger()
        } else {
            self.preferences.size.smaller()
        };
        self.theme = Theme::resolve(self.preferences.appearance, self.preferences.size);
        self.persist_preferences(&*cx);
        cx.notify();
    }

    /// Flips the motion preference, landing anything in flight, persisting the choice.
    pub fn toggle_motion(&mut self, cx: &mut Context<Self>) {
        let next = match self.preferences.motion {
            crate::motion::MotionPreference::Full => crate::motion::MotionPreference::Reduced,
            crate::motion::MotionPreference::Reduced => crate::motion::MotionPreference::Full,
        };
        self.preferences.motion = next;
        self.motion.set_preference(next);
        self.persist_preferences(&*cx);
        cx.notify();
    }

    /// Asks the resident engine to cancel the compile this window started.
    pub fn cancel_compile(&mut self) {
        self.library.cancel_active();
    }

    /// Folds one engine event into exactly one store.
    pub fn fold(&mut self, event: EngineEvent, cx: &mut Context<Self>) {
        match event {
            EngineEvent::Progress(progress) => self.store.apply_progress(progress),
            EngineEvent::ShelfChanged => {
                self.refresh();
                cx.notify();
                return;
            }
            EngineEvent::Replied { errand, reply } => {
                if errand.command_id() != reply.id() {
                    self.note_crossed(&errand, &reply);
                    cx.notify();
                    return;
                }
                self.fold_reply(errand, reply);
            }
        }
        cx.notify();
    }

    fn fold_reply(&mut self, errand: Errand, reply: Reply) {
        match (errand, reply) {
            (Errand::Shelf, Reply::Packages(result)) => self.store.apply_shelf(result),
            (Errand::Health, Reply::Health(health)) => self.store.apply_health(health),
            (Errand::Add { coordinate }, Reply::Added(outcome)) => {
                self.store.finish_job_for(&coordinate, &outcome);
                self.dispatch(Errand::Shelf, Command::Packages);
            }
            (Errand::Remove { coordinate }, Reply::Removed(outcome)) => {
                self.store.apply_removed(&coordinate, &outcome);
                self.dispatch(Errand::Shelf, Command::Packages);
            }
            (Errand::Page { key }, Reply::Page(result)) => self.documents.apply(&key, result),
            (Errand::Card { key }, Reply::Page(result)) => self.documents.apply_card(&key, result),
            (Errand::Outline { .. }, Reply::Outline(result)) => {
                self.context = match result {
                    Ok(outline) => ContextSlot::Ready { outline },
                    Err(error) => ContextSlot::Failed {
                        fault: crate::store::document::page_fault(&error),
                    },
                };
            }
            (Errand::Search, Reply::Searched(terminal)) => {
                if terminal.request.cursor.is_some() {
                    self.search.apply_continuation(terminal);
                } else {
                    self.search.apply(terminal);
                }
            }
            (Errand::IndexSearch { .. }, Reply::IndexSearched(page)) => {
                self.explore.apply_index_search(page);
            }
            (Errand::Versions { name }, Reply::Versions(rows)) => {
                self.explore.apply_versions(&name, rows);
            }
            (Errand::Profile { name }, Reply::Profiled(profile)) => {
                self.explore.apply_profile(&name, profile);
            }
            (errand, reply) => self.note_crossed(&errand, &reply),
        }
    }

    fn note_crossed(&mut self, errand: &Errand, reply: &Reply) {
        self.action_fault = Some(format!(
            "a {} reply arrived on the errand of a {}",
            interface_library::spec(reply.id()).name,
            interface_library::spec(errand.command_id()).name
        ));
    }

    fn persist_preferences(&self, cx: &Context<Self>) {
        let preferences = self.preferences;
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let outcome = preferences.store(&root).err().map(|error| match error {
                PreferencesError::Io { phase, path, source } => {
                    format!("preferences refused while {phase:?} at {}: {source}", path.display())
                }
            });
            let _ = this.update(cx, |workspace, cx| {
                workspace.preferences_fault = outcome;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.synchronize_native_target(window);
        window.set_rem_size(px(self.theme.root_pixels()));
        let elapsed = self
            .frame_clock
            .replace(Instant::now())
            .map_or(Duration::ZERO, |last| last.elapsed());
        if self.motion.advance(elapsed).wanted() {
            window.request_animation_frame();
        }
        crate::views::shell(self, window, cx)
    }
}

/// Resolves the workspace, loads preferences, and opens the library the window will hold resident.
///
/// A compiler host that refuses to open degrades the window to read-only with the refusal stated
/// in the status bar; a root or library that refuses to open at all stops the launch.
///
/// # Errors
///
/// Returns the exact root, library, or preference refusal.
pub fn gather_startup(root_override: Option<PathBuf>) -> Result<Startup, LaunchRefusal> {
    let root = match root_override {
        Some(path) => WorkspaceRoot::absolute(path, "--root").map_err(LaunchRefusal::Root)?,
        None => WorkspaceRoot::resolve().map_err(LaunchRefusal::Root)?,
    };
    let decoded = Preferences::load(&root).map_err(LaunchRefusal::Preferences)?;
    let mut fault = None;
    let library = match Library::open(OpenOptions {
        root: root.clone(),
        compiler: CompilerAttachment::Production,
    }) {
        Ok(library) => library,
        Err(LibraryOpenError::Compiler { detail }) => {
            fault = Some(format!("the compiler host refused: {detail}; running read-only"));
            Library::open(OpenOptions {
                root: root.clone(),
                compiler: CompilerAttachment::Detached,
            })
            .map_err(LaunchRefusal::Library)?
        }
        Err(error) => return Err(LaunchRefusal::Library(error)),
    };
    Ok(Startup {
        library,
        root,
        decoded,
        fault,
    })
}

/// Opens the window and runs the platform loop until the reader closes it.
///
/// # Errors
///
/// Returns the exact refusal when no workspace could be opened.
pub fn run() -> Result<(), LaunchRefusal> {
    let startup = gather_startup(None)?;
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys(keymap());
        let bounds = Bounds::centered(None, size(px(DEFAULT_WINDOW.0), px(DEFAULT_WINDOW.1)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MINIMUM_WINDOW.0), px(MINIMUM_WINDOW.1))),
                app_id: Some(APPLICATION_ID.to_owned()),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from(WINDOW_TITLE)),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            },
            move |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(startup, window, cx));
                let omnibar = workspace.read(cx).omnibar_focus.clone();
                omnibar.focus(window, cx);
                workspace
            },
        );
        if window.is_err() {
            cx.quit();
        } else {
            cx.activate(true);
        }
    });
    Ok(())
}

/// The keymap: platform-chrome bindings are global; field bindings are scoped to their field.
fn keymap() -> Vec<KeyBinding> {
    let omnibar = "Omnibar";
    let field = "AddField";
    vec![
        KeyBinding::new("cmd-k", FocusOmnibar, None),
        KeyBinding::new("cmd-l", FocusOmnibar, None),
        KeyBinding::new("cmd-b", ToggleLibraryPanel, None),
        KeyBinding::new("cmd-alt-b", ToggleOutlinePanel, None),
        KeyBinding::new("cmd-[", NavigateBack, None),
        KeyBinding::new("cmd-]", NavigateForward, None),
        KeyBinding::new("cmd-left", NavigateBack, None),
        KeyBinding::new("cmd-right", NavigateForward, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-shift-a", QuickAdd, None),
        KeyBinding::new("cmd-shift-c", CycleAppearance, None),
        KeyBinding::new("cmd-=", GrowInterface, None),
        KeyBinding::new("cmd--", ShrinkInterface, None),
        KeyBinding::new("cmd-shift-m", ToggleMotion, None),
        KeyBinding::new("enter", Submit, Some(omnibar)),
        KeyBinding::new("escape", Cancel, Some(omnibar)),
        KeyBinding::new("down", StepDown, Some(omnibar)),
        KeyBinding::new("up", StepUp, Some(omnibar)),
        KeyBinding::new("pagedown", PageDown, Some(omnibar)),
        KeyBinding::new("pageup", PageUp, Some(omnibar)),
        KeyBinding::new("cmd-down", SelectLast, Some(omnibar)),
        KeyBinding::new("cmd-up", SelectFirst, Some(omnibar)),
        KeyBinding::new("enter", SubmitAdd, Some(field)),
        KeyBinding::new("escape", CancelAdd, Some(field)),
    ]
}

/// The wall-clock timestamp the relative ages are measured against.
pub(crate) fn now_timestamp() -> interface_library::Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| interface_library::Timestamp(0), |since| {
            interface_library::Timestamp(since.as_secs())
        })
}

/// Writes one string to the platform clipboard.
///
/// The reader's copy affordances consume this; the shell itself never writes.
#[allow(
    dead_code,
    reason = "the card lands this for the reader and search waves, which consume it"
)]
pub(crate) fn write_clipboard(text: &str, cx: &App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
}

/// Opens one URL in the platform's default browser.
///
/// The reader's outbound links consume this; the shell itself never opens.
#[allow(
    dead_code,
    reason = "the card lands this for the reader and search waves, which consume it"
)]
pub(crate) fn open_external(url: &str, cx: &App) {
    cx.open_url(url);
}

/// The page a search terminal's selected hit points at, for tests that drive submission.
#[must_use]
pub fn selected_page(terminal: &SearchTerminal, selected: usize) -> Option<PageKey> {
    terminal
        .hits
        .get(selected)
        .map(|hit| PageKey::of(&hit.symbol.address))
}
