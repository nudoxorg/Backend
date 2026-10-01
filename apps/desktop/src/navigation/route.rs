//! Typed zoom/descent routes.
//!
//! Every content route has one of four depths: Orbit → Package → Page →
//! Source.  A route carries its stable coordinate and selected identity, so a
//! zoom-out operation can retain the selected thing without parsing a URL.

use crate::core::{DocumentId, PackageId, ProjectId};
use crate::model::ObjectId;
use std::fmt;
use std::sync::Arc;

/// A validated source coordinate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Coordinate(Arc<str>);

impl Coordinate {
    /// Creates a coordinate from a non-empty producer spelling.
    pub fn new(value: &str) -> Result<Self, CoordinateError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(CoordinateError::Empty);
        }
        if value.chars().any(char::is_control) {
            return Err(CoordinateError::ControlCharacter);
        }
        Ok(Self(Arc::from(value)))
    }

    /// Returns the stable coordinate spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Coordinate validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinateError {
    /// No coordinate was supplied.
    Empty,
    /// The coordinate cannot cross the persistence or accessibility boundary.
    ControlCharacter,
    /// A release contains an address separator or exceeds its size bound.
    InvalidRelease,
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("coordinate must not be empty"),
            Self::ControlCharacter => f.write_str("coordinate contains a control character"),
            Self::InvalidRelease => f.write_str("release contains an address separator or is too long"),
        }
    }
}

impl std::error::Error for CoordinateError {}

/// The four content depths used for zoom and descent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RouteDepth {
    /// Project/shelf orbit.
    Orbit,
    /// Package overview and package lanes.
    Package,
    /// A declaration/document page.
    Page,
    /// Captured source.
    Source,
}

/// The orbit-level route.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OrbitRoute {
    /// The shelf/home orbit.
    Home,
    /// A selected local or service project.
    Project(ProjectId),
    /// A browsing page at the orbit's depth: a project's tree (find and
    /// compare next). The shell's `Route::Orbit(_)` arms already hold it.
    Browse(super::BrowseRoute),
}

/// One immutable release of a package. Ecosystem-specific admission happens
/// at the package read boundary, where the producer's identity is available.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReleaseId {
    /// The producer confirmed this exact version for its package.
    Admitted(Arc<str>),
    /// Safe saved or requested text awaiting package-specific validation.
    Unresolved(Arc<str>),
    /// The saved spelling is unsafe; keep it for recovery but never read it.
    InvalidSaved(Arc<str>),
}

impl ReleaseId {
    /// Admits one version spelling.
    pub fn new(value: &str) -> Result<Self, CoordinateError> {
        if value.trim().is_empty() {
            return Err(CoordinateError::Empty);
        }
        if value.chars().any(char::is_control) {
            return Err(CoordinateError::ControlCharacter);
        }
        if value != value.trim()
            || value.len() > 256
            || value.chars().any(|part| matches!(part, '/' | '\\' | '?' | '#' | '@'))
        {
            return Err(CoordinateError::InvalidRelease);
        }
        Ok(Self::Unresolved(Arc::from(value)))
    }

    /// Retains an invalid saved address as unread instead of treating it as the working copy.
    #[must_use]
    pub(crate) fn from_persisted(value: &str) -> Self {
        Self::new(value).unwrap_or_else(|_| Self::InvalidSaved(Arc::from(value)))
    }

    /// Whether this release can be used as an index address.
    #[must_use]
    pub const fn is_valid(&self) -> bool { !matches!(self, Self::InvalidSaved(_)) }

    /// Exact saved spelling, including an invalid one for recovery.
    #[must_use]
    pub(crate) fn persisted_wire(&self) -> &str {
        match self { Self::Admitted(value) | Self::Unresolved(value) | Self::InvalidSaved(value) => value }
    }

    /// Returns the version spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        if self.is_valid() { self.persisted_wire() } else { "invalid saved release" }
    }
}

/// Package-level route data.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageRoute {
    /// Optional project selected at the orbit level.
    pub project: Option<ProjectId>,
    /// Stable package coordinate (the release you pin).
    pub package: PackageId,
    /// The selected package lane.
    pub lane: PackageLane,
    /// Selected package object retained when zooming out.
    pub selected: Option<ObjectId>,
    /// The release being viewed, when it is not the pinned one.
    pub at: Option<ReleaseId>,
}

/// Package lanes are closed typed data, not display strings.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackageLane {
    /// Package summary.
    Overview,
    /// Dependency graph.
    Dependencies,
    /// Reverse dependency graph.
    Dependents,
    /// Release history.
    Releases,
    /// Advisory surface.
    Security,
}

/// The three ways a declaration is shown. Switching between them never
/// pushes history: it replaces the current entry's view.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum View {
    /// The structured, language-neutral anatomy.
    #[default]
    Page,
    /// The source text.
    Code,
    /// The dependency graph centred on the declaration.
    Graph,
}

impl View {
    /// Every view, in switcher order.
    pub const ALL: [Self; 3] = [Self::Page, Self::Code, Self::Graph];

    /// Stable lowercase spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Code => "code",
            Self::Graph => "graph",
        }
    }

    /// Parses the stable spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|view| view.as_str() == value)
    }
}

/// One declaration, shown one of three ways, optionally at another release.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolRoute {
    /// Optional project selected at the orbit level.
    pub project: Option<ProjectId>,
    /// Parent package coordinate (the release you pin).
    pub package: PackageId,
    /// Stable declaration coordinate.
    pub id: Coordinate,
    /// The release being viewed, when it is not the pinned one.
    pub at: Option<ReleaseId>,
    /// How it is shown.
    pub view: View,
    /// The source line the code view opens at (the declaration's own when
    /// `None`).
    pub line: Option<u32>,
    /// Selected object retained on zoom-out.
    pub selected: Option<ObjectId>,
}

impl SymbolRoute {
    /// Whether `other` shows the same declaration at the same release (only
    /// the view or line differ): moving between them is not navigation.
    #[must_use]
    pub fn same_place(&self, other: &Self) -> bool {
        self.package == other.package && self.id == other.id && self.at == other.at
    }
}

/// Typed application content route.
///
/// Settings and command palette state lives in [`crate::model::SessionState`]
/// as an orthogonal overlay. Keeping it out of this enum means opening a
/// transient surface cannot mutate content history or change route depth.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Route {
    /// Orbit-level content.
    Orbit(OrbitRoute),
    /// Package-level content.
    Package(PackageRoute),
    /// One declaration, as a page, its code, or its graph.
    Symbol(SymbolRoute),
    /// The whole dependency graph, nothing selected.
    World,
}

/// A transient shell surface layered over a content route.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Overlay {
    /// Keyboard-first local project admission surface.
    AddProject,
    /// Settings surface at a typed page.
    Settings(SettingsPage),
    /// Command palette surface (Ask, ⌘K).
    CommandPalette,
    /// Followed releases: the calm inbox.
    Inbox,
}

/// Settings pages are closed semantic tokens.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SettingsPage {
    /// Appearance and surface preferences.
    #[default]
    Appearance,
    /// Editor integration.
    Editor,
    /// Agent connections.
    Agents,
    /// Connections and local/remote policy.
    Connections,
    /// Privacy and data residency policy.
    Privacy,
    /// Diagnostics and transport health.
    Diagnostics,
    /// Index status.
    Index,
    /// Registry selection.
    Registry,
    /// Semantic legend.
    Legend,
    /// Keyboard and CLI help.
    Help,
}

impl SettingsPage {
    /// Stable order used by the settings focus route.
    pub const ALL: [Self; 10] = [
        Self::Appearance,
        Self::Editor,
        Self::Agents,
        Self::Connections,
        Self::Privacy,
        Self::Diagnostics,
        Self::Index,
        Self::Registry,
        Self::Legend,
        Self::Help,
    ];

    /// Returns a stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Editor => "editor",
            Self::Agents => "agents",
            Self::Connections => "connections",
            Self::Privacy => "privacy",
            Self::Diagnostics => "diagnostics",
            Self::Index => "index",
            Self::Registry => "registry",
            Self::Legend => "legend",
            Self::Help => "help",
        }
    }

    /// Parses only the closed settings vocabulary.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|page| page.as_str() == value)
    }
}

impl Route {
    /// Returns the content depth: a declaration's code view is the deepest.
    #[must_use]
    pub const fn depth(&self) -> Option<RouteDepth> {
        match self {
            Self::Orbit(_) | Self::World => Some(RouteDepth::Orbit),
            Self::Package(_) => Some(RouteDepth::Package),
            Self::Symbol(route) => Some(match route.view {
                View::Code => RouteDepth::Source,
                View::Page | View::Graph => RouteDepth::Page,
            }),
        }
    }

    /// Returns the selected object, if this route carries one.
    #[must_use]
    pub const fn selected(&self) -> Option<ObjectId> {
        match self {
            Self::Package(route) => route.selected,
            Self::Symbol(route) => route.selected,
            Self::Orbit(_) | Self::World => None,
        }
    }

    /// The release being viewed instead of the pinned one, if any.
    #[must_use]
    pub const fn at(&self) -> Option<&ReleaseId> {
        match self {
            Self::Package(route) => route.at.as_ref(),
            Self::Symbol(route) => route.at.as_ref(),
            Self::Orbit(_) | Self::World => None,
        }
    }

    /// The same place viewed at `at` (`None`: the pinned release). Routes
    /// without a release are returned unchanged.
    #[must_use]
    pub fn with_release(&self, at: Option<ReleaseId>) -> Self {
        match self {
            Self::Package(route) => Self::Package(PackageRoute { at, ..route.clone() }),
            Self::Symbol(route) => Self::Symbol(SymbolRoute { at, ..route.clone() }),
            Self::Orbit(_) | Self::World => self.clone(),
        }
    }

    /// The same declaration shown as `view`, or `None` when this route shows
    /// no declaration.
    #[must_use]
    pub fn with_view(&self, view: View) -> Option<Self> {
        match self {
            Self::Symbol(route) => Some(Self::Symbol(SymbolRoute { view, ..route.clone() })),
            Self::Orbit(_) | Self::Package(_) | Self::World => None,
        }
    }

    /// Whether `other` is this place (same declaration or package at the
    /// same release; the view, line and selection may differ).
    #[must_use]
    pub fn same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Symbol(a), Self::Symbol(b)) => a.same_place(b),
            _ => self == other,
        }
    }

    /// Returns a typed parent route for surfacing one depth. A declaration
    /// surfaces to its package whatever its view.
    #[must_use]
    pub fn zoom_out(&self) -> Option<Self> {
        match self {
            Self::Symbol(route) => Some(Self::Package(PackageRoute {
                project: route.project.clone(),
                package: route.package.clone(),
                lane: PackageLane::Overview,
                selected: route.selected,
                at: route.at.clone(),
            })),
            Self::Package(route) => Some(Self::Orbit(
                route
                    .project
                    .clone()
                    .map_or(OrbitRoute::Home, OrbitRoute::Project),
            )),
            Self::World | Self::Orbit(OrbitRoute::Browse(_)) => Some(Self::Orbit(OrbitRoute::Home)),
            Self::Orbit(_) => None,
        }
    }

    /// Returns a stable semantic route key suitable for telemetry and tests.
    /// The view is not part of it: the three views are one place.
    #[must_use]
    pub fn key(&self) -> RouteKey {
        match self {
            Self::Orbit(OrbitRoute::Home) => RouteKey::OrbitHome,
            Self::Orbit(OrbitRoute::Project(id)) => RouteKey::OrbitProject(id.clone()),
            Self::Package(route) => {
                RouteKey::Package(route.project.clone(), route.package.clone(), route.lane)
            }
            Self::Symbol(route) => RouteKey::Symbol(
                route.project.clone(),
                route.package.clone(),
                route.id.clone(),
                route.at.clone(),
            ),
            Self::Orbit(OrbitRoute::Browse(route)) => RouteKey::Browse(route.clone()),
            Self::World => RouteKey::World,
        }
    }
}

/// Stable key used by action trees and deterministic harness probes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RouteKey {
    /// Home orbit.
    OrbitHome,
    /// Project orbit.
    OrbitProject(ProjectId),
    /// Package coordinate.
    Package(Option<ProjectId>, PackageId, PackageLane),
    /// Declaration coordinate and release.
    Symbol(Option<ProjectId>, PackageId, Coordinate, Option<ReleaseId>),
    /// The whole graph.
    World,
    /// A browsing page.
    Browse(super::BrowseRoute),
}

/// A typed selection that can survive Cmd-minus and route replacement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Selection {
    /// An object selected in a package/page/source.
    Object(ObjectId),
    /// A document selected in the shell tree.
    Document(DocumentId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ObjectId;

    #[test]
    fn release_spelling_is_ecosystem_neutral_but_never_a_path_component_escape() {
        for spelling in ["1.1.post1", "1.0-SNAPSHOT", "1.0.0-preview.1", "1!2.0"] {
            assert_eq!(ReleaseId::new(spelling).expect("safe release").as_str(), spelling);
        }
        for spelling in ["../other", "1.0/other", "1.0\\other", " 1.0.0 "] {
            assert!(ReleaseId::new(spelling).is_err());
            assert!(!ReleaseId::from_persisted(spelling).is_valid());
            assert_eq!(ReleaseId::from_persisted(spelling).persisted_wire(), spelling);
        }
    }

    fn symbol(view: View) -> Route {
        Route::Symbol(SymbolRoute {
            project: None,
            package: PackageId::new("pkg").expect("package"),
            id: Coordinate::new("pkg::Thing").expect("coordinate"),
            at: None,
            view,
            line: Some(42),
            selected: Some(ObjectId::test(7)),
        })
    }

    #[test]
    fn depth_model_is_orbit_package_page_source() {
        assert_eq!(symbol(View::Code).depth(), Some(RouteDepth::Source));
        assert_eq!(symbol(View::Page).depth(), Some(RouteDepth::Page));
        assert_eq!(symbol(View::Graph).depth(), Some(RouteDepth::Page));
        let package = symbol(View::Code).zoom_out().expect("package");
        assert_eq!(package.depth(), Some(RouteDepth::Package));
        assert_eq!(package.selected(), Some(ObjectId::test(7)));
        assert_eq!(package.zoom_out().expect("orbit").depth(), Some(RouteDepth::Orbit));
        assert_eq!(Route::World.depth(), Some(RouteDepth::Orbit));
    }

    #[test]
    fn the_three_views_are_one_place_and_one_key() {
        let page = symbol(View::Page);
        let code = page.with_view(View::Code).expect("a declaration has views");
        assert!(page.same_place(&code));
        assert_eq!(page.key(), code.key());
        assert_ne!(page, code);
        let other_release = page.with_release(Some(ReleaseId::new("1.0.0").expect("release")));
        assert!(!page.same_place(&other_release), "another release is another place");
        assert_eq!(Route::World.with_view(View::Code), None);
    }

    #[test]
    fn package_zoom_out_returns_the_selected_project_orbit() {
        let project = ProjectId::test(1).expect("project");
        let route = Route::Package(PackageRoute {
            project: Some(project.clone()),
            package: PackageId::new("pkg").expect("package"),
            lane: PackageLane::Overview,
            selected: None,
            at: None,
        });
        assert_eq!(
            route.zoom_out(),
            Some(Route::Orbit(OrbitRoute::Project(project)))
        );
    }
}
