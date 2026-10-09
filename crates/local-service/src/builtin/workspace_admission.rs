//! Exact local-owner directory admission, independent of cache cardinality.

use backend_engine::WorkspaceDirectoryAdmission;
use std::io;

pub(super) fn admit_local_owner_directories(
    admission: &WorkspaceDirectoryAdmission,
) -> io::Result<()> {
    // These are private application namespaces, not acquired package source
    // trees. Archived builds and object contents are deliberately not visited.
    for path in [
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
    ] {
        admission.admit(path)?;
    }
    for child in ["packs", "objects", "closures", "nodes"] {
        admission.admit(format!("semantic-objects/{child}"))?;
    }
    for root in ["forge/content", "registry/v1/cas/objects"] {
        for child in ["objects", "temps", "transfers", "quarantine"] {
            admission.admit(format!("{root}/{child}"))?;
        }
    }
    for root in [
        "forge/coordination",
        "registry/registry-acquisition/coordination",
    ] {
        for child in ["leases", "objects", "temps"] {
            admission.admit(format!("{root}/{child}"))?;
        }
    }
    Ok(())
}
