use super::*;
use crate::{IndexOperationSourceProfile, PackageCompilerFailure};

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
        source_capture.profiles = vec![source_capture.profiles[0].clone(); 17].into_boxed_slice();
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
