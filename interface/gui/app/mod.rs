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
use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, IntoElement, KeyBinding, Render, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};
use interface_documents::{Outline, PageError};
use interface_identity::{Address, PackageCoordinate};
use interface_library::{
    Command, CommandId, CommandSpec, CompilerAttachment, Library, LibraryOpenError, OpenOptions,
    Reply, WorkspaceRoot, WorkspaceRootError, COMMANDS,
};
use interface_search::SearchTerminal;
use interface_library::render::common::Fault;

use crate::{
    motion::Motion,
    prefs::{Decoded, LineNumber, Preferences, PreferencesError},
    store::document::PageKey,
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
    context: ContextSlot,
    motion: Motion,
    shelf_cursor: usize,
    command_cursor: usize,
    submitted_query: Option<Box<str>>,
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
            context: ContextSlot::Idle,
            motion,
            shelf_cursor: 0,
            command_cursor: 0,
            submitted_query: None,
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
        let _ = self
            .requests
            .send_blocking(engine::EngineRequest { errand, command });
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
        let coordinates: Vec<PackageCoordinate> = self
            .store
            .rows()
            .iter()
            .map(|row| row.coordinate.clone())
            .collect();
        if let Some(request) = self.search.request(&coordinates) {
            self.submitted_query = Some(request.text.as_str().into());
            self.dispatch(Errand::Search, Command::Search(request));
        }
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
    pub fn execute_registry(&mut self, id: CommandId, window: &mut gpui::Window) {
        match id {
            CommandId::Packages | CommandId::Health => self.refresh(),
            CommandId::Add => self.open_add_flow("", window),
            CommandId::Remove => self.remove_selected(),
            CommandId::Outline => {
                let Some(row) = self.store.rows().get(self.shelf_cursor) else {
                    self.action_fault = Some("select a package row first".to_owned());
                    return;
                };
                let coordinate = row.coordinate.clone();
                self.open_outline(&coordinate);
            }
            CommandId::Search | CommandId::Resolve | CommandId::Show | CommandId::Graph => {
                self.omnibar_focus.focus(window);
            }
        }
    }

    /// Opens the inline add flow, seeded with optional text such as an example chip.
    pub fn open_add_flow(&mut self, seed: &str, window: &mut gpui::Window) {
        self.store.add.open_with(seed);
        self.motion.set_add_flow_open(true);
        self.add_focus.focus(window);
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

    /// The reader store, mutably, for the shell's tab and history actions.
    pub fn documents_mut(&mut self) -> &mut crate::store::DocumentStore {
        &mut self.documents
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
    pub fn close_add_flow(&mut self, window: &mut gpui::Window) {
        self.store.add.close();
        self.motion.set_add_flow_open(false);
        self.omnibar_focus.focus(window);
    }

    /// Shows or hides the library panel, persisting the choice.
    pub fn toggle_library_panel(&mut self, cx: &Context<Self>) {
        let open = !self.preferences.library_panel.is_open();
        self.preferences.library_panel = crate::prefs::PanelState::from_open(open);
        self.motion.set_library_open(open);
        self.persist_preferences(cx);
        cx.notify();
    }

    /// Shows or hides the context panel, persisting the choice.
    pub fn toggle_outline_panel(&mut self, cx: &Context<Self>) {
        let open = !self.preferences.outline_panel.is_open();
        self.preferences.outline_panel = crate::prefs::PanelState::from_open(open);
        self.motion.set_outline_open(open);
        self.persist_preferences(cx);
        cx.notify();
    }

    /// Flips the appearance, persisting the choice.
    pub fn cycle_appearance(&mut self, cx: &Context<Self>) {
        self.preferences.appearance = self.preferences.appearance.flipped();
        self.theme = Theme::resolve(self.preferences.appearance, self.preferences.size);
        self.persist_preferences(cx);
        cx.notify();
    }

    /// Steps the interface size, persisting the choice.
    pub fn step_interface(&mut self, larger: bool, cx: &Context<Self>) {
        self.preferences.size = if larger {
            self.preferences.size.larger()
        } else {
            self.preferences.size.smaller()
        };
        self.theme = Theme::resolve(self.preferences.appearance, self.preferences.size);
        self.persist_preferences(cx);
        cx.notify();
    }

    /// Flips the motion preference, landing anything in flight, persisting the choice.
    pub fn toggle_motion(&mut self, cx: &Context<Self>) {
        let next = match self.preferences.motion {
            crate::motion::MotionPreference::Full => crate::motion::MotionPreference::Reduced,
            crate::motion::MotionPreference::Reduced => crate::motion::MotionPreference::Full,
        };
        self.preferences.motion = next;
        self.motion.set_preference(next);
        self.persist_preferences(cx);
        cx.notify();
    }

    /// Asks the resident engine to cancel the compile this window started.
    pub fn cancel_compile(&mut self) {
        self.library.cancel_active();
    }

    /// Folds one engine event into exactly one store.
    pub fn fold(&mut self, event: EngineEvent, cx: &Context<Self>) {
        match event {
            EngineEvent::Progress(progress) => self.store.apply_progress(progress),
            EngineEvent::ShelfChanged => {
                self.refresh();
                cx.notify();
                return;
            }
            EngineEvent::Replied { errand, reply } => {
                if errand.command_id() != reply.id() {
                    self.action_fault = Some(
                        "an engine reply arrived on the errand of another command".to_owned(),
                    );
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
            (Errand::Add { .. }, Reply::Added(outcome)) => {
                self.store.finish_job(&outcome);
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
            (Errand::Search, Reply::Searched(terminal)) => self.search.apply(terminal),
            (Errand::Shelf, other) => self.note_misroute(&other),
        }
    }

    fn note_misroute(&mut self, reply: &Reply) {
        self.action_fault = Some(format!(
            "a {} reply arrived on a shelf errand",
            reply.id().name()
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
                workspace.read(cx).omnibar_focus.focus(window, cx);
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
        KeyBinding::new("cmd-shift-a", CycleAppearance, None),
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

/// The page a search terminal's selected hit points at, for tests that drive submission.
#[must_use]
pub fn selected_page(terminal: &SearchTerminal, selected: usize) -> Option<PageKey> {
    terminal
        .hits
        .get(selected)
        .map(|hit| PageKey::of(&hit.symbol.address))
}
