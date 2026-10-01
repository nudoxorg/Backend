//! The Orbit board's read model: your projects and everything around them.

use super::common::{Known, PackageRef};
use super::package::PackageRecord;
use std::sync::Arc;

/// An exact Cargo package URL admitted from one local registry tree by the
/// active registry source. This is an in-process read receipt, not a fact
/// that may survive serialization: a new composition must re-admit it.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct VerifiedRegistryRelease {
    package: PackageRef,
    authority: Arc<str>,
    generation: u64,
}

impl std::fmt::Debug for VerifiedRegistryRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedRegistryRelease")
            .field("package", &self.package)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl VerifiedRegistryRelease {
    /// Records the owner worker's manifest and registry-source checks.
    #[must_use]
    pub(crate) fn from_checked_release(
        package: PackageRef,
        authority: Arc<str>,
        generation: u64,
    ) -> Self {
        Self {
            package,
            authority,
            generation,
        }
    }

    /// Whether this proof names the resolver's exact purl under the current
    /// registry composition.
    #[must_use]
    pub(crate) fn matches(
        &self,
        package: &PackageRef,
        authority: &str,
        generation: u64,
    ) -> bool {
        self.package == *package
            && self.authority.as_ref() == authority
            && self.generation == generation
    }
}

/// Readiness of one indexed package on the shelf, as its row says.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub enum Readiness {
    /// Readable.
    Ready,
    /// Still indexing.
    Indexing,
    /// Published in a failed state.
    Failed,
}

/// One package the local owner has indexed (a shelf row).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexedPackage {
    /// Exact package locator.
    pub package: PackageRef,
    /// Readable name.
    pub name: Arc<str>,
    /// Readiness the owner published.
    pub readiness: Readiness,
    /// Exact registry identity of a verified local tree, admitted on the
    /// Orbit read worker. It is intentionally not persisted: after a restart
    /// or composition change, the source must be checked again.
    #[serde(skip)]
    pub(crate) verified_registry_release: Option<VerifiedRegistryRelease>,
}

/// One project (a named group of pinned packages).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TreeOpener {
    /// This desktop.
    Desktop,
    /// The command line.
    Cli,
    /// An MCP client, by admitted name.
    Mcp(Arc<str>),
}

/// One node of the shared session tree.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
