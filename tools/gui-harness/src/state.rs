//! The typed state space exercised by the desktop screenshot suite.
//!
//! A visual suite is only useful when it can say what was rendered.  These
//! identifiers are intentionally closed and stable: adding a desktop state
//! requires adding it here, so the state manifest and the generated artifact
//! set cannot silently drift apart.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

/// Every durable page that the desktop can show in its reader.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageState {
    /// The home/browse surface.
    Browse,
    /// A project on the local shelf.
    Project,
    /// A package resolved from the registry.
    Package,
    /// A declaration page with signature, documentation, members, and links.
    Declaration,
    /// A declaration whose source has been opened as a sheet.
    Source,
    /// A source code view reached from a package or declaration.
    Code,
    /// Rendered documentation for a package or declaration.
    Docs,
    /// Rich-IR graph view.
    Graph,
    /// Direct dependency view.
    Dependencies,
    /// Reverse dependency view.
    Dependents,
    /// Registry release history.
    Releases,
    /// Security and advisory view.
    Security,
    /// Search results for source/code symbols.
    CodeSearch,
}

impl PageState {
    /// All page states in reader order.
    pub const ALL: [Self; 13] = [
        Self::Browse,
        Self::Project,
        Self::Package,
        Self::Declaration,
        Self::Source,
        Self::Code,
        Self::Docs,
        Self::Graph,
        Self::Dependencies,
        Self::Dependents,
        Self::Releases,
        Self::Security,
        Self::CodeSearch,
    ];

    /// Stable command-line spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Project => "project",
            Self::Package => "package",
            Self::Declaration => "declaration",
            Self::Source => "source",
            Self::Code => "code",
            Self::Docs => "docs",
            Self::Graph => "graph",
            Self::Dependencies => "dependencies",
            Self::Dependents => "dependents",
            Self::Releases => "releases",
            Self::Security => "security",
            Self::CodeSearch => "code-search",
        }
    }
}

/// Transient or exceptional surfaces layered over a page.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayState {
    /// The search/omnibar field is focused and open.
    Omnibar,
    /// The command palette/omnibar is open.
    Palette,
    /// The settings sheet is open on its appearance page.
    SettingsAppearance,
    /// The settings sheet is open on its editor page.
    SettingsEditor,
    /// The settings sheet is open on its MCP agents page.
    SettingsAgents,
    /// The settings sheet is open on its diagnostics page.
    SettingsDiagnostics,
    /// The settings sheet is open on its legend page.
    SettingsLegend,
    /// A source or hover request is pending and its reserved geometry is shown.
    Loading,
    /// An in-place fault with recovery affordances.
    Fault,
    /// A transient confirmation notice.
    Notice,
    /// Settings page for package indexing and ingest status.
    SettingsIndex,
    /// Settings page for registry selection and cache policy.
    SettingsRegistry,
    /// A valid route with no rows or recent content.
    Empty,
    /// A bounded projection with incomplete coverage.
    Partial,
    /// A disconnected route with its last valid projection retained.
    Offline,
    /// A capability unavailable for the selected ecosystem.
    Unavailable,
    /// A projection older than the admitted service revision.
    Stale,
    /// A registry package marked yanked.
    Yanked,
    /// A package or release carrying a vulnerability finding.
    Vulnerable,
    /// An indexing job is actively progressing.
    Indexing,
}

impl OverlayState {
    /// All overlays in the order used by the desktop state sweep.
    pub const ALL: [Self; 20] = [
        Self::Omnibar,
        Self::Palette,
        Self::SettingsAppearance,
        Self::SettingsEditor,
        Self::SettingsAgents,
        Self::SettingsDiagnostics,
        Self::SettingsLegend,
        Self::Loading,
        Self::Fault,
        Self::Notice,
        Self::SettingsIndex,
        Self::SettingsRegistry,
        Self::Empty,
        Self::Partial,
        Self::Offline,
        Self::Unavailable,
        Self::Stale,
        Self::Yanked,
        Self::Vulnerable,
        Self::Indexing,
    ];

    /// Stable command-line spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Omnibar => "omnibar",
            Self::Palette => "palette",
            Self::SettingsAppearance => "settings-appearance",
            Self::SettingsEditor => "settings-editor",
            Self::SettingsAgents => "settings-agents",
            Self::SettingsDiagnostics => "settings-diagnostics",
            Self::SettingsLegend => "settings-legend",
            Self::Loading => "loading",
            Self::Fault => "fault",
            Self::Notice => "notice",
            Self::SettingsIndex => "settings-index",
            Self::SettingsRegistry => "settings-registry",
            Self::Empty => "empty",
            Self::Partial => "partial",
            Self::Offline => "offline",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Yanked => "yanked",
            Self::Vulnerable => "vulnerable",
            Self::Indexing => "indexing",
        }
    }
}

/// The region that owns keyboard focus.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum FocusState {
    /// The titlebar's omnibar input.
    Omnibar,
    /// The project/library rail.
    Library,
    /// The reader body.
    #[default]
    Reader,
}

impl FocusState {
    /// All focus owners in tab order.
    pub const ALL: [Self; 3] = [Self::Omnibar, Self::Library, Self::Reader];

    /// Stable command-line spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Omnibar => "omnibar",
            Self::Library => "library",
            Self::Reader => "reader",
        }
    }
}

/// Palette used by the design system.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeState {
    /// Dark ink appearance.
    #[default]
    Ink,
    /// Light vellum appearance.
    Vellum,
}

impl ThemeState {
    /// All supported themes.
    pub const ALL: [Self; 2] = [Self::Ink, Self::Vellum];

    /// Stable command-line spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ink => "ink",
            Self::Vellum => "vellum",
        }
    }
}

/// One complete visual state.  It is sufficient to seed a desktop adapter
/// without relying on ambient service, clock, focus, or theme state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuiState {
    /// Stable state identifier used in artifact paths and reports.
    pub id: String,
    /// Reader page, when a page is visible.
    pub page: Option<PageState>,
    /// Overlay, when a transient surface is visible.
    pub overlay: Option<OverlayState>,
    /// Keyboard owner at the instant of capture.
    pub focus: FocusState,
    /// Color appearance.
    pub theme: ThemeState,
    /// Whether reduced motion is enabled.
    pub reduced_motion: bool,
}

impl GuiState {
    /// Creates a state from a stable id and explicit properties.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        page: Option<PageState>,
        overlay: Option<OverlayState>,
    ) -> Self {
        Self {
            id: id.into(),
            page,
            overlay,
            focus: FocusState::default(),
            theme: ThemeState::default(),
            reduced_motion: false,
        }
    }

    /// Produces the deterministic catalog of baseline states.
    #[must_use]
    pub fn catalog() -> Vec<Self> {
        let mut states = Vec::new();
        for page in PageState::ALL {
            states.push(Self::new(page.as_str(), Some(page), None));
            // These surfaces are global shell controls and are legal over
            // every registered reader route. Data-dependent conditions such
            // as offline, loading, fault, empty, and indexing are deliberately
            // absent until a live adapter can inject the corresponding
            // admitted condition; a cross-product sweep would manufacture
            // states the product cannot honestly reach.
            for overlay in [
                OverlayState::Omnibar,
                OverlayState::Palette,
                OverlayState::SettingsAppearance,
                OverlayState::SettingsEditor,
                OverlayState::SettingsAgents,
                OverlayState::SettingsDiagnostics,
                OverlayState::SettingsLegend,
            ] {
                let id = format!("{}--{}", page.as_str(), overlay.as_str());
                states.push(Self::new(id, Some(page), Some(overlay)));
            }
        }
        states.extend([
            Self::new("shell--ink", Some(PageState::Browse), None),
            Self {
                id: "shell--vellum".to_owned(),
                theme: ThemeState::Vellum,
                ..Self::new("shell--vellum", Some(PageState::Browse), None)
            },
            Self {
                id: "shell--reduced-motion".to_owned(),
                reduced_motion: true,
                ..Self::new("shell--reduced-motion", Some(PageState::Browse), None)
            },
        ]);
        states
    }

    /// Returns a state with a different focus owner.
    #[must_use]
    pub const fn with_focus(mut self, focus: FocusState) -> Self {
        self.focus = focus;
        self
    }

    /// Returns a state with a different theme.
    #[must_use]
    pub const fn with_theme(mut self, theme: ThemeState) -> Self {
        self.theme = theme;
        self
    }

    /// Returns a state with reduced motion enabled or disabled.
    #[must_use]
    pub const fn with_reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.reduced_motion = reduced_motion;
        self
    }
}

/// Error returned when a state id or catalog is malformed.
#[derive(Debug, Error)]
pub enum StateError {
    /// A requested state id is absent from the catalog.
    #[error("unknown GUI state {0:?}")]
    Unknown(String),
    /// Two states share one artifact id.
    #[error("duplicate GUI state id {0:?}")]
    Duplicate(String),
    /// A page/overlay pair is not a legal route in the production shell.
    #[error("invalid GUI state combination {0:?}")]
    Impossible(String),
}

/// Parses a state id against the canonical catalog.
pub fn parse_state(id: &str) -> Result<GuiState, StateError> {
    GuiState::catalog()
        .into_iter()
        .find(|state| state.id == id)
        .ok_or_else(|| StateError::Unknown(id.to_owned()))
}

/// Validates state ids, required page/overlay combinations, and duplicates.
pub fn validate_catalog(states: &[GuiState]) -> Result<(), StateError> {
    let mut ids = std::collections::BTreeSet::new();
    for state in states {
        if !ids.insert(state.id.clone()) {
            return Err(StateError::Duplicate(state.id.clone()));
        }
        if state.page.is_none() && state.overlay.is_none() {
            return Err(StateError::Unknown(format!(
                "{} has neither a page nor an overlay",
                state.id
            )));
        }
        if state.overlay.is_some_and(|overlay| {
            !matches!(
                overlay,
                OverlayState::Omnibar
                    | OverlayState::Palette
                    | OverlayState::SettingsAppearance
                    | OverlayState::SettingsEditor
                    | OverlayState::SettingsAgents
                    | OverlayState::SettingsDiagnostics
                    | OverlayState::SettingsLegend
            )
        }) {
            return Err(StateError::Impossible(state.id.clone()));
        }
    }
    Ok(())
}

impl fmt::Display for PageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PageState {
    type Err = StateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
            .ok_or_else(|| StateError::Unknown(value.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_unique_and_complete() {
        let states = GuiState::catalog();
        assert_eq!(
            states.len(),
            PageState::ALL.len() + (PageState::ALL.len() * 7) + 3
        );
        validate_catalog(&states).expect("catalog should be valid");
        assert!(
            states
                .iter()
                .any(|state| state.id == "package--settings-agents")
        );
        assert!(!states.iter().any(|state| state.id == "package--loading"));
        assert!(states.iter().any(|state| state.id == "shell--vellum"));
    }

    #[test]
    fn parser_is_closed_over_the_catalog() {
        for state in GuiState::catalog() {
            assert_eq!(parse_state(&state.id).expect("state should parse"), state);
        }
        assert!(matches!(
            parse_state("made-up"),
            Err(StateError::Unknown(_))
        ));
    }
}
