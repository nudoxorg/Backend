//! Reads for the browsing pages: one `project-tree` round trip, lowered to
//! the words every surface shares.

use super::reads::failure;
use crate::model::browse::{
    BrowseKey, BrowseValue, TreeDestination, TreeModel, TreeReleaseLink, TreeRoleLinks,
    TreeRowLinks,
};
use crate::model::pages::{PackageRef, PageValue, ReadFailure};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply, browse::RoleId};
use backend_present::Engine;
use facet::browse::{Alert, AlertTone, LibraryModel, LibraryReleaseLink, LibraryRole, LibraryRow, Twice};
use facet::browse::library::ReleaseHandle;
use facet::icons::Icon;
use gpui::SharedString;
use std::sync::Arc;

/// A path may wrap between hops, never inside one: `toml 1.1.5` stays whole.
fn unbroken_hops(path: &str) -> SharedString {
    path.split(" → ")
        .map(|hop| hop.replace(' ', "\u{a0}"))
        .collect::<Vec<_>>()
        .join(" → ")
        .into()
}

fn key_part(into: &mut String, part: &str) {
    use std::fmt::Write as _;
    let _ = write!(into, "{}:", part.len());
    into.push_str(part);
}

/// Exact release source, including registry authority, participates in the
/// element key. A reorder never lends another release its focus or disclosure.
fn release_key(version: &str, destination: Option<&TreeDestination>) -> SharedString {
    let mut key = String::new();
    key_part(&mut key, version);
    match destination {
        Some(TreeDestination::Open(package)) => key_part(&mut key, package.as_str()),
        Some(TreeDestination::Unavailable(reason)) => key_part(&mut key, reason),
        None => key_part(&mut key, "unresolved"),
    }
    key.into()
}

pub(crate) fn row_key(row: &backend_present::RowReading, links: Option<&crate::model::browse::TreeRowLinks>) -> SharedString {
    let mut key = String::new();
    key_part(&mut key, &row.name);
    for (at, version) in row.versions.iter().enumerate() {
        let destination = links.and_then(|links| links.releases.get(at))
            .filter(|release| release.version.as_ref() == version)
            .map(|release| &release.destination);
        key_part(&mut key, release_key(version, destination).as_ref());
    }
    key.into()
}

/// Convert the owner's already admitted tree on the read worker. Render
/// callbacks only borrow the result, even when the project has many packages.
fn prepared_library_model(reading: &backend_present::TreeReading, links: &[TreeRoleLinks]) -> LibraryModel {
    let mut say = |text: &str| -> SharedString { text.to_owned().into() };
    let alerts = reading
        .alerts
        .iter()
        .map(|alert| {
            let tone = if alert.title.ends_with("is unmaintained") || alert.title.ends_with("has a notice") {
                AlertTone::Warn
            } else {
                AlertTone::Fault
            };
            let advisory = match &alert.summary {
                Some(summary) => format!("{} · {summary}", alert.id),
                None => alert.id.clone(),
            };
            // The card's lines are not on screen until it opens: not said.
            Alert { title: say(&alert.title), advisory: advisory.into(), path: unbroken_hops(&alert.why), tone }
        })
        .collect();
    let mut facts = Vec::new();
    if let Some(twice) = &reading.twice_line {
        facts.push(say(twice));
    }
    facts.push(say(&reading.health));
    let roles = reading
        .roles
        .iter()
        .enumerate()
        .map(|(role_at, role)| LibraryRole {
            key: role.id.as_str().into(),
            icon: match role.id {
                RoleId::Tests => Icon::ShieldCheck,
                RoleId::Window => Icon::Mosaic,
                RoleId::Languages => Icon::Book,
                RoleId::Formats => Icon::Split,
                RoleId::Store => Icon::Search,
                RoleId::Concurrency => Icon::Zap,
                RoleId::Network => Icon::Globe,
                RoleId::Hashing => Icon::Seal,
                RoleId::Errors => Icon::Alert,
                RoleId::Observe => Icon::Eye,
                RoleId::Memory => Icon::Diamond,
                RoleId::Os => Icon::Settings,
                RoleId::Other => Icon::Layers,
            },
            label: say(role.label),
            serving: role.serving.as_deref().map(&mut say),
            rows: role
                .rows
                .iter()
                .enumerate()
                .map(|(row_at, row)| LibraryRow {
                    key: row_key(row, links.get(role_at)
                        .filter(|links| links.role == role.id)
                        .and_then(|links| links.rows.get(row_at))
                        .filter(|links| links.name.as_ref() == row.name)),
                    name: say(&row.name),
                    at_rest: row.at_rest.as_deref().map(&mut say),
                    why: row.evidence.clone().into(),
                    about: row.description.clone().map(SharedString::from),
                    releases: row.versions.iter().enumerate().map(|(version_at, version)| {
                        let destination = links.get(role_at)
                            .filter(|links| links.role == role.id)
                            .and_then(|links| links.rows.get(row_at))
                            .filter(|links| links.name.as_ref() == row.name)
                            .and_then(|links| links.releases.get(version_at))
                            .filter(|link| link.version.as_ref() == version);
                        let key = release_key(version, destination.map(|link| &link.destination));
                        // Two unresolved copies can have identical visible
                        // version/reason text. They have no action to lend;
                        // disambiguate only those element ids.
                        let key = if matches!(destination.map(|link| &link.destination), Some(TreeDestination::Open(_))) {
                            key
                        } else {
                            format!("{key}#{version_at}").into()
                        };
                        match destination.map(|link| &link.destination) {
                            Some(TreeDestination::Open(_package)) => LibraryReleaseLink {
                                key,
                                version: version.clone().into(),
                                target: Some(ReleaseHandle::new(role_at, row_at, version_at)),
                                unavailable: None,
                            },
                            Some(TreeDestination::Unavailable(reason)) => LibraryReleaseLink {
                                key,
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some(reason.to_string().into()),
                            },
                            None => LibraryReleaseLink {
                                key,
                                version: version.clone().into(),
                                target: None,
                                unavailable: Some("The source for this release was not resolved.".into()),
                            },
                        }
                    }).collect(),
                })
                .collect(),
            brings: role.brings.as_deref().map(&mut say),
        })
        .collect();
    // Yours first, then the shortest path in, then by name: what you can act on leads.
    let mut twice = reading.twice.iter().collect::<Vec<_>>();
    twice.sort_by_key(|twice| {
        let yours = twice.copies.iter().any(|(_, yours)| *yours);
        let shortest = twice.paths.iter().map(|path| path.matches(" → ").count()).min().unwrap_or(usize::MAX);
        (!yours, shortest, twice.name.clone())
    });
    // Paths and verdicts unfold on a click: they are not said at rest.
    let twice = twice
        .into_iter()
        .enumerate()
        .map(|(at, twice)| {
            let shown = at < facet::browse::TWICE_AT_REST;
            let mut said = |text: &str| if shown { say(text) } else { SharedString::from(text.to_owned()) };
            Twice {
            name: said(&twice.name),
            copies: twice.copies.iter().map(|(version, yours)| (said(version), *yours)).collect(),
            paths: twice.paths.iter().map(|path| unbroken_hops(path)).collect(),
            verdict: twice.verdict.clone().into(),
        }})
        .collect::<Vec<_>>();
    LibraryModel {
        name: say(&reading.name),
        lede: say(&reading.lede),
        lede_tip: reading.locked_inactive_note.clone().map(SharedString::from),
        note: reading.source_note.as_deref().map(&mut say),
        alerts,
        facts,
        roles,
        twice_heading: reading.twice_heading.as_ref().map(|(title, caption)| (say(title), say(caption))),
        twice,
    }
}


/// Reads one browsing resource.
///
/// # Errors
/// The owner's failure to read the tree, as a typed fault.
pub fn compose(engine: &mut dyn Engine, key: &BrowseKey) -> Result<PageValue, ReadFailure> {
    match key {
        BrowseKey::Tree(project) => {
            let root = project.service_coordinate().map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    format!("the project path cannot be sent to the owner: {error:?}"),
                ))
            })?;
            let root = ProductText::new(root).map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Protocol, error.to_string()))
            })?;
            match engine.surface(SurfaceCommand::ProjectTree { root }).map_err(|error| failure(&error))? {
                SurfaceReply::ProjectTree(tree) => Ok(PageValue::Browse(BrowseValue::Tree(Arc::new(tree_model(&tree))))),
                _ => Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    "the project-tree reply changed shape",
                ))),
            }
        }
        BrowseKey::FindHome | BrowseKey::Find(_) | BrowseKey::Compare(_) => Err(ReadFailure::Fault(crate::core::ErrorValue::new(
            crate::core::FaultCode::Protocol,
            "find and compare require the page reader's cancellable read context",
        ))),
    }
}

/// The Library page's model of one tree.
#[must_use]
pub fn tree_model(tree: &backend_library::browse::ProjectTree) -> TreeModel {
    let reading = backend_present::read_tree(tree);
    let links: Arc<[TreeRoleLinks]> = reading.roles.iter().map(|role| TreeRoleLinks {
        role: role.id,
        rows: role.rows.iter().map(|row| TreeRowLinks {
            name: Arc::from(row.name.as_str()),
            releases: row.versions.iter().enumerate().map(|(at, version)| TreeReleaseLink {
                version: Arc::from(version.as_str()),
                destination: tree_destination(&tree.root, &row.name, version, row.sources.get(at).and_then(Option::as_ref)),
            }).collect::<Vec<_>>().into(),
        }).collect::<Vec<_>>().into(),
    }).collect::<Vec<_>>().into();
    let prepared = Arc::new(prepared_library_model(&reading, &links));
    TreeModel {
        root: Arc::from(tree.root.as_str()),
        reading,
        links,
        prepared,
    }
}

/// A project tree records source origin separately from its display name.
/// Resolve only the identities that preserve that source on a package route.
/// Vendored paths are checked here on the read worker, never while painting.
fn tree_destination(root: &str, name: &str, version: &str, origin: Option<&backend_library::browse::PackageOrigin>) -> TreeDestination {
    use backend_library::browse::PackageOrigin;
    let unavailable = |words: String| TreeDestination::Unavailable(Arc::from(words));
    match origin {
        Some(origin @ PackageOrigin::Registry { source }) => {
            let coordinate = if origin.is_crates_io_registry() {
                format!("pkg:cargo/{name}@{version}")
            } else {
                let index = source.strip_prefix("registry+").or_else(|| source.strip_prefix("sparse+"));
                let Some(index) = index.filter(|index| !index.is_empty()) else {
                    return unavailable("Cargo did not record a usable registry authority.".to_owned());
                };
                format!("pkg:cargo/{name}@{version}?repository_url={}", encode_purl_qualifier(index))
            };
            PackageRef::parse(&coordinate)
                .map_or_else(|_| unavailable("The registry coordinate could not be admitted.".to_owned()), TreeDestination::Open)
        }
        Some(PackageOrigin::Vendored { path }) if !path.is_empty() => {
            let path = std::path::Path::new(path);
            let path = if path.is_absolute() { path.to_path_buf() } else { std::path::Path::new(root).join(path) };
            match path.canonicalize() {
                Ok(path) if path.is_dir() => PackageRef::parse(&path.to_string_lossy()).map_or_else(
                    |_| unavailable("This local source path could not be admitted.".to_owned()),
                    TreeDestination::Open,
                ),
                _ => unavailable("This local source folder is unavailable on this machine.".to_owned()),
            }
        }
        Some(PackageOrigin::Vendored { .. }) => unavailable("Cargo.lock did not record the local source folder.".to_owned()),
        Some(PackageOrigin::Git { .. }) => unavailable("Git source; this tree does not record its checkout folder.".to_owned()),
        Some(PackageOrigin::Unresolved { source }) => unavailable(format!(
            "Cargo did not establish a supported source{}.",
            source.as_ref().map_or(String::new(), |source| format!(" ({source})"))
        )),
        None => unavailable("This release has no unique source identity in the project tree.".to_owned()),
    }
}

/// Package URL qualifiers use canonical percent escapes. Keep URL authority
/// bytes, including an alternative registry's host and path, in the identity.
fn encode_purl_qualifier(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Merge real package identities once, off the UI thread. Exact names lead;
/// local indexed evidence breaks ties, never popularity or invented fitness.
pub(super) fn find_packages(
    query: &str,
    indexed: &[&backend_library::Row],
    catalog: &[backend_library::RegistryPackageRecord],
    source: Option<&dyn crate::host::registry::RegistrySource>,
) -> Vec<crate::model::browse::FindPackage> {
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use std::collections::BTreeMap;
    let mut candidates = BTreeMap::<PackageRef, FindPackage>::new();
    for row in indexed {
        if !matches!(row.id, backend_library::RowId::Package(_)) { continue; }
        let Ok(package) = PackageRef::parse(&row.label) else { continue; };
        let offer = source.and_then(|source| indexed_release(source, &package));
        // A registry tree is named as its crate, not its directory.
        let name: Arc<str> = offer.as_ref().map_or_else(|| Arc::from(package.display_name()), |offer| Arc::from(offer.release.name.as_str()));
        candidates.insert(package.clone(), FindPackage { package, name, description: None, indexed: true, record: None, offer });
    }
    for row in catalog {
        let record = super::page_mapping::registry_record(row);
        let candidate = candidates.entry(record.package.clone()).or_insert_with(|| FindPackage {
            package: record.package.clone(), name: record.name.clone(), description: None, indexed: false, record: None, offer: None,
        });
        candidate.name = record.name.clone();
        candidate.description = record.description.known().cloned();
        candidate.record = Some(record);
    }
    if let Some(source) = source {
        offer_releases(query, source, &mut candidates);
    }
    rank_packages(query, candidates.into_values().collect())
}

/// How many releases of crates this machine has one query offers.
const OFFERS: usize = 16;

/// The release an indexed root is, when it is one of the source's trees: it
/// is in the library already.
fn indexed_release(source: &dyn crate::host::registry::RegistrySource, package: &crate::model::pages::PackageRef) -> Option<crate::model::release::Offer> {
    use crate::model::release::{Availability, Offer};
    if !package.is_local() {
        return None;
    }
    let root = std::path::Path::new(package.as_str());
    let release = source.release_of(root)?;
    Some(Offer { release, availability: Availability::Unpacked(root.to_path_buf()), library: Some(package.clone()) })
}

/// Crates this machine can supply without the network, at their newest
/// release here, which the library does not have yet: each is an offer to
/// add it, keyed by its package URL, with its manifest's description.
fn offer_releases(
    query: &str,
    source: &dyn crate::host::registry::RegistrySource,
    candidates: &mut std::collections::BTreeMap<crate::model::pages::PackageRef, crate::model::browse::FindPackage>,
) {
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use crate::model::release::{Availability, Offer};
    for release in source.offline(query, OFFERS) {
        if candidates.values().any(|candidate| candidate.offer.as_ref().is_some_and(|offer| offer.release == release)) {
            continue;
        }
        let Ok(package) = PackageRef::parse(&release.purl()) else { continue };
        let availability = source.availability(&release);
        let description = match &availability {
            Availability::Unpacked(tree) => crate::model::source_facts::manifest::read(tree).and_then(|manifest| manifest.description).map(Arc::from),
            Availability::Archive(_)
            | Availability::UnverifiedArchive(_)
            | Availability::Ambiguous { .. }
            | Availability::Download => None,
        };
        let name = Arc::from(release.name.as_str());
        let candidate = candidates.entry(package.clone()).or_insert_with(|| FindPackage { package, name, description: None, indexed: false, record: None, offer: None });
        if candidate.description.is_none() {
            candidate.description = description;
        }
        candidate.offer = Some(Offer { release, availability, library: None });
    }
}

fn rank_packages(query: &str, mut candidates: Vec<crate::model::browse::FindPackage>) -> Vec<crate::model::browse::FindPackage> {
    let query = query.trim().to_lowercase();
    candidates.retain(|candidate| candidate.name.to_lowercase().contains(&query)
        || candidate.description.as_ref().is_some_and(|description| description.to_lowercase().contains(&query)));
    candidates.sort_by_cached_key(|candidate| {
        let name = candidate.name.to_lowercase();
        (name != query, !name.starts_with(&query), !candidate.indexed, name, candidate.package.clone())
    });
    candidates.truncate(64);
    candidates
}

#[cfg(test)]
mod find_tests {
    use super::*;
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use backend_library::browse::PackageOrigin;

    fn candidate(name: &str, indexed: bool) -> FindPackage {
        FindPackage { package: PackageRef::parse(&format!("pkg:cargo/{name}@1.0.0")).unwrap(), name: Arc::from(name), description: None, indexed, record: None, offer: None }
    }

    #[test]
    fn exact_package_names_beat_indexed_partial_matches_and_keep_identity() {
        let ranked = rank_packages("TOML", vec![candidate("toml_edit", true), candidate("toml", false), candidate("serde", true)]);
        assert_eq!(ranked.iter().map(|row| row.name.as_ref()).collect::<Vec<_>>(), ["toml", "toml_edit"]);
        assert!(!ranked[0].indexed);
        assert_eq!(ranked[0].package.as_str(), "pkg:cargo/toml@1.0.0");
    }

    #[test]
    fn unmatched_packages_are_empty_and_large_catalogs_are_bounded() {
        assert!(rank_packages("not-present", vec![candidate("toml", true)]).is_empty());
        let rows = (0..200).map(|at| candidate(&format!("item{at:03}"), true)).collect();
        let ranked = rank_packages("item", rows);
        assert_eq!(ranked.len(), 64);
        assert_eq!(ranked.last().unwrap().name.as_ref(), "item063");
    }

    #[test]
    fn tree_links_preserve_registry_release_and_local_path_without_inventing_git_checkout() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let root = root.to_str().unwrap();
        assert_eq!(
            tree_destination(root, "toml", "1.1.6", Some(&PackageOrigin::Registry { source: "registry+https://github.com/rust-lang/crates.io-index".to_owned() })),
            TreeDestination::Open(PackageRef::parse("pkg:cargo/toml@1.1.6").unwrap()),
        );
        let first = tree_destination(root, "toml", "1.1.6", Some(&PackageOrigin::Registry { source: "registry+https://packages.example.test/first".to_owned() }));
        let second = tree_destination(root, "toml", "1.1.6", Some(&PackageOrigin::Registry { source: "registry+https://packages.example.test/second".to_owned() }));
        let (TreeDestination::Open(first), TreeDestination::Open(second)) = (first, second) else { panic!("observed alternative registries have typed coordinates") };
        assert_eq!(first.as_str(), "pkg:cargo/toml@1.1.6?repository_url=https%3A%2F%2Fpackages.example.test%2Ffirst");
        assert_ne!(first, second, "two authorities cannot alias the same name and version");
        assert_ne!(first.as_str(), "pkg:cargo/toml@1.1.6", "an alternate registry cannot open crates.io");
        assert!(matches!(tree_destination(root, "toml", "1.1.6", Some(&PackageOrigin::Unresolved { source: Some("other+opaque".to_owned()) })), TreeDestination::Unavailable(_)));
        let local = tree_destination(root, "gpui-ce", "0.2.2", Some(&PackageOrigin::Vendored { path: "vendor/gpui-ce".to_owned() }));
        let TreeDestination::Open(path) = local else { panic!("a present vendored source should open") };
        assert_eq!(path.as_str(), std::path::Path::new(root).join("vendor/gpui-ce").canonicalize().unwrap().to_str().unwrap());
        assert!(matches!(
            tree_destination(root, "foo", "1.0.0", Some(&PackageOrigin::Git { source: "git+https://example.invalid/foo?branch=main#0123456789abcdef0123456789abcdef01234567".to_owned() })),
            TreeDestination::Unavailable(reason) if reason.contains("Git source")
        ));
        assert!(matches!(tree_destination(root, "foo", "1.0.0", None), TreeDestination::Unavailable(_)));
    }
}
