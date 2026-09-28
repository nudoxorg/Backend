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
) -> Vec<crate::model::browse::FindPackage> {
    use crate::model::browse::FindPackage;
    use crate::model::pages::PackageRef;
    use std::collections::BTreeMap;
    let mut candidates = BTreeMap::<PackageRef, FindPackage>::new();
    for row in indexed {
        if !matches!(row.id, backend_library::RowId::Package(_)) { continue; }
        let Ok(package) = PackageRef::parse(&row.label) else { continue; };
        let name: Arc<str> = Arc::from(package.display_name());
        candidates.insert(package.clone(), FindPackage { package, name, description: None, indexed: true, record: None });
    }
    for row in catalog {
        let record = super::page_mapping::registry_record(row);
        let candidate = candidates.entry(record.package.clone()).or_insert_with(|| FindPackage {
            package: record.package.clone(), name: record.name.clone(), description: None, indexed: false, record: None,
        });
        candidate.name = record.name.clone();
        candidate.description = record.description.known().cloned();
        candidate.record = Some(record);
    }
    rank_packages(query, candidates.into_values().collect())
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
        FindPackage { package: PackageRef::parse(&format!("pkg:cargo/{name}@1.0.0")).unwrap(), name: Arc::from(name), description: None, indexed, record: None }
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
