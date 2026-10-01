//! A project's dependency tree, read once and shared by every surface.

use super::roles::{Declared, RoleEvidence, RoleId, cohort_role, declared_role, described_role};
use backend_advisory::{
    AdvisoryCoverage, AdvisoryObservation, AdvisoryStatus, FreshnessState,
    cargo_compatibility_class, cargo_version_cmp,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Wire schema of [`ProjectTree`].
pub const PROJECT_TREE_SCHEMA: u16 = 2;

/// The most packages one tree reply admits.
pub const MAX_TREE_PACKAGES: usize = 20_000;

/// Which reader produced a tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reader", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TreeSource {
    /// `cargo metadata`, resolved for this machine's target.
    Cargo {
        /// The target triple Cargo resolved for.
        host: String,
    },
    /// Cargo could not answer: read from `Cargo.lock` alone, so every
    /// platform's packages count and no package states its own metadata.
    Lockfile {
        /// Why Cargo did not answer.
        reason: String,
    },
}

/// Where a package's source comes from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageOrigin {
    /// A registry release, with Cargo's observed source authority intact.
    Registry {
        /// Exact `registry+` or `sparse+` source from metadata or Cargo.lock.
        source: String,
    },
    /// A git checkout.
    Git {
        /// The repository.
        url: String,
    },
    /// A path dependency that replaces a registry release (`[patch]`).
    Vendored {
        /// The directory, relative to the project root when inside it.
        path: String,
    },
    /// The reader could not establish a supported source authority.
    Unresolved {
        /// Cargo's source spelling, if it supplied one.
        source: Option<String>,
    },
}

impl PackageOrigin {
    /// Whether Cargo named a crates.io index authority we know exactly.
    /// Other registries must not be handed to a crates.io-only source.
    #[must_use]
    pub fn is_crates_io_registry(&self) -> bool {
        matches!(self, Self::Registry { source } if matches!(source.as_str(),
            "registry+https://github.com/rust-lang/crates.io-index"
            | "registry+https://index.crates.io/"
            | "sparse+https://index.crates.io/"))
    }
}

/// One step of a path from your code to a package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhyHop {
    /// A member's short name, or a package name.
    pub name: String,
    /// The package version; `None` for one of your members.
    pub version: Option<String>,
}

/// One of your workspace's own packages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeMember {
    /// Its package name (`backend-desktop`).
    pub name: String,
    /// The name without the prefix every member shares (`desktop`).
    pub short: String,
    /// Whether it builds a binary.
    pub has_bin: bool,
}

/// How a package relates to the roles of your direct dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "is", content = "role", rename_all = "kebab-case")]
pub enum PackageRole {
    /// One of your direct dependencies.
    Direct,
    /// Comes only with direct dependencies of this one role.
    Brought(RoleId),
    /// Comes with direct dependencies of several roles.
    Shared,
    /// Reached from your members, but through no direct dependency.
    Unreached,
}

/// One external package that builds for this project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreePackage {
    /// Package name.
    pub name: String,
    /// Exact version, build metadata included.
    pub version: String,
    /// Where its source comes from.
    pub origin: PackageOrigin,
    /// Its SPDX license expression, when it states one.
    pub license: Option<String>,
    /// The shortest path from your code to it.
    pub why: Box<[WhyHop]>,
    /// Which role brings it.
    pub role: PackageRole,
}

/// How one member uses one dependency.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberEdge {
    /// The member's short name.
    pub member: String,
    /// `[dependencies]`.
    pub normal: bool,
    /// `[dev-dependencies]`.
    pub dev: bool,
    /// `[build-dependencies]`.
    pub build: bool,
}

/// One package your members depend on directly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectDependency {
    /// Package name.
    pub name: String,
    /// The versions your members resolve it to, lowest first.
    pub versions: Box<[String]>,
    /// Which members use it, and how.
    pub by: Box<[MemberEdge]>,
    /// Its one-line description.
    pub description: Option<String>,
    /// The role it plays here.
    pub role: RoleId,
    /// Why it plays that role.
    pub evidence: RoleEvidence,
}

/// One copy of a package that is in the tree more than once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateCopy {
    /// Exact version.
    pub version: String,
    /// Cargo's compatibility class (`0.8`, `1`).
    pub class: String,
    /// The shortest path to this copy.
    pub why: Box<[WhyHop]>,
    /// Whether one of your members depends on this copy directly.
    pub yours: bool,
    /// How many packages ask for this copy.
    pub asked_by: u32,
    /// Whether only your own members ask for it, so moving them drops it.
    pub only_yours: bool,
    /// The packages other than your members that ask for it, by name.
    pub also_asked_by: Box<[String]>,
}

/// A package present at more than one incompatible version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Duplicate {
    /// Package name.
    pub name: String,
    /// Its copies, lowest version first.
    pub copies: Box<[DuplicateCopy]>,
}

/// One advisory that affects a package in the tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeAdvisory {
    /// Affected package.
    pub package: String,
    /// Affected version.
    pub version: String,
    /// Advisory identity (`RUSTSEC-2025-0141`).
    pub id: String,
    /// Its title, when the source states one.
    pub summary: Option<String>,
    /// What it says about the package.
    pub statuses: Box<[AdvisoryStatus]>,
    /// The shortest path from your code to the affected package.
    pub why: Box<[WhyHop]>,
}

/// What the advisory sources say about the whole tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeHealth {
    /// The weakest coverage any package got.
    pub coverage: AdvisoryCoverage,
    /// The freshness of that evidence.
    pub freshness: FreshnessState,
    /// Packages whose coverage was complete.
    pub checked: u32,
    /// Packages asked about.
    pub of: u32,
    /// Advisories that affect a package here.
    pub affecting: Box<[TreeAdvisory]>,
}

/// A project's dependency tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectTree {
    /// Wire schema.
    pub schema: u16,
    /// Which reader produced it.
    pub source: TreeSource,
    /// The workspace root.
    pub root: String,
    /// The project's display name (the root folder's name).
    pub name: String,
    /// Your own packages.
    pub members: Box<[TreeMember]>,
    /// Every external package that builds here, by name then version.
    pub packages: Box<[TreePackage]>,
    /// Your direct dependencies, by name.
    pub direct: Box<[DirectDependency]>,
    /// Packages present at more than one incompatible version.
    pub twice: Box<[Duplicate]>,
    /// Advisory evidence for the tree.
    pub health: TreeHealth,
    /// Lockfile packages that build only for other platforms.
    pub other_platforms: u32,
}

impl ProjectTree {
    /// The direct dependencies playing `role`, in display order.
    pub fn in_role(&self, role: RoleId) -> impl Iterator<Item = &DirectDependency> {
        self.direct
            .iter()
            .filter(move |dependency| dependency.role == role)
    }

    /// Packages brought only by direct dependencies of `role`.
    pub fn brought_by(&self, role: RoleId) -> impl Iterator<Item = &TreePackage> {
        self.packages
            .iter()
            .filter(move |package| package.role == PackageRole::Brought(role))
    }

    /// The package record for one exact version.
    #[must_use]
    pub fn package(&self, name: &str, version: &str) -> Option<&TreePackage> {
        let mut matches = self
            .packages
            .iter()
            .filter(|package| package.name == name && package.version == version);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }
}

// ---------------------------------------------------------------------------
// Input: what a reader (Cargo or the lockfile) hands the builder
// ---------------------------------------------------------------------------

/// One package as a reader saw it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TreeInputPackage {
    /// The reader's identity for it (Cargo's package id).
    pub id: String,
    /// Package name.
    pub name: String,
    /// Exact version.
    pub version: String,
    /// Whether it is one of your workspace members.
    pub member: bool,
    /// Whether it builds a binary (members only).
    pub has_bin: bool,
    /// Where its source comes from.
    pub origin: Option<PackageOrigin>,
    /// SPDX license expression.
    pub license: Option<String>,
    /// One-line description.
    pub description: Option<String>,
    /// crates.io categories.
    pub categories: Vec<String>,
    /// crates.io keywords.
    pub keywords: Vec<String>,
}

/// One resolved dependency edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEdge {
    /// Dependent package id.
    pub from: String,
    /// Dependency package id.
    pub to: String,
    /// A normal edge.
    pub normal: bool,
    /// A dev edge.
    pub dev: bool,
    /// A build edge.
    pub build: bool,
}

/// Everything the builder needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeInput {
    /// Which reader produced it.
    pub source: TreeSource,
    /// The workspace root.
    pub root: String,
    /// Members and external packages.
    pub packages: Vec<TreeInputPackage>,
    /// Resolved edges.
    pub edges: Vec<TreeEdge>,
    /// Lockfile packages outside this set (other platforms).
    pub other_platforms: u32,
}

/// Answers "what do the advisory sources say about this release?".
pub trait AdvisoryObserver {
    /// Observes one exact Cargo release.
    fn observe(&self, name: &str, version: &str) -> AdvisoryObservation;
}

impl<F: Fn(&str, &str) -> AdvisoryObservation> AdvisoryObserver for F {
    fn observe(&self, name: &str, version: &str) -> AdvisoryObservation {
        self(name, version)
    }
}

// ---------------------------------------------------------------------------
// The builder
// ---------------------------------------------------------------------------

/// Builds the tree: members, why-paths, roles, duplicates and advisory health.
#[must_use]
pub fn build_tree(input: &TreeInput, advisories: &dyn AdvisoryObserver) -> ProjectTree {
    let index: BTreeMap<&str, usize> = input
        .packages
        .iter()
        .enumerate()
        .map(|(at, package)| (package.id.as_str(), at))
        .collect();
    let prefix = shared_prefix(
        input
            .packages
            .iter()
            .filter(|package| package.member)
            .map(|package| package.name.as_str()),
    );
    let short = |package: &TreeInputPackage| {
        package
            .name
            .strip_prefix(prefix.as_str())
            .unwrap_or(&package.name)
            .to_owned()
    };

    // Adjacency, deterministic: each node's dependencies by (name, version).
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); input.packages.len()];
    let mut incoming: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); input.packages.len()];
    let mut member_edges: BTreeMap<usize, BTreeMap<usize, (bool, bool, bool)>> = BTreeMap::new();
    for edge in &input.edges {
        let (Some(&from), Some(&to)) = (index.get(edge.from.as_str()), index.get(edge.to.as_str()))
        else {
            continue;
        };
        if !outgoing[from].contains(&to) {
            outgoing[from].push(to);
        }
        incoming[to].insert(from);
        if input.packages[from].member && !input.packages[to].member {
            let kinds = member_edges
                .entry(to)
                .or_default()
                .entry(from)
                .or_insert((false, false, false));
            kinds.0 |= edge.normal;
            kinds.1 |= edge.dev;
            kinds.2 |= edge.build;
        }
    }
    let order = |at: &usize| {
        (
            input.packages[*at].name.clone(),
            VersionOrder(input.packages[*at].version.clone()),
        )
    };
    for targets in &mut outgoing {
        targets.sort_by_key(order);
    }

    // Why-paths: one breadth-first search from every member at once,
    // members with a binary first, then by name.
    let mut members: Vec<usize> = (0..input.packages.len())
        .filter(|at| input.packages[*at].member)
        .collect();
    members.sort_by_key(|at| {
        (
            !input.packages[*at].has_bin,
            input.packages[*at].name.clone(),
        )
    });
    let mut parent: Vec<Option<Option<usize>>> = vec![None; input.packages.len()];
    let mut queue = VecDeque::new();
    for &member in &members {
        parent[member] = Some(None);
        queue.push_back(member);
    }
    while let Some(at) = queue.pop_front() {
        for &next in &outgoing[at] {
            if parent[next].is_none() {
                parent[next] = Some(Some(at));
                queue.push_back(next);
            }
        }
    }
    let hop = |at: usize| {
        let package = &input.packages[at];
        if package.member {
            WhyHop {
                name: short(package),
                version: None,
            }
        } else {
            WhyHop {
                name: package.name.clone(),
                version: Some(package.version.clone()),
            }
        }
    };
    let why = |at: usize| -> Box<[WhyHop]> {
        let mut path = Vec::new();
        let mut cursor = Some(at);
        while let Some(node) = cursor {
            path.push(hop(node));
            cursor = parent[node].flatten();
            if path.len() > input.packages.len() {
                break;
            }
        }
        path.reverse();
        if parent[at].is_none() {
            return Box::new([]);
        }
        path.into_boxed_slice()
    };

    // Direct dependencies and their roles.
    let mut direct_nodes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (&dependency, _) in &member_edges {
        direct_nodes
            .entry(input.packages[dependency].name.clone())
            .or_default()
            .push(dependency);
    }
    struct Draft {
        name: String,
        nodes: Vec<usize>,
        by: Vec<MemberEdge>,
        role: Option<(RoleId, RoleEvidence)>,
        described: Option<(RoleId, RoleEvidence)>,
    }
    let mut drafts: Vec<Draft> = direct_nodes
        .into_iter()
        .map(|(name, mut nodes)| {
            nodes.sort_by_key(order);
            let mut by: BTreeMap<String, (bool, bool, bool)> = BTreeMap::new();
            for node in &nodes {
                for (member, kinds) in &member_edges[node] {
                    let entry = by.entry(short(&input.packages[*member])).or_default();
                    entry.0 |= kinds.0;
                    entry.1 |= kinds.1;
                    entry.2 |= kinds.2;
                }
            }
            let by = by
                .into_iter()
                .map(|(member, (normal, dev, build))| MemberEdge {
                    member,
                    normal,
                    dev,
                    build,
                })
                .collect::<Vec<_>>();
            let newest = &input.packages[*nodes.last().unwrap_or(&nodes[0])];
            let declared = Declared {
                categories: &newest.categories,
                keywords: &newest.keywords,
                description: newest.description.as_deref(),
                dev_only: by
                    .iter()
                    .all(|edge| edge.dev && !edge.normal && !edge.build),
            };
            let role = declared_role(&declared);
            let described = described_role(&declared);
            Draft {
                name,
                nodes,
                by,
                role,
                described,
            }
        })
        .collect();
    let cohorts = drafts
        .iter()
        .map(|draft| {
            if draft.role.is_some() {
                return None;
            }
            cohort_role(drafts.iter().filter_map(|peer| {
                let (role, evidence) = peer.role.as_ref()?;
                let declared = matches!(evidence, RoleEvidence::Declared { .. });
                (declared && peer.name != draft.name && peer.by == draft.by)
                    .then_some((peer.name.as_str(), *role))
            }))
        })
        .collect::<Vec<_>>();
    for (draft, cohort) in drafts.iter_mut().zip(cohorts) {
        if draft.role.is_none() {
            draft.role = Some(
                cohort
                    .map(|(role, peer)| (role, RoleEvidence::Cohort { peer }))
                    .or_else(|| draft.described.take())
                    .unwrap_or((RoleId::Other, RoleEvidence::Unknown)),
            );
        }
    }

    // Which roles bring each package.
    let mut brought: Vec<BTreeSet<RoleId>> = vec![BTreeSet::new(); input.packages.len()];
    let mut direct_set = BTreeSet::new();
    for draft in &drafts {
        let role = draft.role.as_ref().map_or(RoleId::Other, |(role, _)| *role);
        for &node in &draft.nodes {
            direct_set.insert(node);
            let mut seen = BTreeSet::new();
            let mut stack = vec![node];
            while let Some(at) = stack.pop() {
                if !seen.insert(at) {
                    continue;
                }
                if at != node {
                    brought[at].insert(role);
                }
                stack.extend(
                    outgoing[at]
                        .iter()
                        .copied()
                        .filter(|next| !input.packages[*next].member),
                );
            }
        }
    }

    let mut externals: Vec<usize> = (0..input.packages.len())
        .filter(|at| !input.packages[*at].member)
        .collect();
    externals.sort_by_key(order);
    let packages = externals
        .iter()
        .map(|&at| {
            let package = &input.packages[at];
            TreePackage {
                name: package.name.clone(),
                version: package.version.clone(),
                origin: package
                    .origin
                    .clone()
                    .unwrap_or(PackageOrigin::Unresolved { source: None }),
                license: package.license.clone(),
                why: why(at),
                role: if direct_set.contains(&at) {
                    PackageRole::Direct
                } else {
                    match brought[at].len() {
                        0 => PackageRole::Unreached,
                        1 => brought[at]
                            .first()
                            .map_or(PackageRole::Unreached, |role| PackageRole::Brought(*role)),
                        _ => PackageRole::Shared,
                    }
                },
            }
        })
        .collect::<Box<[_]>>();

    let direct = drafts
        .into_iter()
        .map(|draft| {
            let newest = &input.packages[*draft.nodes.last().unwrap_or(&draft.nodes[0])];
            let (role, evidence) = draft.role.unwrap_or((RoleId::Other, RoleEvidence::Unknown));
            DirectDependency {
                name: draft.name,
                versions: draft
                    .nodes
                    .iter()
                    .map(|at| input.packages[*at].version.clone())
                    .collect(),
                by: draft.by.into_boxed_slice(),
                description: newest.description.as_deref().map(one_line),
                role,
                evidence,
            }
        })
        .collect::<Box<[_]>>();

    // Packages here at more than one compatibility class.
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &at in &externals {
        by_name
            .entry(input.packages[at].name.as_str())
            .or_default()
            .push(at);
    }
    let twice = by_name
        .into_iter()
        .filter_map(|(name, nodes)| {
            let classes = nodes
                .iter()
                .map(|at| class(&input.packages[*at].version))
                .collect::<BTreeSet<_>>();
            (classes.len() > 1).then(|| Duplicate {
                name: name.to_owned(),
                copies: nodes
                    .iter()
                    .map(|&at| {
                        let askers = &incoming[at];
                        DuplicateCopy {
                            version: input.packages[at].version.clone(),
                            class: class(&input.packages[at].version),
                            why: why(at),
                            yours: member_edges.contains_key(&at),
                            asked_by: u32::try_from(askers.len()).unwrap_or(u32::MAX),
                            only_yours: !askers.is_empty()
                                && askers.iter().all(|asker| input.packages[*asker].member),
                            also_asked_by: askers
                                .iter()
                                .filter(|asker| !input.packages[**asker].member)
                                .map(|asker| input.packages[*asker].name.clone())
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect(),
                        }
                    })
                    .collect(),
            })
        })
        .collect::<Box<[_]>>();

    // Advisory health over every package that builds here.
    let mut coverage = AdvisoryCoverage::Complete;
    let mut freshness = FreshnessState::Fresh;
    let mut checked = 0_u32;
    let mut affecting = Vec::new();
    for &at in &externals {
        let package = &input.packages[at];
        // The observer takes only name and version, without source authority.
        // Another registry (or a vendored/git source) can carry the same
        // spelling without carrying the crates.io release's advisories.
        if !package
            .origin
            .as_ref()
            .is_some_and(PackageOrigin::is_crates_io_registry)
        {
            coverage = weaker_coverage(coverage, AdvisoryCoverage::Unknown);
            freshness = weaker_freshness(freshness, FreshnessState::Unknown);
            continue;
        }
        let observation = advisories.observe(&package.name, &package.version);
        if observation.coverage == AdvisoryCoverage::Complete {
            checked = checked.saturating_add(1);
        }
        coverage = weaker_coverage(coverage, observation.coverage);
        freshness = weaker_freshness(freshness, observation.freshness);
        for advisory in &observation.advisories {
            if advisory.is_withdrawn() {
                continue;
            }
            affecting.push(TreeAdvisory {
                package: package.name.clone(),
                version: package.version.clone(),
                id: advisory.key.canonical.0.clone(),
                summary: advisory.summary.clone(),
                statuses: advisory.statuses(),
                why: why(at),
            });
        }
    }
    if externals.is_empty() {
        coverage = AdvisoryCoverage::Unknown;
        freshness = FreshnessState::Unknown;
    }
    let mut members_out = members
        .iter()
        .map(|&at| TreeMember {
            name: input.packages[at].name.clone(),
            short: short(&input.packages[at]),
            has_bin: input.packages[at].has_bin,
        })
        .collect::<Vec<_>>();
    members_out.sort_by(|left, right| left.name.cmp(&right.name));

    ProjectTree {
        schema: PROJECT_TREE_SCHEMA,
        source: input.source.clone(),
        root: input.root.clone(),
        name: std::path::Path::new(&input.root).file_name().map_or_else(
            || input.root.clone(),
            |name| name.to_string_lossy().into_owned(),
        ),
        members: members_out.into_boxed_slice(),
        packages,
        direct,
        twice,
        health: TreeHealth {
            coverage,
            freshness,
            checked,
            of: u32::try_from(externals.len()).unwrap_or(u32::MAX),
            affecting: affecting.into_boxed_slice(),
        },
        other_platforms: input.other_platforms,
    }
}

/// Orders versions by semver precedence, falling back to text.
#[derive(Clone, Debug, Eq, PartialEq)]
struct VersionOrder(String);

impl Ord for VersionOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        cargo_version_cmp(&self.0, &other.0).unwrap_or_else(|_| self.0.cmp(&other.0))
    }
}

impl PartialOrd for VersionOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn class(version: &str) -> String {
    cargo_compatibility_class(version).unwrap_or_else(|_| version.to_owned())
}

/// The longest `word-` prefix every member name shares; empty for one member.
fn shared_prefix<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names = names.collect::<Vec<_>>();
    if names.len() < 2 {
        return String::new();
    }
    let first = names[0];
    let mut best = String::new();
    for (at, character) in first.char_indices() {
        if character != '-' {
            continue;
        }
        let candidate = &first[..=at];
        if names
            .iter()
            .all(|name| name.starts_with(candidate) && name.len() > candidate.len())
        {
            best = candidate.to_owned();
        } else {
            break;
        }
    }
    best
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const fn coverage_rank(coverage: AdvisoryCoverage) -> u8 {
    match coverage {
        AdvisoryCoverage::Complete => 0,
        AdvisoryCoverage::Partial => 1,
        AdvisoryCoverage::Unknown => 2,
        AdvisoryCoverage::Unavailable => 3,
    }
}

const fn weaker_coverage(left: AdvisoryCoverage, right: AdvisoryCoverage) -> AdvisoryCoverage {
    if coverage_rank(right) > coverage_rank(left) {
        right
    } else {
        left
    }
}

const fn freshness_rank(freshness: FreshnessState) -> u8 {
    match freshness {
        FreshnessState::Fresh => 0,
        FreshnessState::NotModified => 1,
        FreshnessState::Stale => 2,
        FreshnessState::Unknown => 3,
    }
}

const fn weaker_freshness(left: FreshnessState, right: FreshnessState) -> FreshnessState {
    if freshness_rank(right) > freshness_rank(left) {
        right
    } else {
        left
    }
}

/// One advisory source after a refresh.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisorySourceState {
    /// `rustsec`, `osv` or `ghsa`.
    pub source: String,
    /// Explicit OSV coverage scope (`all`, an ecosystem name, or
    /// `unspecified`). Other authorities do not set this field.
    #[serde(default)]
    pub scope: Option<String>,
    /// Whether the source vouches for every package it does not name.
    pub complete: bool,
    /// Advisories the source holds.
    pub advisories: u64,
    /// When it was last observed, in Unix seconds.
    pub observed_at: u64,
    /// Source-provided freshness deadline, in Unix seconds, when known.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Why the last refresh failed, when it did.
    pub error: Option<String>,
}
