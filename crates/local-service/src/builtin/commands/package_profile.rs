//! Current PSC2 capture facts, with bounded discovery and exact diagnostic reads.
use super::super::{BuiltinModelError, BuiltinWorkspaceRelation};
use super::index_operation::IndexOperationJournal;
use backend_engine::WorkspaceSnapshot;
use backend_engine::builtin::{ProductSemanticPublicationKey, semantic_capture_relation};
use backend_library::{
    IndexOperationObservation, IndexOperationProfileRefusal,
    IndexOperationSemanticProfileState as State, IndexOperationSourceProfile,
    IndexSourceCaptureSummary, PackageCompilerDiagnostic, PackageCompilerOutcomeBasis,
    PackageCompilerOutcomeRead, PackageCompilerOutcomeSelector,
    PackageCompilerOutcomes as Outcomes, PackageCompilerOutcomesUnavailable as Unavailable,
    PackageCompilerProfileRefusal, PackageReference,
};

const MAX_PROFILES: usize = 16;

/// Registry metadata remains independent of compiler captures. Local discovery
/// reads only one package prefix of the selected closure (at most 16+1 rows).
pub(super) fn read(
    snapshot: &WorkspaceSnapshot,
    journal: &IndexOperationJournal,
    authority: &super::super::semantic_authority::SemanticAuthority,
    package: &PackageReference,
    read: Option<&PackageCompilerOutcomeRead>,
) -> Result<Option<Outcomes>, BuiltinModelError> {
    if !matches!(package, PackageReference::Local(_)) {
        return Ok(None);
    }
    let selector = read.and_then(PackageCompilerOutcomeRead::selector);
    let unavailable = |reason| {
        Some(
            if selector.is_some()
                && matches!(
                    reason,
                    Unavailable::ProjectNotSelected
                        | Unavailable::CaptureNotRecorded
                        | Unavailable::CaptureNotCurrent
                )
            {
                Outcomes::Stale {
                    package: package.clone(),
                }
            } else {
                Outcomes::Unavailable {
                    package: package.clone(),
                    reason,
                }
            },
        )
    };
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open package-profile source relation: {error}"))
        })?;
    let project_key = backend_library::package_key(package.as_str()).to_bytes();
    let project = source
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read package-profile Project: {error}")))?;
    let Some(project) = project.as_ref().and_then(|row| row.project_fields()) else {
        return Ok(unavailable(Unavailable::ProjectNotSelected));
    };
    if project.label != package.as_str() {
        return Ok(unavailable(Unavailable::ProjectNotSelected));
    }
    let Some(captures) = semantic_capture_relation(snapshot).map_err(|error| {
        BuiltinModelError(format!("open package-profile capture relation: {error}"))
    })?
    else {
        return Ok(unavailable(Unavailable::CaptureNotRecorded));
    };
    let basis = PackageCompilerOutcomeBasis {
        package_key: project_key,
        source_relation_root: *source.root().as_bytes(),
        source_version: project.source_version,
        capture_relation_root: *captures.root().as_bytes(),
    };
    if selector.is_some_and(|selector| selector.basis != basis.identity()) {
        return Ok(Some(Outcomes::Stale {
            package: package.clone(),
        }));
    }
    let page = captures
        .page_from(
            &ProductSemanticPublicationKey::package_lower_bound(package.clone()),
            MAX_PROFILES + 1,
        )
        .map_err(|error| {
            BuiltinModelError(format!("read bounded package-profile captures: {error}"))
        })?;
    let records: Vec<_> = page
        .entries()
        .iter()
        .take_while(|(key, _)| key.package() == package)
        .collect();
    if records.is_empty() {
        return Ok(unavailable(Unavailable::CaptureNotRecorded));
    }
    if records.len() > MAX_PROFILES {
        return Ok(unavailable(Unavailable::CaptureIncomplete));
    }
    let mut capture_basis = None;
    let mut profiles = Vec::with_capacity(records.len());
    let mut refusals = Vec::new();
    let mut full = None;
    let mut coverage = Vec::new();
    let mut coverage_page = None;
    let coverage_read = match read {
        Some(PackageCompilerOutcomeRead::Coverage(selector)) => Some(*selector),
        _ => None,
    };
    for (key, record) in records {
        // The canonical PSC2 decoder already prohibits history/generation keys.
        if !key.is_selected() || key.package_key().to_bytes() != project_key {
            return Ok(unavailable(Unavailable::CaptureIncomplete));
        }
        let capture = record.capture();
        if *capture.source_version() != basis.source_version {
            return Ok(unavailable(Unavailable::CaptureNotCurrent));
        }
        let next_basis = (
            *record.source_commit(),
            *record.source_workspace_root(),
            record.source_workspace_sequence(),
            *record.request_identity(),
            record.operation_key().copied(),
        );
        if capture_basis.is_some_and(|previous| previous != next_basis) {
            return Ok(unavailable(Unavailable::CaptureIncomplete));
        }
        capture_basis = Some(next_basis);
        let profile = IndexOperationSourceProfile {
            profile: backend_library::SemanticLanguageProfile::new(key.profile()),
            source_version: *capture.source_version(),
            input_digest: *capture.input_digest(),
            observation_sequence: capture.observation_sequence(),
            source_count: capture.source_count(),
            state: super::index::index_operation_capture_outcome(record.outcome()),
        };
        if let Some(failure) = record.compiler_failure() {
            let file_key =
                backend_engine::product_source_file_key(project_key, failure.relative_path());
            let row = source
                .lookup(&file_key)
                .map_err(|error| {
                    BuiltinModelError(format!("read compiler diagnostic source: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "compiler diagnostic source is absent from selected package".to_owned(),
                    )
                })?;
            super::super::profile::validate_compiler_failure_source(key, failure, &row)?;
        }
        if let State::Unavailable { reason } | State::Failed { reason, .. } = profile.state {
            let refusal = IndexOperationProfileRefusal {
                profile: profile.profile,
                reason,
                compiler_failure: record.compiler_failure().cloned(),
            };
            refusal
                .admit_for(&profile)
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            let read = PackageCompilerOutcomeSelector {
                basis: basis.identity(),
                profile: profile.profile,
            };
            refusals.push(PackageCompilerProfileRefusal {
                profile: profile.profile,
                reason,
                diagnostic: if record.compiler_failure().is_some() {
                    PackageCompilerDiagnostic::Retained {
                        read: PackageCompilerOutcomeRead::Diagnostic(read),
                    }
                } else {
                    PackageCompilerDiagnostic::NotRecorded
                },
            });
            if selector.is_some_and(|selector| selector.profile == profile.profile) {
                full = Some(refusal);
            }
        }
        if wants_coverage(read, profile.profile)
            && let backend_engine::builtin::ProductSemanticCaptureOutcome::Published {
                coverage: published_coverage,
                claim,
            } = record.outcome()
        {
            use backend_library::{
                PackageCompilerCoverage as Availability, PackageCompilerCoveragePage as Page,
                PackageCompilerCoverageSelector as Selector,
            };
            let observations = authority.selected_source_coverage(key, claim)?;
            match observations {
                None => {
                    coverage.push(Availability::LegacyNotRecorded {
                        profile: profile.profile,
                    });
                    if coverage_read.is_some_and(|read| read.outcome.profile == profile.profile) {
                        coverage_page = Some(Err(Unavailable::CoverageNotRecorded));
                    }
                }
                Some(observations) => {
                    if observations.input_root() != capture.input_digest()
                        || observations.profile() != <[u8; 2]>::from(key.profile())
                        || u64::from(observations.source_count()) != capture.source_count()
                        || match published_coverage {
                            backend_engine::builtin::SemanticPublicationCoverage::Complete => {
                                observations.completed() != observations.source_count()
                            }
                            backend_engine::builtin::SemanticPublicationCoverage::Partial(
                                partial,
                            ) => {
                                observations.completed() != partial.completed().get()
                                    || observations.source_count() != partial.total().get()
                            }
                        }
                    {
                        return Err(BuiltinModelError(
                            "compiler coverage differs from selected capture".to_owned(),
                        ));
                    }
                    let first = Selector {
                        outcome: PackageCompilerOutcomeSelector {
                            basis: basis.identity(),
                            profile: profile.profile,
                        },
                        catalog: observations.identity(),
                        generation: *claim.binding().identity.as_ref(),
                        input_digest: *capture.input_digest(),
                        offset: 0,
                    };
                    coverage.push(Availability::Retained {
                        read: PackageCompilerOutcomeRead::Coverage(first),
                    });
                    if let Some(requested) =
                        coverage_read.filter(|read| read.outcome.profile == profile.profile)
                    {
                        if (Selector {
                            offset: 0,
                            ..requested
                        }) != first
                        {
                            return Ok(Some(Outcomes::Stale {
                                package: package.clone(),
                            }));
                        }
                        let offset = requested.offset as usize;
                        if offset > observations.rows().len() {
                            return Err(BuiltinModelError(
                                "compiler coverage offset is outside catalog".to_owned(),
                            ));
                        }
                        let row = coverage_row(&observations, offset, |path, identity| {
                            let file_key =
                                backend_engine::product_source_file_key(project_key, path);
                            let row = source
                                .lookup(&file_key)
                                .map_err(|e| {
                                    BuiltinModelError(format!("read coverage source: {e}"))
                                })?
                                .ok_or_else(|| {
                                    BuiltinModelError("compiler coverage source absent".to_owned())
                                })?;
                            let file = row.file_fields().ok_or_else(|| {
                                BuiltinModelError(
                                    "compiler coverage source is not a file".to_owned(),
                                )
                            })?;
                            if file.project != project_key
                                || file.path != path
                                || file.source_identity.as_ref().map(|id| id.as_ref())
                                    != Some(identity)
                            {
                                return Err(BuiltinModelError(
                                    "compiler coverage source differs from selected frontier"
                                        .to_owned(),
                                ));
                            }
                            Ok(())
                        })?;
                        let rows = row.into_iter().collect::<Vec<_>>().into_boxed_slice();
                        let end = offset + rows.len();
                        let page = Page {
                            generation: *claim.binding().identity.as_ref(),
                            input_digest: *capture.input_digest(),
                            source_count: observations.source_count(),
                            completed: observations.completed(),
                            row_count: observations.rows().len() as u32,
                            rows,
                            next: (end < observations.rows().len()).then_some(
                                PackageCompilerOutcomeRead::Coverage(Selector {
                                    offset: end as u32,
                                    ..requested
                                }),
                            ),
                        };
                        coverage_page = Some(
                            if serde_json::to_vec(&page)
                                .map_err(|e| BuiltinModelError(e.to_string()))?
                                .len()
                                > 4096
                            {
                                Err(Unavailable::CoverageRowTooLarge)
                            } else {
                                Ok(page)
                            },
                        );
                    }
                }
            }
        }
        profiles.push(profile);
    }
    profiles.sort_by_key(|profile| profile.profile);
    refusals.sort_by_key(|refusal| refusal.profile);
    let Some((
        commit_identity,
        workspace_root,
        workspace_sequence,
        request_identity,
        operation_key,
    )) = capture_basis
    else {
        return Ok(unavailable(Unavailable::CaptureNotRecorded));
    };
    let summary = IndexSourceCaptureSummary {
        producer_package: package.clone(),
        request_identity,
        commit_identity,
        workspace_root,
        workspace_sequence,
        profiles: profiles.into_boxed_slice(),
    };
    summary
        .admit()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    // The durable keyed receipt supplies an independent complete profile set.
    // Its semantic states may lag a selected terminal update, so compare the
    // exact source/input/observation tuples rather than substituting its state.
    if let Some(key) = operation_key {
        let Some(IndexOperationObservation::Known(status)) = journal
            .observation(
                backend_library::IndexOperationKey::from_bytes(key)
                    .map_err(|error| BuiltinModelError(error.to_string()))?,
                None,
            )
            .map_err(|error| BuiltinModelError(format!("read capture profile receipt: {error}")))?
        else {
            return Ok(unavailable(Unavailable::CaptureIncomplete));
        };
        if !receipt_matches_capture(&status, &summary) {
            return Ok(unavailable(Unavailable::CaptureIncomplete));
        }
    }
    if let Some(selector) = coverage_read {
        let page = match coverage_page {
            Some(Ok(page)) => page,
            Some(Err(reason)) => return Ok(unavailable(reason)),
            None => return Ok(unavailable(Unavailable::ProfileNotPublished)),
        };
        let result = Outcomes::Coverage {
            package: package.clone(),
            basis,
            selector,
            page,
        };
        result
            .admit_request(package, read)
            .map_err(|e| BuiltinModelError(e.to_string()))?;
        return Ok(Some(result));
    }
    let outcomes = match selector {
        Some(selector) => match full {
            Some(refusal) if refusal.compiler_failure.is_some() => Outcomes::Diagnostic {
                basis,
                selector: *selector,
                source_capture: summary,
                refusal,
            },
            Some(_) => return Ok(unavailable(Unavailable::DiagnosticNotRecorded)),
            None => return Ok(unavailable(Unavailable::ProfileNotRefused)),
        },
        None => Outcomes::Summary {
            basis,
            source_capture: summary,
            refusals: refusals.into_boxed_slice(),
            coverage: Some(coverage.into_boxed_slice()),
        },
    };
    outcomes
        .admit_request(package, read)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    Ok(Some(outcomes))
}

fn wants_coverage(
    read: Option<&PackageCompilerOutcomeRead>,
    profile: backend_library::SemanticLanguageProfile,
) -> bool {
    match read {
        None | Some(PackageCompilerOutcomeRead::Current) => true,
        Some(PackageCompilerOutcomeRead::Coverage(read)) => read.outcome.profile == profile,
        Some(PackageCompilerOutcomeRead::Diagnostic(_)) => false,
    }
}

/// Only this indexed row is checked against the source frontier. Other
/// profiles and earlier gaps have no source lookup work in this request.
fn coverage_row(
    catalog: &backend_semantic::ir::SourceCoverageManifest,
    offset: usize,
    check_source: impl FnOnce(&str, &[u8; 32]) -> Result<(), BuiltinModelError>,
) -> Result<Option<backend_semantic::ir::SourceCoverageRow>, BuiltinModelError> {
    if offset > catalog.rows().len() {
        return Err(BuiltinModelError(
            "compiler coverage offset outside catalog".to_owned(),
        ));
    }
    let row = catalog.rows().get(offset);
    if let Some(backend_semantic::ir::SourceCoverageRow::Gap { path, source, .. }) = row {
        check_source(path, source)?;
    }
    Ok(row.cloned())
}

fn receipt_matches_capture(
    status: &backend_library::IndexOperationStatus,
    summary: &IndexSourceCaptureSummary,
) -> bool {
    let Some(expected) = status.source_capture.as_ref() else {
        return false;
    };
    status.package == summary.producer_package
        && expected.commit_identity() == &summary.commit_identity
        && expected.workspace_root() == &summary.workspace_root
        && expected.workspace_sequence() == summary.workspace_sequence
        && expected.profiles().len() == summary.profiles.len()
        && expected
            .profiles()
            .iter()
            .zip(summary.profiles.iter())
            .all(|(a, b)| {
                (
                    a.profile,
                    a.source_version,
                    a.input_digest,
                    a.observation_sequence,
                    a.source_count,
                ) == (
                    b.profile,
                    b.source_version,
                    b.input_digest,
                    b.observation_sequence,
                    b.source_count,
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_current_capture_receipt_refuses_missing_foreign_and_changed_profile_basis() {
        use backend_library::{
            CompileExecutionIntent, IndexOperationKey, IndexOperationSourceCaptureReceipt,
            IndexOperationState, IndexOperationStatus, SemanticLanguageProfile,
        };
        let package = PackageReference::parse("/capture-receipt").expect("local");
        let profiles = ["typescript", "python"].map(|name| IndexOperationSourceProfile {
            profile: SemanticLanguageProfile::from_name(name).expect("profile"),
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 1,
            source_count: 3,
            state: State::Unavailable {
                reason: backend_library::IndexOperationSemanticUnavailableReason::Rejected,
            },
        });
        let summary = IndexSourceCaptureSummary {
            producer_package: package.clone(),
            request_identity: [6; 32],
            commit_identity: [7; 32],
            workspace_root: [8; 32],
            workspace_sequence: 1,
            profiles: profiles.to_vec().into_boxed_slice(),
        };
        let operation = IndexOperationKey::from_bytes([9; 32]).expect("key");
        let status = IndexOperationStatus::new(
            operation,
            package,
            CompileExecutionIntent::Interactive,
            IndexOperationState::Accepted,
        )
        .with_source_capture(Some(
            IndexOperationSourceCaptureReceipt::from_checked_parts(
                operation,
                [7; 32],
                [8; 32],
                1,
                summary.profiles.clone(),
            )
            .expect("exact source receipt"),
        ));
        assert!(receipt_matches_capture(&status, &summary));
        for mutation in 0..9 {
            let mut changed = summary.clone();
            match mutation {
                0 => changed.profiles = changed.profiles[..1].to_vec().into_boxed_slice(),
                1 => changed.profiles[1] = changed.profiles[0].clone(),
                2 => changed.profiles[0].source_version = [99; 32],
                3 => changed.profiles[0].input_digest = [99; 32],
                4 => changed.profiles[0].observation_sequence += 1,
                5 => changed.profiles[0].source_count += 1,
                6 => changed.producer_package = PackageReference::parse("/other").expect("local"),
                7 => changed.workspace_root = [99; 32],
                _ => changed.commit_identity = [99; 32],
            }
            assert!(
                !receipt_matches_capture(&status, &changed),
                "mutation {mutation}"
            );
        }
        let mut absent = status.clone();
        absent.source_capture = None;
        assert!(!receipt_matches_capture(&absent, &summary));
        let mut lagging = summary.clone();
        lagging.profiles[0].state = State::Pending { prior: None };
        assert!(
            receipt_matches_capture(&status, &lagging),
            "semantic state may lag; exact captured source tuples may not"
        );
    }
    #[test]
    fn coverage_followup_filters_profiles_and_checks_only_the_requested_indexed_source() {
        use backend_library::{PackageCompilerCoverageSelector, SemanticLanguageProfile};
        use backend_semantic::ir::{
            SourceCoverageGapCause, SourceCoverageManifest, SourceCoverageRow,
        };
        use std::cell::Cell;
        let profile = SemanticLanguageProfile::from_name("typescript").unwrap();
        let selector = PackageCompilerCoverageSelector {
            outcome: PackageCompilerOutcomeSelector {
                basis: [3; 32],
                profile,
            },
            catalog: [4; 32],
            generation: [5; 32],
            input_digest: [7; 32],
            offset: 2048,
        };
        let request = PackageCompilerOutcomeRead::Coverage(selector);
        let loads = Cell::new(0);
        let lookups = Cell::new(0);
        let source =
            *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                b"spec",
            )
            .as_ref();
        let catalog = SourceCoverageManifest::new(
            [7; 32],
            [1, 0],
            [7; 32],
            4097,
            1,
            (0..4096)
                .map(|i| SourceCoverageRow::Gap {
                    path: format!("src/{i:05}.spec.ts"),
                    source,
                    byte_len: 4,
                    cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                })
                .collect(),
        )
        .unwrap();
        for candidate in ["python", "typescript", "rust"] {
            if wants_coverage(
                Some(&request),
                SemanticLanguageProfile::from_name(candidate).unwrap(),
            ) {
                loads.set(loads.get() + 1);
                let row = coverage_row(&catalog, selector.offset as usize, |path, seen| {
                    lookups.set(lookups.get() + 1);
                    assert_eq!(path, "src/02048.spec.ts");
                    assert_eq!(*seen, source);
                    Ok(())
                })
                .unwrap()
                .unwrap();
                assert_eq!(row.path(), "src/02048.spec.ts");
            }
        }
        assert_eq!(
            loads.get(),
            1,
            "other published profiles do not load their catalogs"
        );
        assert_eq!(
            lookups.get(),
            1,
            "earlier/unrequested rows perform zero source lookups"
        );
        let last = coverage_row(&catalog, 4095, |path, _| {
            lookups.set(lookups.get() + 1);
            assert_eq!(path, "src/04095.spec.ts");
            Ok(())
        })
        .unwrap();
        assert!(last.is_some());
        assert_eq!(lookups.get(), 2);
        assert!(
            coverage_row(&catalog, 4096, |_, _| panic!(
                "past end does not touch source"
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            coverage_row(&catalog, 4097, |_, _| panic!(
                "invalid index does not touch source"
            ))
            .is_err()
        );
        let diagnostic = PackageCompilerOutcomeRead::Diagnostic(selector.outcome);
        assert!(!wants_coverage(Some(&diagnostic), profile));
    }
}
