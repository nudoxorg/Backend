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
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("coordinate must not be empty"),
            Self::ControlCharacter => f.write_str("coordinate contains a control character"),
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
}

/// Package-level route data.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageRoute {
    /// Optional project selected at the orbit level.
    pub project: Option<ProjectId>,
    /// Stable package coordinate.
    pub package: PackageId,
    /// The selected package lane.
    pub lane: PackageLane,
    /// Selected package object retained when zooming out.
    pub selected: Option<ObjectId>,
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

/// Page-level route data.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageRoute {
    /// Optional project selected at the orbit level.
    pub project: Option<ProjectId>,
    /// Parent package coordinate.
    pub package: PackageId,
    /// Stable page coordinate.
    pub coordinate: Coordinate,
    /// Selected object retained on source descent and zoom-out.
    pub selected: Option<ObjectId>,
}

/// Source-level route data.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceRoute {
    /// Optional project selected at the orbit level.
    pub project: Option<ProjectId>,
    /// Parent package coordinate.
    pub package: PackageId,
    /// Parent page coordinate.
    pub page: Coordinate,
    /// Source line selected by the reader.
    pub line: u32,
    /// Selected object retained on zoom-out.
    pub selected: Option<ObjectId>,
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
    /// Declaration/document page.
    Page(PageRoute),
    /// Captured source view.
    Source(SourceRoute),
}

/// A transient shell surface layered over a content route.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Overlay {
    /// Keyboard-first local project admission surface.
    AddProject,
    /// Settings surface at a typed page.
    Settings(SettingsPage),
    /// Command palette surface.
    CommandPalette,
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
    /// Returns the content depth.
    #[must_use]
    pub const fn depth(&self) -> Option<RouteDepth> {
        match self {
            Self::Orbit(_) => Some(RouteDepth::Orbit),
            Self::Package(_) => Some(RouteDepth::Package),
            Self::Page(_) => Some(RouteDepth::Page),
            Self::Source(_) => Some(RouteDepth::Source),
        }
    }

    /// Returns the selected object, if this route carries one.
    #[must_use]
    pub const fn selected(&self) -> Option<ObjectId> {
        match self {
            Self::Package(route) => route.selected,
            Self::Page(route) => route.selected,
            Self::Source(route) => route.selected,
            Self::Orbit(_) => None,
        }
    }

    /// Returns a typed parent route for Cmd-minus / zoom-out.
    #[must_use]
    pub fn zoom_out(&self) -> Option<Self> {
        match self {
            Self::Source(route) => Some(Self::Page(PageRoute {
                project: route.project.clone(),
                package: route.package.clone(),
                coordinate: route.page.clone(),
                selected: route.selected,
            })),
            Self::Page(route) => Some(Self::Package(PackageRoute {
                project: route.project.clone(),
                package: route.package.clone(),
                lane: PackageLane::Overview,
                selected: route.selected,
            })),
            Self::Package(route) => Some(Self::Orbit(
                route
                    .project
                    .clone()
                    .map_or(OrbitRoute::Home, OrbitRoute::Project),
            )),
            Self::Orbit(_) => None,
        }
    }

    /// Returns a stable semantic route key suitable for telemetry and tests.
    #[must_use]
    pub fn key(&self) -> RouteKey {
        match self {
            Self::Orbit(OrbitRoute::Home) => RouteKey::OrbitHome,
            Self::Orbit(OrbitRoute::Project(id)) => RouteKey::OrbitProject(id.clone()),
            Self::Package(route) => {
                RouteKey::Package(route.project.clone(), route.package.clone(), route.lane)
            }
            Self::Page(route) => RouteKey::Page(
                route.project.clone(),
                route.package.clone(),
                route.coordinate.clone(),
            ),
            Self::Source(route) => RouteKey::Source(
                route.project.clone(),
                route.package.clone(),
                route.page.clone(),
                route.line,
            ),
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
    /// Page coordinate.
    Page(Option<ProjectId>, PackageId, Coordinate),
    /// Source coordinate and line.
    Source(Option<ProjectId>, PackageId, Coordinate, u32),
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

    fn source() -> Route {
        Route::Source(SourceRoute {
            project: None,
            package: PackageId::new("pkg").expect("package"),
            page: Coordinate::new("pkg::Thing").expect("coordinate"),
            line: 42,
            selected: Some(ObjectId::test(7)),
        })
    }

    #[test]
    fn depth_model_is_orbit_package_page_source() {
        assert_eq!(source().depth(), Some(RouteDepth::Source));
        let page = source().zoom_out().expect("page");
        assert_eq!(page.depth(), Some(RouteDepth::Page));
        assert_eq!(page.selected(), Some(ObjectId::test(7)));
        let package = page.zoom_out().expect("package");
        assert_eq!(package.depth(), Some(RouteDepth::Package));
        assert_eq!(package.selected(), Some(ObjectId::test(7)));
        assert_eq!(
            package.zoom_out().expect("orbit").depth(),
            Some(RouteDepth::Orbit)
        );
    }

    #[test]
    fn package_zoom_out_returns_the_selected_project_orbit() {
        let project = ProjectId::test(1).expect("project");
        let route = Route::Package(PackageRoute {
            project: Some(project.clone()),
            package: PackageId::new("pkg").expect("package"),
            lane: PackageLane::Overview,
            selected: None,
        });
        assert_eq!(
            route.zoom_out(),
            Some(Route::Orbit(OrbitRoute::Project(project)))
        );
    }
}
