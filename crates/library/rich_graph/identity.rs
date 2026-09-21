//! Stable identities and typed relation discriminants for rich graph facts.

use crate::{DependencyScope, RowId, SemanticLinkKind};
use serde::{Deserialize, Serialize};

/// A stable graph node identity derived from canonical identity material.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphNodeId([u8; 32]);

impl GraphNodeId {
    /// Creates an identity from already admitted bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derives a node identity from one visible row identity.
    #[must_use]
    pub fn for_row(row: RowId) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.row.v1\0");
        hasher.update(row.stable_key().as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Derives a node identity from a compiler declaration identity.
    #[must_use]
    pub fn for_symbol(symbol: crate::SymbolKey) -> Self {
        Self::for_row(RowId::Symbol(symbol))
    }

    /// Derives a node identity from a package reference.
    #[must_use]
    pub fn for_package(package: &crate::PackageReference) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.package-reference.v1\0");
        match package {
            crate::PackageReference::Purl(purl) => {
                hasher.update(b"purl\0");
                hasher.update(purl.as_str().as_bytes());
            }
            crate::PackageReference::Local(label) => {
                hasher.update(b"local\0");
                hasher.update(label.as_str().as_bytes());
            }
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Derives a node identity from an unresolved, authority-qualified package target.
    #[must_use]
    pub fn for_unresolved_package(
        target: &crate::PackageDependencyTarget,
        authority: GraphAuthority,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.package-lineage.v1\0");
        hasher.update(&[graph_authority_tag(authority)]);
        hasher.update(target.ecosystem.as_str().as_bytes());
        hasher.update(&[0]);
        hasher.update(target.name.as_str().as_bytes());
        Self(*hasher.finalize().as_bytes())
    }
}

/// The relation family used by layout, filtering, and product surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphRelationFamily {
    /// Compiler-resolved declaration and occurrence edges.
    Code,
    /// Outgoing package dependency edges.
    Dependency,
    /// Reverse package dependency edges.
    Dependent,
}

/// The typed relationship represented by a rich graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GraphEdgeKind {
    /// A compiler semantic relation.
    Code {
        /// Compiler-owned relation kind.
        relation: SemanticLinkKind,
    },
    /// An outgoing package dependency requirement.
    Dependency {
        /// Resolver scope of the requirement.
        scope: DependencyScope,
        /// Whether the dependency is optional for default resolution.
        optional: bool,
    },
    /// A reverse package dependency requirement.
    Dependent {
        /// Resolver scope of the requirement.
        scope: DependencyScope,
        /// Whether the dependency is optional for default resolution.
        optional: bool,
    },
}

impl GraphEdgeKind {
    /// Returns the coarse relation family.
    #[must_use]
    pub const fn family(self) -> GraphRelationFamily {
        match self {
            Self::Code { .. } => GraphRelationFamily::Code,
            Self::Dependency { .. } => GraphRelationFamily::Dependency,
            Self::Dependent { .. } => GraphRelationFamily::Dependent,
        }
    }

    fn encode_key(self, output: &mut Vec<u8>) {
        match self {
            Self::Code { relation } => {
                output.push(0);
                output.push(semantic_link_tag(relation));
            }
            Self::Dependency { scope, optional } => {
                output.push(1);
                output.push(dependency_scope_tag(scope));
                output.push(u8::from(optional));
            }
            Self::Dependent { scope, optional } => {
                output.push(2);
                output.push(dependency_scope_tag(scope));
                output.push(u8::from(optional));
            }
        }
    }
}

/// The authority which supplied a graph fact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphAuthority {
    /// Native compiler or language oracle publication.
    SemanticGeneration,
    /// Authenticated registry metadata.
    RegistryMetadata,
    /// Dependency manifest inside a package archive.
    ArchiveManifest,
    /// Manifest published by a source forge.
    ForgeManifest,
    /// Manifest read from a local project.
    LocalManifest,
}

fn graph_authority_tag(authority: GraphAuthority) -> u8 {
    match authority {
        GraphAuthority::SemanticGeneration => 0,
        GraphAuthority::RegistryMetadata => 1,
        GraphAuthority::ArchiveManifest => 2,
        GraphAuthority::ForgeManifest => 3,
        GraphAuthority::LocalManifest => 4,
    }
}

pub(super) fn graph_authority_for_dependency(
    authority: crate::DependencyAuthority,
) -> GraphAuthority {
    match authority {
        crate::DependencyAuthority::RegistryMetadata => GraphAuthority::RegistryMetadata,
        crate::DependencyAuthority::ArchiveManifest => GraphAuthority::ArchiveManifest,
        crate::DependencyAuthority::ForgeManifest => GraphAuthority::ForgeManifest,
        crate::DependencyAuthority::LocalManifest => GraphAuthority::LocalManifest,
    }
}

/// Stable identity for one directed relationship.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphEdgeId([u8; 32]);

impl GraphEdgeId {
    /// Creates an identity from already admitted bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derives a stable identity from endpoints and typed relationship.
    #[must_use]
    pub fn derive(from: GraphNodeId, to: GraphNodeId, kind: GraphEdgeKind) -> Self {
        Self::derive_with_detail(from, to, kind, None)
    }

    /// Derives an identity while retaining a producer supplied edge detail.
    ///
    /// Dependency requirements are part of the canonical package fact, so two
    /// requirements between the same package pair must not collapse into one
    /// edge merely because their resolver scope is equal.
    #[must_use]
    pub fn derive_with_detail(
        from: GraphNodeId,
        to: GraphNodeId,
        kind: GraphEdgeKind,
        detail: Option<&str>,
    ) -> Self {
        let mut preimage = Vec::with_capacity(1 + 32 + 32 + 3 + detail.map_or(1, str::len));
        preimage.extend_from_slice(b"nudox.rich-ir.graph.edge.v1\0");
        preimage.extend_from_slice(from.as_bytes());
        preimage.extend_from_slice(to.as_bytes());
        kind.encode_key(&mut preimage);
        match detail {
            Some(detail) => {
                preimage.push(1);
                preimage.extend_from_slice(&(detail.len() as u32).to_be_bytes());
                preimage.extend_from_slice(detail.as_bytes());
            }
            None => preimage.push(0),
        }
        Self(*blake3::hash(&preimage).as_bytes())
    }
}

fn semantic_link_tag(kind: SemanticLinkKind) -> u8 {
    match kind {
        SemanticLinkKind::Calls => 0,
        SemanticLinkKind::MethodCall => 1,
        SemanticLinkKind::TypeReference => 2,
        SemanticLinkKind::Reads => 3,
        SemanticLinkKind::Writes => 4,
        SemanticLinkKind::Imports => 5,
        SemanticLinkKind::Implements => 6,
        SemanticLinkKind::Overrides => 7,
        SemanticLinkKind::Reexports => 8,
        SemanticLinkKind::Inherits => 9,
        SemanticLinkKind::Documents => 10,
    }
}

fn dependency_scope_tag(scope: DependencyScope) -> u8 {
    match scope {
        DependencyScope::Runtime => 0,
        DependencyScope::Optional => 1,
        DependencyScope::Development => 2,
        DependencyScope::Build => 3,
        DependencyScope::Peer => 4,
    }
}
