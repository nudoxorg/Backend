//! Durable, bounded remote package acquisition.
//!
//! This module is the single acquisition authority for the workspace. It owns
//! the one contract shared by every ecosystem: an admitted source endpoint, an
//! exact pinned package coordinate, a bounded metadata page and archive fetch,
//! content-address verification, and an atomically committed receipt. No other
//! crate defines a second registry transport, cursor, or cache.
//!
//! A registry is an external effect, never an authority over the workspace
//! head.  This module durably records an intent before network I/O, stores
//! content-addressed archive bytes beneath a deterministic per-ecosystem root
//! ([`storage_root`]), then commits the feed cursor and exact publication
//! receipt in one synced journal record.
//!
//! Availability is explicit and typed. [`AcquisitionOutcome::Offline`] is
//! returned without any network effect when local-first policy is configured,
//! [`AcquisitionOutcome::Unavailable`] is a durable retryable terminal, and
//! [`AcquisitionOutcome::RetryAfter`] carries a bounded server delay. A source
//! extent that exceeds a configured cap produces a measured, typed
//! [`AcquisitionError::Overrun`] or [`TransportFailure::Overrun`] instead of a
//! panic or a silent skip.

mod discovery;
mod ecosystem;
mod facts;
mod feed;
mod frontier;
mod identity;
mod owner;
mod router;
mod transport;
mod wire;

/// Coalesces development metadata into the matching runtime edge when both
/// declarations describe the same source and exact target coordinate.
///
/// Package name alone is never enough to merge two edges: source package,
/// source authority, full target (including its requirement and resolution),
/// and evidence must all agree. A runtime row is retained as the representative
/// so its declared optionality remains authoritative.
pub(crate) fn coalesce_runtime_development_dependency_rows(
    rows: Vec<backend_library::PackageDependencyRecord>,
) -> Vec<backend_library::PackageDependencyRecord> {
    use backend_library::{
        DependencyEvidence, DependencyScope, PackageDependencyRecord, PackageDependencyTarget,
        PackageGraphSourceAuthority, PackageReference,
    };
    use std::collections::BTreeSet;

    type Coordinate = (
        PackageReference,
        PackageGraphSourceAuthority,
        PackageDependencyTarget,
        DependencyEvidence,
    );

    fn coordinate(row: &PackageDependencyRecord) -> Coordinate {
        (
            row.source.clone(),
            row.source_authority,
            row.target.clone(),
            row.evidence,
        )
    }

    let mut rows = backend_library::collapse_dependency_rows(rows);
    let runtime_coordinates = rows
        .iter()
        .filter(|row| row.scope == DependencyScope::Runtime)
        .map(coordinate)
        .collect::<BTreeSet<_>>();
    rows.retain(|row| {
        row.scope != DependencyScope::Development || !runtime_coordinates.contains(&coordinate(row))
    });
    rows
}

#[cfg(test)]
mod tests;

pub use discovery::{
    ConanRecipeRef, ConanRecipeTree, CratesRecentPage, CratesRecentRelease, CratesSparsePackage,
    CratesSparseRelease, DiscoveryAdvisory, DiscoveryBatch, DiscoveryCompleteness, DiscoveryCursor,
    DiscoveryError, DiscoveryFacet, DiscoveryFact, DiscoveryMetadata, DiscoveryObservedAt,
    DiscoveryPackageRetraction, DiscoveryReleaseObservation, DiscoverySourceIdentity,
    DiscoveryStanding, GoModuleIndexPage, MAX_DISCOVERY_CURSOR_BYTES, MAX_DISCOVERY_PAGE_ITEMS,
    MAX_DISCOVERY_PROJECTS, MavenSearchPage, NpmChangedPackage, NpmChangesPage, NpmPackument,
    NugetCatalogEvent, NugetCatalogLeafRef, NugetCatalogPageRef, NugetCatalogPlan, PypiProjectList,
    PypiProjectMetadata, crates_sparse_index_path, parse_conan_recipe_tree,
    parse_conan_recipe_versions, parse_crates_recent_page, parse_crates_sparse_package,
    parse_go_module_index_page, parse_maven_search_page, parse_npm_changes_page,
    parse_npm_packument_document, parse_nuget_catalog_index, parse_nuget_catalog_leaf,
    parse_nuget_catalog_page, parse_pypi_project_list, parse_pypi_project_metadata,
};
pub use ecosystem::{
    ChecksumAlgorithm, EcosystemAdapter, NativeArtifact, NativeArtifactKind, NativeDistTag,
    NativeFeature, NativeRelease, RegistryChecksum,
};
pub use facts::{DownloadCount, DownloadCountGap, ReleaseFacts, ReleaseStanding, SecurityStanding};
/// Preferred closed name for the seven native registry grammars.
pub use identity::RegistryEcosystem as RegistryKind;
pub use identity::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, CanonicalFeedV1, FeedCursor,
    FeedSchema, PackageCoordinate, PackageName, PackageVersion, ProvenanceDigest,
    PublishedArtifactClaim, RegistryCoordinate, RegistryEcosystem, RegistryEndpoint, RegistryId,
    RemoteRegistry, admit_registry_coordinate,
};
pub use owner::{
    AcquisitionError, AcquisitionIntent, AcquisitionOutcome, AcquisitionReceipt, PublishedPackage,
    RegistryOwner, RegistryReadiness, RegistryRecovery, storage_root,
};
pub use router::{
    CARGO_SPARSE_INDEX, CONAN_CENTER, GO_MODULE_PROXY, MAVEN_CENTRAL, NPM_REGISTRY, NUGET_V3,
    PYPI_SIMPLE_API, REGISTRY_SOURCE_ROOT_VERSION, RegistryConfigurationError, RegistryRoute,
    RegistrySource, RegistrySourceSet,
};
pub use transport::{
    ArchiveArtifact, FeedPage, FeedRequest, HttpRegistryTransport, RegistryTransport,
    RemotePackage, TransportFailure, TransportResult,
};
