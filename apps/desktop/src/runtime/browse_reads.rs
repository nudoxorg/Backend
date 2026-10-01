//! Reads for the browsing pages: one `project-tree` round trip, lowered to
//! the words every surface shares.

use super::reads::failure;
use crate::model::browse::{
    BrowseKey, BrowseValue, TreeDestination, TreeModel, TreeReleaseLink, TreeRoleLinks,
    TreeRowLinks,
};
use crate::model::pages::{PackageRef, PageValue, ReadFailure};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply};
use backend_present::Engine;
use std::sync::Arc;

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
    TreeModel {
        root: Arc::from(tree.root.as_str()),
        reading,
        links,
    }
}

/// A project tree records source origin separately from its display name.
/// Resolve only the identities that preserve that source on a package route.
/// Vendored paths are checked here on the read worker, never while painting.
fn tree_destination(root: &str, name: &str, version: &str, origin: Option<&backend_library::browse::PackageOrigin>) -> TreeDestination {
    use backend_library::browse::PackageOrigin;
    let unavailable = |words: String| TreeDestination::Unavailable(Arc::from(words));
    match origin {
        Some(PackageOrigin::Registry) => {
            PackageRef::parse(&format!("pkg:cargo/{name}@{version}"))
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
        Some(PackageOrigin::Git { url }) => unavailable(format!("Git source at {url}; this tree does not record its checkout folder.")),
        None => unavailable("This release has no unique source identity in the project tree.".to_owned()),
    }
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
            tree_destination(root, "toml", "1.1.6", Some(&PackageOrigin::Registry)),
            TreeDestination::Open(PackageRef::parse("pkg:cargo/toml@1.1.6").unwrap()),
        );
        let local = tree_destination(root, "gpui-ce", "0.2.2", Some(&PackageOrigin::Vendored { path: "vendor/gpui-ce".to_owned() }));
        let TreeDestination::Open(path) = local else { panic!("a present vendored source should open") };
        assert_eq!(path.as_str(), std::path::Path::new(root).join("vendor/gpui-ce").canonicalize().unwrap().to_str().unwrap());
        assert!(matches!(
            tree_destination(root, "foo", "1.0.0", Some(&PackageOrigin::Git { url: "https://example.invalid/foo".to_owned() })),
            TreeDestination::Unavailable(reason) if reason.contains("Git source")
        ));
        assert!(matches!(tree_destination(root, "foo", "1.0.0", None), TreeDestination::Unavailable(_)));
    }
}
