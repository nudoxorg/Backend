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
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
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

/// Dependency facts associated with one canonical source package.
pub type PackageDependencySourceFacts = (
    PackageReference,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
);

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
    /// Precedence when the same target name appears more than once.
    ///
    /// Higher values win over lower ones: Runtime, then Build, Optional, Peer,
    /// and finally Development.
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

fn should_replace_dependency(
    existing: &PackageDependencyRecord,
    candidate: &PackageDependencyRecord,
) -> bool {
    match candidate.scope.rank().cmp(&existing.scope.rank()) {
        Ordering::Greater => true,
        Ordering::Equal => existing.optional && !candidate.optional,
        Ordering::Less => false,
    }
}

/// Collapses duplicate target names, retaining the highest-ranked scope.
///
/// At equal rank, a non-optional row replaces an optional one.
#[must_use]
pub fn collapse_dependency_rows(
    rows: Vec<PackageDependencyRecord>,
) -> Vec<PackageDependencyRecord> {
    let mut index_by_target = BTreeMap::<(RegistryEcosystem, ProductText), usize>::new();
    let mut collapsed = Vec::with_capacity(rows.len());
    for row in rows {
        let key = (row.target.ecosystem, row.target.name.clone());
        if let Some(&existing_index) = index_by_target.get(&key) {
            if should_replace_dependency(&collapsed[existing_index], &row) {
                collapsed[existing_index] = row;
            }
        } else {
            index_by_target.insert(key, collapsed.len());
            collapsed.push(row);
        }
    }
    collapsed
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
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.package-dependency.v1\0");
        hasher.update(source.as_str().as_bytes());
        hasher.update(target.name.as_str().as_bytes());
        hasher.update(target.requirement.as_str().as_bytes());
        hasher.update(&[target.ecosystem as u8, scope as u8, u8::from(optional)]);
        if let Some(resolved) = &target.resolved {
            hasher.update(&[1]);
            hasher.update(resolved.as_str().as_bytes());
        } else {
            hasher.update(&[0]);
        }
        hasher.update(&[evidence.authority as u8]);
        hasher.update(&evidence.frontier);
        hasher.update(&evidence.provenance);
        Self {
            source,
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
        Self::new(
            self.source.clone(),
            self.target.clone(),
            self.scope,
            self.optional,
            self.evidence,
        )
        .facts_version
    }
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
    by_source: BTreeMap<String, usize>,
    reverse: BTreeMap<(RegistryEcosystem, String), Vec<ReverseEdge>>,
    first_gap: Option<ProductText>,
}

#[derive(Clone, Debug)]
struct ReverseEdge {
    source_index: usize,
    resolved: Option<String>,
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
        /// Packages that declare a counted edge onto the target.
        sources: BTreeSet<PackageReference>,
        /// First unknown or unavailable fact, in source order.
        gap: Option<ProductText>,
    },
}

impl PackageGraphIndex {
    /// Builds forward and reverse adjacency from source order.
    ///
    /// The first fact for a source spelling wins the forward map. Duplicate
    /// coordinates later in the slice stay in the reverse map when they carry
    /// a counted edge, matching a scan that inserts into a set.
    #[must_use]
    pub fn from_facts(facts: &[PackageDependencySourceFacts]) -> Self {
        let mut index = Self::default();
        for (source_index, (source, state)) in facts.iter().enumerate() {
            index
                .by_source
                .entry(source.as_str().to_owned())
                .or_insert(source_index);
            match state {
                DependencyFacts::Known(rows) => {
                    for row in rows.iter() {
                        if !matches!(
                            row.scope,
                            DependencyScope::Runtime | DependencyScope::Optional
                        ) {
                            continue;
                        }
                        index
                            .reverse
                            .entry((row.target.ecosystem, row.target.name.as_str().to_owned()))
                            .or_default()
                            .push(ReverseEdge {
                                source_index,
                                resolved: row
                                    .target
                                    .resolved
                                    .as_ref()
                                    .map(|resolved| resolved.as_str().to_owned()),
                            });
                    }
                }
                DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
                    if index.first_gap.is_none() {
                        index.first_gap = Some(reason.clone());
                    }
                }
            }
        }
        index
    }

    /// Returns the first fact whose source spelling matches `package`.
    #[must_use]
    pub fn dependencies<'a>(
        &self,
        facts: &'a [PackageDependencySourceFacts],
        package: &PackageReference,
    ) -> Option<&'a DependencyFacts<Box<[PackageDependencyRecord]>>> {
        self.by_source
            .get(package.as_str())
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
            .get(&(ecosystem, target.lineage_name().to_owned()))
        {
            for edge in edges {
                let matches_version = edge
                    .resolved
                    .as_ref()
                    .is_none_or(|resolved| resolved.as_str() == target.as_str());
                if !matches_version {
                    continue;
                }
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
    fn collapse_keeps_higher_rank_and_non_optional_at_equal_rank() {
        let runtime = dependency_row("serde", DependencyScope::Runtime, false);
        let development = dependency_row("serde", DependencyScope::Development, false);
        let collapsed = collapse_dependency_rows(vec![development.clone(), runtime.clone()]);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].scope, DependencyScope::Runtime);

        let optional_runtime = dependency_row("serde", DependencyScope::Runtime, true);
        let required_runtime = dependency_row("serde", DependencyScope::Runtime, false);
        let collapsed = collapse_dependency_rows(vec![optional_runtime, required_runtime.clone()]);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].scope, DependencyScope::Runtime);
        assert!(!collapsed[0].optional);
        assert_eq!(collapsed[0].facts_version, required_runtime.facts_version);
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
            source_ref,
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
                PackageReference::parse("pkg:cargo/unknown-src@1.0.0").expect("unknown"),
                DependencyFacts::Unknown(ProductText::new("first gap").expect("gap")),
            ),
            (
                PackageReference::parse("pkg:cargo/later-gap@1.0.0").expect("later"),
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
                .map(PackageReference::as_str)
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
        assert!(
            PackageGraphIndex::from_facts(std::slice::from_ref(&known))
                .dependencies(std::slice::from_ref(&known), &known.0)
                .expect("forward")
                .is_known()
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
                    facts.first().expect("duplicate source").0.clone()
                } else {
                    PackageReference::parse(&spelling).expect("source")
                };
                match next() % 5 {
                    0 => facts.push((
                        source_ref,
                        DependencyFacts::Unknown(
                            ProductText::new(format!("unknown-{source_index}")).expect("reason"),
                        ),
                    )),
                    1 => facts.push((
                        source_ref,
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
                            rows.push(PackageDependencyRecord::new(
                                source_ref.clone(),
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
                        facts.push((source_ref, DependencyFacts::Known(rows.into_boxed_slice())));
                    }
                }
            }
            let index = PackageGraphIndex::from_facts(&facts);
            let mut queries = vec![
                PackageReference::parse("local-pkg").expect("local"),
                PackageReference::parse("pkg:cargo/missing@9.0.0").expect("missing"),
            ];
            for (source, state) in &facts {
                queries.push(source.clone());
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
                assert_eq!(
                    index.dependencies(&facts, query),
                    facts
                        .iter()
                        .find(|(source, _)| source.as_str() == query.as_str())
                        .map(|(_, state)| state),
                    "forward lookup diverged for {}",
                    query.as_str()
                );
            }
        }
    }
}
