//! The Orbit board's read model: your projects and everything around them.

use super::common::{Known, PackageRef};
use super::package::PackageRecord;
use std::sync::Arc;

/// Readiness of one indexed package on the shelf, as its row says.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Readiness {
    /// Readable.
    Ready,
    /// Still indexing.
    Indexing,
    /// Published in a failed state.
    Failed,
}

/// One package the local owner has indexed (a shelf row).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedPackage {
    /// Exact package locator.
    pub package: PackageRef,
    /// Readable name.
    pub name: Arc<str>,
    /// Readiness the owner published.
    pub readiness: Readiness,
}

/// One project (a named group of pinned packages).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrbitProject {
    /// Stable identity.
    pub id: u64,
    /// Unique name.
    pub name: Arc<str>,
    /// Bound lockfile path, when one is bound.
    pub lockfile: Option<Arc<str>>,
    /// Pinned members.
    pub members: Arc<[PackageRef]>,
}

/// What one shared-tree node displays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeSubject {
    /// A package.
    Package(PackageRef),
    /// A declaration label.
    Declaration(Arc<str>),
    /// A catalog exploration, with its filter.
    Explore(Option<Arc<str>>),
    /// A documentation search.
    Search(Arc<str>),
    /// A registry publisher.
    Owner(Arc<str>),
}

/// Which surface opened a tree node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeOpener {
    /// This desktop.
    Desktop,
    /// The command line.
    Cli,
    /// An MCP client, by admitted name.
    Mcp(Arc<str>),
}

/// One node of the shared session tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeNode {
    /// Stable identity.
    pub id: u64,
    /// Parent node.
    pub parent: Option<u64>,
    /// Displayed subject.
    pub subject: TreeSubject,
    /// Short title.
    pub title: Arc<str>,
    /// Opening surface.
    pub opener: TreeOpener,
    /// Whether this node is active.
    pub active: bool,
}

/// Everything the Orbit board renders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrbitModel {
    /// Packages indexed by the local owner.
    pub indexed: Known<Arc<[IndexedPackage]>>,
    /// Projects and their members.
    pub projects: Known<Arc<[OrbitProject]>>,
    /// The local registry catalog.
    pub explore: Known<Arc<[PackageRecord]>>,
    /// The shared session tree (desktop, CLI, and MCP nodes).
    pub tree: Known<Arc<[TreeNode]>>,
}
