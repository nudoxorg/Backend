//! Exact coverage vectors produced by the semantic publication relation.
//!
//! Every assertion compares whole `Coverage` values, including lane and
//! reason. Counting entries would pass for a vector that reports the wrong
//! shape entirely, which is exactly the defect these tests exist to pin.

use super::{ActivatedProfiles, SemanticDeployment, SemanticTally};
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationClaim,
    SemanticPublicationCoverage, SemanticUnavailableReason,
};
use backend_engine::publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};
use backend_engine::publication::manifest::{CompilationManifestFacts, CompilationManifestFormat};
use backend_engine::{Lane, Reason, ViewCoverage};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use backend_store::hydration::VerifiedGenerationFacts;
use backend_version::{
    ContentId, DependencySetDomain, GenerationId, IrManifestDomain, IrManifestEncoding,
};

/// Builds one selected key for a version-pinned Cargo coordinate.
fn selected_key(name: &str) -> ProductSemanticPublicationKey {
    let label = format!("pkg:cargo/{name}@1.0.0");
    let package = backend_engine::PackageReference::parse(label.clone()).expect("package");
    let coordinate = backend_semantic::vocabulary::PackageUrl::parse(label).expect("coordinate");
    ProductSemanticPublicationKey::new(
        package,
        coordinate,
        LanguageProfile::Rust(RustEdition::Rust2024),
    )
    .expect("selected semantic key")
}

/// Builds a publication claim bound to one deterministic generation seed.
fn claim(seed: &[u8]) -> SemanticPublicationClaim {
    let generation = VerifiedGenerationFacts {
        pinned_root: GenerationId::from_canonical_bytes(seed),
        dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"coverage-dependencies"),
    };
    let manifest =
        backend_version::ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
            b"coverage-manifest",
        );
    let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
    let binding =
        CompilationBindingView::write_into(generation, manifest, &mut binding_bytes).expect("bind");
    SemanticPublicationClaim::admit(
        CompilationManifestFacts {
            identity: manifest,
            format: CompilationManifestFormat::SemanticV2,
            fragment_count: 1,
            byte_length: 1,
        },
        *binding,
    )
    .expect("publication claim")
}

/// A published record whose authority observed its complete project scope.
fn published(seed: &[u8]) -> ProductSemanticPublicationRecord {
    ProductSemanticPublicationRecord::Published {
        coverage: SemanticPublicationCoverage::Complete,
        claim: claim(seed),
    }
}

/// Folds an owned row set into a tally, activating the named keys.
fn tally(
    rows: &[(
        ProductSemanticPublicationKey,
        ProductSemanticPublicationRecord,
    )],
    activated: &ActivatedProfiles,
) -> SemanticTally {
    let mut tally = SemanticTally::default();
    for (key, record) in rows {
        tally.observe(key, record, activated).expect("observe row");
    }
    tally
}

/// Marks every supplied key as activated in process.
fn activated(keys: &[&ProductSemanticPublicationKey]) -> ActivatedProfiles {
    keys.iter()
        .map(|key| (key.package_key(), key.profile()))
        .collect()
}

#[test]
fn terminal_rows_report_an_unconfigured_lane_and_never_a_fraction() {
    let rows: Vec<_> = ["alpha", "beta", "gamma"]
        .into_iter()
        .map(|name| {
            (
                selected_key(name),
                ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Toolchain),
            )
        })
        .collect();
    let coverage = tally(&rows, &ActivatedProfiles::new()).coverage(SemanticDeployment::Configured);
    assert_eq!(
        coverage,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        "terminal semantic rows were not reported as an unavailable lane"
    );
    assert!(
        !coverage
            .iter()
            .any(|entry| matches!(entry, ViewCoverage::Partial { .. })),
        "a terminal semantic lane was rendered as a fraction that can never advance"
    );
}

#[test]
fn rejected_declared_scope_outranks_missing_project_authority() {
    let rows = vec![
        (
            selected_key("rejected"),
            ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Rejected),
        ),
        (
            selected_key("authority"),
            ProductSemanticPublicationRecord::Unavailable(
                SemanticUnavailableReason::ProjectAuthority,
            ),
        ),
    ];
    let forward = tally(&rows, &ActivatedProfiles::new()).coverage(SemanticDeployment::Configured);
    let mut reversed = rows;
    reversed.reverse();
    let backward =
        tally(&reversed, &ActivatedProfiles::new()).coverage(SemanticDeployment::Configured);
    assert_eq!(
        forward,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Incomplete,
            },
        ],
        "a missing authority masked a rejected declared scope"
    );
    assert_eq!(
        forward, backward,
        "terminal precedence depended on row order"
    );
}

#[test]
fn published_rows_still_report_their_exact_in_flight_fraction() {
    let done = selected_key("published-complete");
    let waiting = selected_key("published-pending");
    let rows = vec![
        (done.clone(), published(b"coverage-complete")),
        (waiting, published(b"coverage-pending")),
    ];
    assert_eq!(
        tally(&rows, &activated(&[&done])).coverage(SemanticDeployment::Configured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 1,
                total: 2,
            },
        ],
        "in-flight published rows lost their exact fraction"
    );
}

#[test]
fn terminal_rows_do_not_inflate_an_in_flight_denominator() {
    let done = selected_key("published-complete");
    let waiting = selected_key("published-pending");
    let rows = vec![
        (done.clone(), published(b"coverage-complete")),
        (waiting, published(b"coverage-pending")),
        (
            selected_key("no-toolchain"),
            ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Toolchain),
        ),
        (
            selected_key("no-authority"),
            ProductSemanticPublicationRecord::Unavailable(
                SemanticUnavailableReason::ProjectAuthority,
            ),
        ),
    ];
    assert_eq!(
        tally(&rows, &activated(&[&done])).coverage(SemanticDeployment::Configured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 1,
                total: 2,
            },
        ],
        "terminal rows were counted into the in-flight denominator"
    );
}

#[test]
fn a_workspace_finishing_its_publications_transitions_from_partial_to_complete() {
    let first = selected_key("transition-first");
    let second = selected_key("transition-second");
    let rows = vec![
        (first.clone(), published(b"coverage-transition-first")),
        (second.clone(), published(b"coverage-transition-second")),
    ];
    let before = tally(&rows, &activated(&[&first])).coverage(SemanticDeployment::Configured);
    let after =
        tally(&rows, &activated(&[&first, &second])).coverage(SemanticDeployment::Configured);
    assert_eq!(
        before,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 1,
                total: 2,
            },
        ],
        "the in-flight half of the transition lost its exact fraction"
    );
    assert_eq!(
        after,
        vec![ViewCoverage::Complete],
        "a fully activated workspace did not settle on complete coverage"
    );
}

#[test]
fn an_empty_semantic_relation_reports_only_the_source_lane() {
    assert_eq!(
        SemanticTally::default().coverage(SemanticDeployment::Configured),
        vec![ViewCoverage::Complete],
        "an empty semantic relation qualified the semantic lane"
    );
}

#[test]
fn immutable_generation_rows_are_never_counted() {
    let key = selected_key("generation-history");
    let record = published(b"coverage-generation");
    let ProductSemanticPublicationRecord::Published { claim, .. } = &record else {
        panic!("published fixture changed shape");
    };
    let history = key.for_generation(claim.binding().identity);
    assert_eq!(
        tally(&[(history, record.clone())], &ActivatedProfiles::new())
            .coverage(SemanticDeployment::Configured),
        vec![ViewCoverage::Complete],
        "an immutable generation row was projected as selected coverage"
    );
}

#[test]
fn an_unconfigured_deployment_reports_the_lane_once_and_terminally() {
    let done = selected_key("configured-complete");
    let rows = vec![(done.clone(), published(b"coverage-deployment"))];
    let coverage = tally(&rows, &activated(&[&done])).coverage(SemanticDeployment::Unconfigured);
    assert_eq!(
        coverage,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        "an unconfigured embedding lane was reported as complete"
    );
    assert_eq!(
        super::reconcile_semantic_lane(&coverage, SemanticDeployment::Unconfigured),
        coverage,
        "reconciliation double-emitted the semantic lane"
    );
}

#[test]
fn reconciliation_adds_the_deployment_terminal_only_when_the_lane_is_silent() {
    let complete = vec![ViewCoverage::Complete];
    assert_eq!(
        super::reconcile_semantic_lane(&complete, SemanticDeployment::Unconfigured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        "an unconfigured embedding lane was dropped from a complete reply"
    );
    assert_eq!(
        super::reconcile_semantic_lane(&complete, SemanticDeployment::Configured),
        complete,
        "a configured deployment invented a semantic terminal"
    );
    let in_flight = vec![
        ViewCoverage::Complete,
        ViewCoverage::Partial {
            lane: Lane::Semantic,
            completed: 1,
            total: 2,
        },
    ];
    assert_eq!(
        super::reconcile_semantic_lane(&in_flight, SemanticDeployment::Unconfigured),
        in_flight,
        "reconciliation replaced live in-flight work with a terminal reason"
    );
}

#[test]
fn native_refusal_is_not_masked_by_optional_embeddings_once_nothing_else_is_moving() {
    for reason in [
        SemanticUnavailableReason::Rejected,
        SemanticUnavailableReason::Cancelled,
    ] {
        let refused = selected_key("native-refused");
        let pending = selected_key("native-pending");
        let mut observed = SemanticTally::default();
        observed
            .observe_current(
                &refused,
                None,
                Some(
                    backend_engine::builtin::ProductSemanticCaptureOutcome::Unavailable { reason },
                ),
                &ActivatedProfiles::new(),
            )
            .unwrap();
        // A sibling profile that is still pending is work that is genuinely
        // moving. The lane reads as indexing until it settles, and the refusal
        // is reported the moment nothing else is in flight. The project row
        // applies the same precedence (see `merge_package_state`).
        let mut moving = observed;
        moving
            .observe_current(
                &pending,
                None,
                Some(
                    backend_engine::builtin::ProductSemanticCaptureOutcome::Pending { prior: None },
                ),
                &ActivatedProfiles::new(),
            )
            .unwrap();
        for deployment in [
            SemanticDeployment::Configured,
            SemanticDeployment::Unconfigured,
        ] {
            assert_eq!(
                moving.coverage(deployment),
                vec![
                    ViewCoverage::Complete,
                    ViewCoverage::Partial {
                        lane: Lane::Semantic,
                        completed: 0,
                        total: 1
                    }
                ]
            );
        }
        let expected = if reason == SemanticUnavailableReason::Cancelled {
            Reason::Cancelled
        } else {
            Reason::Incomplete
        };
        for deployment in [
            SemanticDeployment::Configured,
            SemanticDeployment::Unconfigured,
        ] {
            assert_eq!(
                observed.coverage(deployment),
                vec![
                    ViewCoverage::Complete,
                    ViewCoverage::Unavailable {
                        lane: Lane::Semantic,
                        reason: expected
                    }
                ]
            );
        }
    }
}

#[test]
fn retained_complete_generation_does_not_certify_a_new_pending_or_failed_capture() {
    use backend_engine::builtin::{ProductSemanticCaptureOutcome, SemanticPublicationVersion};
    let key = selected_key("retained");
    let selected = published(b"retained-complete");
    let prior = SemanticPublicationVersion::new(
        SemanticPublicationCoverage::Complete,
        claim(b"retained-complete"),
    );
    let active = activated(&[&key]);
    let mut pending = SemanticTally::default();
    pending
        .observe_current(
            &key,
            Some(&selected),
            Some(ProductSemanticCaptureOutcome::Pending { prior: Some(prior) }),
            &active,
        )
        .unwrap();
    assert_eq!(
        pending.coverage(SemanticDeployment::Configured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 0,
                total: 1
            }
        ]
    );
    for reason in [
        SemanticUnavailableReason::Rejected,
        SemanticUnavailableReason::Toolchain,
        SemanticUnavailableReason::ProjectAuthority,
    ] {
        let mut failed = SemanticTally::default();
        failed
            .observe_current(
                &key,
                Some(&selected),
                Some(ProductSemanticCaptureOutcome::Failed { prior, reason }),
                &active,
            )
            .unwrap();
        assert_eq!(
            failed.coverage(SemanticDeployment::Unconfigured),
            vec![
                ViewCoverage::Complete,
                ViewCoverage::Unavailable {
                    lane: Lane::Semantic,
                    reason: Reason::Incomplete
                }
            ]
        );
    }
    assert_eq!(
        selected,
        published(b"retained-complete"),
        "qualifying freshness must not discard retained evidence"
    );
}

#[test]
fn published_capture_requires_exact_selected_claim_and_coverage() {
    use backend_engine::builtin::ProductSemanticCaptureOutcome;
    let key = selected_key("correlated");
    let selected = published(b"selected");
    let mut observed = SemanticTally::default();
    assert!(
        observed
            .observe_current(
                &key,
                Some(&selected),
                Some(ProductSemanticCaptureOutcome::Published {
                    coverage: SemanticPublicationCoverage::Complete,
                    claim: claim(b"foreign")
                }),
                &activated(&[&key])
            )
            .is_err()
    );
    assert!(
        observed
            .observe_current(
                &key,
                None,
                Some(ProductSemanticCaptureOutcome::Published {
                    coverage: SemanticPublicationCoverage::Complete,
                    claim: claim(b"selected")
                }),
                &activated(&[&key])
            )
            .is_err()
    );
    assert_eq!(
        observed,
        SemanticTally::default(),
        "refused correlation changed the tally"
    );
    observed
        .observe_current(
            &key,
            Some(&selected),
            Some(ProductSemanticCaptureOutcome::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim(b"selected"),
            }),
            &activated(&[&key]),
        )
        .unwrap();
    assert_eq!(
        observed.coverage(SemanticDeployment::Configured),
        vec![ViewCoverage::Complete]
    );
}

#[test]
fn published_partial_scope_is_settled_usable_and_never_a_failure() {
    use backend_engine::builtin::{PartialSemanticCoverage, ProductSemanticCaptureOutcome};
    use std::num::NonZeroU32;
    let key = selected_key("partial");
    let coverage = SemanticPublicationCoverage::Partial(
        PartialSemanticCoverage::new(NonZeroU32::new(1).unwrap(), NonZeroU32::new(2).unwrap())
            .unwrap(),
    );
    let record = ProductSemanticPublicationRecord::Published {
        coverage,
        claim: claim(b"partial"),
    };
    let mut observed = SemanticTally::default();
    observed
        .observe_current(
            &key,
            Some(&record),
            Some(ProductSemanticCaptureOutcome::Published {
                coverage,
                claim: claim(b"partial"),
            }),
            &activated(&[&key]),
        )
        .unwrap();
    // A publication whose authority checked only part of the declared scope
    // is readable. It is reported as a settled gap, not as an unavailable
    // lane, and never as a fraction that tells the reader to wait.
    for deployment in [
        SemanticDeployment::Configured,
        SemanticDeployment::Unconfigured,
    ] {
        assert_eq!(
            observed.coverage(deployment),
            vec![
                ViewCoverage::Complete,
                ViewCoverage::Gapped {
                    lane: Lane::Semantic
                }
            ]
        );
    }
    assert_eq!(
        super::capture_row_state(ProductSemanticCaptureOutcome::Published {
            coverage,
            claim: claim(b"partial")
        }),
        backend_engine::RowState::Ready,
        "a published partial scope keeps the project usable"
    );
    assert_eq!(
        super::publication_row_state(&record),
        backend_engine::RowState::Ready
    );
}

#[test]
fn a_refused_profile_beside_a_published_one_is_partial_and_only_total_refusal_fails() {
    use backend_engine::RowState;
    use backend_engine::builtin::ProductSemanticCaptureOutcome;
    let usable = selected_key("usable");
    let refused = selected_key("refused");
    let mut mixed = SemanticTally::default();
    mixed
        .observe_current(
            &usable,
            Some(&published(b"usable")),
            Some(ProductSemanticCaptureOutcome::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim(b"usable"),
            }),
            &activated(&[&usable]),
        )
        .unwrap();
    mixed
        .observe_current(
            &refused,
            None,
            Some(ProductSemanticCaptureOutcome::Unavailable {
                reason: SemanticUnavailableReason::Rejected,
            }),
            &ActivatedProfiles::new(),
        )
        .unwrap();
    assert_eq!(
        mixed.coverage(SemanticDeployment::Configured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Gapped {
                lane: Lane::Semantic
            }
        ],
        "a profile refused while another published is partial, not failed"
    );
    let mut total = SemanticTally::default();
    for key in [&usable, &refused] {
        total
            .observe_current(
                key,
                None,
                Some(ProductSemanticCaptureOutcome::Unavailable {
                    reason: SemanticUnavailableReason::Rejected,
                }),
                &ActivatedProfiles::new(),
            )
            .unwrap();
    }
    assert_eq!(
        total.coverage(SemanticDeployment::Configured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Incomplete
            }
        ],
        "only a total refusal is a failed lane"
    );
    // The project row applies the same precedence, in any order.
    let states = [RowState::Ready, RowState::Loading, RowState::Failed];
    for first in states {
        for second in states {
            let merged = super::merge_package_state(first, second);
            assert_eq!(merged, super::merge_package_state(second, first));
            let expected = if first == RowState::Loading || second == RowState::Loading {
                RowState::Loading
            } else if first == RowState::Ready || second == RowState::Ready {
                RowState::Ready
            } else {
                RowState::Failed
            };
            assert_eq!(merged, expected, "{first:?} then {second:?}");
        }
    }
}

#[test]
fn current_capture_scope_ignores_history_and_preserves_optional_embedding_controls() {
    use backend_engine::builtin::ProductSemanticCaptureOutcome;
    let key = selected_key("scope-controls");
    let selected = published(b"scope-controls");
    let history = key.for_generation(claim(b"older-history").binding().identity);
    let mut observed = SemanticTally::default();
    observed
        .observe_current(
            &history,
            None,
            Some(ProductSemanticCaptureOutcome::Unavailable {
                reason: SemanticUnavailableReason::Rejected,
            }),
            &ActivatedProfiles::new(),
        )
        .unwrap();
    assert_eq!(
        observed,
        SemanticTally::default(),
        "immutable history widened declared scope"
    );
    observed
        .observe_current(
            &key,
            Some(&selected),
            Some(ProductSemanticCaptureOutcome::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim(b"scope-controls"),
            }),
            &activated(&[&key]),
        )
        .unwrap();
    assert_eq!(
        observed.coverage(SemanticDeployment::Configured),
        vec![ViewCoverage::Complete]
    );
    assert_eq!(
        observed.coverage(SemanticDeployment::Unconfigured),
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured
            }
        ]
    );
    let mut pending = SemanticTally::default();
    pending
        .observe_current(
            &key,
            Some(&selected),
            Some(ProductSemanticCaptureOutcome::Pending { prior: None }),
            &activated(&[&key]),
        )
        .unwrap();
    for deployment in [
        SemanticDeployment::Configured,
        SemanticDeployment::Unconfigured,
    ] {
        assert_eq!(
            pending.coverage(deployment),
            vec![
                ViewCoverage::Complete,
                ViewCoverage::Partial {
                    lane: Lane::Semantic,
                    completed: 0,
                    total: 1
                }
            ]
        );
    }
}

#[test]
fn legacy_package_scope_retains_native_refusal_and_optional_configuration() {
    use backend_engine::RowState;
    for reason in [
        SemanticUnavailableReason::Rejected,
        SemanticUnavailableReason::Cancelled,
    ] {
        assert_eq!(
            super::publication_row_state(&ProductSemanticPublicationRecord::Unavailable(reason)),
            RowState::Failed
        );
    }
    for reason in [
        SemanticUnavailableReason::Toolchain,
        SemanticUnavailableReason::ProjectAuthority,
    ] {
        assert_eq!(
            super::publication_row_state(&ProductSemanticPublicationRecord::Unavailable(reason)),
            RowState::Ready
        );
        assert_eq!(
            super::capture_row_state(
                backend_engine::builtin::ProductSemanticCaptureOutcome::Unavailable { reason }
            ),
            RowState::Failed,
            "actually requested native scope is unavailable"
        );
    }
    assert_eq!(
        super::publication_row_state(&published(b"legacy-complete")),
        RowState::Ready
    );
}

/// The lane line and the project row are two renderings of one set of capture
/// facts. For every combination of per-profile outcomes up to three profiles,
/// the pair the owner publishes must agree in meaning: a row is loading
/// exactly when the lane is a moving fraction, failed exactly when the lane is
/// unavailable for work that was asked for, and ready exactly when the lane is
/// complete or settled with gaps. A pair outside that table is a lie.
#[test]
fn lane_and_project_row_always_agree_for_every_profile_outcome_combination() {
    use backend_engine::RowState;
    use backend_engine::builtin::{
        PartialSemanticCoverage, ProductSemanticCaptureOutcome as Outcome, SemanticPublicationVersion,
    };
    use std::num::NonZeroU32;
    let prior = SemanticPublicationVersion::new(
        SemanticPublicationCoverage::Complete,
        claim(b"agreement-prior"),
    );
    let gaps = SemanticPublicationCoverage::Partial(
        PartialSemanticCoverage::new(NonZeroU32::new(1).unwrap(), NonZeroU32::new(2).unwrap())
            .unwrap(),
    );
    let outcomes = |seed: &[u8]| {
        vec![
            Outcome::Pending { prior: None },
            Outcome::Pending { prior: Some(prior) },
            Outcome::Failed {
                prior,
                reason: SemanticUnavailableReason::Rejected,
            },
            Outcome::Unavailable {
                reason: SemanticUnavailableReason::Rejected,
            },
            Outcome::Unavailable {
                reason: SemanticUnavailableReason::Cancelled,
            },
            Outcome::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim: claim(seed),
            },
            Outcome::Published {
                coverage: gaps,
                claim: claim(seed),
            },
        ]
    };
    let names = ["agree-a", "agree-b", "agree-c"];
    let mut checked = 0_usize;
    for count in 1..=3_usize {
        let choices = outcomes(b"agreement").len();
        for code in 0..choices.pow(u32::try_from(count).unwrap()) {
            let mut tally = SemanticTally::default();
            let mut row: Option<RowState> = None;
            let mut remainder = code;
            let mut picked = Vec::new();
            for name in &names[..count] {
                let outcome = outcomes(name.as_bytes())[remainder % choices];
                remainder /= choices;
                picked.push(outcome);
                let key = selected_key(name);
                let selected = match outcome {
                    Outcome::Published { coverage, claim } => {
                        Some(ProductSemanticPublicationRecord::Published { coverage, claim })
                    }
                    _ => None,
                };
                tally
                    .observe_current(&key, selected.as_ref(), Some(outcome), &activated(&[&key]))
                    .unwrap();
                let state = super::capture_row_state(outcome);
                row = Some(row.map_or(state, |prior| super::merge_package_state(prior, state)));
            }
            let lane = tally.coverage(SemanticDeployment::Configured);
            let lane = lane
                .iter()
                .copied()
                .find(|entry| super::is_semantic_lane(*entry));
            let row = row.unwrap();
            let agrees = match (row, lane) {
                (RowState::Loading, Some(ViewCoverage::Partial { .. })) => true,
                (RowState::Failed, Some(ViewCoverage::Unavailable { reason, .. })) => {
                    !reason.is_outside_declared_scope()
                }
                (RowState::Ready, None | Some(ViewCoverage::Gapped { .. })) => true,
                _ => false,
            };
            assert!(
                agrees,
                "row {row:?} and lane {lane:?} disagree for outcomes {picked:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 7 + 49 + 343);
}
