//! Read models of the browsing pages.
//!
//! The words come from `backend-present` ([`backend_present::TreeReading`]),
//! so the Library page says exactly what `backend project-tree` prints.

use crate::core::LocalProjectId;
use crate::navigation::BrowseRoute;
use crate::navigation::CompareSet;
use crate::model::pages::{DeclRef, Known, PackageDossier, PackageRecord, PackageRef, SearchPage, SearchQuery, SignatureText};
use std::fmt;
use std::sync::Arc;

/// Identity of one browsing resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BrowseKey {
    /// A project's dependency tree.
    Tree(LocalProjectId),
    /// Local package discovery before a query is entered.
    FindHome,
    /// Indexed answers to one query.
    Find(SearchQuery),
    /// Bounded comparison selection.
    Compare(CompareSet),
}

impl From<&BrowseRoute> for BrowseKey {
    fn from(route: &BrowseRoute) -> Self {
        match route {
            BrowseRoute::Tree(project) => Self::Tree(project.clone()),
            BrowseRoute::FindHome => Self::FindHome,
            BrowseRoute::Find(query) => Self::Find(query.clone()),
            BrowseRoute::Compare(selection) => Self::Compare(selection.clone()),
        }
    }
}

impl fmt::Display for BrowseKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tree(project) => write!(formatter, "tree {}", project.display_lossy()),
            Self::FindHome => formatter.write_str("find"),
            Self::Find(query) => write!(formatter, "find {:?}", query.text),
            Self::Compare(selection) => write!(formatter, "compare {:?}", selection.packages()),
        }
    }
}

/// One browsing resource's value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowseValue {
    /// A project's tree, read.
    Tree(Arc<TreeModel>),
    /// Search results with their original coverage evidence.
    Find(Arc<FindModel>),
    /// Package dossiers read for this comparison.
    Compare(Arc<CompareModel>),
}

impl BrowseValue {
    /// The tree, when this value is one.
    #[must_use]
    pub fn tree(&self) -> Option<&TreeModel> {
        match self {
            Self::Tree(tree) => Some(tree),
            Self::Find(_) | Self::Compare(_) => None,
        }
    }
}

/// Immutable evidence shared by every comparison presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompareModel {
    /// In the same order as the route's bounded selection.
    pub packages: Arc<[PackageDossier]>,
    /// Indexed operation evidence, aligned with the package order.
    pub apis: Arc<[Known<PackageApi>]>,
    /// UI-ready operation rows and exact-name alignment, prepared once by
    /// the read worker so pointer renders only borrow immutable rows.
    pub prepared: Arc<facet::browse::compare::Model>,
}

/// The bounded indexed declarations available for a package comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageApi {
    /// Exact package represented.
    pub package: PackageRef,
    /// At most 2,048 indexed declarations; visibility is not inferred.
    pub items: Arc<[ApiItem]>,
    /// False when the producer or this projection stopped before the end.
    pub complete: bool,
}

/// An indexed declaration's evidence for a visual operation inspector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiItem {
    /// Exact kind, identity and source location.
    pub decl: DeclRef,
    /// Recorded spelling, interpreted by the presentation rather than printed as code.
    pub signature: Known<SignatureText>,
    /// Authored first paragraph when present.
    pub summary: Option<Arc<str>>,
}

/// Search evidence assembled on the read worker, never by a render callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindModel {
    /// Declaration matches, retaining unavailable and empty as distinct states.
    pub answers: Known<SearchPage>,
    /// Matching packages from the local index and configured catalog.
    pub packages: Arc<[FindPackage]>,
    /// Whether both package sources answered. Partial rows remain useful.
    pub package_coverage: Known<()>,
    /// UI-ready candidates, recorded callable projections, and coverage
    /// words, prepared once by the read worker.
    pub prepared: Arc<facet::browse::find::Model>,
}

/// A lightweight candidate: a full dossier is only read when inspected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindPackage {
    /// Exact package identity.
    pub package: PackageRef,
    /// Producer's readable name.
    pub name: Arc<str>,
    /// Recorded description, never inferred from its name.
    pub description: Option<Arc<str>>,
    /// Present in the local index; this does not imply a direct dependency.
    pub indexed: bool,
    /// Catalog facts when available.
    pub record: Option<PackageRecord>,
    /// The registry release behind it, which can be added to the library
    /// (or already is).
    pub offer: Option<crate::model::release::Offer>,
}

/// A project's tree as the Library page shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeModel {
    /// The workspace root the tree was read at.
    pub root: Arc<str>,
    /// Every sentence of the page.
    pub reading: backend_present::TreeReading,
    /// Exact release destinations prepared by the read worker, aligned with
    /// `reading.roles`; source gaps remain explicit per release.
    pub links: Arc<[TreeRoleLinks]>,
    /// UI-ready words and stable action keys prepared once on the read lane.
    /// Drawing the page only borrows this model; it never rescans the tree.
    pub prepared: Arc<facet::browse::LibraryModel>,
}

/// Destinations for the rows of one derived role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeRoleLinks {
    /// Guards the alignment with the prose role.
    pub role: backend_library::browse::RoleId,
    /// One entry per direct dependency in this role.
    pub rows: Arc<[TreeRowLinks]>,
}

/// Destinations for all versions of one direct dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeRowLinks {
    /// Guards the alignment with the prose row.
    pub name: Arc<str>,
    /// Exact version identity and destination or its explicit reason.
    pub releases: Arc<[TreeReleaseLink]>,
}

/// One version's source-backed destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeReleaseLink {
    /// Version from the same project-tree reply as its source.
    pub version: Arc<str>,
    /// A route only when the source identity admits one.
    pub destination: TreeDestination,
}

/// Whether this exact source can open as a local package page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeDestination {
    /// A typed package locator, with origin and version preserved.
    Open(PackageRef),
    /// Why this tree cannot open this source as a package page.
    Unavailable(Arc<str>),
}
