//! Forge-only package detail projection.
//!
//! Forge acquisition establishes source and manifest facts, not registry
//! publication facts. This projection therefore keeps exact package versions
//! separate from the commit pin that supplied a manifest and leaves registry
//! standing, downloads, and advisory evidence unknown.

use backend_engine::{
    ForgeCoordinate, ForgeObjectId, ForgePackageManifest, ForgeResolution, ForgeRevision,
    ForgeSearchRecord,
};
use backend_library::{
    AdvisoryPackageDto, PackageCoordinate, RegistryDownloadCount, RegistryEvidenceFacet,
    RegistryFactAvailability,
};
use std::sync::Arc;

const MAX_FORGE_PACKAGE_DETAIL_ROWS: usize = 256;

/// One source-attributed manifest view from an admitted forge repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct ForgePackageDetail {
    /// Exact repository coordinate, including its requested revision.
    pub(in crate::builtin) source: ForgeCoordinate,
    /// Authority resolution bound to `source`.
    pub(in crate::builtin) resolution: ForgeResolution,
    /// Manifest as admitted from the source tree.
    pub(in crate::builtin) manifest: ForgePackageManifest,
    /// Package PURL only when both name and version were recorded in the
    /// manifest. A resolved Git commit is never used as a package version.
    pub(in crate::builtin) package_coordinate: Option<PackageCoordinate>,
    /// Whether the row names a package release or only a pinned source tree.
    pub(in crate::builtin) pin: ForgePackagePin,
    /// Repository facts reported by the forge authority.
    pub(in crate::builtin) repository_metadata: Arc<backend_engine::ForgeRepositoryMetadata>,
    /// Registry-only facts not established by forge acquisition.
    pub(in crate::builtin) registry: ForgeRegistryEvidence,
}

/// How a forge manifest row is addressed by package detail clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) enum ForgePackagePin {
    /// The manifest itself recorded this exact package version.
    PackageVersion { coordinate: PackageCoordinate },
    /// The source is pinned to a resolved Git revision, but no actual package
    /// release coordinate could be established from the manifest.
    PinnedRevision {
        requested_revision: ForgeRevision,
        resolved_commit: ForgeObjectId,
    },
}

/// Typed registry evidence coverage for a forge-only package row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) struct ForgeRegistryEvidence {
    /// Forge acquisition has no registry yank or release-standing authority.
    pub(in crate::builtin) yanked: RegistryEvidenceFacet<bool>,
    /// Forge acquisition has no registry download telemetry.
    pub(in crate::builtin) downloads: RegistryDownloadCount,
    /// Forge acquisition has no registry advisory coverage.
    pub(in crate::builtin) advisory: AdvisoryPackageDto,
}

impl Default for ForgeRegistryEvidence {
    fn default() -> Self {
        Self {
            yanked: RegistryEvidenceFacet::Unknown,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unknown),
            advisory: AdvisoryPackageDto::unknown(),
        }
    }
}

/// Projects every admitted manifest, retaining unversioned source pins.
pub(in crate::builtin) fn project_package_details(
    records: &[ForgeSearchRecord],
) -> Result<Vec<ForgePackageDetail>, ForgePackageViewError> {
    let mut details = Vec::new();
    for record in records {
        record
            .resolution
            .validate_for(&record.coordinate)
            .map_err(|_| ForgePackageViewError::ResolutionMismatch)?;
        let repository_metadata = Arc::new(record.metadata.clone());
        for manifest in &record.manifests {
            if details.len() == MAX_FORGE_PACKAGE_DETAIL_ROWS {
                return Err(ForgePackageViewError::Bounds);
            }
            details.push(project_manifest(
                record,
                manifest,
                repository_metadata.clone(),
            ));
        }
    }
    details.sort_by(|left, right| {
        left.source
            .canonical()
            .cmp(&right.source.canonical())
            .then_with(|| left.manifest.path.cmp(&right.manifest.path))
            .then_with(|| {
                left.package_coordinate
                    .as_ref()
                    .map(|coordinate| coordinate.as_str())
                    .cmp(
                        &right
                            .package_coordinate
                            .as_ref()
                            .map(|coordinate| coordinate.as_str()),
                    )
            })
    });
    Ok(details)
}

/// Resolves only exact package versions actually recorded by a manifest.
/// Pinned source revisions remain available from [`project_package_details`]
/// but can never be found by pretending their commit is a package version.
pub(in crate::builtin) fn find_package_versions(
    records: &[ForgeSearchRecord],
    requested: &PackageCoordinate,
) -> Result<Vec<ForgePackageDetail>, ForgePackageViewError> {
    let mut details = Vec::new();
    for record in records {
        record
            .resolution
            .validate_for(&record.coordinate)
            .map_err(|_| ForgePackageViewError::ResolutionMismatch)?;
        for manifest in &record.manifests {
            if manifest_package_coordinate(manifest).as_ref() != Some(requested) {
                continue;
            }
            if details.len() == MAX_FORGE_PACKAGE_DETAIL_ROWS {
                return Err(ForgePackageViewError::Bounds);
            }
            details.push(project_manifest(
                record,
                manifest,
                Arc::new(record.metadata.clone()),
            ));
        }
    }
    Ok(details)
}

fn project_manifest(
    record: &ForgeSearchRecord,
    manifest: &ForgePackageManifest,
    repository_metadata: Arc<backend_engine::ForgeRepositoryMetadata>,
) -> ForgePackageDetail {
    let package_coordinate = manifest_package_coordinate(manifest);
    let pin = match &package_coordinate {
        Some(coordinate) => ForgePackagePin::PackageVersion {
            coordinate: coordinate.clone(),
        },
        None => ForgePackagePin::PinnedRevision {
            requested_revision: record.coordinate.revision().clone(),
            resolved_commit: record.resolution.commit.clone(),
        },
    };
    ForgePackageDetail {
        source: record.coordinate.clone(),
        resolution: record.resolution.clone(),
        manifest: manifest.clone(),
        package_coordinate,
        pin,
        repository_metadata,
        registry: ForgeRegistryEvidence::default(),
    }
}

fn manifest_package_coordinate(manifest: &ForgePackageManifest) -> Option<PackageCoordinate> {
    let (backend_engine::ForgeFact::Recorded(name), backend_engine::ForgeFact::Recorded(version)) =
        (&manifest.name, &manifest.version)
    else {
        return None;
    };
    let package_name = if manifest.ecosystem == backend_library::RegistryEcosystem::Maven {
        name.as_str().replace(':', "/")
    } else {
        name.as_str().to_owned()
    };
    PackageCoordinate::parse(format!(
        "pkg:{}/{}@{}",
        manifest.ecosystem.package_type().as_str(),
        package_name,
        version.as_str()
    ))
    .ok()
}

/// Integrity failures in the durable forge catalog that prevent a package
/// detail projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::builtin) enum ForgePackageViewError {
    /// The requested revision, authority, or validator no longer matches its
    /// exact forge coordinate.
    ResolutionMismatch,
    /// The projected response would exceed its fixed row bound.
    Bounds,
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::{
        DependencyFacts, ForgeFact, ForgeRepositoryMetadata, ForgeUnavailableReason,
    };
    use backend_library::{ProductText, RegistryEcosystem};
    use std::sync::Arc;

    fn record(revision: &str, version: Option<&str>) -> ForgeSearchRecord {
        let coordinate =
            ForgeCoordinate::parse(format!("https://github.com/acme/widget@tag:{revision}"))
                .expect("forge coordinate");
        let commit =
            ForgeObjectId::parse("0123456789012345678901234567890123456789").expect("commit");
        let tree = ForgeObjectId::parse("1123456789012345678901234567890123456789").expect("tree");
        let resolution = ForgeResolution::for_coordinate(
            &coordinate,
            commit,
            Some(tree),
            format!("{revision}-validator"),
        )
        .expect("bound resolution");
        let version = version.map_or(
            ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
            |version| ForgeFact::Recorded(ProductText::new(version).expect("version text")),
        );
        let manifest = ForgePackageManifest {
            path: Arc::from("Cargo.toml"),
            ecosystem: RegistryEcosystem::Cargo,
            name: ForgeFact::Recorded(ProductText::new("widget").expect("name text")),
            version,
            dependencies: DependencyFacts::Known(Box::default()),
        };
        ForgeSearchRecord {
            coordinate,
            resolution,
            archive: [0; 32],
            metadata: ForgeRepositoryMetadata::unavailable(
                "acme",
                ForgeUnavailableReason::AuthorityOmitted,
            ),
            manifests: vec![manifest].into_boxed_slice(),
        }
    }

    #[test]
    fn versioned_and_unversioned_manifests_keep_distinct_pin_meaning() {
        let records = [record("v1.4.0", Some("1.4.0")), record("main", None)];
        let details = project_package_details(&records).expect("package detail projection");
        assert_eq!(details.len(), 2);

        let version = PackageCoordinate::parse("pkg:cargo/widget@1.4.0").expect("package version");
        let version_rows = find_package_versions(&records, &version).expect("exact version");
        assert_eq!(version_rows.len(), 1);
        assert!(matches!(
            &version_rows[0].pin,
            ForgePackagePin::PackageVersion { .. }
        ));

        let pinned = details
            .iter()
            .find(|detail| detail.source.revision() != records[0].coordinate.revision())
            .expect("unversioned pin");
        assert!(pinned.package_coordinate.is_none());
        assert!(matches!(
            &pinned.pin,
            ForgePackagePin::PinnedRevision {
                requested_revision: ForgeRevision::Tag(_),
                resolved_commit,
            } if resolved_commit == &pinned.resolution.commit
        ));
        let commit_as_release = PackageCoordinate::parse(format!(
            "pkg:cargo/widget@{}",
            pinned.resolution.commit.as_hex()
        ))
        .expect("test commit-shaped PURL");
        assert!(
            find_package_versions(&records, &commit_as_release)
                .expect("exact lookup")
                .is_empty()
        );

        assert_eq!(pinned.registry.yanked, RegistryEvidenceFacet::Unknown);
        assert_eq!(
            pinned.registry.downloads,
            RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unknown)
        );
        assert_eq!(
            pinned.registry.advisory.coverage,
            backend_library::AdvisoryCoverage::Unknown
        );
    }
}
