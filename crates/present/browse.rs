//! The words for a project's tree, spelled once for every surface.
//!
//! The desktop's Library page, the CLI and MCP read the same
//! [`ProjectTree`] through [`read_tree`], so "bincode 1.3.3 is unmaintained"
//! and "desktop → toml 0.8.23" are the same sentences everywhere.

use backend_library::browse::{
    DirectDependency, Duplicate, LockedInactiveCoverage, LockfileGraphCoverage,
    LockfileWorkspaceMembership, MemberEdge, PackageOrigin, ProjectTree, RoleEvidence, RoleId,
    TreeAdvisory, TreeSource, WhyHop,
};
use backend_library::{AdvisoryCoverage, AdvisoryStatus, FreshnessState, PackageReference};
use std::collections::BTreeMap;

/// A whole tree, in words.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeReading {
    /// The project's name (`backend`).
    pub name: String,
    /// "Your 44 packages lean on 75 others directly, and 884 in all."
    pub lede: String,
    /// A truthful note about lockfile rows outside the current resolution.
    pub locked_inactive_note: Option<String>,
    /// How the tree was read, when not by Cargo for this machine.
    pub source_note: Option<String>,
    /// One line per advisory that affects the tree.
    pub alerts: Box<[AlertReading]>,
    /// "60 crates are here twice", or a count-aware line when some have
    /// three or more versions.
    pub twice_line: Option<String>,
    /// What the advisory sources could say.
    pub health: String,
    /// The roles, in display order, with their dependencies.
    pub roles: Box<[RoleReading]>,
    /// "Here twice" or "Multiple versions", then the recorded count.
    pub twice_heading: Option<(String, String)>,
    /// The packages present at more than one incompatible version.
    pub twice: Box<[TwiceReading]>,
}

/// One advisory that affects the tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlertReading {
    /// "bincode 1.3.3 is unmaintained".
    pub title: String,
    /// The affected package.
    pub package: String,
    /// Its version.
    pub version: String,
    /// The advisory (`RUSTSEC-2025-0141`).
    pub id: String,
    /// The advisory's own title.
    pub summary: Option<String>,
    /// How it got into your tree.
    pub why: String,
}

/// One role and the dependencies playing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleReading {
    /// Which role.
    pub id: RoleId,
    /// "speaks formats".
    pub label: &'static str,
    /// "for engine, store and advisory".
    pub serving: Option<String>,
    /// Its direct dependencies.
    pub rows: Box<[RowReading]>,
    /// "and 15 crates that come with them".
    pub brings: Option<String>,
}

/// One direct dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowReading {
    /// Package name.
    pub name: String,
    /// The one quiet descriptor a row may carry at rest ("tests only",
    /// "twice · 0.22.1 · 0.23.1").
    pub at_rest: Option<String>,
    /// Why it plays its role, with who uses it.
    pub evidence: String,
    /// Its own one-line description.
    pub description: Option<String>,
    /// The versions your members resolve it to.
    pub versions: Box<[String]>,
    /// Exact owner-issued source reference for each resolved release, in
    /// `versions` order. Equal version text may still name distinct sources.
    pub sources: Box<[Option<PackageReference>]>,
    /// Exact source spelling for each release, even when a file receipt is
    /// unavailable and its action must remain disabled.
    pub origins: Box<[PackageOrigin]>,
}

/// A package present more than once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TwiceReading {
    /// Package name.
    pub name: String,
    /// Each copy's display version, and whether it is yours.
    pub copies: Box<[(String, bool)]>,
    /// Each copy's path from your code, in the same order.
    pub paths: Box<[String]>,
    /// What moving would do.
    pub verdict: String,
}

/// Reads a tree into sentences.
#[must_use]
pub fn read_tree(tree: &ProjectTree) -> TreeReading {
    let members = tree.members.len();
    let lede = match &tree.source {
        TreeSource::Lockfile {
            workspace_membership: LockfileWorkspaceMembership::Unknown,
            ..
        } => format!(
            "Cargo.lock lists {} package rows; workspace membership is unknown.",
            count(tree.packages.len()),
        ),
        TreeSource::Cargo { .. } => format!(
            "Your {} {} on {} {} directly, and {} in all.",
            count(members),
            if members == 1 {
                "package leans"
            } else {
                "packages lean"
            },
            count(tree.direct.len()),
            if tree.direct.len() == 1 {
                "other"
            } else {
                "others"
            },
            count(tree.packages.len()),
        ),
    };
    let locked_inactive_note = match tree.locked_inactive_coverage {
        LockedInactiveCoverage::Complete if tree.locked_inactive > 0 => {
            let count_text = count(tree.locked_inactive as usize);
            let noun = if tree.locked_inactive == 1 {
                "package is"
            } else {
                "packages are"
            };
            Some(format!(
                "{count_text} {noun} locked but inactive for the current target/features"
            ))
        }
        LockedInactiveCoverage::Complete if tree.locked_inactive == 0 => None,
        LockedInactiveCoverage::Unavailable => Some(
            "inactive rows cannot be counted without both the current resolution and Cargo.lock"
                .to_owned(),
        ),
        LockedInactiveCoverage::Partial { unmatched_packages } if tree.locked_inactive == 0 => {
            Some(format!(
                "some package rows could not be matched exactly ({unmatched_packages}); the inactive-package count is a lower bound"
            ))
        }
        LockedInactiveCoverage::Partial { unmatched_packages } => {
            let count_text = count(tree.locked_inactive as usize);
            let noun = if tree.locked_inactive == 1 {
                "package is"
            } else {
                "packages are"
            };
            Some(format!(
                "at least {count_text} {noun} locked but inactive for the current target/features; {unmatched_packages} package row(s) could not be matched exactly"
            ))
        }
    };
    let source_note = match &tree.source {
        TreeSource::Cargo { .. } => None,
        TreeSource::Lockfile {
            reason,
            coverage,
            workspace_membership: LockfileWorkspaceMembership::Unknown,
        } => Some(match coverage {
            LockfileGraphCoverage::Complete => format!(
                "Read from Cargo.lock alone, without target/feature filtering or package metadata; workspace membership and local paths are unknown: {reason}"
            ),
            LockfileGraphCoverage::Partial {
                ambiguous_edges,
                ambiguous_package_rows,
            } => format!(
                "Read from Cargo.lock alone, without target/feature filtering or package metadata; workspace membership and local paths are unknown: {reason}; {ambiguous_edges} dependency edge(s) could not be attributed and {ambiguous_package_rows} package row(s) share an indistinguishable source identity"
            ),
        }),
    };
    let alerts = tree.health.affecting.iter().map(alert).collect();
    let all_pairs = tree.twice.iter().all(|duplicate| duplicate.copies.len() == 2);
    let twice_line = (!tree.twice.is_empty()).then(|| {
        if all_pairs {
            format!("{} {} here twice", count(tree.twice.len()), if tree.twice.len() == 1 { "crate is" } else { "crates are" })
        } else {
            format!("{} {} at more than one version", count(tree.twice.len()), if tree.twice.len() == 1 { "crate appears" } else { "crates appear" })
        }
    });
    let twice_by_name: BTreeMap<&str, &Duplicate> =
        tree.twice.iter().map(|duplicate| (duplicate.name.as_str(), duplicate)).collect();
    let roles = RoleId::ALL
        .into_iter()
        .filter_map(|role| {
            let mut direct = tree.in_role(role).collect::<Vec<_>>();
            if direct.is_empty() {
                return None;
            }
            direct.sort_by(|left, right| right.by.len().cmp(&left.by.len()).then_with(|| left.name.cmp(&right.name)));
            let brought = tree.brought_by(role).count();
            Some(RoleReading {
                id: role,
                label: role_label(role),
                serving: serving(&direct),
                rows: direct.iter().map(|dependency| row(dependency, &twice_by_name)).collect(),
                brings: (brought > 0).then(|| {
                    format!(
                        "and {} {} with them",
                        count(brought),
                        if brought == 1 { "crate that comes" } else { "crates that come" }
                    )
                }),
            })
        })
        .collect();
    TreeReading {
        name: tree.name.clone(),
        lede,
        locked_inactive_note,
        source_note,
        alerts,
        twice_line,
        health: health(tree),
        roles,
        twice_heading: (!tree.twice.is_empty()).then(|| {
            (
                if all_pairs { "Here twice" } else { "Multiple versions" }.to_owned(),
                format!(
                    "{} {} at more than one version",
                    count(tree.twice.len()),
                    if tree.twice.len() == 1 { "crate appears" } else { "crates appear" }
                ),
            )
        }),
        twice: tree
            .twice
            .iter()
            .map(|duplicate| {
                twice(
                    duplicate,
                    matches!(&tree.source, TreeSource::Cargo { .. }),
                )
            })
            .collect(),
    }
}

/// The words for one role.
#[must_use]
pub const fn role_label(role: RoleId) -> &'static str {
    match role {
        RoleId::Tests => "checks our work",
        RoleId::Window => "draws the window",
        RoleId::Languages => "reads languages",
        RoleId::Formats => "speaks formats",
        RoleId::Store => "keeps and finds",
        RoleId::Concurrency => "runs things at once",
        RoleId::Network => "talks to the network",
        RoleId::Hashing => "fingerprints and packs",
        RoleId::Errors => "names what went wrong",
        RoleId::Observe => "watches itself run",
        RoleId::Memory => "shapes memory",
        RoleId::Os => "talks to the system",
        RoleId::Other => "other",
    }
}

/// "desktop → toml 0.8.23": the path from your code, versions without build metadata.
#[must_use]
pub fn why_line(hops: &[WhyHop]) -> String {
    hops.iter()
        .map(|hop| match &hop.version {
            Some(version) => format!("{} {}", hop.name, display_version(version)),
            None => hop.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// A version as people read it: `1.1.5+spec-1.1.0` is `1.1.5`.
#[must_use]
pub fn display_version(version: &str) -> &str {
    version.split_once('+').map_or(version, |(core, _)| core)
}

fn alert(advisory: &TreeAdvisory) -> AlertReading {
    let what = advisory
        .statuses
        .iter()
        .find_map(|status| match status {
            AdvisoryStatus::Unmaintained => Some("is unmaintained"),
            AdvisoryStatus::Unsound => Some("is unsound"),
            AdvisoryStatus::Vulnerable => Some("has a known vulnerability"),
            AdvisoryStatus::Malicious => Some("is malicious"),
            AdvisoryStatus::Notice => Some("has a notice"),
            _ => None,
        })
        .unwrap_or("has an advisory");
    AlertReading {
        title: format!("{} {} {what}", advisory.package, display_version(&advisory.version)),
        package: advisory.package.clone(),
        version: advisory.version.clone(),
        id: advisory.id.clone(),
        summary: advisory.summary.clone(),
        why: why_line(&advisory.why),
    }
}

fn health(tree: &ProjectTree) -> String {
    let of = count(tree.health.of as usize);
    let line = match tree.health.coverage {
        AdvisoryCoverage::Complete => format!("advisories checked for {} of {of}", count(tree.health.checked as usize)),
        AdvisoryCoverage::Partial => format!("advisories from a partial source, not a full check of {of}"),
        AdvisoryCoverage::Unknown => "advisories not checked: no source is configured".to_owned(),
        AdvisoryCoverage::Unavailable => "advisories unavailable: a source could not be reached".to_owned(),
    };
    if tree.health.freshness == FreshnessState::Stale {
        format!("{line} · stale")
    } else {
        line
    }
}

fn members_list(edges: &[MemberEdge]) -> String {
    let names = edges.iter().map(|edge| edge.member.as_str()).collect::<Vec<_>>();
    if names.len() > 4 {
        return format!("{} and {} more", names[..3].join(", "), names.len() - 3);
    }
    and_list(&names)
}

fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn serving(direct: &[&DirectDependency]) -> Option<String> {
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for dependency in direct {
        for edge in &dependency.by {
            *by.entry(edge.member.as_str()).or_default() += 1;
        }
    }
    let mut ranked = by.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let names = ranked.iter().take(3).map(|(name, _)| *name).collect::<Vec<_>>();
    (!names.is_empty()).then(|| format!("for {}", and_list(&names)))
}

fn row(dependency: &DirectDependency, twice: &BTreeMap<&str, &Duplicate>) -> RowReading {
    let used_by = members_list(&dependency.by);
    let evidence = match &dependency.evidence {
        RoleEvidence::DevOnly => format!("only a dev-dependency of {used_by}"),
        RoleEvidence::Declared { words } => format!(
            "{} · used by {used_by}",
            words.iter().take(3).map(String::as_str).collect::<Vec<_>>().join(" · ")
        ),
        RoleEvidence::Described { phrase } => format!("its description says “{phrase}” · used by {used_by}"),
        RoleEvidence::Cohort { peer } => format!("declares nothing · used by {used_by}, like {peer}"),
        RoleEvidence::Unknown => format!("no category, keyword, telling description or peer · used by {used_by}"),
    };
    let at_rest = twice
        .get(dependency.name.as_str())
        .map(|duplicate| {
            format!(
                "{} · {}",
                if duplicate.copies.len() == 2 { "twice".to_owned() } else { format!("{} versions", duplicate.copies.len()) },
                duplicate
                    .copies
                    .iter()
                    .map(|copy| display_version(&copy.version))
                    .collect::<Vec<_>>()
                    .join(" · ")
            )
        })
        .or_else(|| (dependency.evidence == RoleEvidence::DevOnly).then(|| "tests only".to_owned()));
    RowReading {
        name: dependency.name.clone(),
        at_rest,
        evidence,
        description: dependency.description.clone(),
        versions: dependency.versions.clone(),
        sources: dependency.package_references.clone(),
        origins: dependency.package_origins.clone(),
    }
}

/// `2` reads "2.x"; `0.8` stays "0.8".
fn class_words(class: &str) -> String {
    if class.contains('.') { class.to_owned() } else { format!("{class}.x") }
}

fn twice(duplicate: &Duplicate, workspace_membership_known: bool) -> TwiceReading {
    let copies = duplicate
        .copies
        .iter()
        .map(|copy| (display_version(&copy.version).to_owned(), copy.yours))
        .collect::<Box<[_]>>();
    let paths = duplicate
        .copies
        .iter()
        .map(|copy| {
            if workspace_membership_known {
                why_line(&copy.why)
            } else {
                "Workspace path unknown".to_owned()
            }
        })
        .collect();
    let yours = duplicate.copies.iter().find(|copy| copy.yours);
    let newest_other = duplicate.copies.iter().rev().find(|copy| !copy.yours);
    let verdict = if !workspace_membership_known {
        "Workspace membership is unknown; cannot tell which copy, if any, is yours to move."
            .to_owned()
    } else {
        match (yours, newest_other) {
            (Some(_), None) => "Every copy is yours to move.".to_owned(),
            (Some(yours), Some(other)) if yours.only_yours => {
                format!("Moving yours to {} drops a copy.", display_version(&other.version))
            }
            (Some(yours), Some(other)) => {
                let names = yours.also_asked_by.iter().map(String::as_str).collect::<Vec<_>>();
                format!(
                    "Moving yours to {} keeps both: {} still {} for {}.",
                    display_version(&other.version),
                    and_list(&names),
                    if names.len() == 1 { "asks" } else { "ask" },
                    class_words(&yours.class)
                )
            }
            _ => {
                let most = duplicate.copies.iter().max_by_key(|copy| copy.asked_by);
                let none = if duplicate.copies.len() > 2 { "None" } else { "Neither" };
                most.map_or_else(
                    || format!("{none} is yours to move."),
                    |copy| {
                        format!(
                            "{none} is yours to move: {} {} for {}.",
                            count(copy.asked_by as usize),
                            if copy.asked_by == 1 {
                                "crate still asks"
                            } else {
                                "crates still ask"
                            },
                            class_words(&copy.class)
                        )
                    },
                )
            }
        }
    };
    TwiceReading {
        name: duplicate.name.clone(),
        copies,
        paths,
        verdict,
    }
}

/// 1189 → "1,189".
#[must_use]
pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
