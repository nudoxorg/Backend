//! Exact local-owner directory admission, independent of cache cardinality.

use backend_engine::WorkspaceDirectoryAdmission;
use std::io;

pub(super) fn admit_local_owner_directories(
    admission: &WorkspaceDirectoryAdmission,
) -> io::Result<()> {
    // These are private application namespaces, not acquired package source
    // trees. Archived builds and object contents are deliberately not visited.
    // The whole set is admitted together so that each directory that gains
    // entries is flushed once rather than once per created child.
    admission.admit_all(local_owner_directories())
}

/// Every private directory a local owner expects to exist, parents before children.
fn local_owner_directories() -> Vec<String> {
    let mut directories = [
        "compiler",
        "compiler/artifacts",
        "compiler/journal",
        "compiler/native-work",
        "cache/embedding",
        "search-index-v2",
        "registry-discovery",
        "forge/coordination",
        "forge/content",
        "registry/v1/cas/objects",
        "registry/registry-acquisition/coordination",
    ]
    .map(str::to_owned)
    .to_vec();
    for child in ["packs", "objects", "closures", "nodes"] {
        directories.push(format!("semantic-objects/{child}"));
    }
    for root in ["forge/content", "registry/v1/cas/objects"] {
        for child in ["objects", "temps", "transfers", "quarantine"] {
            directories.push(format!("{root}/{child}"));
        }
    }
    for root in [
        "forge/coordination",
        "registry/registry-acquisition/coordination",
    ] {
        for child in ["leases", "objects", "temps"] {
            directories.push(format!("{root}/{child}"));
        }
    }
    directories
}

#[cfg(test)]
mod tests {
    use super::local_owner_directories;

    #[test]
    fn the_admitted_set_is_the_exact_local_owner_namespace() {
        let directories = local_owner_directories();
        assert_eq!(directories.len(), 11 + 4 + 8 + 6);
        for required in [
            "compiler/journal",
            "semantic-objects/closures",
            "forge/content/quarantine",
            "registry/v1/cas/objects/transfers",
            "registry/registry-acquisition/coordination/leases",
        ] {
            assert!(directories.iter().any(|path| path == required), "{required}");
        }
        let mut unique = directories.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), directories.len(), "no directory is listed twice");
    }
}
