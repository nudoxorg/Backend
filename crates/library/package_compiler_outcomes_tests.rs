use super::*;
use crate::{IndexOperationSourceProfile, PackageCompilerFailure};

type TestResult = Result<(), String>;

fn checked<T, E: std::fmt::Debug>(result: Result<T, E>) -> Result<T, String> {
    result.map_err(|error| format!("{error:?}"))
}

fn fixture() -> (PackageReference, PackageCompilerOutcomes) {
    let package = PackageReference::parse("/compiler-outcomes").expect("local");
    let basis = PackageCompilerOutcomeBasis {
        package_key: crate::package_key(package.as_str()).to_bytes(),
        source_relation_root: [3; 32],
        source_version: [4; 32],
        capture_relation_root: [5; 32],
    };
    let profiles: Vec<SemanticLanguageProfile> = ["[1,0]", "[2,4]"]
        .into_iter()
        .map(|encoded| serde_json::from_str(encoded).expect("closed TS/Python profile"))
        .collect();
    let capture = IndexSourceCaptureSummary {
        cargo_target: None,
        producer_package: package.clone(),
        request_identity: [6; 32],
        commit_identity: [7; 32],
        workspace_root: [8; 32],
        workspace_sequence: 1,
        profiles: profiles
            .iter()
            .map(|profile| IndexOperationSourceProfile {
                profile: *profile,
                source_version: [4; 32],
                input_digest: [9; 32],
                observation_sequence: 1,
                source_count: 3,
                state: State::Unavailable {
                    reason: IndexOperationSemanticUnavailableReason::Rejected,
                },
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    let refusals = profiles
        .into_iter()
        .map(|profile| PackageCompilerProfileRefusal {
            profile,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            diagnostic: PackageCompilerDiagnostic::Retained {
                read: PackageCompilerOutcomeRead::Diagnostic(PackageCompilerOutcomeSelector {
                    basis: basis.identity(),
                    profile,
                }),
            },
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    (
        package,
        PackageCompilerOutcomes::Summary {
            coverage: None,
            basis,
            source_capture: capture,
            refusals,
        },
    )
}

fn failure(path: &str) -> PackageCompilerFailure {
    use crate::interface::{CompilerAttempt, CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::{BuildError, EntityId};
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};
    let attempt = CompilerAttempt {
        source: SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes"),
            byte_len: 12,
        },
        recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
    };
    PackageCompilerFailure::from_fragment_failure(
        path,
        attempt,
        &CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
            owner: EntityId::new(7),
            start: 18,
            end: 24,
        }),
    )
    .expect("bounded typed compiler failure")
}

#[test]
fn all_refused_discovery_then_each_exact_diagnostic_preserves_distinct_failures() {
    let (package, summary) = fixture();
    summary
        .admit_request(&package, None)
        .expect("all refused is valid discovery");
    let PackageCompilerOutcomes::Summary {
        basis,
        source_capture,
        refusals,
        ..
    } = &summary
    else {
        panic!("summary")
    };
    let failures = [failure("src/app.ts"), failure("src/app.py")];
    let mut read_back = Vec::new();
    for (refusal, failure) in refusals.iter().zip(failures.iter()) {
        let PackageCompilerDiagnostic::Retained {
            read: PackageCompilerOutcomeRead::Diagnostic(read),
        } = &refusal.diagnostic
        else {
            panic!("selector")
        };
        let read = *read;
        let full = PackageCompilerOutcomes::Diagnostic {
            basis: *basis,
            selector: read,
            source_capture: source_capture.clone(),
            refusal: IndexOperationProfileRefusal {
                profile: refusal.profile,
                reason: refusal.reason,
                compiler_failure: Some(failure.clone()),
            },
        };
        full.admit_request(
            &package,
            Some(&PackageCompilerOutcomeRead::Diagnostic(read)),
        )
        .expect("selected full native DTO");
        let encoded = serde_json::to_vec(&full).expect("complete serialization");
        let decoded: PackageCompilerOutcomes =
            serde_json::from_slice(&encoded).expect("strict typed roundtrip");
        assert_eq!(decoded, full);
        let PackageCompilerOutcomes::Diagnostic { refusal, .. } = decoded else {
            panic!("diagnostic")
        };
        read_back.push(refusal.compiler_failure.expect("retained failure"));
    }
    assert_eq!(read_back.as_slice(), &failures);
}

#[test]
fn discovery_refuses_missing_duplicate_wrong_profile_reason_and_source_basis() {
    let (package, original) = fixture();
    for mutation in 0..7 {
        let mut summary = original.clone();
        let PackageCompilerOutcomes::Summary {
            basis,
            source_capture,
            refusals,
            ..
        } = &mut summary
        else {
            panic!("summary")
        };
        let other_profile = refusals[1].profile;
        match mutation {
            0 => *refusals = refusals[..1].to_vec().into_boxed_slice(),
            1 => refusals[1] = refusals[0].clone(),
            2 => refusals[0].reason = IndexOperationSemanticUnavailableReason::Toolchain,
            3 => source_capture.profiles[0].source_version = [99; 32],
            4 => basis.package_key = [99; 32],
            5 => {
                if let PackageCompilerDiagnostic::Retained {
                    read: PackageCompilerOutcomeRead::Diagnostic(read),
                } = &mut refusals[0].diagnostic
                {
                    read.basis = [99; 32];
                }
            }
            _ => {
                if let PackageCompilerDiagnostic::Retained {
                    read: PackageCompilerOutcomeRead::Diagnostic(read),
                } = &mut refusals[0].diagnostic
                {
                    read.profile = other_profile;
                }
            }
        }
        assert!(
            summary.admit_request(&package, None).is_err(),
            "mutation {mutation}"
        );
    }
    assert!(
        original
            .admit_request(&PackageReference::parse("/other").expect("local"), None)
            .is_err()
    );
    let mut invented = original.clone();
    if let PackageCompilerOutcomes::Summary { refusals, .. } = &mut invented {
        refusals[0].diagnostic = PackageCompilerDiagnostic::Retained {
            read: PackageCompilerOutcomeRead::Current,
        };
    }
    assert!(
        invented.admit().is_err(),
        "a discovery request is never a diagnostic selector"
    );
    let mut over_limit = original.clone();
    if let PackageCompilerOutcomes::Summary { source_capture, .. } = &mut over_limit {
        let profiles: Vec<SemanticLanguageProfile> = (0..8)
            .flat_map(|family| (0..8).map(move |dialect| [family, dialect]))
            .filter_map(|bytes| serde_json::from_value(serde_json::json!(bytes)).ok())
            .filter(|profile: &SemanticLanguageProfile| profile.profile().is_ok())
            .take(17)
            .collect();
        assert_eq!(
            profiles.len(),
            17,
            "use distinct already-admitted canonical dialects"
        );
        let first = source_capture.profiles[0].clone();
        source_capture.profiles = profiles
            .into_iter()
            .map(|profile| IndexOperationSourceProfile {
                profile,
                ..first.clone()
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut at_limit = source_capture.clone();
        at_limit.profiles = at_limit.profiles[..16].to_vec().into_boxed_slice();
        at_limit
            .admit()
            .expect("same exact tuples and strict order fit at sixteen");
        assert!(
            source_capture.admit().is_err(),
            "only the count exceeds the bound"
        );
    }
    assert!(
        over_limit.admit().is_err(),
        "seventeen rows cannot claim a complete bounded capture; no product profile expansion"
    );
}

#[test]
fn pending_published_partial_and_missing_diagnostic_are_distinct_states() {
    let (package, mut summary) = fixture();
    let PackageCompilerOutcomes::Summary {
        source_capture,
        refusals,
        ..
    } = &mut summary
    else {
        panic!("summary")
    };
    source_capture.profiles[0].state = State::Published {
        generation: [10; 32],
        coverage: crate::IndexOperationSemanticCoverage::Complete,
    };
    *refusals = refusals[1..].to_vec().into_boxed_slice();
    refusals[0].diagnostic = PackageCompilerDiagnostic::NotRecorded;
    summary
        .admit_request(&package, None)
        .expect("partial plus absence is honest");
    let PackageCompilerOutcomes::Summary { source_capture, .. } = &mut summary else {
        panic!("summary")
    };
    source_capture.profiles[0].state = State::Pending { prior: None };
    summary
        .admit_request(&package, None)
        .expect("pending never turned into a refusal");
}

#[test]
fn full_read_rejects_undiscovered_selector_and_registry_selector_before_dispatch() {
    let (package, summary) = fixture();
    let PackageCompilerOutcomes::Summary {
        basis,
        source_capture,
        refusals,
        ..
    } = summary
    else {
        panic!("summary")
    };
    let selector = PackageCompilerOutcomeSelector {
        basis: basis.identity(),
        profile: refusals[0].profile,
    };
    let diagnostic = PackageCompilerOutcomes::Diagnostic {
        basis,
        selector,
        source_capture,
        refusal: IndexOperationProfileRefusal {
            profile: selector.profile,
            reason: refusals[0].reason,
            compiler_failure: Some(failure("src/app.ts")),
        },
    };
    let mut wrong = selector;
    wrong.basis = [99; 32];
    assert!(
        diagnostic
            .admit_request(
                &package,
                Some(&PackageCompilerOutcomeRead::Diagnostic(wrong))
            )
            .is_err()
    );
    assert!(diagnostic.admit_request(&package, None).is_err());
    let mut absent = diagnostic.clone();
    if let PackageCompilerOutcomes::Diagnostic { refusal, .. } = &mut absent {
        refusal.compiler_failure = None;
    }
    assert!(
        absent
            .admit_request(
                &package,
                Some(&PackageCompilerOutcomeRead::Diagnostic(selector))
            )
            .is_err(),
        "a selector cannot invent a recorded diagnostic"
    );
    assert!(
        crate::SurfaceCommand::PackageProfile {
            package: PackageReference::parse("pkg:pypi/example@1").expect("registry"),
            compiler_outcome: Some(PackageCompilerOutcomeRead::Diagnostic(selector))
        }
        .admit()
        .is_err()
    );
    let stale = PackageCompilerOutcomes::Stale {
        package: package.clone(),
    };
    stale
        .admit_request(
            &package,
            Some(&PackageCompilerOutcomeRead::Diagnostic(selector)),
        )
        .expect("typed stale, not a substitute diagnostic");
    assert!(stale.admit_request(&package, None).is_err());
    assert!(
        stale
            .admit_request(&package, Some(&PackageCompilerOutcomeRead::Current))
            .is_err(),
        "current discovery carries no basis that can be stale"
    );
    for reason in [
        PackageCompilerOutcomesUnavailable::ProjectNotSelected,
        PackageCompilerOutcomesUnavailable::CaptureNotRecorded,
        PackageCompilerOutcomesUnavailable::CaptureNotCurrent,
        PackageCompilerOutcomesUnavailable::ProfileNotRefused,
        PackageCompilerOutcomesUnavailable::DiagnosticNotRecorded,
        PackageCompilerOutcomesUnavailable::CaptureIncomplete,
    ] {
        let unavailable = PackageCompilerOutcomes::Unavailable {
            package: package.clone(),
            reason,
        };
        let current_allowed = !matches!(
            reason,
            PackageCompilerOutcomesUnavailable::ProfileNotRefused
                | PackageCompilerOutcomesUnavailable::DiagnosticNotRecorded
        );
        let diagnostic_allowed = matches!(
            reason,
            PackageCompilerOutcomesUnavailable::ProfileNotRefused
                | PackageCompilerOutcomesUnavailable::DiagnosticNotRecorded
                | PackageCompilerOutcomesUnavailable::CaptureIncomplete
        );
        assert_eq!(
            unavailable
                .admit_request(&package, Some(&PackageCompilerOutcomeRead::Current))
                .is_ok(),
            current_allowed,
            "{reason:?}"
        );
        assert_eq!(
            unavailable
                .admit_request(
                    &package,
                    Some(&PackageCompilerOutcomeRead::Diagnostic(selector))
                )
                .is_ok(),
            diagnostic_allowed,
            "{reason:?}"
        );
    }
}

#[test]
fn package_profile_correlates_explicit_capture_mode_and_preserves_registry_absence() {
    use crate::{SurfaceCommand as C, SurfaceReply as R, admit_surface_reply};
    let (package, summary) = fixture();
    let current = C::PackageProfile {
        package: package.clone(),
        compiler_outcome: Some(PackageCompilerOutcomeRead::Current),
    };
    let default = C::PackageProfile {
        package,
        compiler_outcome: None,
    };
    let empty = R::PackageProfile {
        latest: None,
        versions: 0,
        candidate_authority: None,
        source_metadata: None,
        compiler_outcomes: None,
    };
    assert!(
        admit_surface_reply(&default, &empty).is_err(),
        "a local reply cannot omit capture availability"
    );
    let capture = R::PackageCompilerOutcomes(summary.clone());
    admit_surface_reply(&current, &capture).expect("explicit read family");
    let C::PackageProfile { package, .. } = &current else {
        panic!("request")
    };
    let stale = R::PackageCompilerOutcomes(PackageCompilerOutcomes::Stale {
        package: package.clone(),
    });
    assert!(
        admit_surface_reply(&current, &stale).is_err(),
        "basis-free discovery cannot claim a stale selector"
    );
    let diagnostic = C::PackageProfile {
        package: package.clone(),
        compiler_outcome: Some(PackageCompilerOutcomeRead::Diagnostic(
            PackageCompilerOutcomeSelector {
                basis: [99; 32],
                profile: SemanticLanguageProfile::from_name("python").expect("closed"),
            },
        )),
    };
    admit_surface_reply(&diagnostic, &stale)
        .expect("the exact old diagnostic selection can be stale");
    assert!(
        admit_surface_reply(&default, &capture).is_err(),
        "explicit reads cannot erase default packaging fields"
    );
    let with_metadata = R::PackageProfile {
        latest: None,
        versions: 0,
        candidate_authority: None,
        source_metadata: None,
        compiler_outcomes: Some(summary),
    };
    admit_surface_reply(&default, &with_metadata)
        .expect("default preserves registry and compiler projections");
    assert!(admit_surface_reply(&current, &with_metadata).is_err());
    let registry = C::PackageProfile {
        package: PackageReference::parse("pkg:pypi/example@1").expect("registry"),
        compiler_outcome: None,
    };
    admit_surface_reply(&registry, &empty).expect("registry-only semantics unchanged");
}

#[test]
fn coverage_pages_bind_generation_input_catalog_and_exact_continuation_without_refusal_substitution()
    -> TestResult
{
    use backend_semantic::ir::{SourceCoverageGapCause, SourceCoverageRow};
    let (package, summary) = fixture();
    let PackageCompilerOutcomes::Summary { basis, .. } = summary else {
        panic!("basis")
    };
    let first = PackageCompilerCoverageSelector {
        outcome: PackageCompilerOutcomeSelector {
            basis: basis.identity(),
            profile: SemanticLanguageProfile::from_name("typescript")
                .ok_or_else(|| "missing TypeScript profile fixture".to_owned())?,
        },
        catalog: [6; 32],
        generation: [7; 32],
        input_digest: [9; 32],
        offset: 0,
    };
    let source =
        *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            b"spec",
        )
        .as_ref();
    let page = PackageCompilerCoveragePage {
        generation: first.generation,
        input_digest: first.input_digest,
        source_count: 50,
        completed: 41,
        row_count: 10,
        rows: vec![SourceCoverageRow::Gap {
            path: "src/app.spec.ts".into(),
            source,
            byte_len: 4,
            cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
            diagnostic: None,
        }]
        .into_boxed_slice(),
        next: Some(PackageCompilerOutcomeRead::Coverage(
            PackageCompilerCoverageSelector { offset: 1, ..first },
        )),
    };
    let observed = PackageCompilerOutcomes::Coverage {
        package: package.clone(),
        basis,
        selector: first,
        page,
    };
    let request = PackageCompilerOutcomeRead::Coverage(first);
    checked(observed.admit_request(&package, Some(&request)))?;
    let mut batched = observed.clone();
    let PackageCompilerOutcomes::Coverage { page, .. } = &mut batched else {
        unreachable!()
    };
    let mut second = page.rows[0].clone();
    if let SourceCoverageRow::Gap { path, .. } = &mut second {
        *path = "src/second.spec.ts".to_owned();
    }
    page.rows = vec![page.rows[0].clone(), second].into_boxed_slice();
    page.next = Some(PackageCompilerOutcomeRead::Coverage(
        PackageCompilerCoverageSelector { offset: 2, ..first },
    ));
    checked(batched.admit_request(&package, Some(&request)))?;
    let PackageCompilerOutcomes::Coverage { page, .. } = &mut batched else {
        unreachable!()
    };
    page.next = Some(PackageCompilerOutcomeRead::Coverage(
        PackageCompilerCoverageSelector { offset: 1, ..first },
    ));
    assert!(batched.admit_request(&package, Some(&request)).is_err());
    let mut terminal = observed.clone();
    let PackageCompilerOutcomes::Coverage {
        selector: terminal_selector,
        page: terminal_page,
        ..
    } = &mut terminal
    else {
        unreachable!()
    };
    terminal_selector.offset = terminal_page.row_count;
    terminal_page.rows = Box::default();
    terminal_page.next = None;
    let terminal_request = PackageCompilerOutcomeRead::Coverage(*terminal_selector);
    checked(terminal.admit_request(&package, Some(&terminal_request)))?;
    assert!(observed.admit_request(&package, None).is_err());
    assert!(
        observed
            .admit_request(
                &package,
                Some(&PackageCompilerOutcomeRead::Diagnostic(first.outcome))
            )
            .is_err()
    );
    for change in 0..11 {
        let mut forged = observed.clone();
        let PackageCompilerOutcomes::Coverage { selector, page, .. } = &mut forged else {
            unreachable!()
        };
        match change {
            0 => page.generation[0] ^= 1,
            1 => page.input_digest[0] ^= 1,
            2 => page.next = None,
            3 => {
                let Some(PackageCompilerOutcomeRead::Coverage(next)) = &mut page.next else {
                    unreachable!()
                };
                next.offset = 0;
            }
            4 => {
                let Some(PackageCompilerOutcomeRead::Coverage(next)) = &mut page.next else {
                    unreachable!()
                };
                next.catalog[0] ^= 1;
            }
            5 => page.rows = Box::default(),
            6 => selector.offset = 11,
            7 => page.rows = vec![page.rows[0].clone(), page.rows[0].clone()].into_boxed_slice(),
            8 => page.completed = 51,
            9 => {
                page.source_count = 1;
                page.completed = 0;
                page.row_count = 16_640;
            }
            _ => {
                let SourceCoverageRow::Gap { cause, .. } = &mut page.rows[0] else {
                    unreachable!()
                };
                *cause = SourceCoverageGapCause::PythonSourceSyntaxUnavailable;
            }
        }
        assert!(
            forged.admit_request(&package, Some(&request)).is_err(),
            "forged page {change}"
        );
    }
    let mut tsx = observed.clone();
    let PackageCompilerOutcomes::Coverage { selector, page, .. } = &mut tsx else {
        unreachable!()
    };
    selector.outcome.profile = SemanticLanguageProfile::from_name("tsx")
        .ok_or_else(|| "missing TSX profile fixture".to_owned())?;
    if let Some(PackageCompilerOutcomeRead::Coverage(next)) = &mut page.next {
        next.outcome.profile = selector.outcome.profile;
    }
    let tsx_read = PackageCompilerOutcomeRead::Coverage(*selector);
    checked(tsx.admit_request(&package, Some(&tsx_read)))?;
    // A persisted observation is distinct from honest legacy absence, even when 41/50 is known.
    let unavailable = PackageCompilerOutcomes::Unavailable {
        package: package.clone(),
        reason: PackageCompilerOutcomesUnavailable::CoverageNotRecorded,
    };
    checked(unavailable.admit_request(&package, Some(&request)))?;
    let mut changed = first;
    changed.catalog[0] ^= 1;
    assert!(
        observed
            .admit_request(
                &package,
                Some(&PackageCompilerOutcomeRead::Coverage(changed))
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn coverage_summary_distinguishes_legacy_absence_from_complete_published_profile_partition()
    -> TestResult
{
    let (package, mut summary) = fixture();
    let PackageCompilerOutcomes::Summary {
        source_capture,
        refusals,
        coverage,
        ..
    } = &mut summary
    else {
        panic!("summary")
    };
    for profile in source_capture.profiles.iter_mut() {
        profile.state = State::Published {
            generation: [7; 32],
            coverage: crate::IndexOperationSemanticCoverage::Complete,
        };
    }
    *refusals = Box::default();
    let published = source_capture
        .profiles
        .iter()
        .map(|profile| PackageCompilerCoverage::LegacyNotRecorded {
            profile: profile.profile,
        })
        .collect::<Vec<_>>();
    *coverage = Some(published.clone().into_boxed_slice());
    checked(summary.admit_request(&package, None))?;
    for entries in [
        vec![],
        published[..1].to_vec(),
        vec![published[0].clone(), published[0].clone()],
    ] {
        let mut incomplete = summary.clone();
        let PackageCompilerOutcomes::Summary { coverage, .. } = &mut incomplete else {
            unreachable!()
        };
        *coverage = Some(entries.into_boxed_slice());
        assert!(incomplete.admit().is_err());
    }
    let mut old = checked(serde_json::to_value(&summary))?;
    old.as_object_mut()
        .ok_or_else(|| "serialized summary should be an object".to_owned())?
        .remove("coverage");
    let old: PackageCompilerOutcomes = checked(serde_json::from_value(old))?;
    checked(old.admit_request(&package, None))?;
    let PackageCompilerOutcomes::Summary { coverage, .. } = old else {
        panic!("legacy")
    };
    assert!(
        coverage.is_none(),
        "old wire absence remains distinguishable"
    );
    Ok(())
}

#[test]
fn coverage_stale_legacy_unpublished_and_oversized_results_pass_only_exact_surface_read_mode()
    -> TestResult
{
    let (package, summary) = fixture();
    let PackageCompilerOutcomes::Summary { basis, .. } = summary else {
        panic!("basis")
    };
    let selector = PackageCompilerCoverageSelector {
        outcome: PackageCompilerOutcomeSelector {
            basis: basis.identity(),
            profile: SemanticLanguageProfile::from_name("typescript")
                .ok_or_else(|| "missing TypeScript profile fixture".to_owned())?,
        },
        catalog: [4; 32],
        generation: [5; 32],
        input_digest: [9; 32],
        offset: 0,
    };
    let command = crate::SurfaceCommand::PackageProfile {
        package: package.clone(),
        compiler_outcome: Some(PackageCompilerOutcomeRead::Coverage(selector)),
    };
    let diagnostic = crate::SurfaceCommand::PackageProfile {
        package: package.clone(),
        compiler_outcome: Some(PackageCompilerOutcomeRead::Diagnostic(selector.outcome)),
    };
    let stale = crate::SurfaceReply::PackageCompilerOutcomes(PackageCompilerOutcomes::Stale {
        package: package.clone(),
    });
    checked(crate::admit_surface_reply(&command, &stale))?;
    for reason in [
        PackageCompilerOutcomesUnavailable::CoverageNotRecorded,
        PackageCompilerOutcomesUnavailable::CoverageNotRecordedCapacity,
        PackageCompilerOutcomesUnavailable::ProfileNotPublished,
        PackageCompilerOutcomesUnavailable::CoverageRowTooLarge,
    ] {
        let reply =
            crate::SurfaceReply::PackageCompilerOutcomes(PackageCompilerOutcomes::Unavailable {
                package: package.clone(),
                reason,
            });
        checked(crate::admit_surface_reply(&command, &reply))?;
        assert!(crate::admit_surface_reply(&diagnostic, &reply).is_err());
    }
    Ok(())
}

#[test]
fn coverage_capacity_summary_preserves_published_partial_and_never_advertises_a_cause_read()
    -> TestResult
{
    let (package, mut summary) = fixture();
    let PackageCompilerOutcomes::Summary {
        source_capture,
        refusals,
        coverage,
        ..
    } = &mut summary
    else {
        unreachable!()
    };
    let mut profiles = source_capture.profiles.to_vec();
    profiles.truncate(1);
    profiles[0].profile = SemanticLanguageProfile::from_name("typescript")
        .ok_or_else(|| "missing TypeScript profile fixture".to_owned())?;
    profiles[0].source_count = 20_001;
    profiles[0].state = State::Published {
        generation: [7; 32],
        coverage: crate::IndexOperationSemanticCoverage::Partial {
            completed: 1,
            total: 20_001,
        },
    };
    let profile = profiles[0].profile;
    source_capture.profiles = profiles.into_boxed_slice();
    *refusals = Box::default();
    *coverage = Some(
        vec![PackageCompilerCoverage::NotRecordedCapacity {
            profile,
            reason: backend_semantic::ir::SourceCoverageCapacityReason::RowCount,
        }]
        .into_boxed_slice(),
    );
    checked(summary.admit_request(&package, Some(&PackageCompilerOutcomeRead::Current)))?;
    let encoded = checked(serde_json::to_value(&summary))?;
    assert_eq!(encoded["coverage"][0]["state"], "not-recorded-capacity");
    assert!(encoded["coverage"][0].get("read").is_none());
    assert_eq!(
        encoded["source_capture"]["profiles"][0]["state"]["state"],
        "published"
    );
    let cold: PackageCompilerOutcomes = checked(serde_json::from_value(encoded))?;
    assert_eq!(cold, summary);
    checked(cold.admit_request(&package, None))?;
    Ok(())
}
