//! Stable product navigation and command-palette selection state.

use wave_application_core::InputText;

/// Upper bound for commands displayed by the palette before a caller offers a narrower query.
pub const MAX_PALETTE_RESULTS: usize = 100;

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

impl Route {
    /// Returns the user-facing route name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Libraries => "Libraries",
            Self::Search => "Search",
            Self::Connections => "Connections",
            Self::Settings => "Settings",
        }
    }

    /// Returns the stable GPUI element identity for this route.
    #[must_use]
    pub const fn element_id(self) -> &'static str {
        match self {
            Self::Home => "route-home",
            Self::Libraries => "route-libraries",
            Self::Search => "route-search",
            Self::Connections => "route-connections",
            Self::Settings => "route-settings",
        }
    }
}

/// A stable, non-positional palette command identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandId {
    /// Open the Home route.
    OpenHome,
    /// Open local library management.
    OpenLibraries,
    /// Open retrieval.
    OpenSearch,
    /// Open AI-client configuration.
    OpenConnections,
    /// Open application settings.
    OpenSettings,
}

impl CommandId {
    /// Returns the command's stable visible label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenHome => "Open Home",
            Self::OpenLibraries => "Open Libraries",
            Self::OpenSearch => "Open Search",
            Self::OpenConnections => "Connect Claude",
            Self::OpenSettings => "Open Settings",
        }
    }

    const fn route(self) -> Route {
        match self {
            Self::OpenHome => Route::Home,
            Self::OpenLibraries => Route::Libraries,
            Self::OpenSearch => Route::Search,
            Self::OpenConnections => Route::Connections,
            Self::OpenSettings => Route::Settings,
        }
    }
}

const COMMANDS: [CommandId; 5] = [
    CommandId::OpenHome,
    CommandId::OpenLibraries,
    CommandId::OpenSearch,
    CommandId::OpenConnections,
    CommandId::OpenSettings,
];

/// A bounded keyboard movement in the palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteDirection {
    /// Advance to the next selectable command.
    Next,
    /// Move to the previous selectable command.
    Previous,
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
    query: PaletteQuery,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self {
            visible: false,
            selected: CommandId::OpenHome,
            query: PaletteQuery::default(),
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
        self.query = PaletteQuery::default();
    }

    /// Replaces the visible-only query after transport-width validation.
    ///
    pub fn replace_query(&mut self, query: InputText) {
        self.query = PaletteQuery::from(query);
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

    /// Advances the selected command and returns its derived row index for reveal.
    #[must_use]
    pub fn move_selection(&mut self, direction: PaletteDirection) -> Option<usize> {
        let count = self.result_count();
        if count == 0 {
            return None;
        }
        let selected = self.selected_index().unwrap_or(0);
        let next = match direction {
            PaletteDirection::Next => (selected + 1) % count,
            PaletteDirection::Previous => (selected + count - 1) % count,
        };
        let command = self.command_at(next)?;
        self.selected = command;
        Some(next)
    }

    /// Selects the current command's route and dismisses the palette.
    #[must_use]
    pub fn confirm(&mut self) -> Option<Route> {
        let selected = self.selected_index()?;
        let command = self.command_at(selected)?;
        self.dismiss();
        Some(command.route())
    }

    /// Returns the bounded number of filtered command rows.
    #[must_use]
    pub fn result_count(self) -> usize {
        COMMANDS
            .into_iter()
            .filter(|command| command_matches(*command, self.query))
            .take(MAX_PALETTE_RESULTS)
            .count()
    }

    /// Resolves a virtual row index to its stable command identity.
    #[must_use]
    pub fn command_at(self, wanted_index: usize) -> Option<CommandId> {
        COMMANDS
            .into_iter()
            .filter(|command| command_matches(*command, self.query))
            .nth(wanted_index)
    }

    /// Returns the derived row index for the selected stable command.
    #[must_use]
    pub fn selected_index(self) -> Option<usize> {
        COMMANDS
            .into_iter()
            .filter(|command| command_matches(*command, self.query))
            .position(|command| command == self.selected)
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaletteQuery {
    bytes: [u8; wave_application_core::INPUT_TEXT_BYTES],
    length: usize,
}

impl Default for PaletteQuery {
    fn default() -> Self {
        Self {
            bytes: [0; wave_application_core::INPUT_TEXT_BYTES],
            length: 0,
        }
    }
}

impl From<InputText> for PaletteQuery {
    fn from(value: InputText) -> Self {
        let input = value.as_ref();
        let mut bytes = [0; wave_application_core::INPUT_TEXT_BYTES];
        bytes[..input.len()].copy_from_slice(input);
        Self {
            bytes,
            length: input.len(),
        }
    }
}

fn command_matches(command: CommandId, query: PaletteQuery) -> bool {
    let needle = &query.bytes[..query.length];
    if needle.is_empty() {
        return true;
    }
    let label = command.label().as_bytes();
    label
        .windows(needle.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
}
