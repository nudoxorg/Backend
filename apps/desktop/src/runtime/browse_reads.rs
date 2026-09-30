//! Reads for the browsing pages: one `project-tree` round trip, lowered to
//! the words every surface shares.

use super::reads::failure;
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{PageValue, ReadFailure};
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
    TreeModel {
        root: Arc::from(tree.root.as_str()),
        reading: backend_present::read_tree(tree),
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
            Availability::Archive(_) | Availability::Download => None,
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
}
