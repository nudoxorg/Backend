//! Stable product navigation and command-palette selection state.

use wave_application_core::{InputText, InputTextJoinError};

use crate::Surface;

/// Upper bound for commands displayed by the palette before a caller offers a narrower query.
pub const MAX_PALETTE_RESULTS: usize = 100;

/// Number of palette rows moved by one page-navigation action.
pub const PALETTE_PAGE_ROWS: usize = 8;

/// One stable application destination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Route {
    /// The first stable frame and product overview.
    #[default]
    Home,
    /// Local library lifecycle and index coverage.
    Libraries,
    /// Exact, lexical, graph, and vector retrieval.
    Search,
    /// AI-client and MCP connection setup.
    Connections,
    /// Product preferences and diagnostics.
    Settings,
}

/// Immutable visual facts for one route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteFacts {
    /// Stable destination identity.
    pub route: Route,
    /// Human-visible label.
    pub label: &'static str,
    /// Stable rendered element identity.
    pub element_id: &'static str,
    /// Discoverable cross-platform accelerator, when this route has one.
    pub shortcut: Option<ShortcutFacts>,
}

/// Immutable visible accelerator facts without render-time operating-system detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutFacts {
    /// macOS presentation of this accelerator.
    pub apple: &'static str,
    /// Windows and Linux presentation of this accelerator.
    pub other: &'static str,
}

/// The closed application information architecture, in rail order.
pub const ROUTES: [RouteFacts; 5] = [
    RouteFacts {
        route: Route::Home,
        label: "Home",
        element_id: "route-home",
        shortcut: None,
    },
    RouteFacts {
        route: Route::Libraries,
        label: "Libraries",
        element_id: "route-libraries",
        shortcut: None,
    },
    RouteFacts {
        route: Route::Search,
        label: "Search",
        element_id: "route-search",
        shortcut: None,
    },
    RouteFacts {
        route: Route::Connections,
        label: "Connections",
        element_id: "route-connections",
        shortcut: None,
    },
    RouteFacts {
        route: Route::Settings,
        label: "Settings",
        element_id: "route-settings",
        shortcut: Some(ShortcutFacts {
            apple: "⌘,",
            other: "Ctrl ,",
        }),
    },
];

/// A stable, non-positional palette command identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandId {
    /// Open one product route.
    OpenRoute(Route),
    /// Reveal a named status surface at the owning destination.
    InspectSurface(Surface),
    /// Focus one real typed application action's form.
    FocusAction(ServiceAction),
}

/// A closed action that the shell can expose without parsing a stringly command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAction {
    /// Compile one selected package through the canonical compiler seam.
    Generate,
    /// Inspect immutable snapshot status.
    SnapshotStatus,
    /// Submit an exact or lexical query.
    Search,
    /// Submit a graph query.
    Graph,
    /// Submit a vector query.
    Vector,
    /// Inspect local/remote placement for one snapshot.
    Locality,
    /// Inspect capability health.
    Health,
    /// Request local recovery for the selected verified bundle.
    RecoverLocal,
    /// Request recovery after a typed immutable mismatch.
    RecoverInconsistent,
    /// Release the selected local verified bundle.
    ReleaseLocal,
    /// Observe the selected operation through the service's typed operation stream.
    PollExecution,
    /// Cancel the selected operation through its typed cancellation authority.
    Cancel,
}

/// Immutable facts describing one palette command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandFacts {
    /// Stable command identity.
    pub id: CommandId,
    /// Destination selected on confirmation.
    pub destination: Route,
    /// Human-visible command label.
    pub label: &'static str,
    /// Discoverable cross-platform accelerator, when this command has one.
    pub shortcut: Option<ShortcutFacts>,
}

/// A closed rejection of an in-place palette edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteEditError {
    /// The edit supplied no visible text.
    EmptyInput,
    /// The fixed transport-width query field cannot retain the requested text.
    InputTooLong {
        /// Requested UTF-8 byte length after the edit.
        actual: usize,
        /// Fixed accepted UTF-8 byte length.
        maximum: usize,
    },
    /// The joined query length cannot be represented by `usize`.
    InputLengthOverflow {
        /// Existing query prefix byte length.
        prefix: usize,
        /// Inserted query byte length.
        inserted: usize,
        /// Existing query suffix byte length.
        suffix: usize,
    },
    /// Backspace was requested for an empty query.
    NothingToErase,
}

/// A bounded keyboard movement in the palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteDirection {
    /// Advance to the next selectable command.
    Next,
    /// Move to the previous selectable command.
    Previous,
    /// Select the first filtered command.
    First,
    /// Select the final filtered command.
    Last,
    /// Advance by the fixed visible-row count.
    NextPage,
    /// Move back by the fixed visible-row count.
    PreviousPage,
}

/// Product-owned, bounded palette state.
///
/// The selected command is an identity; a filtered display index is derived only when rendering.
/// This keeps keyboard and pointer selection stable if a future command registry changes order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandPalette {
    /// Whether the palette is currently visible.
    pub visible: bool,
    /// Stable selected command identity while the palette is visible.
    pub selected: CommandId,
    query: Option<InputText>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self {
            visible: false,
            selected: CommandId::OpenRoute(Route::Home),
            query: None,
        }
    }
}

impl CommandPalette {
    /// Opens the palette and preserves the authoritative selection identity.
    pub fn open(&mut self) {
        self.visible = true;
    }

    /// Dismisses the palette and removes its transient query.
    pub fn dismiss(&mut self) {
        self.visible = false;
        self.query = None;
    }

    /// Replaces the visible-only query after transport-width validation.
    pub fn replace_query(&mut self, query: InputText) {
        self.query = (!query.as_ref().is_empty()).then_some(query);
        self.select_first_visible();
    }

    /// Appends keyboard text to the visible-only query when the fixed UI bound permits it.
    ///
    /// # Errors
    ///
    /// Returns [`PaletteEditError`] when no state mutation is permitted by the fixed query
    /// invariant.
    pub fn append_text(&mut self, text: &str) -> Result<(), PaletteEditError> {
        self.query = append_query(self.query, text)?;
        self.select_first_visible();
        Ok(())
    }

    /// Removes the final UTF-8 character from the visible-only query.
    ///
    /// # Errors
    ///
    /// Returns [`PaletteEditError::NothingToErase`] when the canonical query is empty.
    pub fn erase_last_character(&mut self) -> Result<(), PaletteEditError> {
        self.query = erase_query_character(self.query)?;
        self.select_first_visible();
        Ok(())
    }

    fn select_first_visible(&mut self) {
        if self.selected_index().is_none()
            && let Some(command) = self.command_at(0)
        {
            self.selected = command;
        }
    }

    /// Selects one command from a pointer or virtual-list row before confirmation.
    pub fn select(&mut self, command: CommandId) {
        self.selected = command;
    }

    /// Moves the selected stable identity and returns its derived virtual row for reveal.
    #[must_use]
    pub fn move_selection(&mut self, direction: PaletteDirection) -> Option<usize> {
        let count = self.result_count();
        if count == 0 {
            return None;
        }
        let selected = self.selected_index().unwrap_or(0);
        let last = count - 1;
        let next = match direction {
            PaletteDirection::Next => selected.saturating_add(1).min(last),
            PaletteDirection::Previous => selected.saturating_sub(1),
            PaletteDirection::First => 0,
            PaletteDirection::Last => last,
            PaletteDirection::NextPage => selected.saturating_add(PALETTE_PAGE_ROWS).min(last),
            PaletteDirection::PreviousPage => selected.saturating_sub(PALETTE_PAGE_ROWS),
        };
        let command = self.command_at(next)?;
        self.selected = command;
        Some(next)
    }

    /// Selects the current command and dismisses the palette.
    #[must_use]
    pub fn confirm(&mut self) -> Option<CommandId> {
        let selected = self.selected_index()?;
        let command = self.command_at(selected)?;
        self.dismiss();
        Some(command)
    }

    /// Returns the bounded number of filtered command rows.
    #[must_use]
    pub fn result_count(self) -> usize {
        (0..COMMAND_COUNT)
            .filter_map(command_at_unfiltered)
            .filter(|command| command_matches(*command, self.query))
            .count()
    }

    /// Resolves a virtual row index to its stable command identity.
    #[must_use]
    pub fn command_at(self, wanted_index: usize) -> Option<CommandId> {
        (0..COMMAND_COUNT)
            .filter_map(command_at_unfiltered)
            .filter(|command| command_matches(*command, self.query))
            .nth(wanted_index)
    }

    /// Returns the derived row index for the selected stable command.
    #[must_use]
    pub fn selected_index(self) -> Option<usize> {
        (0..COMMAND_COUNT)
            .filter_map(command_at_unfiltered)
            .filter(|command| command_matches(*command, self.query))
            .position(|command| command == self.selected)
    }

    /// Returns the optional validated query used by the production renderer.
    #[cfg(feature = "real-gpui")]
    pub(crate) fn query_text(&self) -> Option<&str> {
        self.query.as_deref()
    }
}

/// Navigation and visible-only command-palette state for one shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NavigationState {
    /// Selected information-architecture route.
    pub route: Route,
    /// Palette state owned by this view rather than a global singleton.
    pub palette: CommandPalette,
}

impl NavigationState {
    /// Selects a route from navigation, a status item, or a confirmed command.
    pub fn select_route(&mut self, route: Route) {
        self.route = route;
    }

    /// Focuses one closed action form and selects the action's destination.
    pub fn select_action(&mut self, action: ServiceAction) {
        self.route = action_destination(action);
    }
}

fn append_query(
    current: Option<InputText>,
    appended: &str,
) -> Result<Option<InputText>, PaletteEditError> {
    if appended.is_empty() {
        return Err(PaletteEditError::EmptyInput);
    }
    let current = current.as_deref().map_or("", |current| current);
    InputText::try_from_parts(current, appended, "")
        .map(Some)
        .map_err(palette_join_error)
}

fn erase_query_character(
    current: Option<InputText>,
) -> Result<Option<InputText>, PaletteEditError> {
    let value = current.ok_or(PaletteEditError::NothingToErase)?;
    if value.is_empty() {
        return Err(PaletteEditError::NothingToErase);
    }
    let boundary = value
        .char_indices()
        .next_back()
        .map_or(0, |(boundary, _character)| boundary);
    if boundary == 0 {
        return Ok(None);
    }
    InputText::try_from_parts(&value[..boundary], "", "")
        .map(Some)
        .map_err(palette_join_error)
}

const fn palette_join_error(error: InputTextJoinError) -> PaletteEditError {
    match error {
        InputTextJoinError::LengthOverflow {
            prefix,
            inserted,
            suffix,
        } => PaletteEditError::InputLengthOverflow {
            prefix,
            inserted,
            suffix,
        },
        InputTextJoinError::InputTooLong(error) => PaletteEditError::InputTooLong {
            actual: error.actual,
            maximum: error.maximum,
        },
    }
}

pub(crate) const fn route_facts(route: Route) -> RouteFacts {
    match route {
        Route::Home => ROUTES[0],
        Route::Libraries => ROUTES[1],
        Route::Search => ROUTES[2],
        Route::Connections => ROUTES[3],
        Route::Settings => ROUTES[4],
    }
}

pub(crate) const fn command_facts(command: CommandId) -> CommandFacts {
    match command {
        CommandId::OpenRoute(route) => CommandFacts {
            id: command,
            destination: route,
            label: route_facts(route).label,
            shortcut: route_facts(route).shortcut,
        },
        CommandId::InspectSurface(surface) => CommandFacts {
            id: command,
            destination: surface_destination(surface),
            label: surface_facts(surface).label,
            shortcut: None,
        },
        CommandId::FocusAction(action) => CommandFacts {
            id: command,
            destination: action_destination(action),
            label: action_label(action),
            shortcut: None,
        },
    }
}

pub(crate) const fn surface_destination(surface: Surface) -> Route {
    match surface {
        Surface::Generation | Surface::Adaptive => Route::Home,
        Surface::Execution => Route::Connections,
        Surface::Index => Route::Libraries,
        Surface::Graph | Surface::Vector => Route::Search,
        Surface::Health => Route::Settings,
    }
}

pub(crate) const fn action_destination(action: ServiceAction) -> Route {
    match action {
        ServiceAction::Generate => Route::Home,
        ServiceAction::SnapshotStatus | ServiceAction::Locality => Route::Libraries,
        ServiceAction::Search | ServiceAction::Graph | ServiceAction::Vector => Route::Search,
        ServiceAction::Health => Route::Settings,
        ServiceAction::RecoverLocal
        | ServiceAction::RecoverInconsistent
        | ServiceAction::ReleaseLocal
        | ServiceAction::PollExecution
        | ServiceAction::Cancel => Route::Connections,
    }
}

pub(crate) const fn action_label(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Generate => "Generate package",
        ServiceAction::SnapshotStatus => "Inspect snapshot status",
        ServiceAction::Search => "Search exact and lexical",
        ServiceAction::Graph => "Search graph",
        ServiceAction::Vector => "Search vector",
        ServiceAction::Locality => "Inspect local placement",
        ServiceAction::Health => "Inspect capability health",
        ServiceAction::RecoverLocal => "Recover local analyzer",
        ServiceAction::RecoverInconsistent => "Recover immutable mismatch",
        ServiceAction::ReleaseLocal => "Release local analyzer",
        ServiceAction::PollExecution => "Observe selected operation",
        ServiceAction::Cancel => "Cancel selected operation",
    }
}

pub(crate) const fn surface_facts(surface: Surface) -> SurfaceFacts {
    match surface {
        Surface::Generation => SURFACES[0],
        Surface::Adaptive => SURFACES[1],
        Surface::Execution => SURFACES[2],
        Surface::Index => SURFACES[3],
        Surface::Graph => SURFACES[4],
        Surface::Vector => SURFACES[5],
        Surface::Health => SURFACES[6],
    }
}

/// Immutable visual facts for one application surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceFacts {
    /// Stable surface identity.
    pub surface: Surface,
    /// Human-visible label.
    pub label: &'static str,
    /// Stable rendered element identity.
    pub element_id: &'static str,
}

/// Stable product surface presentation order.
pub const SURFACES: [SurfaceFacts; 7] = [
    SurfaceFacts {
        surface: Surface::Generation,
        label: "Generation",
        element_id: "generation",
    },
    SurfaceFacts {
        surface: Surface::Adaptive,
        label: "Placement",
        element_id: "adaptive",
    },
    SurfaceFacts {
        surface: Surface::Execution,
        label: "Execution",
        element_id: "execution",
    },
    SurfaceFacts {
        surface: Surface::Index,
        label: "Index",
        element_id: "index",
    },
    SurfaceFacts {
        surface: Surface::Graph,
        label: "Graph",
        element_id: "graph",
    },
    SurfaceFacts {
        surface: Surface::Vector,
        label: "Vector",
        element_id: "vector",
    },
    SurfaceFacts {
        surface: Surface::Health,
        label: "Health",
        element_id: "health",
    },
];

const COMMAND_COUNT: usize = 24;

const fn command_at_unfiltered(index: usize) -> Option<CommandId> {
    match index {
        0 => Some(CommandId::OpenRoute(Route::Home)),
        1 => Some(CommandId::OpenRoute(Route::Libraries)),
        2 => Some(CommandId::OpenRoute(Route::Search)),
        3 => Some(CommandId::OpenRoute(Route::Connections)),
        4 => Some(CommandId::OpenRoute(Route::Settings)),
        5 => Some(CommandId::InspectSurface(Surface::Generation)),
        6 => Some(CommandId::InspectSurface(Surface::Adaptive)),
        7 => Some(CommandId::InspectSurface(Surface::Execution)),
        8 => Some(CommandId::InspectSurface(Surface::Index)),
        9 => Some(CommandId::InspectSurface(Surface::Graph)),
        10 => Some(CommandId::InspectSurface(Surface::Vector)),
        11 => Some(CommandId::InspectSurface(Surface::Health)),
        12 => Some(CommandId::FocusAction(ServiceAction::Generate)),
        13 => Some(CommandId::FocusAction(ServiceAction::SnapshotStatus)),
        14 => Some(CommandId::FocusAction(ServiceAction::Search)),
        15 => Some(CommandId::FocusAction(ServiceAction::Graph)),
        16 => Some(CommandId::FocusAction(ServiceAction::Vector)),
        17 => Some(CommandId::FocusAction(ServiceAction::Locality)),
        18 => Some(CommandId::FocusAction(ServiceAction::Health)),
        19 => Some(CommandId::FocusAction(ServiceAction::RecoverLocal)),
        20 => Some(CommandId::FocusAction(ServiceAction::RecoverInconsistent)),
        21 => Some(CommandId::FocusAction(ServiceAction::ReleaseLocal)),
        22 => Some(CommandId::FocusAction(ServiceAction::PollExecution)),
        23 => Some(CommandId::FocusAction(ServiceAction::Cancel)),
        _ => None,
    }
}

fn command_matches(command: CommandId, query: Option<InputText>) -> bool {
    let needle = query.as_ref().map_or(&[][..], |value| value.as_ref());
    if needle.is_empty() {
        return true;
    }
    let facts = command_facts(command);
    if facts
        .label
        .as_bytes()
        .windows(needle.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
    {
        return true;
    }
    false
}
