//! Read models of the browsing pages.
//!
//! The words come from `backend-present` ([`backend_present::TreeReading`]),
//! so the Library page says exactly what `backend project-tree` prints.

use crate::core::LocalProjectId;
use crate::model::pages::{
    DeclRef, Known, PackageDossier, PackageRecord, PackageRef, SearchPage, SearchQuery,
    SignatureText,
};
use crate::navigation::BrowseRoute;
use crate::navigation::CargoSourcePath;
use crate::navigation::CompareSet;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

/// Identity of one browsing resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BrowseKey {
    /// A project's dependency tree.
    Tree(LocalProjectId),
    /// Bounded current package-relative file addresses from one exact owner tree.
    CargoSourceInventory(CargoSourceInventoryKey),
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
            Self::CargoSourceInventory(key) => write!(
                formatter,
                "Cargo files {} in {}",
                key.package,
                key.project.display_lossy()
            ),
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
    /// Paths observed under one current Cargo source receipt, without file bytes.
    CargoSourceInventory(Arc<CargoSourceInventoryModel>),
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
            Self::CargoSourceInventory(_) | Self::Find(_) | Self::Compare(_) => None,
        }
    }
}

/// Address for a current bounded source-file listing. The project is an
/// owner-checked tree address, never a capability to read client paths.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CargoSourceInventoryKey {
    /// Exact project tree that introduced the source-qualified package.
    pub project: LocalProjectId,
    /// Full qualified package reference, including its Cargo authority digest.
    pub package: PackageRef,
}

/// Navigation hints observed by the owner. Each file still requires its own
/// source read and content digest before bytes can be shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoSourceInventoryModel {
    /// Exact qualified package in the owner reply.
    pub package: PackageRef,
    /// Sorted, bounded canonical package-relative paths.
    pub paths: Arc<[CargoSourcePath]>,
    /// Whether the owner enumerated every supported safe source file.
    pub coverage: backend_library::CargoPackageSourceInventoryCoverageV1,
    /// Source observation revision shared with independently checked files.
    pub source_revision: [u8; 32],
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
    /// Every observed external row, including lockfile rows without a role.
    /// Stable exact keys and destinations are prepared on the read worker.
    pub inventory_links: Arc<[TreeInventoryLink]>,
    /// Exact source-qualified packages admitted by this owner tree, including
    /// transitive/unknown-role rows that have no direct dependency button.
    /// Prepared on the read worker for bounded package-page lookups.
    pub source_packages: Arc<BTreeSet<PackageRef>>,
    /// UI-ready words and stable action keys prepared once on the read lane.
    /// Drawing the page only borrows this model; it never rescans the tree.
    pub prepared: Arc<facet::browse::LibraryModel>,
}

/// One package inventory row aligned with `TreeReading::inventory`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeInventoryLink {
    /// Exact row identity including source spelling; occurrence distinguishes
    /// only literally indistinguishable duplicate rows.
    pub key: Arc<str>,
    /// Full observed package name and version guard positional hints.
    pub name: Arc<str>,
    pub version: Arc<str>,
    /// No action exists without an exact current owner source receipt.
    pub destination: TreeDestination,
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
    /// Stable identity of this row's exact multiset of source releases.
    pub key: Arc<str>,
    /// Exact version identity and destination or its explicit reason.
    pub releases: Arc<[TreeReleaseLink]>,
}

/// One version's source-backed destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeReleaseLink {
    /// Version from the same project-tree reply as its source.
    pub version: Arc<str>,
    /// Stable exact source identity, including a disambiguator only for
    /// indistinguishable duplicate occurrences.
    pub key: Arc<str>,
    /// A route only when the source identity admits one.
    pub destination: TreeDestination,
    /// Full exact source spelling when equal visible versions need disambiguation.
    pub source_detail: Option<Arc<str>>,
}

/// Whether this exact source can open as a local package page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeDestination {
    /// A typed package locator, with origin and version preserved.
    Open(PackageRef),
    /// Why this tree cannot open this source as a package page.
    Unavailable(Arc<str>),
}
