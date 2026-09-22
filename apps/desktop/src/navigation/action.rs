//! Stable actions, semantic families, accessibility metadata, and palette data.

use super::intent::Intent;
use super::route::SettingsPage;
use crate::core::ids::DocumentId;

/// Stable action IDs are persisted in keymaps and harness traces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u16)]
pub enum ActionId {
    /// Navigate home.
    OpenHome = 1,
    /// Move one route level up.
    ZoomOut = 2,
    /// Navigate backward.
    Back = 3,
    /// Navigate forward.
    Forward = 4,
    /// Focus the command palette.
    OpenCommandPalette = 5,
    /// Dismiss the top overlay.
    DismissOverlay = 6,
    /// Open settings.
    OpenSettings = 7,
    /// Toggle reduced motion.
    ToggleReducedMotion = 8,
    /// Move selection up.
    MoveUp = 9,
    /// Move selection down.
    MoveDown = 10,
    /// Activate the focused control.
    Activate = 11,
    /// Stop a background request.
    Stop = 12,
    /// Open the native project folder picker.
    AddProject = 13,
    /// Collapse or expand the project shelf.
    ToggleShelf = 14,
    /// Open keyboard and CLI help.
    OpenHelp = 15,
    /// Switch the surface appearance.
    ToggleAppearance = 16,
    /// Switch local-only and registry metadata policy.
    TogglePrivacy = 17,
    /// Collapse or expand the context panel.
    ToggleContext = 18,
}

impl ActionId {
    /// All actions in stable registry order.
    pub const ALL: [Self; 18] = [
        Self::OpenHome,
        Self::ZoomOut,
        Self::Back,
        Self::Forward,
        Self::OpenCommandPalette,
        Self::DismissOverlay,
        Self::OpenSettings,
        Self::ToggleReducedMotion,
        Self::MoveUp,
        Self::MoveDown,
        Self::Activate,
        Self::Stop,
        Self::AddProject,
        Self::ToggleShelf,
        Self::OpenHelp,
        Self::ToggleAppearance,
        Self::TogglePrivacy,
        Self::ToggleContext,
    ];

    /// Rehydrates one action from the stable keyboard binding id.
    #[must_use]
    pub const fn from_u16(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::OpenHome,
            2 => Self::ZoomOut,
            3 => Self::Back,
            4 => Self::Forward,
            5 => Self::OpenCommandPalette,
            6 => Self::DismissOverlay,
            7 => Self::OpenSettings,
            8 => Self::ToggleReducedMotion,
            9 => Self::MoveUp,
            10 => Self::MoveDown,
            11 => Self::Activate,
            12 => Self::Stop,
            13 => Self::AddProject,
            14 => Self::ToggleShelf,
            15 => Self::OpenHelp,
            16 => Self::ToggleAppearance,
            17 => Self::TogglePrivacy,
            18 => Self::ToggleContext,
            _ => return None,
        })
    }

    /// Stable persisted spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenHome => "open-home",
            Self::ZoomOut => "zoom-out",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::OpenCommandPalette => "open-command-palette",
            Self::DismissOverlay => "dismiss-overlay",
            Self::OpenSettings => "open-settings",
            Self::ToggleReducedMotion => "toggle-reduced-motion",
            Self::MoveUp => "move-up",
            Self::MoveDown => "move-down",
            Self::Activate => "activate",
            Self::Stop => "stop",
            Self::AddProject => "add-project",
            Self::ToggleShelf => "toggle-shelf",
            Self::OpenHelp => "open-help",
            Self::ToggleAppearance => "toggle-appearance",
            Self::TogglePrivacy => "toggle-privacy",
            Self::ToggleContext => "toggle-context",
        }
    }

    /// Returns the semantic action family.
    #[must_use]
    pub const fn family(self) -> SemanticFamily {
        match self {
            Self::OpenHome | Self::ZoomOut | Self::Back | Self::Forward => {
                SemanticFamily::Navigation
            }
            Self::OpenCommandPalette | Self::OpenSettings | Self::AddProject | Self::OpenHelp => {
                SemanticFamily::Command
            }
            Self::DismissOverlay
            | Self::ToggleReducedMotion
            | Self::ToggleShelf
            | Self::ToggleAppearance
            | Self::TogglePrivacy
            | Self::ToggleContext => SemanticFamily::Shell,
            Self::MoveUp | Self::MoveDown | Self::Activate => SemanticFamily::Selection,
            Self::Stop => SemanticFamily::Engine,
        }
    }

    /// Returns the one voice channel used by this action.
    #[must_use]
    pub const fn voice(self) -> VoiceChannel {
        match self {
            Self::Stop => VoiceChannel::Stop,
            Self::MoveUp | Self::MoveDown => VoiceChannel::Focus,
            Self::Activate
            | Self::OpenCommandPalette
            | Self::OpenSettings
            | Self::AddProject
            | Self::OpenHelp
            | Self::ToggleAppearance
            | Self::TogglePrivacy
            | Self::ToggleShelf
            | Self::ToggleContext => VoiceChannel::Action,
            Self::OpenHome
            | Self::ZoomOut
            | Self::Back
            | Self::Forward
            | Self::DismissOverlay
            | Self::ToggleReducedMotion => VoiceChannel::Wait,
        }
    }

    /// Returns an action specification for palette and accessibility output.
    #[must_use]
    pub const fn spec(self) -> ActionSpec {
        ActionSpec {
            id: self,
            family: self.family(),
            voice: self.voice(),
            label: self.label(),
            role: AccessibilityRole::Button,
            shortcut: self.shortcut(),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::OpenHome => "Open home",
            Self::ZoomOut => "Zoom out",
            Self::Back => "Go back",
            Self::Forward => "Go forward",
            Self::OpenCommandPalette => "Open command palette",
            Self::DismissOverlay => "Dismiss overlay",
            Self::OpenSettings => "Open settings",
            Self::ToggleReducedMotion => "Toggle reduced motion",
            Self::MoveUp => "Move up",
            Self::MoveDown => "Move down",
            Self::Activate => "Activate",
            Self::Stop => "Stop request",
            Self::AddProject => "Add project",
            Self::ToggleShelf => "Toggle project shelf",
            Self::OpenHelp => "Open keyboard and CLI help",
            Self::ToggleAppearance => "Toggle appearance",
            Self::TogglePrivacy => "Toggle privacy policy",
            Self::ToggleContext => "Toggle context panel",
        }
    }

    const fn shortcut(self) -> Option<KeyChord> {
        match self {
            Self::OpenHome => Some(KeyChord::new("cmd-0")),
            Self::ZoomOut => Some(KeyChord::new("cmd-minus")),
            Self::Back => Some(KeyChord::new("cmd-left")),
            Self::Forward => Some(KeyChord::new("cmd-right")),
            Self::OpenCommandPalette => Some(KeyChord::new("cmd-shift-p")),
            Self::DismissOverlay => Some(KeyChord::new("escape")),
            Self::OpenSettings => Some(KeyChord::new("cmd-,")),
            Self::ToggleReducedMotion => Some(KeyChord::new("cmd-shift-m")),
            Self::MoveUp => Some(KeyChord::new("up")),
            Self::MoveDown => Some(KeyChord::new("down")),
            Self::Activate => Some(KeyChord::new("enter")),
            Self::Stop => Some(KeyChord::new("escape")),
            Self::AddProject => Some(KeyChord::new("cmd-n")),
            Self::ToggleShelf => Some(KeyChord::new("cmd-b")),
            Self::OpenHelp => Some(KeyChord::new("cmd-shift-/")),
            Self::ToggleAppearance => Some(KeyChord::new("cmd-shift-a")),
            Self::TogglePrivacy => Some(KeyChord::new("cmd-shift-y")),
            Self::ToggleContext => Some(KeyChord::new("cmd-backslash")),
        }
    }

    /// Converts an action that needs no payload into a typed intent.
    #[must_use]
    pub const fn intent(self) -> Option<Intent> {
        match self {
            Self::OpenHome => Some(Intent::Navigate(super::route::Route::Orbit(
                super::route::OrbitRoute::Home,
            ))),
            Self::ZoomOut => Some(Intent::ZoomOut),
            Self::Back => Some(Intent::Back),
            Self::Forward => Some(Intent::Forward),
            Self::OpenCommandPalette => Some(Intent::OpenCommandPalette),
            Self::DismissOverlay => Some(Intent::DismissOverlay),
            Self::OpenSettings => Some(Intent::OpenSettings(SettingsPage::Appearance)),
            Self::ToggleReducedMotion => Some(Intent::ToggleReducedMotion),
            Self::AddProject => Some(Intent::OpenFolderPicker),
            Self::ToggleShelf => Some(Intent::ToggleShelf),
            Self::OpenHelp => Some(Intent::OpenHelp),
            Self::ToggleAppearance => Some(Intent::ToggleAppearance),
            Self::TogglePrivacy => Some(Intent::TogglePrivacy),
            Self::ToggleContext => Some(Intent::ToggleContext),
            Self::MoveUp | Self::MoveDown | Self::Activate => None,
            Self::Stop => Some(Intent::Stop),
        }
    }
}

/// Closed semantic action families for color/voice policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticFamily {
    /// Route and zoom actions.
    Navigation,
    /// Palette and settings actions.
    Command,
    /// Shell state actions.
    Shell,
    /// Selection movement and activation.
    Selection,
    /// Engine lifecycle actions.
    Engine,
}

/// The four product voice channels.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VoiceChannel {
    /// A user action was accepted.
    Action,
    /// Focus moved.
    Focus,
    /// The system is waiting for a result.
    Wait,
    /// A request or run was stopped.
    Stop,
}

/// Closed semantic state channel. Bevel treatment belongs only to this channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticState {
    /// Dense data in the untouched state.
    Plain,
    /// Pointer is over the control.
    Hovered,
    /// Keyboard focus is visible.
    Focused,
    /// Control is selected.
    Selected,
    /// Work is pending.
    Loading,
    /// Work failed.
    Error,
    /// Resource is unavailable.
    Unavailable,
}

/// Closed accessibility role vocabulary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AccessibilityRole {
    /// A button.
    Button,
    /// A tab.
    Tab,
    /// A dialog.
    Dialog,
    /// A list or collection.
    List,
    /// A list row.
    ListItem,
    /// A text input.
    TextInput,
}

/// Stable key chord spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyChord(&'static str);

impl KeyChord {
    const fn new(value: &'static str) -> Self {
        Self(value)
    }

    /// Returns the stable key spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Accessibility and command palette metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ActionSpec {
    /// Stable action ID.
    pub id: ActionId,
    /// Semantic family.
    pub family: SemanticFamily,
    /// Voice channel.
    pub voice: VoiceChannel,
    /// Accessible and palette label.
    pub label: &'static str,
    /// Accessible role.
    pub role: AccessibilityRole,
    /// Optional shortcut.
    pub shortcut: Option<KeyChord>,
}

/// One semantic action node published by the shell.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ActionNode {
    /// Stable metadata.
    pub spec: ActionSpec,
    /// Current semantic state.
    pub state: SemanticState,
    /// Whether activation is allowed.
    pub enabled: bool,
    /// Stable traversal order.
    pub focus_order: u16,
}

/// Command palette query state; actions remain typed even when filtered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteState {
    /// Whether the palette is visible.
    pub open: bool,
    /// Current free-text filter.
    pub query: String,
    /// Stable selected action.
    pub selected: Option<ActionId>,
    /// Matching actions in registry order.
    pub results: Vec<ActionId>,
}

impl Default for CommandPaletteState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: None,
            results: ActionId::ALL.to_vec(),
        }
    }
}

impl CommandPaletteState {
    /// Filters the closed action registry by a user query.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        let query = self.query.to_ascii_lowercase();
        self.results = ActionId::ALL
            .into_iter()
            .filter(|action| action.spec().label.to_ascii_lowercase().contains(&query))
            .collect();
        self.selected = self.results.first().copied();
    }

    /// Returns the currently selected action.
    #[must_use]
    pub const fn selected(&self) -> Option<ActionId> {
        self.selected
    }
}

impl crate::core::ActionCatalog for CommandPaletteState {
    fn actions(&self) -> &[ActionId] {
        &self.results
    }
}

/// A typed document focus target used by key dispatch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DocumentTarget(pub DocumentId);
