// Authority parser and policy feed both product commands. This is not a public
// process journey or a persisted authority/reopen test.
#[test]
fn advisory_authority_matrix_reaches_package_and_advisory_reads() {
    use backend_engine::advisory::{
        AcquisitionDecision, AcquisitionGate, AdvisoryAuthority, AdvisoryCoverage, AdvisorySource,
        AdvisoryStatus, AuthorityFeed, FreshnessState, OfflinePolicy, OsvFeedScope, PolicyReason,
        normalize_package,
    };
    const POSITIVE: &[u8] = br#"{"schema_version":"1.3.1","id":"OSV-MATRIX-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"fixture"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#;
    const EMPTY: &[u8] = br#"{"vulns":[]}"#;
    let root = fixture("advisory-authority-matrix");
    let package = PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("coordinate");
    let identity = normalize_package("cargo", "fixture").expect("identity");
    for (name, configured, positive, complete, unavailable, now, coverage, freshness) in [
        (
            "unconfigured",
            false,
            false,
            false,
            false,
            20,
            AdvisoryCoverage::Unknown,
            FreshnessState::Unknown,
        ),
        (
            "fresh-empty",
            true,
            false,
            true,
            false,
            20,
            AdvisoryCoverage::Complete,
            FreshnessState::Fresh,
        ),
        (
            "fresh-positive",
            true,
            true,
            true,
            false,
            20,
            AdvisoryCoverage::Complete,
            FreshnessState::Fresh,
        ),
        (
            "partial-empty",
            true,
            false,
            false,
            false,
            20,
            AdvisoryCoverage::Partial,
            FreshnessState::Fresh,
        ),
        (
            "partial-positive",
            true,
            true,
            false,
            false,
            20,
            AdvisoryCoverage::Partial,
            FreshnessState::Fresh,
        ),
        (
            "stale-empty",
            true,
            false,
            true,
            false,
            111,
            AdvisoryCoverage::Complete,
            FreshnessState::Stale,
        ),
        (
            "stale-positive",
            true,
            true,
            true,
            false,
            111,
            AdvisoryCoverage::Complete,
            FreshnessState::Stale,
        ),
        (
            "unavailable-empty",
            true,
            false,
            true,
            true,
            20,
            AdvisoryCoverage::Unavailable,
            FreshnessState::Fresh,
        ),
        (
            "unavailable-positive",
            true,
            true,
            true,
            true,
            20,
            AdvisoryCoverage::Unavailable,
            FreshnessState::Fresh,
        ),
    ] {
        let mut authority = AdvisoryAuthority::new(100);
        if configured {
            authority.configure_sources([AdvisorySource::Osv]);
            authority.configure_osv_scope(Some(OsvFeedScope::All));
            let mut feed = AuthorityFeed::parse(
                AdvisorySource::Osv,
                if positive { POSITIVE } else { EMPTY },
                10,
                None,
                None,
            )
            .expect("valid fake OSV feed");
            feed.osv_scope = Some(OsvFeedScope::All);
            feed.complete = complete;
            authority.apply(feed).expect("admit fake feed");
            if unavailable {
                authority.mark_unavailable(AdvisorySource::Osv, 20);
            }
        }
        let (observation, decision) = authority.decide(
            &identity,
            "1.0.0",
            AcquisitionGate {
                offline: OfflinePolicy::Warn,
            },
            now,
            false,
        );
        assert_eq!(observation.coverage, coverage, "{name} authority coverage");
        assert_eq!(
            observation.freshness, freshness,
            "{name} authority freshness"
        );
        assert_eq!(
            observation.advisories.len(),
            usize::from(positive),
            "{name} source facts"
        );
        if positive {
            assert!(
                matches!(&decision, AcquisitionDecision::Deny(reasons)
                if reasons.contains(&PolicyReason::Advisory(AdvisoryStatus::Vulnerable))),
                "{name} positive policy"
            );
        } else if coverage != AdvisoryCoverage::Complete || freshness != FreshnessState::Fresh {
            assert!(
                !matches!(&decision, AcquisitionDecision::Allow),
                "{name} is not clean"
            );
        }
        let projected = AdvisoryPackageDto::from_observation(&observation, decision);
        let unacquired = projected.clone().without_registry_standing();
        let empty_index = CatalogLookupIndex::from_catalog(&[]);
        let SurfaceReply::Advisory(discovered) =
            read_advisory_with_unacquired(&package, None, &[], &empty_index, Some(unacquired))
                .expect("unacquired advisory")
        else {
            panic!("{name} unacquired advisory reply")
        };
        assert_eq!(discovered.coverage, coverage, "{name} unacquired coverage");
        assert_eq!(
            discovered.freshness, freshness,
            "{name} unacquired freshness"
        );
        assert_eq!(
            discovered.registry_standing_observed, None,
            "{name} standing must be unknown"
        );
        assert!(
            discovered
                .reasons
                .contains(&PolicyReason::RegistryStandingUnknown)
        );
        assert_eq!(
            discovered.advisories.len(),
            usize::from(positive),
            "{name} unacquired positives"
        );
        let mut row = registry_row(package.as_str(), "fixture");
        row.advisory = projected;
        let catalog = vec![row];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let mut state = ProductState::open(root.join(format!("{name}.json"))).expect("state");
        let graph = indexed_graph(Vec::new());
        let package_reply = state
            .execute(
                SurfaceCommand::Package {
                    package: package.clone(),
                },
                &view(),
                &catalog,
                &index,
                &graph,
                None,
            )
            .expect("package read");
        let advisory_reply = state
            .execute(
                SurfaceCommand::Advisory {
                    package: package.clone(),
                    override_evidence: None,
                },
                &view(),
                &catalog,
                &index,
                &graph,
                None,
            )
            .expect("advisory read");
        let SurfaceReply::Package(rows) = package_reply else {
            panic!("{name} package reply")
        };
        let SurfaceReply::Advisory(advisory) = advisory_reply else {
            panic!("{name} advisory reply")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].advisory.coverage, coverage,
            "{name} package coverage"
        );
        assert_eq!(
            rows[0].advisory.freshness, freshness,
            "{name} package freshness"
        );
        assert_eq!(advisory.coverage, coverage, "{name} advisory coverage");
        assert_eq!(advisory.freshness, freshness, "{name} advisory freshness");
        assert_eq!(
            advisory.advisories.len(),
            usize::from(positive),
            "{name} advisory objects"
        );
        assert_eq!(
            rows[0]
                .advisory
                .clone()
                .without_registry_standing()
                .decision,
            advisory.decision,
            "{name} policy result"
        );
        if positive {
            assert_eq!(advisory.advisories[0].canonical_id, "OSV-MATRIX-1");
            assert!(
                advisory.advisories[0]
                    .statuses
                    .contains(&AdvisoryStatus::Vulnerable)
            );
            assert_eq!(advisory.advisories[0].coverage, coverage);
            assert_eq!(advisory.advisories[0].freshness, freshness);
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn advisory_read_requires_current_complete_registry_standing_evidence() {
    use backend_library::{
        RegistryPackageFactCompleteness, RegistryPackageFactFreshness, RegistryReleaseStanding,
    };
    let package = PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    for stage in [
        "current",
        "partial",
        "unavailable",
        "unknown",
        "historical",
        "expired",
        "no-authority",
    ] {
        let mut row = registry_row(package.as_str(), "fixture");
        row.standing = if stage == "current" {
            RegistryReleaseStanding::Available
        } else {
            RegistryReleaseStanding::Yanked
        };
        // Simulate a previously serialized or cached current marker. It must
        // not remain current after the source proof changes or disappears.
        row.advisory.registry_standing_observed = Some(true);
        row.advisory.yanked = true;
        row.advisory.unlisted = true;
        row.advisory.decision = backend_library::AcquisitionDecision::Allow;
        row.advisory.reasons = Box::new([]);
        if stage != "no-authority" {
            let mut authority = current_test_authority(&row);
            match stage {
                "partial" => authority.standing = RegistryPackageFactCompleteness::Partial,
                "unavailable" => authority.standing = RegistryPackageFactCompleteness::Unavailable,
                "unknown" => authority.standing = RegistryPackageFactCompleteness::Unknown,
                "historical" => {
                    authority.release_facts_freshness = RegistryPackageFactFreshness::Historical
                }
                "expired" => {
                    if let RegistryPackageFactFreshness::Current {
                        ref mut valid_until_millis,
                        ref mut observed_at_millis,
                        ..
                    } = authority.release_facts_freshness
                    {
                        *observed_at_millis = 0;
                        *valid_until_millis = 1;
                    }
                }
                _ => {}
            }
            row.authority = Some(authority);
        }
        let catalog = vec![row];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let SurfaceReply::Advisory(advisory) =
            read_advisory(&package, None, &catalog, &index).expect("advisory")
        else {
            panic!("{stage} reply")
        };
        assert_eq!(
            advisory.registry_standing_observed,
            (stage == "current").then_some(true),
            "{stage}"
        );
        assert_eq!(
            advisory.yanked,
            stage != "current",
            "{stage} retains source-backed positive"
        );
        assert_eq!(
            advisory.unlisted,
            stage != "current",
            "{stage} retains legacy positive unless current complete source replaces it"
        );
        if stage == "current" {
            assert!(matches!(
                advisory.decision,
                backend_library::AcquisitionDecision::Allow
            ));
        } else {
            assert!(
                matches!(&advisory.decision, backend_library::AcquisitionDecision::Warn(reasons)
                if reasons.contains(&backend_library::PolicyReason::RegistryStandingUnknown)),
                "{stage} cannot imply a cleared registry"
            );
        }
    }
}
