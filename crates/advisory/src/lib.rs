//! Typed security-advisory ingestion and acquisition policy.
//!
//! This module is deliberately independent from archive acquisition.  An advisory is a
//! versioned claim about a package identity, while the registry owner remains the authority for
//! bytes and release selection.  Keeping those two facts separate means a refreshed advisory
//! frontier can invalidate an installation without downloading or re-indexing an artifact.

mod journal;
mod model;
mod parse;
mod policy;
mod version;
mod wire;

pub use journal::{
    AdvisoryDelta, AdvisoryJournal, AdvisoryJournalError, AdvisorySync, Checkpoint, FeedFreshness,
    SyncMode, WithdrawalRecord,
};
pub use model::{
    Advisory, AdvisoryCategory, AdvisoryKey, AdvisorySchema, AdvisorySource, AdvisoryStatus,
    AffectedRange, Alias, AliasGraph, AliasGraphError, CanonicalAdvisoryId, Evidence, EvidenceKind,
    FreshnessState, MalwareCoverage, NativeAdvisoryId, PackageIdentity, Reference, Severity,
    SeverityLevel, VersionEvent, VersionEventKind, VersionMatcher, VersionSyntax,
};
pub use parse::{
    GhsaParseError, ParseError, RustSecParseError, parse_ghsa_global, parse_osv, parse_rustsec,
};
pub use policy::{
    AcquisitionDecision, AcquisitionGate, AdvisoryCoverage, AdvisoryObservation, OfflinePolicy,
    OverrideEvidence, PolicyReason,
};
pub use version::{
    NormalizedVersion, PackageNormalizationError, VersionCompareError, matches, normalize_package,
    normalize_version, range_matches,
};
pub use wire::{AdvisoryDecisionDto, AdvisoryPackageDto, AdvisorySurfaceDto};

#[cfg(test)]
mod tests {
    use super::*;

    fn osv(events: &str) -> String {
        format!(
            r#"{{
                "schema_version":"1.3.1", "id":"OSV-TEST-1", "modified":"2026-01-02T00:00:00Z",
                "published":"2026-01-01T00:00:00Z", "aliases":["CVE-TEST-1"],
                "affected":[{{"package":{{"ecosystem":"Cargo","name":"demo"}},"ranges":[{{"type":"SEMVER","events":{events}}}],"versions":["1.0.0"]}}],
                "references":[{{"type":"ADVISORY","url":"https://example.invalid/advisory"}}]
            }}"#
        )
    }

    fn object() -> Advisory {
        parse_osv(osv(r#"[{"introduced":"0"},{"fixed":"1.2.0"},{"introduced":"2.0.0"},{"last_affected":"2.1.0"},{"introduced":"3.0.0"},{"limit":"3.1.0"}]"#).as_bytes(), 7).expect("valid OSV")
    }

    #[test]
    fn osv_event_boundaries_are_not_lexicographic() {
        let advisory = object();
        let range = &advisory.affected[0];
        assert!(!version::range_matches(range, "1.10.0").expect("semver"));
        assert!(version::range_matches(range, "2.1.0").expect("last affected"));
        assert!(!version::range_matches(range, "2.1.1").expect("after last affected"));
        assert!(version::range_matches(range, "3.0.5").expect("limit interval"));
        assert!(!version::range_matches(range, "3.1.0").expect("limit is exclusive"));
    }

    #[test]
    fn rustsec_patched_and_unaffected_are_distinct() {
        let source = br#"
[advisory]
id = "RUSTSEC-2026-0001"
package = "demo"
date = "2026-01-01"
aliases = ["CVE-TEST-1"]
categories = ["unsound"]
url = "https://example.invalid/rustsec"
[versions]
patched = [">= 1.2.0"]
unaffected = ["< 1.0.0"]
"#;
        let advisory = parse_rustsec(source, 9).expect("valid RustSec");
        let range = &advisory.affected[0];
        assert!(!version::range_matches(range, "0.9.5").expect("unaffected"));
        assert!(version::range_matches(range, "1.0.5").expect("outside unaffected"));
        assert!(!version::range_matches(range, "1.2.0").expect("patched"));
        assert!(version::range_matches(range, "1.1.0").expect("affected"));
        assert_eq!(advisory.statuses().as_ref(), &[AdvisoryStatus::Unsound]);
    }

    #[test]
    fn ghsa_requires_explicit_malware_coverage() {
        let source = br#"{"ghsa_id":"GHSA-test","vulnerabilities":[],"severity":"high"}"#;
        assert_eq!(
            parse_ghsa_global(source, 1),
            Err(GhsaParseError::MissingMalwareCoverage)
        );
        let source = br#"{"ghsa_id":"GHSA-test","malware_coverage":true,"vulnerabilities":[{"package":{"ecosystem":"npm","name":"demo"},"vulnerable_version_range":">= 1.0.0, < 2.0.0"}]}"#;
        let advisory = parse_ghsa_global(source, 1).expect("explicit coverage");
        assert_eq!(advisory.malware, MalwareCoverage::Covered);
    }

    #[test]
    fn aliases_cannot_silently_merge_contradictory_packages() {
        let first = object();
        let mut second = object();
        second.key.native.id = "OSV-TEST-2".to_owned();
        second.aliases = Box::new([Alias {
            value: "CVE-TEST-1".to_owned(),
            source: AdvisorySource::Osv,
        }]);
        second.affected[0].package.name = "other".to_owned();
        let mut graph = AliasGraph::default();
        graph.admit(&first).expect("first claim");
        assert!(matches!(
            graph.admit(&second),
            Err(AliasGraphError::PackageConflict { .. })
        ));
    }

    #[test]
    fn one_source_can_publish_a_new_revision_without_alias_claim_conflict() {
        let first = object();
        let mut revised = first.clone();
        revised.modified = Some("2026-03-01T00:00:00Z".to_owned());
        revised.withdrawn = Some("2026-03-02T00:00:00Z".to_owned());
        let mut graph = AliasGraph::default();
        graph.admit(&first).expect("first revision");
        assert_eq!(
            graph.admit(&revised).expect("same source revision"),
            CanonicalAdvisoryId("OSV-TEST-1".to_owned())
        );
    }

    #[test]
    fn normalization_is_ecosystem_specific_and_unsupported_is_typed() {
        assert_eq!(
            normalize_package("PyPI", "Requests_Pkg")
                .expect("pypi")
                .name,
            "requests-pkg"
        );
        assert_eq!(
            normalize_package("npm", "@Scope/Package")
                .expect("npm")
                .name,
            "@scope/package"
        );
        assert_eq!(
            normalize_package("maven", "org.example:demo")
                .expect("maven")
                .name,
            "org.example:demo"
        );
        assert!(matches!(
            normalize_package("unknown", "demo"),
            Err(PackageNormalizationError::UnsupportedEcosystem(_))
        ));
        assert!(matches!(
            normalize_version(VersionSyntax::Unsupported, "1.0"),
            Err(VersionCompareError::Unsupported(_))
        ));
        let alpha = normalize_version(VersionSyntax::Pep440, "1.0a1").expect("pep alpha");
        let beta = normalize_version(VersionSyntax::Pep440, "1.0b1").expect("pep beta");
        let release = normalize_version(VersionSyntax::Pep440, "1.0").expect("pep release");
        let post = normalize_version(VersionSyntax::Pep440, "1.0.post1").expect("pep post");
        assert!(alpha < beta && beta < release && release < post);
    }

    #[test]
    fn journal_advances_empty_and_304_checkpoints_and_defers_snapshot_removals() {
        let advisory = object();
        let mut journal = AdvisoryJournal::new();
        let freshness = FeedFreshness {
            etag: Some("x".to_owned()),
            last_modified: None,
            observed_at: 1,
            not_modified: false,
        };
        let checkpoint = journal
            .apply(AdvisorySync {
                mode: SyncMode::Snapshot,
                complete: false,
                entries: vec![AdvisoryDelta::Upsert(advisory.clone())],
                freshness: freshness.clone(),
            })
            .expect("page");
        assert_eq!(checkpoint.sequence, 1);
        assert!(
            journal
                .get(&CanonicalAdvisoryId("OSV-TEST-1".to_owned()))
                .is_some()
        );
        let checkpoint = journal
            .apply(AdvisorySync {
                mode: SyncMode::Snapshot,
                complete: true,
                entries: Vec::new(),
                freshness: FeedFreshness {
                    not_modified: true,
                    ..freshness.clone()
                },
            })
            .expect("304");
        assert_eq!(checkpoint.sequence, 2);
        assert!(
            journal
                .get(&CanonicalAdvisoryId("OSV-TEST-1".to_owned()))
                .is_some()
        );
        journal
            .apply(AdvisorySync {
                mode: SyncMode::Snapshot,
                complete: true,
                entries: Vec::new(),
                freshness,
            })
            .expect("complete snapshot");
        assert!(
            journal
                .get(&CanonicalAdvisoryId("OSV-TEST-1".to_owned()))
                .is_none()
        );
    }

    #[test]
    fn gate_separates_yank_warning_from_vulnerability_block() {
        let observation = AdvisoryObservation {
            advisories: Box::new([object()]),
            coverage: AdvisoryCoverage::Complete,
            freshness: FreshnessState::Fresh,
            offline: false,
            yanked: true,
            unlisted: false,
        };
        let gate = AcquisitionGate {
            offline: OfflinePolicy::Warn,
        };
        assert!(matches!(
            gate.decide(&observation),
            AcquisitionDecision::Deny(_)
        ));
        let clean = AdvisoryObservation {
            advisories: Box::new([]),
            coverage: observation.coverage,
            freshness: observation.freshness,
            offline: observation.offline,
            yanked: true,
            unlisted: observation.unlisted,
        };
        assert!(matches!(gate.decide(&clean), AcquisitionDecision::Warn(_)));
        let override_evidence = OverrideEvidence {
            actor: "ci".to_owned(),
            reason: "pinned emergency patch".to_owned(),
            policy_version: 1,
            expires_at: None,
        };
        assert!(matches!(
            gate.decide_with_override(&observation, override_evidence, 1),
            AcquisitionDecision::Warn(_)
        ));
    }

    #[test]
    fn withdrawal_delta_is_auditable_without_reactivating_a_match() {
        let advisory = object();
        let key = advisory.key.canonical.clone();
        let mut journal = AdvisoryJournal::new();
        let freshness = FeedFreshness {
            etag: Some("withdrawal-1".to_owned()),
            last_modified: None,
            observed_at: 10,
            not_modified: false,
        };
        journal
            .apply(AdvisorySync {
                mode: SyncMode::Delta,
                complete: true,
                entries: vec![AdvisoryDelta::Upsert(advisory)],
                freshness: freshness.clone(),
            })
            .expect("upsert");
        journal
            .apply(AdvisorySync {
                mode: SyncMode::Delta,
                complete: true,
                entries: vec![AdvisoryDelta::Withdraw {
                    key: key.clone(),
                    withdrawn: "2026-02-01T00:00:00Z".to_owned(),
                    evidence: Evidence {
                        snapshot: Some("withdrawal-1".to_owned()),
                        observed_at: 11,
                        verification: EvidenceKind::AuthenticatedDigest,
                    },
                }],
                freshness,
            })
            .expect("withdrawal");
        assert_eq!(journal.withdrawal_history(&key).len(), 1);
        assert_eq!(
            journal
                .get(&key)
                .and_then(|advisory| advisory.withdrawn.as_deref()),
            Some("2026-02-01T00:00:00Z")
        );
        let observation = AdvisoryObservation {
            advisories: Box::new([journal.get(&key).expect("active advisory").clone()]),
            coverage: AdvisoryCoverage::Complete,
            freshness: FreshnessState::Fresh,
            offline: false,
            yanked: false,
            unlisted: false,
        };
        assert!(matches!(
            (AcquisitionGate {
                offline: OfflinePolicy::Warn
            })
            .decide(&observation),
            AcquisitionDecision::Warn(_)
        ));
        let encoded = serde_json::to_vec(&journal).expect("serialize journal");
        let restored: AdvisoryJournal = serde_json::from_slice(&encoded).expect("restore journal");
        assert_eq!(restored, journal);
    }

    #[test]
    fn fail_closed_covers_offline_stale_and_unknown_frontiers() {
        let gate = AcquisitionGate {
            offline: OfflinePolicy::FailClosed,
        };
        let observation = AdvisoryObservation {
            advisories: Box::new([]),
            coverage: AdvisoryCoverage::Unknown,
            freshness: FreshnessState::Unknown,
            offline: true,
            yanked: false,
            unlisted: false,
        };
        assert!(matches!(
            gate.decide(&observation),
            AcquisitionDecision::Deny(_)
        ));
        let cached = AdvisoryObservation {
            advisories: observation.advisories.clone(),
            freshness: FreshnessState::Stale,
            offline: true,
            coverage: AdvisoryCoverage::Complete,
            yanked: observation.yanked,
            unlisted: observation.unlisted,
        };
        assert!(matches!(
            (AcquisitionGate {
                offline: OfflinePolicy::AllowCached
            })
            .decide(&cached),
            AcquisitionDecision::Warn(_)
        ));
        assert!(matches!(
            (AcquisitionGate {
                offline: OfflinePolicy::Warn
            })
            .decide(&observation),
            AcquisitionDecision::Warn(_)
        ));
    }

    #[test]
    fn product_dto_round_trips_source_facts_without_claiming_cleanliness() {
        let advisory = object();
        let dto = AdvisorySurfaceDto::from_advisory(
            &advisory,
            AdvisoryCoverage::Partial,
            FreshnessState::NotModified,
        );
        let encoded = serde_json::to_vec(&dto).expect("serialize DTO");
        let decoded: AdvisorySurfaceDto = serde_json::from_slice(&encoded).expect("decode DTO");
        assert_eq!(decoded, dto);
        assert_eq!(decoded.coverage, AdvisoryCoverage::Partial);
        assert_eq!(decoded.freshness, FreshnessState::NotModified);
        assert!(decoded.statuses.contains(&AdvisoryStatus::Vulnerable));
    }
}
