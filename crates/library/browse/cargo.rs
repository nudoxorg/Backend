//! Readers that turn Cargo's own answers into a [`TreeInput`].
//!
//! `cargo metadata --filter-platform <host>` is the authority: it resolves
//! features, targets and renames exactly as a build would, and it carries
//! each package's license, description, categories and keywords. `Cargo.lock`
//! adds what builds only elsewhere, and stands in, reduced, when Cargo cannot
//! answer.

use super::tree::{PackageOrigin, TreeEdge, TreeInput, TreeInputPackage, TreeSource};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Why a reader could not produce a tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoTreeError {
    /// The metadata was not the JSON Cargo writes.
    Metadata(String),
    /// The lockfile was not TOML Cargo writes.
    Lockfile(String),
}

impl std::fmt::Display for CargoTreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metadata(detail) => write!(formatter, "cargo metadata is unreadable: {detail}"),
            Self::Lockfile(detail) => write!(formatter, "Cargo.lock is unreadable: {detail}"),
        }
    }
}

impl std::error::Error for CargoTreeError {}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn words(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

/// Retain Cargo's source spelling so a later reader never guesses a registry
/// from a display name. An unfamiliar source stays explicitly unresolved.
fn source_origin(source: &str) -> PackageOrigin {
    if let Some(url) = source.strip_prefix("git+") {
        PackageOrigin::Git {
            url: url.split(['?', '#']).next().unwrap_or(url).to_owned(),
        }
    } else if source.starts_with("registry+") || source.starts_with("sparse+") {
        PackageOrigin::Registry {
            source: source.to_owned(),
        }
    } else {
        PackageOrigin::Unresolved {
            source: Some(source.to_owned()),
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn alternative_and_unrecognized_sources_keep_their_observed_authority() {
        let first = source_origin("registry+https://one.example.test/index");
        let second = source_origin("registry+https://two.example.test/index");
        assert_ne!(first, second);
        assert!(!first.is_crates_io_registry());
        assert_eq!(
            first,
            PackageOrigin::Registry {
                source: "registry+https://one.example.test/index".to_owned()
            }
        );
        assert_eq!(
            source_origin("other+opaque"),
            PackageOrigin::Unresolved {
                source: Some("other+opaque".to_owned())
            }
        );
    }

    #[test]
    fn lockfile_edges_keep_source_ids_and_refuse_ambiguous_name_versions() {
        let lock = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = [
  "widget 1.0.0 (registry+https://one.example.test/index)",
  "widget 1.0.0 (registry+https://two.example.test/index)",
]
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://one.example.test/index"
[[package]]
name = "widget"
version = "1.0.0"
source = "registry+https://two.example.test/index"
"#;
        let input = lockfile_input(
            lock,
            "/workspace/app",
            &BTreeSet::new(),
            "Cargo unavailable",
        )
        .expect("lockfile");
        assert_ne!(input.packages[1].id, input.packages[2].id);
        assert_eq!(input.edges.len(), 2);
        assert_ne!(input.edges[0].to, input.edges[1].to);

        let ambiguous = lock.replace(
            "  \"widget 1.0.0 (registry+https://one.example.test/index)\",\n  \"widget 1.0.0 (registry+https://two.example.test/index)\",",
            "  \"widget 1.0.0\",",
        );
        let input = lockfile_input(
            &ambiguous,
            "/workspace/app",
            &BTreeSet::new(),
            "Cargo unavailable",
        )
        .expect("ambiguous lockfile");
        assert!(
            input.edges.is_empty(),
            "a name/version-only edge cannot choose a registry"
        );
        assert!(
            matches!(input.source, TreeSource::Lockfile { reason } if reason.contains("could not be attributed"))
        );
    }
}

/// Reads `cargo metadata --format-version 1 --filter-platform <host>`.
///
/// `lockfile` (the project's `Cargo.lock`) only counts the packages that
/// build for other platforms; without it that count is zero.
///
/// # Errors
///
/// Returns [`CargoTreeError`] when either document is not in Cargo's format.
pub fn metadata_input(
    metadata: &[u8],
    host: &str,
    lockfile: Option<&str>,
) -> Result<TreeInput, CargoTreeError> {
    let root: Value = serde_json::from_slice(metadata)
        .map_err(|error| CargoTreeError::Metadata(error.to_string()))?;
    let workspace_root = text(&root, "workspace_root")
        .ok_or_else(|| CargoTreeError::Metadata("no workspace_root".to_owned()))?;
    let members: BTreeSet<String> = words(&root, "workspace_members").into_iter().collect();
    let listed = root
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| CargoTreeError::Metadata("no packages".to_owned()))?;
    let mut packages = Vec::with_capacity(listed.len());
    for package in listed {
        let id = text(package, "id")
            .ok_or_else(|| CargoTreeError::Metadata("a package has no id".to_owned()))?;
        let member = members.contains(&id);
        let source = package.get("source").and_then(Value::as_str);
        let origin = if member {
            None
        } else {
            Some(match source {
                Some(source) => source_origin(source),
                None => PackageOrigin::Vendored {
                    path: text(package, "manifest_path")
                        .and_then(|manifest| {
                            let directory = Path::new(&manifest).parent()?.to_path_buf();
                            Some(
                                directory
                                    .strip_prefix(&workspace_root)
                                    .map_or(directory.clone(), Path::to_path_buf)
                                    .to_string_lossy()
                                    .into_owned(),
                            )
                        })
                        .unwrap_or_default(),
                },
            })
        };
        packages.push(TreeInputPackage {
            name: text(package, "name")
                .ok_or_else(|| CargoTreeError::Metadata(format!("{id} has no name")))?,
            version: text(package, "version")
                .ok_or_else(|| CargoTreeError::Metadata(format!("{id} has no version")))?,
            id,
            member,
            has_bin: package
                .get("targets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|target| words(target, "kind").iter().any(|kind| kind == "bin")),
            origin,
            license: text(package, "license"),
            description: text(package, "description"),
            categories: words(package, "categories"),
            keywords: words(package, "keywords"),
        });
    }
    let mut edges = Vec::new();
    for node in root
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(Value::as_array)
        .ok_or_else(|| CargoTreeError::Metadata("no resolve graph".to_owned()))?
    {
        let from = text(node, "id")
            .ok_or_else(|| CargoTreeError::Metadata("a node has no id".to_owned()))?;
        for dependency in node
            .get("deps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(to) = text(dependency, "pkg") else {
                continue;
            };
            let mut edge = TreeEdge {
                from: from.clone(),
                to,
                normal: false,
                dev: false,
                build: false,
            };
            for kind in dependency
                .get("dep_kinds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match kind.get("kind").and_then(Value::as_str) {
                    Some("dev") => edge.dev = true,
                    Some("build") => edge.build = true,
                    _ => edge.normal = true,
                }
            }
            if !(edge.normal || edge.dev || edge.build) {
                edge.normal = true;
            }
            edges.push(edge);
        }
    }
    let other_platforms = match lockfile {
        Some(lockfile) => {
            let member_names = packages
                .iter()
                .filter(|package| package.member)
                .map(|package| package.name.as_str())
                .collect::<BTreeSet<_>>();
            let here = packages
                .iter()
                .filter(|package| !package.member)
                .map(|package| (package.name.as_str(), package.version.as_str()))
                .collect::<BTreeSet<_>>();
            let locked = locked_packages(lockfile)?;
            u32::try_from(
                locked
                    .iter()
                    .filter(|package| !member_names.contains(package.name.as_str()))
                    .filter(|package| {
                        !here.contains(&(package.name.as_str(), package.version.as_str()))
                    })
                    .count(),
            )
            .unwrap_or(u32::MAX)
        }
        None => 0,
    };
    Ok(TreeInput {
        source: TreeSource::Cargo {
            host: host.to_owned(),
        },
        root: workspace_root,
        packages,
        edges,
        other_platforms,
    })
}

struct Locked {
    name: String,
    version: String,
    source: Option<String>,
    dependencies: Vec<String>,
}

fn locked_packages(lockfile: &str) -> Result<Vec<Locked>, CargoTreeError> {
    let document: toml::Value = lockfile
        .parse()
        .map_err(|error: toml::de::Error| CargoTreeError::Lockfile(error.to_string()))?;
    let listed = document
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| CargoTreeError::Lockfile("no [[package]] entries".to_owned()))?;
    listed
        .iter()
        .map(|package| {
            let field = |key: &str| {
                package
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .map(ToOwned::to_owned)
            };
            Ok(Locked {
                name: field("name")
                    .ok_or_else(|| CargoTreeError::Lockfile("a package has no name".to_owned()))?,
                version: field("version").ok_or_else(|| {
                    CargoTreeError::Lockfile("a package has no version".to_owned())
                })?,
                source: field("source"),
                dependencies: package
                    .get("dependencies")
                    .and_then(toml::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            })
        })
        .collect()
}

/// Reads `Cargo.lock` alone, for when Cargo cannot answer.
///
/// Every platform's packages count, dependency kinds are unknown (every
/// edge reads as normal), and no package states its own metadata, so roles
/// fall to "other". `patched` names the packages the root manifest replaces
/// with a path (`[patch]`), which the lockfile cannot tell from members.
///
/// # Errors
///
/// Returns [`CargoTreeError::Lockfile`] when `lockfile` is not Cargo's format.
pub fn lockfile_input(
    lockfile: &str,
    root: &str,
    patched: &BTreeSet<String>,
    reason: &str,
) -> Result<TreeInput, CargoTreeError> {
    let locked = locked_packages(lockfile)?;
    let id =
        |package: &Locked| format!("{} {} {:?}", package.name, package.version, package.source);
    let mut by_name: BTreeMap<&str, Vec<&Locked>> = BTreeMap::new();
    for package in &locked {
        by_name
            .entry(package.name.as_str())
            .or_default()
            .push(package);
    }
    let packages = locked
        .iter()
        .map(|package| {
            let member = package.source.is_none() && !patched.contains(&package.name);
            TreeInputPackage {
                id: id(package),
                name: package.name.clone(),
                version: package.version.clone(),
                member,
                has_bin: false,
                origin: if member {
                    None
                } else {
                    Some(match package.source.as_deref() {
                        Some(source) => source_origin(source),
                        None => PackageOrigin::Vendored {
                            path: String::new(),
                        },
                    })
                },
                ..TreeInputPackage::default()
            }
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut ambiguous_edges = 0_usize;
    for package in &locked {
        for dependency in &package.dependencies {
            let mut parts = dependency.split_whitespace();
            let name = parts.next().unwrap_or_default();
            let second = parts.next();
            let (version, source) = match second {
                Some(source) if source.starts_with('(') => (None, Some(source)),
                version => (version, parts.next()),
            };
            let source = match source {
                Some(source) => match source
                    .strip_prefix('(')
                    .and_then(|source| source.strip_suffix(')'))
                {
                    Some(source) => Some(source),
                    None => continue,
                },
                None => None,
            };
            if parts.next().is_some() {
                continue;
            }
            let Some(candidates) = by_name.get(name) else {
                continue;
            };
            let matches = candidates
                .iter()
                .filter(|candidate| {
                    version.is_none_or(|version| candidate.version == version)
                        && source.is_none_or(|source| candidate.source.as_deref() == Some(source))
                })
                .copied()
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                ambiguous_edges = ambiguous_edges.saturating_add(1);
            }
            if let [target] = matches.as_slice() {
                edges.push(TreeEdge {
                    from: id(package),
                    to: id(target),
                    normal: true,
                    dev: false,
                    build: false,
                });
            }
        }
    }
    Ok(TreeInput {
        source: TreeSource::Lockfile {
            reason: if ambiguous_edges == 0 {
                reason.to_owned()
            } else {
                format!(
                    "{reason}; {ambiguous_edges} dependency edge(s) could not be attributed to one source"
                )
            },
        },
        root: root.to_owned(),
        packages,
        edges,
        other_platforms: 0,
    })
}
