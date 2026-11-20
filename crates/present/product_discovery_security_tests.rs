use super::*;
use backend_library::{
    RegistryDiscoveryCompleteness, RegistryDiscoveryFreshness, RegistryDiscoveryMetadata,
    RegistryDiscoveryStanding,
};

#[test]
fn discovery_empty_source_list_does_not_assert_complete_security_or_backfill() {
    let candidate = RegistryDiscoveryCandidate {
        source: [1; 32],
        coordinate: backend_library::PackageCoordinate::parse("pkg:pypi/fixture@1.0.0")
            .expect("pinned package"),
        standing: RegistryDiscoveryStanding::Published,
        completeness: RegistryDiscoveryCompleteness::Incomplete,
        caught_up: false,
        freshness: RegistryDiscoveryFreshness::Historical {
            observed_at_millis: 1,
        },
        proof: [2; 32],
        metadata: RegistryDiscoveryMetadata {
            advisories: RegistryEvidenceFacet::Known(Box::new([])),
            ..RegistryDiscoveryMetadata::default()
        },
    };
    let row = discovery_row(&candidate);
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag.contains("source reports no advisory records")
                && tag.contains("check advisory authority for coverage"))
    );
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag.contains("not included in discovery"))
    );
    assert!(row.tags().iter().any(|tag| tag == "source not caught up"));
    assert!(
        !row.tags()
            .iter()
            .any(|tag| tag == "no advisories reported" || tag == "source backfill in progress")
    );
}

#[test]
fn discovery_row_displays_only_source_reported_release_text() {
    let candidate = RegistryDiscoveryCandidate {
        source: [1; 32],
        coordinate: backend_library::PackageCoordinate::parse("pkg:nuget/example@1.0.0").unwrap(),
        standing: RegistryDiscoveryStanding::Published,
        completeness: RegistryDiscoveryCompleteness::Incomplete,
        caught_up: false,
        freshness: RegistryDiscoveryFreshness::Historical { observed_at_millis: 1 },
        proof: [2; 32],
        metadata: RegistryDiscoveryMetadata {
            description: RegistryEvidenceFacet::Known(backend_library::ProductText::new("source description").unwrap()),
            license: RegistryEvidenceFacet::Known(backend_library::ProductText::new("MIT").unwrap()),
            ..RegistryDiscoveryMetadata::default()
        },
    };
    let row = discovery_row(&candidate);
    assert!(row.tags().iter().any(|tag| tag == "description: source description"));
    assert!(row.tags().iter().any(|tag| tag == "license: MIT"));
    assert!(!row.tags().iter().any(|tag| tag.starts_with("published:")));
    assert!(row.tags().iter().any(|tag| tag == "historical observation"));
}

#[test]
fn retained_positive_is_labeled_historical_and_source_scoped() {
    let advisory = backend_library::RegistryDiscoveryAdvisory {
        id: backend_library::ProductText::new("GHSA-history").expect("id"),
        aliases: RegistryEvidenceFacet::Unknown,
        summary: RegistryEvidenceFacet::Unknown,
        severity: RegistryEvidenceFacet::Unknown,
        fixed_in: RegistryEvidenceFacet::Unknown,
    };
    let candidate = RegistryDiscoveryCandidate {
        source: [1; 32],
        coordinate: backend_library::PackageCoordinate::parse("pkg:npm/fixture@1.0.0")
            .expect("pinned package"),
        standing: RegistryDiscoveryStanding::Published,
        completeness: RegistryDiscoveryCompleteness::Incomplete,
        caught_up: false,
        freshness: RegistryDiscoveryFreshness::Historical {
            observed_at_millis: 10,
        },
        proof: [7; 32],
        metadata: RegistryDiscoveryMetadata {
            advisories: RegistryEvidenceFacet::Known(Box::new([advisory])),
            ..RegistryDiscoveryMetadata::default()
        },
    };
    let row = discovery_row(&candidate);
    assert!(row.tags().iter().any(|tag| tag == "historical observation"));
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag.contains("1 source advisory record(s)"))
    );
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag.contains("check advisory authority for coverage"))
    );
    let group = RegistryPackageSearchGroup {
        kind: RegistrySearchGroupKind::Discovered,
        source: candidate.source,
        ecosystem: backend_library::RegistryEcosystem::Npm,
        lineage: backend_library::ProductText::new("fixture").expect("lineage"),
        releases: Box::new([RegistrySearchRelease::Discovered(candidate)]),
        release_match_scope: RegistryReleaseMatchScope::LineageMetadataOnly,
        more_releases: false,
    };
    let row = registry_search_group_row(&group);
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag == "includes historical source evidence")
    );
    assert!(
        row.tags()
            .iter()
            .any(|tag| tag == "package metadata matched across source observations")
    );
}

#[test]
fn package_and_advisory_render_explicit_coverage_and_freshness_matrix() {
    use backend_library::{
        AcquisitionDecision, AdvisoryCoverage, AdvisoryPackageDto, AdvisoryStatus,
        AdvisorySurfaceDto, FreshnessState, PolicyReason, RegistryDownloadCount, RegistryEcosystem,
        RegistryFactAvailability, RegistryNativeMetadata, RegistryPackageRecord,
        RegistryReleaseStanding, SeverityLevel,
    };
    let native = RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test");
    for (coverage, freshness, positive) in [
        (AdvisoryCoverage::Unknown, FreshnessState::Unknown, false),
        (AdvisoryCoverage::Unavailable, FreshnessState::Fresh, false),
        (AdvisoryCoverage::Unavailable, FreshnessState::Unknown, true),
        (AdvisoryCoverage::Partial, FreshnessState::Fresh, false),
        (AdvisoryCoverage::Partial, FreshnessState::Fresh, true),
        (AdvisoryCoverage::Complete, FreshnessState::Stale, false),
        (AdvisoryCoverage::Complete, FreshnessState::Stale, true),
        (AdvisoryCoverage::Complete, FreshnessState::Fresh, false),
        (AdvisoryCoverage::Complete, FreshnessState::Fresh, true),
    ] {
        let mut advisory = AdvisoryPackageDto::unknown();
        advisory.coverage = coverage;
        advisory.freshness = freshness;
        if positive {
            advisory.decision = AcquisitionDecision::Deny(Box::new([PolicyReason::Advisory(
                AdvisoryStatus::Vulnerable,
            )]));
            advisory.advisories = Box::new([AdvisorySurfaceDto {
                schema: 1,
                canonical_id: "OSV-MATRIX-1".to_owned(),
                source_ids: Box::new([]),
                aliases: Box::new([]),
                statuses: Box::new([AdvisoryStatus::Vulnerable]),
                categories: Box::new([]),
                affected: Box::new([]),
                fixed_ranges: Box::new([]),
                severity: SeverityLevel::Unknown,
                malware: backend_advisory::MalwareCoverage::NotCovered,
                coverage,
                freshness,
                withdrawn: None,
                yanked: false,
                unlisted: false,
                summary: None,
            }]);
        }
        let row = RegistryPackageRecord {
            coordinate: backend_library::PackageReference::parse("pkg:cargo/fixture@1.0.0")
                .expect("coordinate"),
            ecosystem: RegistryEcosystem::Cargo,
            name: backend_library::ProductText::from_static("fixture"),
            version: backend_library::ProductText::from_static("1.0.0"),
            bytes: 0,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version: [0; 32],
            authority: None,
            native_metadata_version: native.identity().expect("native identity"),
            native_metadata: native.clone(),
            forge_sources: Box::new([]),
            advisory: advisory.clone(),
        };
        let package = product_view(&SurfaceReply::Package(Box::new([row])));
        let details = product_view(&SurfaceReply::Advisory(advisory));
        let coverage = wire_words(&coverage);
        let freshness = wire_words(&freshness);
        assert!(
            package.records()[0]
                .tags()
                .contains(&format!("advisory: {coverage} / {freshness}"))
        );
        assert!(
            details.records()[0]
                .tags()
                .contains(&format!("coverage: {coverage}"))
        );
        assert!(
            details.records()[0]
                .tags()
                .contains(&format!("freshness: {freshness}"))
        );
        if positive {
            assert!(
                details
                    .records()
                    .iter()
                    .any(|record| record.title() == "OSV-MATRIX-1")
            );
            assert!(
                details.records()[0]
                    .tags()
                    .iter()
                    .any(|tag| tag.starts_with("decision: deny"))
            );
        }
    }
}

#[test]
fn discovered_package_and_advisory_keep_authority_separate_from_source() {
    let package =
        backend_library::PackageCoordinate::parse("pkg:pypi/fixture@1.0.0").expect("coordinate");
    let authority = backend_library::AdvisoryPackageDto::unknown();
    assert_eq!(authority.registry_standing_observed, None);
    let observation = backend_library::RegistryPackageDiscoveryObservation::Unavailable {
        source: Some([3; 32]),
        reason: backend_library::ProductText::from_static("fixture unavailable"),
    };
    let reply = SurfaceReply::PackageDiscovery {
        package,
        observation: observation.clone(),
        advisory: Some(authority.clone()),
    };
    let view = product_view(&reply);
    let projection = view
        .package_discovery()
        .expect("source and authority projection");
    assert_eq!(projection.observation, observation);
    assert_eq!(projection.advisory.as_ref(), Some(&authority));
    let standalone = product_view(&SurfaceReply::Advisory(authority));
    let package_security = view
        .records()
        .iter()
        .find(|row| row.title() == "security decision")
        .expect("package security");
    assert_eq!(package_security.tags(), standalone.records()[0].tags());
    assert!(
        package_security
            .tags()
            .iter()
            .any(|tag| tag == "yanked: unknown")
    );
    assert!(
        package_security
            .tags()
            .iter()
            .any(|tag| tag == "unlisted: unknown")
    );
}

#[test]
fn advisory_standing_requires_observation_and_keeps_legacy_positive() {
    let mut advisory = backend_library::AdvisoryPackageDto::unknown();
    let unknown = product_view(&SurfaceReply::Advisory(advisory.clone()));
    assert!(
        unknown.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "yanked: unknown")
    );
    assert!(
        unknown.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "unlisted: unknown")
    );
    advisory.yanked = true;
    let historical = product_view(&SurfaceReply::Advisory(advisory.clone()));
    assert!(
        historical.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "yanked: reported; current standing unverified")
    );
    assert!(
        historical.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "unlisted: unknown")
    );
    advisory.registry_standing_observed = Some(true);
    let known = product_view(&SurfaceReply::Advisory(advisory));
    assert!(
        known.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "yanked: true")
    );
    assert!(
        known.records()[0]
            .tags()
            .iter()
            .any(|tag| tag == "unlisted: false")
    );
}
