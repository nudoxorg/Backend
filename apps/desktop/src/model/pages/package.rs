//! The Package (Territory) board's read model: one package as a dossier.

use super::common::{DeclRef, Known, PackageRef};
use crate::model::local_package::ReadmeBlock;
use std::sync::Arc;

/// Everything the Package board renders about one package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDossier {
    /// The package asked for.
    pub package: PackageRef,
    /// Headline record (registry release or local manifest).
    pub record: Known<PackageRecord>,
    /// Recorded releases, newest first when the producer orders them.
    pub versions: Known<Arc<[VersionEntry]>>,
    /// Outgoing dependency edges.
    pub dependencies: Known<Arc<[Dependency]>>,
    /// Packages that depend on this one.
    pub dependents: Known<Arc<[PackageRecord]>>,
    /// Modules and their items, for the mosaic.
    pub outline: Known<OutlineTree>,
    /// README, as structured blocks.
    pub readme: Known<Arc<[ReadmeBlock]>>,
}

/// Where a record's facts came from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordSource {
    /// A committed local registry publication.
    Registry,
    /// The local project's own manifest.
    LocalManifest,
}

/// Registry release standing (the bevel voice of a release tick).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Standing {
    /// Offered to new resolutions.
    Available,
    /// Withdrawn by the publisher.
    Yanked,
    /// Retained with a replacement recommendation.
    Deprecated,
    /// Hidden from listings.
    Unlisted,
    /// Excluded by ecosystem version policy.
    Retracted,
    /// Deleted upstream.
    Removed,
}

impl Standing {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Yanked => "yanked",
            Self::Deprecated => "deprecated",
            Self::Unlisted => "unlisted",
            Self::Retracted => "retracted",
            Self::Removed => "removed",
        }
    }
}

/// Download telemetry that never turns "not recorded" into zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Downloads {
    /// Exact cumulative count.
    Exact(u64),
    /// Estimated count.
    Approximate(u64),
}

/// Condensed advisory state for the dossier hero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvisorySummary {
    /// Number of advisories matching this exact release.
    pub advisories: usize,
    /// Highest normalized severity among them, when any carry one.
    pub worst: Option<backend_library::SeverityLevel>,
    /// Acquisition decision word: allow, warn, deny.
    pub decision: Arc<str>,
    /// Coverage of the configured authorities.
    pub coverage: backend_library::AdvisoryCoverage,
    /// Freshness of the advisory frontier.
    pub freshness: backend_library::FreshnessState,
}

/// One package record, from a registry release or a local manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRecord {
    /// Exact package locator.
    pub package: PackageRef,
    /// Where the facts came from.
    pub source: RecordSource,
    /// Registry-native or manifest name.
    pub name: Arc<str>,
    /// Immutable version.
    pub version: Known<Arc<str>>,
    /// Ecosystem spelling.
    pub ecosystem: Known<Arc<str>>,
    /// Release standing.
    pub standing: Known<Standing>,
    /// Downloads.
    pub downloads: Known<Downloads>,
    /// Archive size in bytes.
    pub bytes: Known<u64>,
    /// Advisory state.
    pub advisory: Known<AdvisorySummary>,
    /// One-line description.
    pub description: Known<Arc<str>>,
    /// License label.
    pub license: Known<Arc<str>>,
}

/// One tick on the release comb.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionEntry {
    /// Exact release locator.
    pub package: PackageRef,
    /// Version text.
    pub version: Arc<str>,
    /// Release standing.
    pub standing: Standing,
    /// Whether this is the release the dossier is about.
    pub current: bool,
}

/// Dependency resolver scope.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DependencyScope {
    /// Normal runtime dependency.
    Runtime,
    /// Optional feature or extra.
    Optional,
    /// Development and test.
    Development,
    /// Build time.
    Build,
    /// Supplied by the consumer.
    Peer,
}

impl DependencyScope {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Optional => "optional",
            Self::Development => "dev",
            Self::Build => "build",
            Self::Peer => "peer",
        }
    }
}

/// One outgoing dependency edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Registry-qualified name.
    pub name: Arc<str>,
    /// Requirement exactly as written.
    pub requirement: Arc<str>,
    /// Resolver scope.
    pub scope: DependencyScope,
    /// Whether the edge is excluded from default resolution.
    pub optional: bool,
    /// Exact release a resolver selected, when known.
    pub resolved: Option<PackageRef>,
}

/// One node of the package outline (a mosaic stone).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineNode {
    /// The declaration.
    pub decl: DeclRef,
    /// Contained declarations in outline order.
    pub children: Arc<[OutlineNode]>,
}

impl OutlineNode {
    /// Returns the number of declarations in this subtree, itself included.
    #[must_use]
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }
}

/// The package outline as a forest: modules (or top-level items) at the
/// roots, their items below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineTree {
    /// Top-level nodes in outline order.
    pub roots: Arc<[OutlineNode]>,
    /// Whether every page of the producer's outline was read.
    pub complete: bool,
}

impl OutlineTree {
    /// Returns the number of declarations in the tree.
    #[must_use]
    pub fn count(&self) -> usize {
        self.roots.iter().map(OutlineNode::count).sum()
    }

    /// Visits every node depth-first.
    pub fn walk(&self) -> impl Iterator<Item = &OutlineNode> {
        let mut stack: Vec<&OutlineNode> = self.roots.iter().rev().collect();
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            stack.extend(node.children.iter().rev());
            Some(node)
        })
    }
}
