//! Canonical package dependency facts shared by acquisition, storage, and clients.
//!
//! A dependency is deliberately not modelled as a pair of strings.  Registries
//! publish requirements before a resolver chooses a concrete version, while a
//! lockfile or a local checkout may publish an exact target.  Keeping both the
//! requirement and the optional resolution lets callers answer useful graph
//! questions without pretending that an unresolved requirement is a resolved
//! edge.

use crate::{PackageReference, ProductAdmissionError, ProductText, RegistryEcosystem};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Returns the source-selection policy package/archive adapters should use
/// when they inspect a local checkout.  Keeping this constructor in the
/// package-graph module makes the graph and source ingest share one policy
/// without coupling either parser to the local daemon.
#[must_use]
pub fn source_selection_policy() -> backend_discovery::DiscoveryPolicy {
    backend_discovery::DiscoveryPolicy::default()
}

/// Starts the shared deterministic traversal for a package/archive adapter.
///
/// The iterator is intentionally returned before any identity or delta work:
/// callers can apply their own bounded byte reader while retaining one source
/// selection policy and one symlink/path-confinement boundary.
#[must_use]
pub fn discover_source_entries(
    root: impl AsRef<Path>,
    policy: backend_discovery::DiscoveryPolicy,
) -> backend_discovery::Discovery {
    policy.walk(root)
}

/// Selects regular source candidates for package/archive graph adapters.
///
/// Traversal policy deliberately stops at this boundary: callers receive
/// deterministic paths and decide which manifest or source identity to derive
/// from each byte stream. This keeps file identity and graph deltas reusable
/// for code-forge and archive sources without duplicating ignore semantics.
pub fn discover_source_files(
    root: impl AsRef<Path>,
    policy: backend_discovery::DiscoveryPolicy,
) -> Result<Vec<PathBuf>, backend_discovery::DiscoveryError> {
    discover_source_entries(root, policy)
        .filter_map(|entry| match entry {
            Ok(entry) if entry.is_file() => Some(Ok(entry.path().to_owned())),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

/// Maximum dependency rows in one package graph answer.
pub const MAX_PACKAGE_GRAPH_ROWS: usize = 2_048;

/// Stable, typed identity of a configured registry authority.
///
/// The bytes are the source identity assigned by registry configuration. A
/// package coordinate alone is deliberately insufficient because mirrors and
/// private registries may publish the same PURL independently.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RegistryAuthorityId([u8; 32]);

impl RegistryAuthorityId {
    /// Wraps the stable identity of an already configured registry source.
    #[must_use]
    pub const fn from_configured_source(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width configured source identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Authority identity that scopes one package graph source.
///
/// `Unattributed` is a compatibility bucket for facts that have not been
/// bound to a source owner. It is not a registry identity and may never be
/// upgraded to one by parsing a package coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "kebab-case")]
pub enum PackageGraphSourceAuthority {
    /// A configured registry endpoint/router identity.
    Registry(RegistryAuthorityId),
    /// A code forge authority identity, distinct from all registry IDs.
    Forge([u8; 32]),
    /// An archive or immutable package artifact authority.
    Archive([u8; 32]),
    /// A local workspace/project authority.
    Local([u8; 32]),
    /// No source-owner identity was supplied.
    Unattributed,
}

impl PackageGraphSourceAuthority {
    /// Stable schema tag used by the local graph index.
    #[must_use]
    pub const fn kind_tag(self) -> i64 {
        match self {
            Self::Unattributed => 0,
            Self::Registry(_) => 1,
            Self::Forge(_) => 2,
            Self::Archive(_) => 3,
            Self::Local(_) => 4,
        }
    }

    /// Fixed-width authority payload. The unattributed bucket is all zeroes;
    /// the kind tag keeps it disjoint from a real authority with zero bytes.
    #[must_use]
    pub const fn id_bytes(self) -> [u8; 32] {
        match self {
            Self::Registry(id) => id.as_bytes(),
            Self::Forge(id) | Self::Archive(id) | Self::Local(id) => id,
            Self::Unattributed => [0; 32],
        }
    }

    /// Whether this source-owner type is compatible with the evidence class.
    #[must_use]
    pub const fn matches_evidence(self, evidence: DependencyAuthority) -> bool {
        match self {
            Self::Registry(_) => matches!(evidence, DependencyAuthority::RegistryMetadata),
            Self::Forge(_) => matches!(evidence, DependencyAuthority::ForgeManifest),
            Self::Archive(_) => matches!(evidence, DependencyAuthority::ArchiveManifest),
            Self::Local(_) => matches!(evidence, DependencyAuthority::LocalManifest),
            Self::Unattributed => true,
        }
    }
}

/// Exact graph key for one package coordinate at one source authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGraphSourceKey {
    /// Canonical package coordinate, which may be shared across authorities.
    pub coordinate: PackageReference,
    /// Source owner that published the facts.
    pub authority: PackageGraphSourceAuthority,
}

impl PackageGraphSourceKey {
    /// Constructs an exact source key.
    #[must_use]
    pub const fn new(coordinate: PackageReference, authority: PackageGraphSourceAuthority) -> Self {
        Self {
            coordinate,
            authority,
        }
    }

    /// Constructs a key for legacy or unbound facts without implying registry
    /// ownership.
    #[must_use]
    pub const fn unattributed(coordinate: PackageReference) -> Self {
        Self::new(coordinate, PackageGraphSourceAuthority::Unattributed)
    }

    /// Canonical coordinate spelling, for diagnostics and presentation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.coordinate.as_str()
    }
}

/// Dependency facts associated with one exact coordinate/authority pair.
pub type PackageDependencySourceFacts = (
    PackageGraphSourceKey,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
);

/// Validated immutable dependency facts with their canonical witness cached.
///
/// The fact slice and witness are private. Clones share the immutable facts and
/// copy the already-computed witness, so consumers cannot invalidate the cache
/// after construction.
#[derive(Clone, Debug)]
pub struct CheckedPackageGraphFacts {
    facts: Arc<[PackageDependencySourceFacts]>,
    source_witnesses: Arc<[[u8; 32]]>,
    witness: [u8; 32],
}

impl CheckedPackageGraphFacts {
    /// Validates and canonicalizes graph facts, then computes their witness once.
    pub fn new(
        mut facts: Vec<PackageDependencySourceFacts>,
    ) -> Result<Self, ProductAdmissionError> {
        let mut source_keys = BTreeSet::new();
        for (source, state) in &mut facts {
            if !source_keys.insert(source.clone()) {
                return Err(ProductAdmissionError::DependencyShape);
            }
            let DependencyFacts::Known(rows) = state else {
                continue;
            };
            if rows.len() > MAX_PACKAGE_GRAPH_ROWS {
                return Err(ProductAdmissionError::RowBound);
            }
            let mut identities = BTreeSet::new();
            for row in rows.iter() {
                if row.source != source.coordinate
                    || row.source_authority != source.authority
                    || !source.authority.matches_evidence(row.evidence.authority)
                    || row.facts_version != row.recomputed_version()
                    || !identities.insert(row.facts_version)
                {
                    return Err(ProductAdmissionError::DependencyShape);
                }
            }
            rows.sort_unstable_by_key(|row| row.facts_version);
        }
        // Keep source entries in the same order as Turso's keyed source
        // witness table. Projection sync can then merge borrowed slices
        // directly without allocating and sorting a second source vector.
        facts.sort_unstable_by(|(left, _), (right, _)| {
            left.coordinate
                .as_str()
                .cmp(right.coordinate.as_str())
                .then_with(|| left.authority.kind_tag().cmp(&right.authority.kind_tag()))
                .then_with(|| left.authority.id_bytes().cmp(&right.authority.id_bytes()))
        });
        // Keep one digest beside each immutable source entry. Consumers can
        // compare a checked snapshot with a persisted per-source index without
        // re-hashing every edge during every projection update.
        let source_witnesses: Arc<[[u8; 32]]> = facts
            .iter()
            .map(|(source, state)| package_dependency_source_facts_witness_canonical(source, state))
            .collect::<Vec<_>>()
            .into();
        let witness = package_dependency_facts_witness_from_sources(&source_witnesses);
        Ok(Self {
            facts: facts.into(),
            source_witnesses,
            witness,
        })
    }

    /// Borrows facts in canonical coordinate/authority order without exposing
    /// mutable access to the snapshot.
    #[must_use]
    pub fn facts(&self) -> &[PackageDependencySourceFacts] {
        &self.facts
    }

    /// Borrows source witnesses in the same order as [`Self::facts`]. Each
    /// digest covers its source coordinate and authority, known edge facts or
    /// the complete unknown/unavailable state, and both declared and
    /// recomputed edge identities. The domain is independent from the global
    /// graph witness so the two can evolve without aliasing.
    #[must_use]
    pub fn source_witnesses(&self) -> &[[u8; 32]] {
        &self.source_witnesses
    }

    /// Returns the canonical witness computed during construction.
    #[must_use]
    pub const fn witness(&self) -> [u8; 32] {
        self.witness
    }
}

fn package_dependency_source_facts_witness(
    source: &PackageGraphSourceKey,
    state: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> [u8; 32] {
    package_dependency_source_facts_witness_inner(source, state, false)
}

fn package_dependency_source_facts_witness_canonical(
    source: &PackageGraphSourceKey,
    state: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> [u8; 32] {
    package_dependency_source_facts_witness_inner(source, state, true)
}

fn package_dependency_source_facts_witness_inner(
    source: &PackageGraphSourceKey,
    state: &DependencyFacts<Box<[PackageDependencyRecord]>>,
    canonical_edges: bool,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.package-dependency-source-facts.v1\0");

    // Stream the canonical nested package-reference field encoding, avoiding
    // a temporary encoded source or edge vector.
    let (kind, coordinate) = match &source.coordinate {
        PackageReference::Purl(value) => (1_u8, value.as_str()),
        PackageReference::Local(value) => (2_u8, value.as_str()),
    };
    let reference_len = (1 + 8 + 1 + 1 + 8 + coordinate.len()) as u64;
    hasher.update(&[1]);
    hasher.update(&reference_len.to_be_bytes());
    hasher.update(&[1]);
    hasher.update(&1_u64.to_be_bytes());
    hasher.update(&[kind]);
    hasher.update(&[2]);
    hasher.update(&(coordinate.len() as u64).to_be_bytes());
    hasher.update(coordinate.as_bytes());

    hasher.update(&[6]);
    hasher.update(&8_u64.to_be_bytes());
    hasher.update(&source.authority.kind_tag().to_be_bytes());
    hasher.update(&[7]);
    hasher.update(&32_u64.to_be_bytes());
    hasher.update(&source.authority.id_bytes());

    match state {
        DependencyFacts::Known(rows) => {
            hasher.update(&[2]);
            hasher.update(&1_u64.to_be_bytes());
            hasher.update(&[1]);
            if canonical_edges
                || rows.windows(2).all(|pair| {
                    (pair[0].recomputed_version(), pair[0].facts_version)
                        <= (pair[1].recomputed_version(), pair[1].facts_version)
                })
            {
                for row in rows.iter() {
                    hash_source_edge(&mut hasher, row.recomputed_version(), row.facts_version);
                }
            } else {
                // Unchecked callers may present rows in any order. Keep this
                // fallback bounded to one source while checked snapshots use
                // their already canonical row order and allocate no edge list.
                let mut edges = rows
                    .iter()
                    .map(|row| (row.recomputed_version(), row.facts_version))
                    .collect::<Vec<_>>();
                edges.sort_unstable();
                for (content_version, declared_version) in edges {
                    hash_source_edge(&mut hasher, content_version, declared_version);
                }
            }
        }
        DependencyFacts::Unknown(reason) => {
            hasher.update(&[2]);
            hasher.update(&1_u64.to_be_bytes());
            hasher.update(&[2]);
            hash_field(&mut hasher, 4, reason.as_str().as_bytes());
        }
        DependencyFacts::Unavailable(reason) => {
            hasher.update(&[2]);
            hasher.update(&1_u64.to_be_bytes());
            hasher.update(&[3]);
            hash_field(&mut hasher, 4, reason.as_str().as_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn hash_source_edge(hasher: &mut blake3::Hasher, content: [u8; 32], declared: [u8; 32]) {
    hash_field(hasher, 3, &content);
    hash_field(hasher, 5, &declared);
}

/// Why one dependency fact was observed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyAuthority {
    /// The registry's authenticated package metadata published the edge.
    RegistryMetadata,
    /// The package archive contained a manifest that declared the edge.
    ArchiveManifest,
    /// A code-forge manifest declared the edge.
    ForgeManifest,
    /// A local project manifest declared the edge.
    LocalManifest,
}

/// Scope in which a dependency participates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyScope {
    /// Normal runtime dependency.
    Runtime,
    /// Optional feature or extra dependency.
    Optional,
    /// Development and test dependency.
    Development,
    /// Build-time dependency.
    Build,
    /// Peer dependency supplied by the consuming application.
    Peer,
}

impl DependencyScope {
    /// Presentation precedence for callers that need one representative row.
    ///
    /// Graph admission and identity preserve every scope and never use this
    /// preference to discard a dependency fact.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Runtime => 5,
            Self::Build => 4,
            Self::Optional => 3,
            Self::Peer => 2,
            Self::Development => 1,
        }
    }
}

/// Returns the canonical `optional` flag for one dependency row.
///
/// Only [`DependencyScope::Optional`] rows and runtime rows whose manifest or
/// registry metadata explicitly marks them optional retain `optional = true`.
#[must_use]
pub const fn dependency_optional(scope: DependencyScope, declared_optional: bool) -> bool {
    match scope {
        DependencyScope::Optional => true,
        DependencyScope::Runtime => declared_optional,
        DependencyScope::Development | DependencyScope::Build | DependencyScope::Peer => false,
    }
}

/// Sorts dependency rows and removes only identical facts.
///
/// Requirements, targets, and scopes remain distinct graph edges even when
/// they share a target name. Only repeated copies with the same complete
/// content identity are collapsed.
#[must_use]
pub fn collapse_dependency_rows(
    rows: Vec<PackageDependencyRecord>,
) -> Vec<PackageDependencyRecord> {
    let mut rows = rows;
    // Deduplicate the complete admitted row, not its supplied digest. A
    // malformed caller must not be able to hide a distinct fact behind a
    // copied `facts_version` before admission verifies that digest.
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// A package lineage plus the version requirement written by its source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDependencyTarget {
    /// Ecosystem whose resolver owns the requirement grammar.
    pub ecosystem: RegistryEcosystem,
    /// Registry-qualified package name (for example `serde` or `org:artifact`).
    pub name: ProductText,
    /// Exact source spelling of the version requirement.
    pub requirement: ProductText,
    /// Exact package selected by a lockfile or resolver, when one is known.
    pub resolved: Option<PackageReference>,
}

impl PackageDependencyTarget {
    /// Admits one dependency target while retaining the resolver grammar.
    pub fn new(
        ecosystem: RegistryEcosystem,
        name: impl Into<String>,
        requirement: impl Into<String>,
        resolved: Option<PackageReference>,
    ) -> Result<Self, ProductAdmissionError> {
        let target = Self {
            ecosystem,
            name: ProductText::new(name)?,
            requirement: ProductText::new(requirement)?,
            resolved,
        };
        if let Some(reference) = &target.resolved {
            let PackageReference::Purl(purl) = reference else {
                return Err(ProductAdmissionError::PackageReference);
            };
            if purl.package_type().registry() != Some(ecosystem)
                || purl.lineage_name() != target.name.as_str()
            {
                return Err(ProductAdmissionError::PackageReference);
            }
        }
        Ok(target)
    }
}

/// Authority and content identities attached to one edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEvidence {
    /// Authority class that emitted the fact.
    pub authority: DependencyAuthority,
    /// Versioned source frontier (registry page, forge commit, or local root).
    pub frontier: [u8; 32],
    /// Digest of the exact source row or manifest bytes.
    pub provenance: [u8; 32],
}

/// One canonical outgoing package dependency edge.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDependencyRecord {
    /// Exact package release declaring the dependency.
    pub source: PackageReference,
    /// Source owner that published this edge. This is included in the edge's
    /// content identity so identical coordinates from distinct registries do
    /// not collapse in resident or durable indexes.
    pub source_authority: PackageGraphSourceAuthority,
    /// Declared target lineage and requirement.
    pub target: PackageDependencyTarget,
    /// Resolver scope of this edge.
    pub scope: DependencyScope,
    /// Whether the edge is excluded from the default resolution.
    pub optional: bool,
    /// Versioned source evidence.
    pub evidence: DependencyEvidence,
    /// Content identity of all fields above.
    pub facts_version: [u8; 32],
}

impl PackageDependencyRecord {
    /// Constructs a record and derives its stable content identity.
    pub fn new(
        source: PackageReference,
        target: PackageDependencyTarget,
        scope: DependencyScope,
        optional: bool,
        evidence: DependencyEvidence,
    ) -> Self {
        Self::new_with_source_authority(
            source,
            PackageGraphSourceAuthority::Unattributed,
            target,
            scope,
            optional,
            evidence,
        )
    }

    /// Constructs a source-bound record and derives its stable content identity.
    #[must_use]
    pub fn new_with_source_authority(
        source: PackageReference,
        source_authority: PackageGraphSourceAuthority,
        target: PackageDependencyTarget,
        scope: DependencyScope,
        optional: bool,
        evidence: DependencyEvidence,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.package-dependency.v3\0");
        hash_field(&mut hasher, 1, &package_reference_bytes(&source));
        hash_field(&mut hasher, 2, &source_authority.kind_tag().to_be_bytes());
        hash_field(&mut hasher, 3, &source_authority.id_bytes());
        hash_field(&mut hasher, 4, &[target.ecosystem as u8]);
        hash_field(&mut hasher, 5, target.name.as_str().as_bytes());
        hash_field(&mut hasher, 6, target.requirement.as_str().as_bytes());
        match &target.resolved {
            Some(resolved) => hash_field(&mut hasher, 7, &package_reference_bytes(resolved)),
            None => hash_field(&mut hasher, 7, &[0]),
        }
        hash_field(&mut hasher, 8, &[dependency_scope_tag(scope)]);
        hash_field(&mut hasher, 9, &[u8::from(optional)]);
        hash_field(
            &mut hasher,
            10,
            &[dependency_authority_tag(evidence.authority)],
        );
        hash_field(&mut hasher, 11, &evidence.frontier);
        hash_field(&mut hasher, 12, &evidence.provenance);
        Self {
            source,
            source_authority,
            target,
            scope,
            optional,
            evidence,
            facts_version: *hasher.finalize().as_bytes(),
        }
    }

    /// Recomputes the content identity for admission checks.
    #[must_use]
    pub fn recomputed_version(&self) -> [u8; 32] {
        Self::new_with_source_authority(
            self.source.clone(),
            self.source_authority,
            self.target.clone(),
            self.scope,
            self.optional,
            self.evidence,
        )
        .facts_version
    }

    /// Rebinds a decoded fact to the source owner that published it, deriving
    /// a new content identity that includes that owner.
    #[must_use]
    pub fn with_source_authority(self, source_authority: PackageGraphSourceAuthority) -> Self {
        Self::new_with_source_authority(
            self.source,
            source_authority,
            self.target,
            self.scope,
            self.optional,
            self.evidence,
        )
    }
}

/// Computes a canonical witness for the complete dependency-facts input.
///
/// Source witnesses are sorted before hashing, so callers that present the
/// same relation in a different iteration order get the same witness. Each
/// source witness includes its state and exact authority key, as well as both
/// the recomputed and declared identities of every edge.
#[must_use]
pub fn package_dependency_facts_witness(facts: &[PackageDependencySourceFacts]) -> [u8; 32] {
    let source_witnesses = facts
        .iter()
        .map(|(source, state)| package_dependency_source_facts_witness(source, state))
        .collect::<Vec<_>>();
    package_dependency_facts_witness_from_sources(&source_witnesses)
}

fn package_dependency_facts_witness_from_sources(source_witnesses: &[[u8; 32]]) -> [u8; 32] {
    let mut source_witnesses = source_witnesses.to_vec();
    source_witnesses.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.package-dependency-facts.v3\0");
    for witness in source_witnesses {
        hash_field(&mut hasher, 1, &witness);
    }
    *hasher.finalize().as_bytes()
}

fn dependency_scope_tag(scope: DependencyScope) -> u8 {
    match scope {
        DependencyScope::Runtime => 1,
        DependencyScope::Optional => 2,
        DependencyScope::Development => 3,
        DependencyScope::Build => 4,
        DependencyScope::Peer => 5,
    }
}

fn dependency_authority_tag(authority: DependencyAuthority) -> u8 {
    match authority {
        DependencyAuthority::RegistryMetadata => 1,
        DependencyAuthority::ArchiveManifest => 2,
        DependencyAuthority::ForgeManifest => 3,
        DependencyAuthority::LocalManifest => 4,
    }
}

fn package_reference_bytes(reference: &PackageReference) -> Vec<u8> {
    let (kind, value) = match reference {
        PackageReference::Purl(value) => (1, value.as_str()),
        PackageReference::Local(value) => (2, value.as_str()),
    };
    let mut encoded = Vec::with_capacity(19 + value.len());
    append_field(&mut encoded, 1, &[kind]);
    append_field(&mut encoded, 2, value.as_bytes());
    encoded
}

fn hash_field(hasher: &mut blake3::Hasher, tag: u8, value: &[u8]) {
    hasher.update(&[tag]);
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn append_field(output: &mut Vec<u8>, tag: u8, value: &[u8]) {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

/// Availability of dependency metadata at one immutable source frontier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum DependencyFacts<T> {
    /// Complete set of edges observed at this frontier (possibly empty).
    Known(T),
    /// The source is valid but does not publish dependency metadata.
    Unknown(ProductText),
    /// The source should publish metadata, but it was unavailable or rejected.
    Unavailable(ProductText),
}

impl<T> DependencyFacts<T> {
    /// Returns whether this value carries an actual edge set.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

/// Validates and canonicalizes a dependency edge collection.
pub fn admit_dependency_rows(
    mut rows: Vec<PackageDependencyRecord>,
) -> Result<Box<[PackageDependencyRecord]>, ProductAdmissionError> {
    if rows.len() > MAX_PACKAGE_GRAPH_ROWS {
        return Err(ProductAdmissionError::RowBound);
    }
    let mut identities = BTreeSet::new();
    for row in &rows {
        if row.facts_version != row.recomputed_version() || !identities.insert(row.facts_version) {
            return Err(ProductAdmissionError::DependencyShape);
        }
    }
    rows.sort_unstable_by_key(|row| row.facts_version);
    Ok(rows.into_boxed_slice())
}

/// Reverse package edges retained for repeated dependent lookups.
///
/// The index owns only the coordinates a lookup compares. Runtime and
/// optional edges are addressable by ecosystem and lineage. An unresolved
/// requirement matches every version of that lineage; a resolved edge matches
/// only that exact package URL. Development, build, and peer edges are omitted
/// because dependent answers do not count them.
#[derive(Clone, Debug, Default)]
pub struct PackageGraphIndex {
    by_source: BTreeMap<PackageGraphSourceKey, usize>,
    by_coordinate: BTreeMap<String, Vec<usize>>,
    reverse: BTreeMap<RegistryEcosystem, BTreeMap<String, ReverseEdges>>,
    first_gap: Option<ProductText>,
    checked_witness: Option<[u8; 32]>,
}

#[derive(Clone, Debug)]
struct ReverseEdge {
    source_index: usize,
    row_index: usize,
}

#[derive(Clone, Debug, Default)]
struct ReverseEdges {
    unresolved: Vec<ReverseEdge>,
    resolved: BTreeMap<String, Vec<ReverseEdge>>,
}

/// Sources that declare a runtime or optional edge onto one package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependentSources {
    /// Reverse lookup is defined for a pinned package URL.
    NotPurl,
    /// Matching sources, plus the first unknown or unavailable reason.
    ///
    /// The reason is reported only when no runtime or optional source matched.
    /// A later gap does not replace an earlier one.
    Matched {
        /// Exact package/authority pairs that declare a counted edge.
        sources: BTreeSet<PackageGraphSourceKey>,
        /// First unknown or unavailable fact, in source order.
        gap: Option<ProductText>,
    },
}

/// Result of a coordinate-only forward graph lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageDependencyLookup<'a> {
    /// No source fact has this coordinate.
    Missing,
    /// Exactly one source authority owns this coordinate.
    Exact {
        /// Exact source authority selected by the unique coordinate match.
        source: &'a PackageGraphSourceKey,
        /// Facts observed from that authority.
        facts: &'a DependencyFacts<Box<[PackageDependencyRecord]>>,
    },
    /// Several authorities publish the same coordinate; the caller must pick
    /// one exact key rather than merging or selecting by iteration order.
    Ambiguous(Box<[PackageGraphSourceKey]>),
}

impl PackageGraphIndex {
    /// Builds an index whose reverse postings are bound to this checked
    /// immutable facts snapshot.
    #[must_use]
    pub fn from_checked_facts(facts: &CheckedPackageGraphFacts) -> Self {
        let mut index = Self::from_facts(facts.facts());
        index.checked_witness = Some(facts.witness());
        index
    }

    /// Whether this index was built for the supplied immutable graph facts.
    pub(crate) fn is_bound_to(&self, facts: &CheckedPackageGraphFacts) -> bool {
        self.checked_witness == Some(facts.witness())
    }

    /// Builds forward and reverse adjacency from source/authority order.
    ///
    /// Multiple registries may publish the same coordinate. Their facts stay
    /// separate in both directions; malformed rows whose embedded source key
    /// disagrees with the containing key are ignored by this convenience
    /// index and rejected by [`CheckedPackageGraphFacts::new`].
    #[must_use]
    pub fn from_facts(facts: &[PackageDependencySourceFacts]) -> Self {
        let mut index = Self::default();
        for (source_index, (source, state)) in facts.iter().enumerate() {
            if !index.by_source.contains_key(source) {
                index.by_source.insert(source.clone(), source_index);
                index
                    .by_coordinate
                    .entry(source.coordinate.as_str().to_owned())
                    .or_default()
                    .push(source_index);
            }
            match state {
                DependencyFacts::Known(rows) => {
                    for (row_index, row) in rows.iter().enumerate() {
                        if !matches!(
                            row.scope,
                            DependencyScope::Runtime | DependencyScope::Optional
                        ) || row.source != source.coordinate
                            || row.source_authority != source.authority
                        {
                            continue;
                        }
                        let posting = index
                            .reverse
                            .entry(row.target.ecosystem)
                            .or_default()
                            .entry(row.target.name.as_str().to_owned())
                            .or_default();
                        let edge = ReverseEdge {
                            source_index,
                            row_index,
                        };
                        if let Some(resolved) = &row.target.resolved {
                            posting
                                .resolved
                                .entry(resolved.as_str().to_owned())
                                .or_default()
                                .push(edge);
                        } else {
                            posting.unresolved.push(edge);
                        }
                    }
                }
                DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                    if index.first_gap.is_none() {
                        index.first_gap = Some(reason.clone());
                    }
                }
            }
        }
        for names in index.reverse.values_mut() {
            for postings in names.values_mut() {
                postings
                    .unresolved
                    .sort_unstable_by_key(|edge| reverse_edge_id(facts, edge));
                for edges in postings.resolved.values_mut() {
                    edges.sort_unstable_by_key(|edge| reverse_edge_id(facts, edge));
                }
            }
        }
        index
    }

    /// Returns the unique source fact for `package`, or the exact source
    /// choices when several authorities publish that coordinate.
    #[must_use]
    pub fn dependencies<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> PackageDependencyLookup<'a> {
        let Some(indices) = self.by_coordinate.get(package.as_str()) else {
            return PackageDependencyLookup::Missing;
        };
        if indices.len() != 1 {
            return PackageDependencyLookup::Ambiguous(
                indices
                    .iter()
                    .filter_map(|index| facts.get(*index).map(|(key, _)| key.clone()))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            );
        }
        facts
            .get(indices[0])
            .map(|(source, state)| PackageDependencyLookup::Exact {
                source,
                facts: state,
            })
            .unwrap_or(PackageDependencyLookup::Missing)
    }

    /// Returns the fact for one exact package/authority key.
    #[must_use]
    pub fn dependencies_for_source<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        source: &PackageGraphSourceKey,
    ) -> Option<&'a DependencyFacts<Box<[PackageDependencyRecord]>>> {
        self.by_source
            .get(source)
            .and_then(|index| facts.get(*index))
            .map(|(_, state)| state)
    }

    /// Returns the packages that depend on `package` under the counted scopes.
    #[must_use]
    pub fn dependent_sources(
        &self,
        facts: &[PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> DependentSources {
        let PackageReference::Purl(target) = package else {
            return DependentSources::NotPurl;
        };
        let Some(ecosystem) = target.package_type().registry() else {
            return DependentSources::Matched {
                sources: BTreeSet::new(),
                gap: self.first_gap.clone(),
            };
        };
        let mut sources = BTreeSet::new();
        if let Some(edges) = self
            .reverse
            .get(&ecosystem)
            .and_then(|names| names.get(target.lineage_name()))
        {
            for edge in edges
                .unresolved
                .iter()
                .chain(edges.resolved.get(target.as_str()).into_iter().flatten())
            {
                if let Some((source, _)) = facts.get(edge.source_index) {
                    sources.insert(source.clone());
                }
            }
        }
        DependentSources::Matched {
            sources,
            gap: self.first_gap.clone(),
        }
    }

    /// Returns the exact reverse edge page for a pinned registry package.
    /// The unresolved and exact-version postings are independently sorted by
    /// edge identity, so this visits only the matching posting suffix and at
    /// most `limit + 1` rows.
    pub(crate) fn dependent_edges_page<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
        after: Option<[u8; 32]>,
        limit: u16,
    ) -> (Vec<&'a PackageDependencyRecord>, bool) {
        let PackageReference::Purl(target) = package else {
            return (Vec::new(), false);
        };
        let Some(ecosystem) = target.package_type().registry() else {
            return (Vec::new(), false);
        };
        let Some(postings) = self
            .reverse
            .get(&ecosystem)
            .and_then(|names| names.get(target.lineage_name()))
        else {
            return (Vec::new(), false);
        };

        let unresolved = &postings.unresolved;
        let resolved = postings
            .resolved
            .get(target.as_str())
            .map_or(&[][..], Vec::as_slice);
        let mut unresolved_index = after.map_or(0, |edge_id| {
            unresolved.partition_point(|edge| reverse_edge_id(facts, edge) <= edge_id)
        });
        let mut resolved_index = after.map_or(0, |edge_id| {
            resolved.partition_point(|edge| reverse_edge_id(facts, edge) <= edge_id)
        });
        let page_bound = usize::from(limit).saturating_add(1);
        let mut rows = Vec::with_capacity(page_bound);
        while rows.len() < page_bound {
            let next = match (
                unresolved.get(unresolved_index),
                resolved.get(resolved_index),
            ) {
                (None, None) => break,
                (Some(edge), None) => {
                    unresolved_index += 1;
                    edge
                }
                (None, Some(edge)) => {
                    resolved_index += 1;
                    edge
                }
                (Some(left), Some(right)) => {
                    if reverse_edge_id(facts, left) <= reverse_edge_id(facts, right) {
                        unresolved_index += 1;
                        left
                    } else {
                        resolved_index += 1;
                        right
                    }
                }
            };
            let Some((_, DependencyFacts::Known(source_rows))) = facts.get(next.source_index)
            else {
                continue;
            };
            if let Some(row) = source_rows.get(next.row_index) {
                rows.push(row);
            }
        }
        let more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        (rows, more)
    }
}

fn reverse_edge_id(facts: &[PackageDependencySourceFacts], edge: &ReverseEdge) -> [u8; 32] {
    facts
        .get(edge.source_index)
        .and_then(|(_, state)| match state {
            DependencyFacts::Known(rows) => rows.get(edge.row_index),
            DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => None,
        })
        .map(|row| row.facts_version)
        .expect("reverse posting must point to a known dependency row")
}

/// Walks every fact the same way the reverse index does.
///
/// Benchmarks and tests use this as the baseline. Production answers use
/// [`PackageGraphIndex`].
#[must_use]
pub fn linear_dependent_sources(
    facts: &[PackageDependencySourceFacts],
    package: &PackageReference,
) -> DependentSources {
    let PackageReference::Purl(target) = package else {
        return DependentSources::NotPurl;
    };
    let ecosystem = target.package_type().registry();
    let mut sources = BTreeSet::new();
    let mut gap = None;
    for (source, state) in facts {
        match state {
            DependencyFacts::Known(rows) => {
                if rows.iter().any(|row| {
                    matches!(
                        row.scope,
                        DependencyScope::Runtime | DependencyScope::Optional
                    ) && Some(row.target.ecosystem) == ecosystem
                        && row.target.name.as_str() == target.lineage_name()
                        && row
                            .target
                            .resolved
                            .as_ref()
                            .is_none_or(|resolved| resolved.as_str() == target.as_str())
                }) {
                    sources.insert(source.clone());
                }
            }
            DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                if gap.is_none() {
                    gap = Some(reason.clone());
                }
            }
        }
    }
    DependentSources::Matched { sources, gap }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn source() -> PackageReference {
        PackageReference::parse("pkg:cargo/demo@1.0.0").expect("valid source")
    }

    fn edge(requirement: &str, frontier: u8) -> PackageDependencyRecord {
        PackageDependencyRecord::new(
            source(),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", requirement, None)
                .expect("valid target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [frontier; 32],
                provenance: [frontier.saturating_add(1); 32],
            },
        )
    }

    #[test]
    fn facts_identity_includes_requirement_and_source_evidence() {
        assert_ne!(edge("^1", 1).facts_version, edge("^2", 1).facts_version);
        assert_ne!(edge("^1", 1).facts_version, edge("^1", 2).facts_version);
    }

    #[test]
    fn facts_identity_changes_for_each_graph_field() {
        let base = edge("^1", 1);
        let evidence = base.evidence;
        let record = |source: PackageReference,
                      target: PackageDependencyTarget,
                      scope: DependencyScope,
                      optional: bool,
                      evidence: DependencyEvidence| {
            PackageDependencyRecord::new(source, target, scope, optional, evidence)
        };
        let target = |ecosystem, name: &str, requirement: &str, resolved: Option<&str>| {
            PackageDependencyTarget::new(
                ecosystem,
                name,
                requirement,
                resolved.map(|value| PackageReference::parse(value).expect("resolved")),
            )
            .expect("target")
        };
        let variants = [
            record(
                PackageReference::Local(ProductText::new("other source").expect("source")),
                base.target.clone(),
                base.scope,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                target(RegistryEcosystem::Npm, "serde", "^1", None),
                base.scope,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                target(RegistryEcosystem::Cargo, "tokio", "^1", None),
                base.scope,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                target(RegistryEcosystem::Cargo, "serde", "^2", None),
                base.scope,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                target(
                    RegistryEcosystem::Cargo,
                    "serde",
                    "^1",
                    Some("pkg:cargo/serde@1.0.0"),
                ),
                base.scope,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                base.target.clone(),
                DependencyScope::Development,
                base.optional,
                evidence,
            ),
            record(
                base.source.clone(),
                base.target.clone(),
                base.scope,
                true,
                evidence,
            ),
            record(
                base.source.clone(),
                base.target.clone(),
                base.scope,
                base.optional,
                DependencyEvidence {
                    authority: DependencyAuthority::ArchiveManifest,
                    ..evidence
                },
            ),
            record(
                base.source.clone(),
                base.target.clone(),
                base.scope,
                base.optional,
                DependencyEvidence {
                    frontier: [3; 32],
                    ..evidence
                },
            ),
            record(
                base.source.clone(),
                base.target.clone(),
                base.scope,
                base.optional,
                DependencyEvidence {
                    provenance: [4; 32],
                    ..evidence
                },
            ),
        ];
        assert!(
            variants
                .iter()
                .all(|variant| variant.facts_version != base.facts_version)
        );
    }

    #[test]
    fn checked_graph_facts_cache_an_immutable_witness_and_track_mutations() {
        let source = PackageGraphSourceKey::unattributed(source());
        let original_edge = edge("^1", 1);
        let original_facts = vec![(
            source.clone(),
            DependencyFacts::Known(vec![original_edge.clone()].into_boxed_slice()),
        )];
        let expected_witness = package_dependency_facts_witness(&original_facts);
        let checked = CheckedPackageGraphFacts::new(original_facts.clone()).expect("check facts");
        let checked_clone = checked.clone();
        assert_eq!(checked.witness(), expected_witness);
        assert_eq!(checked_clone.witness(), expected_witness);
        assert_eq!(checked.source_witnesses().len(), checked.facts().len());
        assert_eq!(
            checked.source_witnesses()[0],
            package_dependency_source_facts_witness(&source, &checked.facts()[0].1)
        );
        assert_eq!(
            checked.source_witnesses().as_ptr(),
            checked_clone.source_witnesses().as_ptr()
        );
        assert!(std::ptr::eq(
            checked.facts().as_ptr(),
            checked_clone.facts().as_ptr()
        ));
        assert_eq!(checked.facts(), original_facts.as_slice());

        let changed_edge = edge("^2", 1);
        let changed_requirement = CheckedPackageGraphFacts::new(vec![(
            source.clone(),
            DependencyFacts::Known(vec![changed_edge].into_boxed_slice()),
        )])
        .expect("check changed requirement");
        assert_ne!(changed_requirement.witness(), checked.witness());
        assert_ne!(
            changed_requirement.source_witnesses()[0],
            checked.source_witnesses()[0]
        );
        assert_eq!(checked.witness(), expected_witness);

        let changed_state = CheckedPackageGraphFacts::new(vec![(
            source.clone(),
            DependencyFacts::Unavailable(ProductText::new("metadata unavailable").expect("reason")),
        )])
        .expect("check changed state");
        assert_ne!(changed_state.witness(), checked.witness());
        assert_ne!(
            changed_state.source_witnesses()[0],
            checked.source_witnesses()[0]
        );

        let mut caller_owned = original_facts;
        caller_owned.clear();
        assert_eq!(checked.facts().len(), 1);
        let DependencyFacts::Known(cached_rows) = &checked.facts()[0].1 else {
            panic!("checked dependency rows should stay known");
        };
        assert_eq!(cached_rows.as_ref(), std::slice::from_ref(&original_edge));
    }

    #[test]
    fn checked_graph_facts_reject_stale_duplicate_and_mismatched_rows() {
        let source = PackageGraphSourceKey::unattributed(source());
        let valid = edge("^1", 1);
        let mut stale = valid.clone();
        stale.facts_version = [0; 32];
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![(
                source.clone(),
                DependencyFacts::Known(vec![stale].into_boxed_slice()),
            )])
            .unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![(
                source.clone(),
                DependencyFacts::Known(vec![valid.clone(), valid.clone()].into_boxed_slice()),
            )])
            .unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
        let other_source = PackageGraphSourceKey::unattributed(
            PackageReference::parse("pkg:cargo/other@1.0.0").expect("other"),
        );
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![(
                other_source,
                DependencyFacts::Known(vec![valid].into_boxed_slice()),
            )])
            .unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
    }

    #[test]
    fn legacy_dependency_fact_identities_are_rejected_for_refetch() {
        let mut legacy = edge("^1", 4);
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.package-dependency.v1\0");
        hasher.update(legacy.source.as_str().as_bytes());
        hasher.update(legacy.target.name.as_str().as_bytes());
        hasher.update(legacy.target.requirement.as_str().as_bytes());
        hasher.update(&[
            legacy.target.ecosystem as u8,
            legacy.scope as u8,
            u8::from(legacy.optional),
        ]);
        if let Some(resolved) = &legacy.target.resolved {
            hasher.update(&[1]);
            hasher.update(resolved.as_str().as_bytes());
        } else {
            hasher.update(&[0]);
        }
        hasher.update(&[legacy.evidence.authority as u8]);
        hasher.update(&legacy.evidence.frontier);
        hasher.update(&legacy.evidence.provenance);
        legacy.facts_version = *hasher.finalize().as_bytes();

        assert_ne!(legacy.facts_version, legacy.recomputed_version());
        assert_eq!(
            admit_dependency_rows(vec![legacy.clone()]).unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
        let source_key = PackageGraphSourceKey::unattributed(legacy.source.clone());
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![(
                source_key,
                DependencyFacts::Known(vec![legacy].into_boxed_slice()),
            )])
            .unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
    }

    #[test]
    fn facts_identity_frames_ambiguous_adjacent_strings() {
        let make = |source: &str, name: &str| {
            PackageDependencyRecord::new(
                PackageReference::Local(ProductText::new(source).expect("source")),
                PackageDependencyTarget::new(RegistryEcosystem::Cargo, name, "^1", None)
                    .expect("target"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: [1; 32],
                    provenance: [2; 32],
                },
            )
        };
        let first = make("a", "beta");
        let second = make("ab", "eta");
        assert_eq!(
            format!(
                "{}{}{}",
                first.source.as_str(),
                first.target.name.as_str(),
                first.target.requirement.as_str()
            ),
            format!(
                "{}{}{}",
                second.source.as_str(),
                second.target.name.as_str(),
                second.target.requirement.as_str()
            )
        );
        assert_ne!(first.facts_version, second.facts_version);
        assert_eq!(first.facts_version, first.recomputed_version());
        assert_eq!(second.facts_version, second.recomputed_version());
    }

    #[test]
    fn admission_sorts_and_rejects_duplicate_fact_identities() {
        let first = edge("^1", 1);
        let second = edge("^2", 2);
        let admitted = admit_dependency_rows(vec![second.clone(), first.clone()]).expect("admit");
        assert_eq!(
            admitted[0].facts_version,
            first.facts_version.min(second.facts_version)
        );
        assert!(admit_dependency_rows(vec![first.clone(), first]).is_err());
    }

    #[test]
    fn unknown_and_unavailable_are_distinct_wire_states() {
        let unknown = DependencyFacts::<Box<[PackageDependencyRecord]>>::Unknown(
            ProductText::new("registry omits this field").expect("reason"),
        );
        let unavailable = DependencyFacts::<Box<[PackageDependencyRecord]>>::Unavailable(
            ProductText::new("registry request failed").expect("reason"),
        );
        assert_ne!(unknown, unavailable);
        let encoded = serde_json::to_string(&unknown).expect("encode");
        assert!(encoded.contains("unknown"));
    }

    #[test]
    fn graph_facts_witness_is_order_independent_and_detects_stale_ids() {
        let first = edge("^1", 1);
        let second = edge("^2", 2);
        let facts = vec![
            (
                PackageGraphSourceKey::unattributed(source()),
                DependencyFacts::Known(vec![first.clone(), second.clone()].into_boxed_slice()),
            ),
            (
                PackageGraphSourceKey::unattributed(PackageReference::Local(
                    ProductText::new("local source").expect("local source"),
                )),
                DependencyFacts::Unknown(ProductText::new("no manifest").expect("reason")),
            ),
        ];
        let reordered = vec![
            (
                PackageGraphSourceKey::unattributed(PackageReference::Local(
                    ProductText::new("local source").expect("local source"),
                )),
                DependencyFacts::Unknown(ProductText::new("no manifest").expect("reason")),
            ),
            (
                PackageGraphSourceKey::unattributed(source()),
                DependencyFacts::Known(vec![second, first.clone()].into_boxed_slice()),
            ),
        ];
        assert_eq!(
            package_dependency_facts_witness(&facts),
            package_dependency_facts_witness(&reordered)
        );
        let checked = CheckedPackageGraphFacts::new(facts.clone()).expect("check facts");
        let checked_reordered =
            CheckedPackageGraphFacts::new(reordered.clone()).expect("check reordered facts");
        assert_eq!(checked.witness(), package_dependency_facts_witness(&facts));
        assert_eq!(checked.facts(), checked_reordered.facts());
        assert_eq!(
            checked.source_witnesses(),
            checked_reordered.source_witnesses()
        );

        let mut mutated = first;
        mutated.target.requirement = ProductText::new("^9").expect("mutated requirement");
        let mutated_facts = vec![(
            PackageGraphSourceKey::unattributed(source()),
            DependencyFacts::Known(vec![mutated].into_boxed_slice()),
        )];
        let original_facts = vec![(
            PackageGraphSourceKey::unattributed(source()),
            DependencyFacts::Known(vec![edge("^1", 1)].into_boxed_slice()),
        )];
        assert_ne!(
            package_dependency_facts_witness(&original_facts),
            package_dependency_facts_witness(&mutated_facts)
        );
    }

    #[test]
    fn package_source_adapter_uses_the_shared_selection_policy() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nudox-package-discovery-{suffix}"));
        fs::create_dir_all(root.join("src")).expect("source directory");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("dependency directory");
        fs::write(root.join("src/lib.rs"), b"pub fn source() {}").expect("source");
        fs::write(
            root.join("node_modules/pkg/lib.rs"),
            b"pub fn generated() {}",
        )
        .expect("generated");

        let paths = discover_source_files(&root, source_selection_policy()).expect("discover");
        let relative = paths
            .iter()
            .map(|path| path.strip_prefix(&root).expect("relative").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(relative, [PathBuf::from("src/lib.rs")]);
        let _ = fs::remove_dir_all(root);
    }

    fn dependency_row(
        name: &str,
        scope: DependencyScope,
        optional: bool,
    ) -> PackageDependencyRecord {
        PackageDependencyRecord::new(
            source(),
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, name, "^1", None)
                .expect("valid target"),
            scope,
            optional,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [1; 32],
                provenance: [2; 32],
            },
        )
    }

    #[test]
    fn dependency_optional_is_false_for_development_build_and_peer() {
        assert!(!dependency_optional(DependencyScope::Development, true));
        assert!(!dependency_optional(DependencyScope::Build, true));
        assert!(!dependency_optional(DependencyScope::Peer, true));
        assert!(dependency_optional(DependencyScope::Optional, false));
        assert!(dependency_optional(DependencyScope::Runtime, true));
        assert!(!dependency_optional(DependencyScope::Runtime, false));
    }

    #[test]
    fn collapse_preserves_scope_requirement_and_target_distinctions() {
        let runtime = dependency_row("serde", DependencyScope::Runtime, false);
        let development = dependency_row("serde", DependencyScope::Development, false);
        let optional_runtime = dependency_row("serde", DependencyScope::Runtime, true);
        let required_runtime = dependency_row("serde", DependencyScope::Runtime, false);
        let different_requirement = edge("^2", 1);
        let different_target = dependency_row("tokio", DependencyScope::Runtime, false);
        let collapsed = collapse_dependency_rows(vec![
            development.clone(),
            runtime.clone(),
            optional_runtime.clone(),
            required_runtime.clone(),
            different_requirement.clone(),
            different_target.clone(),
            runtime.clone(),
        ]);
        assert_eq!(collapsed.len(), 5);
        assert_eq!(runtime.facts_version, required_runtime.facts_version);
        let identities = collapsed
            .iter()
            .map(|row| row.facts_version)
            .collect::<BTreeSet<_>>();
        assert!(identities.contains(&development.facts_version));
        assert!(identities.contains(&runtime.facts_version));
        assert!(identities.contains(&optional_runtime.facts_version));
        assert!(identities.contains(&required_runtime.facts_version));
        assert!(identities.contains(&different_requirement.facts_version));
        assert!(identities.contains(&different_target.facts_version));
    }

    #[test]
    fn collapse_cannot_hide_a_distinct_row_with_a_forged_fact_id() {
        let first = dependency_row("serde", DependencyScope::Runtime, false);
        let mut forged = dependency_row("tokio", DependencyScope::Runtime, false);
        forged.facts_version = first.facts_version;
        let rows = collapse_dependency_rows(vec![first, forged]);
        assert_eq!(rows.len(), 2);
        assert!(admit_dependency_rows(rows).is_err());
    }

    fn fact(
        source: &str,
        scope: DependencyScope,
        name: &str,
        ecosystem: RegistryEcosystem,
        resolved: Option<&str>,
        frontier: u8,
    ) -> PackageDependencySourceFacts {
        let source_ref = PackageReference::parse(source).expect("source");
        let resolved = resolved.map(|value| PackageReference::parse(value).expect("resolved"));
        let row = PackageDependencyRecord::new(
            source_ref.clone(),
            PackageDependencyTarget::new(ecosystem, name, "^1", resolved).expect("target"),
            scope,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [frontier; 32],
                provenance: [frontier.wrapping_add(1); 32],
            },
        );
        (
            PackageGraphSourceKey::unattributed(source_ref),
            DependencyFacts::Known(vec![row].into_boxed_slice()),
        )
    }

    #[test]
    fn reverse_index_matches_the_linear_scan_on_adversarial_edges() {
        let target_v1 = PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("v1");
        let target_v2 = PackageReference::parse("pkg:cargo/target-lib@2.0.0").expect("v2");
        let mut facts = vec![
            fact(
                "pkg:cargo/exact@1.0.0",
                DependencyScope::Runtime,
                "target-lib",
                RegistryEcosystem::Cargo,
                Some("pkg:cargo/target-lib@1.0.0"),
                1,
            ),
            fact(
                "pkg:cargo/other-version@1.0.0",
                DependencyScope::Runtime,
                "target-lib",
                RegistryEcosystem::Cargo,
                Some("pkg:cargo/target-lib@2.0.0"),
                2,
            ),
            fact(
                "pkg:cargo/unresolved@1.0.0",
                DependencyScope::Runtime,
                "target-lib",
                RegistryEcosystem::Cargo,
                None,
                3,
            ),
            fact(
                "pkg:cargo/optional-src@1.0.0",
                DependencyScope::Optional,
                "target-lib",
                RegistryEcosystem::Cargo,
                None,
                4,
            ),
            fact(
                "pkg:cargo/dev-src@1.0.0",
                DependencyScope::Development,
                "target-lib",
                RegistryEcosystem::Cargo,
                None,
                5,
            ),
            fact(
                "pkg:cargo/build-src@1.0.0",
                DependencyScope::Build,
                "target-lib",
                RegistryEcosystem::Cargo,
                None,
                6,
            ),
            fact(
                "pkg:cargo/peer-src@1.0.0",
                DependencyScope::Peer,
                "target-lib",
                RegistryEcosystem::Cargo,
                None,
                7,
            ),
            fact(
                "pkg:npm/same-name@1.0.0",
                DependencyScope::Runtime,
                "target-lib",
                RegistryEcosystem::Npm,
                None,
                8,
            ),
            (
                PackageGraphSourceKey::unattributed(
                    PackageReference::parse("pkg:cargo/unknown-src@1.0.0").expect("unknown"),
                ),
                DependencyFacts::Unknown(ProductText::new("first gap").expect("gap")),
            ),
            (
                PackageGraphSourceKey::unattributed(
                    PackageReference::parse("pkg:cargo/later-gap@1.0.0").expect("later"),
                ),
                DependencyFacts::Unavailable(ProductText::new("second gap").expect("gap")),
            ),
        ];
        let index = PackageGraphIndex::from_facts(&facts);
        let indexed = index.dependent_sources(&facts, &target_v1);
        let linear = linear_dependent_sources(&facts, &target_v1);
        assert_eq!(indexed, linear);
        let DependentSources::Matched { sources, gap } = indexed else {
            panic!("purl lookup");
        };
        assert_eq!(
            sources
                .iter()
                .map(PackageGraphSourceKey::as_str)
                .collect::<Vec<_>>(),
            vec![
                "pkg:cargo/exact@1.0.0",
                "pkg:cargo/optional-src@1.0.0",
                "pkg:cargo/unresolved@1.0.0",
            ]
        );
        assert_eq!(gap.expect("gap").as_str(), "first gap");
        let v2 = index.dependent_sources(&facts, &target_v2);
        assert_eq!(v2, linear_dependent_sources(&facts, &target_v2));
        let DependentSources::Matched { sources, .. } = v2 else {
            panic!("v2");
        };
        assert!(
            sources
                .iter()
                .any(|source| source.as_str() == "pkg:cargo/other-version@1.0.0")
        );
        assert!(
            sources
                .iter()
                .all(|source| source.as_str() != "pkg:cargo/exact@1.0.0")
        );
        assert_eq!(
            index.dependent_sources(
                &facts,
                &PackageReference::parse("local-pkg").expect("local")
            ),
            DependentSources::NotPurl
        );
        facts.retain(|(_, state)| !matches!(state, DependencyFacts::Known(_)));
        let gaps = PackageGraphIndex::from_facts(&facts);
        let DependentSources::Matched { sources, gap } = gaps.dependent_sources(&facts, &target_v1)
        else {
            panic!("gaps");
        };
        assert!(sources.is_empty());
        assert_eq!(gap.expect("only gap").as_str(), "first gap");
        let known = fact(
            "pkg:cargo/exact@1.0.0",
            DependencyScope::Runtime,
            "target-lib",
            RegistryEcosystem::Cargo,
            Some("pkg:cargo/target-lib@1.0.0"),
            1,
        );
        assert!(matches!(
            PackageGraphIndex::from_facts(std::slice::from_ref(&known))
                .dependencies(std::slice::from_ref(&known), &known.0.coordinate),
            PackageDependencyLookup::Exact { facts, .. } if facts.is_known()
        ));
    }

    #[test]
    fn same_coordinate_from_two_registries_stays_distinct_and_forge_cannot_impersonate_one() {
        let coordinate = PackageReference::parse("pkg:cargo/shared@1.0.0").expect("source");
        let target = PackageReference::parse("pkg:cargo/target@1.0.0").expect("target");
        let registry_a = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x11; 32]),
        );
        let registry_b = PackageGraphSourceAuthority::Registry(
            RegistryAuthorityId::from_configured_source([0x22; 32]),
        );
        let record = |authority| {
            PackageDependencyRecord::new_with_source_authority(
                coordinate.clone(),
                authority,
                PackageDependencyTarget::new(
                    RegistryEcosystem::Cargo,
                    "target",
                    "*",
                    Some(target.clone()),
                )
                .expect("dependency target"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: [3; 32],
                    provenance: [4; 32],
                },
            )
        };
        let facts = vec![
            (
                PackageGraphSourceKey::new(coordinate.clone(), registry_a),
                DependencyFacts::Known(vec![record(registry_a)].into_boxed_slice()),
            ),
            (
                PackageGraphSourceKey::new(coordinate.clone(), registry_b),
                DependencyFacts::Known(vec![record(registry_b)].into_boxed_slice()),
            ),
        ];
        let checked = CheckedPackageGraphFacts::new(facts).expect("two exact registry facts");
        let index = PackageGraphIndex::from_facts(checked.facts());
        assert!(matches!(
            index.dependencies(checked.facts(), &coordinate),
            PackageDependencyLookup::Ambiguous(keys)
                if keys.len() == 2 && keys[0] != keys[1]
        ));
        let dependents = index.dependent_sources(checked.facts(), &target);
        let DependentSources::Matched { sources, .. } = dependents else {
            panic!("target is a pinned package URL");
        };
        assert_eq!(sources.len(), 2);
        assert!(sources.iter().all(|key| key.coordinate == coordinate));
        assert_ne!(
            sources.iter().next().expect("registry A"),
            sources.iter().next_back().expect("registry B")
        );

        let forged = PackageDependencyRecord::new_with_source_authority(
            coordinate.clone(),
            registry_a,
            PackageDependencyTarget::new(RegistryEcosystem::Cargo, "target", "*", None)
                .expect("dependency target"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::ForgeManifest,
                frontier: [5; 32],
                provenance: [6; 32],
            },
        );
        assert_eq!(
            CheckedPackageGraphFacts::new(vec![(
                PackageGraphSourceKey::new(coordinate, registry_a),
                DependencyFacts::Known(vec![forged].into_boxed_slice()),
            )])
            .unwrap_err(),
            ProductAdmissionError::DependencyShape
        );
    }

    #[test]
    fn reverse_index_matches_linear_scan_across_seeded_graphs() {
        let mut state = 0x7a89_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            state
        };
        let ecosystems = [
            (RegistryEcosystem::Cargo, "cargo"),
            (RegistryEcosystem::Npm, "npm"),
            (RegistryEcosystem::Pypi, "pypi"),
        ];
        let scopes = [
            DependencyScope::Runtime,
            DependencyScope::Optional,
            DependencyScope::Development,
            DependencyScope::Build,
            DependencyScope::Peer,
        ];
        for _graph in 0..64 {
            let source_count = usize::try_from(next() % 24).expect("count");
            let mut facts = Vec::<PackageDependencySourceFacts>::with_capacity(source_count);
            for source_index in 0..source_count {
                let &(ecosystem, token) = ecosystems
                    .get(usize::try_from(next() % 3).expect("eco"))
                    .expect("ecosystem");
                let name = format!("lib-{}", next() % 5);
                let version = next() % 3;
                let spelling = format!("pkg:{token}/{name}@{version}.0.0");
                let reuse = !facts.is_empty() && next() % 7 == 0;
                let source_ref = if reuse {
                    facts
                        .first()
                        .expect("duplicate source")
                        .0
                        .coordinate
                        .clone()
                } else {
                    PackageReference::parse(&spelling).expect("source")
                };
                let source_authority = PackageGraphSourceAuthority::Registry(
                    RegistryAuthorityId::from_configured_source(
                        [u8::try_from(source_index).expect("bounded source ordinal"); 32],
                    ),
                );
                let source_key = PackageGraphSourceKey::new(source_ref.clone(), source_authority);
                match next() % 5 {
                    0 => facts.push((
                        source_key,
                        DependencyFacts::Unknown(
                            ProductText::new(format!("unknown-{source_index}")).expect("reason"),
                        ),
                    )),
                    1 => facts.push((
                        source_key,
                        DependencyFacts::Unavailable(
                            ProductText::new(format!("unavailable-{source_index}"))
                                .expect("reason"),
                        ),
                    )),
                    _ => {
                        let edge_count = usize::try_from(next() % 5).expect("edges");
                        let mut rows = Vec::with_capacity(edge_count);
                        for edge in 0..edge_count {
                            let target_name = format!("dep-{}", next() % 4);
                            let target_version = next() % 3;
                            let resolved = if next() % 2 == 0 {
                                Some(
                                    PackageReference::parse(format!(
                                        "pkg:{token}/{target_name}@{target_version}.0.0"
                                    ))
                                    .expect("resolved"),
                                )
                            } else {
                                None
                            };
                            let scope = *scopes
                                .get(usize::try_from(next() % 5).expect("scope"))
                                .expect("scope");
                            rows.push(PackageDependencyRecord::new_with_source_authority(
                                source_ref.clone(),
                                source_authority,
                                PackageDependencyTarget::new(
                                    ecosystem,
                                    target_name,
                                    "^1",
                                    resolved,
                                )
                                .expect("target"),
                                scope,
                                next() % 2 == 0,
                                DependencyEvidence {
                                    authority: DependencyAuthority::RegistryMetadata,
                                    frontier: [u8::try_from(edge).unwrap_or(0); 32],
                                    provenance: [u8::try_from(source_index).unwrap_or(0); 32],
                                },
                            ));
                        }
                        facts.push((source_key, DependencyFacts::Known(rows.into_boxed_slice())));
                    }
                }
            }
            let index = PackageGraphIndex::from_facts(&facts);
            let mut queries = vec![
                PackageReference::parse("local-pkg").expect("local"),
                PackageReference::parse("pkg:cargo/missing@9.0.0").expect("missing"),
            ];
            for (source, state) in &facts {
                queries.push(source.coordinate.clone());
                if let DependencyFacts::Known(rows) = state {
                    for row in rows.iter() {
                        if let Some(resolved) = &row.target.resolved {
                            queries.push(resolved.clone());
                        }
                        queries.push(
                            PackageReference::parse(format!(
                                "pkg:cargo/{}@7.0.0",
                                row.target.name.as_str()
                            ))
                            .expect("other version"),
                        );
                    }
                }
            }
            for query in &queries {
                assert_eq!(
                    index.dependent_sources(&facts, query),
                    linear_dependent_sources(&facts, query),
                    "dependents diverged for {}",
                    query.as_str()
                );
                let matches = facts
                    .iter()
                    .filter(|(source, _)| source.coordinate == *query)
                    .collect::<Vec<_>>();
                let expected = match matches.as_slice() {
                    [] => PackageDependencyLookup::Missing,
                    [(source, state)] => PackageDependencyLookup::Exact {
                        source,
                        facts: state,
                    },
                    many => PackageDependencyLookup::Ambiguous(
                        many.iter()
                            .map(|(source, _)| (*source).clone())
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ),
                };
                assert_eq!(
                    index.dependencies(&facts, query),
                    expected,
                    "forward lookup diverged for {}",
                    query.as_str()
                );
            }
        }
    }

    #[test]
    fn warm_reverse_lookup_beats_a_full_fact_scan() {
        const SOURCES: usize = 4_096;
        const EDGES: usize = 8;
        let mut facts = Vec::with_capacity(SOURCES);
        for source_index in 0..SOURCES {
            let source = PackageReference::parse(format!("pkg:cargo/source-{source_index}@1.0.0"))
                .expect("source");
            let source_key = PackageGraphSourceKey::unattributed(source.clone());
            let mut rows = Vec::with_capacity(EDGES);
            for edge in 0..EDGES {
                let (name, resolved, scope) = if source_index < 4 && edge == 0 {
                    (
                        "target-lib",
                        Some(
                            PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target"),
                        ),
                        DependencyScope::Runtime,
                    )
                } else if source_index == 5 && edge == 0 {
                    ("target-lib", None, DependencyScope::Development)
                } else if source_index == 6 && edge == 0 {
                    ("target-lib", None, DependencyScope::Optional)
                } else {
                    ("other-lib", None, DependencyScope::Runtime)
                };
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    PackageDependencyTarget::new(RegistryEcosystem::Cargo, name, "^1", resolved)
                        .expect("target"),
                    scope,
                    false,
                    DependencyEvidence {
                        authority: DependencyAuthority::RegistryMetadata,
                        frontier: [u8::try_from(edge).unwrap_or(0); 32],
                        provenance: [u8::try_from(source_index).unwrap_or(0); 32],
                    },
                ));
            }
            facts.push((source_key, DependencyFacts::Known(rows.into_boxed_slice())));
        }
        let target = PackageReference::parse("pkg:cargo/target-lib@1.0.0").expect("target");
        let index = PackageGraphIndex::from_facts(&facts);
        assert_eq!(
            index.dependent_sources(&facts, &target),
            linear_dependent_sources(&facts, &target)
        );
        let mut lookup_samples = Vec::with_capacity(9);
        let mut scan_samples = Vec::with_capacity(9);
        let mut cold_samples = Vec::with_capacity(9);
        for sample in 0..11 {
            let cold_started = std::time::Instant::now();
            let cold = PackageGraphIndex::from_facts(&facts).dependent_sources(&facts, &target);
            let cold_elapsed = cold_started.elapsed().as_nanos();
            let lookup_started = std::time::Instant::now();
            let looked = index.dependent_sources(&facts, &target);
            let lookup_elapsed = lookup_started.elapsed().as_nanos();
            let scan_started = std::time::Instant::now();
            let scanned = linear_dependent_sources(&facts, &target);
            let scan_elapsed = scan_started.elapsed().as_nanos();
            std::hint::black_box((cold, looked, scanned));
            if sample >= 2 {
                cold_samples.push(cold_elapsed);
                lookup_samples.push(lookup_elapsed);
                scan_samples.push(scan_elapsed);
            }
        }
        cold_samples.sort_unstable();
        lookup_samples.sort_unstable();
        scan_samples.sort_unstable();
        let cold_median = cold_samples[cold_samples.len() / 2];
        let lookup_median = lookup_samples[lookup_samples.len() / 2];
        let scan_median = scan_samples[scan_samples.len() / 2];
        eprintln!(
            "reverse_index cold_median_ns={cold_median} lookup_median_ns={lookup_median} \
             scan_median_ns={scan_median} sources={SOURCES}"
        );
        assert!(
            lookup_median.saturating_mul(32) < scan_median,
            "lookup {lookup_median} ns vs scan {scan_median} ns"
        );
    }
}
