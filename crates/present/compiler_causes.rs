//! Optional bounded cause reads for failed package rows, never served coverage.
use crate::drive::Engine;
use crate::fault::{Fault, Operand};
use crate::identity::Coordinate;
use backend_library::{
    PackageCompilerDiagnostic, PackageCompilerOutcomeRead as Read,
    PackageCompilerOutcomes as Outcomes, PackageReference, RowId, RowState, SurfaceCommand,
    SurfaceReply, ViewSnapshot,
};
use std::collections::BTreeMap;

/// At most two failed projects and one retained diagnostic each. Healthy rows
/// trigger no extra calls; missing/stale/oversized evidence remains absent.
const MAX_PROJECTS: usize = 2;
const MAX_CAUSE_BYTES: usize = 4096;

pub(crate) fn failed_package_causes(
    engine: &mut dyn Engine,
    snapshot: &ViewSnapshot,
) -> BTreeMap<backend_library::PackageKey, Fault> {
    let failed: Vec<_> = snapshot
        .root
        .rows()
        .iter()
        .filter_map(|row| match (row.id, row.state) {
            (RowId::Package(key), RowState::Failed) => Some((key, row.label.as_str())),
            _ => None,
        })
        .take(MAX_PROJECTS)
        .collect();
    if failed.is_empty() || engine.revision().ok() != Some(snapshot.root.root()) {
        return BTreeMap::new();
    }
    let mut found = BTreeMap::new();
    for (key, label) in failed {
        if let Some(fault) = package_cause(engine, snapshot, key, label) {
            found.insert(key, fault);
        }
    }
    // A source-equal but capture-changed response must not explain another
    // selected view. Reject the whole optional addition if the view moved.
    if engine.revision().ok() != Some(snapshot.root.root()) {
        return BTreeMap::new();
    }
    found
}

fn outcomes(reply: SurfaceReply) -> Option<Outcomes> {
    match reply {
        SurfaceReply::PackageCompilerOutcomes(value) => Some(value),
        SurfaceReply::PackageProfile {
            compiler_outcomes, ..
        } => compiler_outcomes,
        _ => None,
    }
}

fn package_cause(
    engine: &mut dyn Engine,
    snapshot: &ViewSnapshot,
    key: backend_library::PackageKey,
    label: &str,
) -> Option<Fault> {
    let package = PackageReference::parse(label.to_owned()).ok()?;
    if !matches!(&package, PackageReference::Local(_)) {
        return None;
    }
    // Explicit Current reads use the existing bounded capture-prefix route,
    // independently of registry metadata and full compiler coverage pages.
    let read = Read::Current;
    let summary = outcomes(
        engine
            .surface(SurfaceCommand::PackageProfile {
                package: package.clone(),
                compiler_outcome: Some(read),
            })
            .ok()?,
    )?;
    summary.admit_request(&package, Some(&read)).ok()?;
    let Outcomes::Summary {
        basis,
        source_capture,
        refusals,
        ..
    } = summary
    else {
        return None;
    };
    if basis.package_key != key.to_bytes()
        || basis.source_relation_root != *snapshot.root.basis().root.as_bytes()
    {
        return None;
    }
    let read = refusals
        .iter()
        .find_map(|refusal| match &refusal.diagnostic {
            PackageCompilerDiagnostic::Retained {
                read: Read::Diagnostic(selector),
            } if selector.basis == basis.identity() && selector.profile == refusal.profile => {
                Some(Read::Diagnostic(*selector))
            }
            _ => None,
        })?;
    let diagnostic = outcomes(
        engine
            .surface(SurfaceCommand::PackageProfile {
                package: package.clone(),
                compiler_outcome: Some(read),
            })
            .ok()?,
    )?;
    diagnostic.admit_request(&package, Some(&read)).ok()?;
    let Outcomes::Diagnostic {
        basis: observed_basis,
        source_capture: observed_capture,
        refusal,
        ..
    } = diagnostic
    else {
        return None;
    };
    if observed_basis != basis || observed_capture != source_capture {
        return None;
    }
    let failure = refusal.compiler_failure.as_ref()?;
    let fault = Fault::compiler_refusal(failure, Operand::Coordinate(Coordinate::new(label)));
    // Never truncate a cause into a different claim. Preserve absence when a
    // complete exact fault exceeds this small optional presentation budget.
    let encoded = serde_json::to_vec(&crate::dto::FaultDto::new(&fault)).ok()?;
    (encoded.len() <= MAX_CAUSE_BYTES).then_some(fault)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{Answer, Detail, Theme};
    use backend_client::ClientError;
    use backend_library::{
        Basis, Command, CommandReply, Coverage, Cursor, Frontier, HealthReport,
        IndexOperationProfileRefusal, IndexOperationSemanticProfileState as State,
        IndexOperationSemanticUnavailableReason as Reason, IndexOperationSourceProfile,
        IndexSourceCaptureSummary, Lane, Library, PackageCompilerFailure,
        PackageCompilerOutcomeBasis, PackageCompilerOutcomeSelector, PackageCompilerProfileRefusal,
        ReplyDto, Row, SemanticLanguageProfile, ViewRoot, ViewStateRoot, object_version,
        package_key, view_key, view_state_root,
    };

    const PROJECT: &str = "/workspace/compiler-refused";
    const CAUSE: &str = "typescript_native_project_check_work_budget_exhausted";
    #[derive(Clone, Copy)]
    enum Mode {
        Known,
        Cancelled,
        NoEvidence,
        Stale,
        WrongSource,
        ChangedView,
    }
    struct Fake {
        library: Library,
        snapshot: ViewSnapshot,
        health: HealthReport,
        mode: Mode,
        surfaces: usize,
        package_reads: usize,
    }
    fn native_failure() -> PackageCompilerFailure {
        use backend_library::interface::{
            AuthorityDiagnosticClass, AuthorityPhase, CompilerAttempt, CompilerCause,
            CompilerDiagnostic, CompilerTerminal, SourceAuthority, TypeScriptAuthorityFailureKind,
        };
        use backend_semantic::vocabulary::{Language, Stage};
        use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};
        PackageCompilerFailure::from_package_terminal(
            "src/main.ts",
            &CompilerTerminal::Compile {
                attempted: CompilerAttempt {
                    source: SourceAuthority {
                        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source"),
                        byte_len: 6,
                    },
                    recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
                },
                cause: CompilerCause::Authority {
                    phase: AuthorityPhase::Resolve,
                    class: AuthorityDiagnosticClass::Authority,
                    diagnostic: CompilerDiagnostic::from_native(
                        b"private native source bytes",
                        27,
                        false,
                    )
                    .map(|d| {
                        d.with_typescript_failure(
                            TypeScriptAuthorityFailureKind::NativeProjectCheckWorkBudgetExhausted,
                        )
                    }),
                },
            },
        )
        .expect("closed native failure")
        .expect("compiler failure projected")
    }
    impl Fake {
        fn new(mode: Mode, state: RowState, count: usize, coverage: Vec<Coverage>) -> Self {
            let basis = Basis::new(
                view_state_root(&[(PROJECT.to_owned(), "source".to_owned())]),
                object_version(b"source"),
            );
            let rows = (0..count)
                .map(|index| {
                    let label = if index == 0 {
                        PROJECT.to_owned()
                    } else {
                        format!("{PROJECT}-{index}")
                    };
                    let key = package_key(&label);
                    let mut row = Row::in_package(RowId::Package(key), basis, key, label);
                    row.state = state;
                    row
                })
                .collect::<Vec<_>>();
            let root = ViewRoot::new_incomplete(
                view_key(b"cause view"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 2),
                rows,
                Vec::new(),
            )
            .expect("incomplete fixture");
            let cursor = Cursor::for_view_root(&root);
            let library = Library::from_view(root.clone(), cursor).expect("catalog");
            let CommandReply::Packages(snapshot) =
                library.execute(Command::Packages).expect("packages")
            else {
                panic!("packages reply")
            };
            let health = HealthReport::from_admitted_parts(
                backend_library::RevisionReceipt::new(root.root(), cursor, basis.object),
                basis,
                coverage.into_boxed_slice(),
                root.row_count(),
                backend_library::CapabilityInventory::explicitly_unavailable(),
            );
            Self {
                library,
                snapshot,
                health,
                mode,
                surfaces: 0,
                package_reads: 0,
            }
        }
        fn diagnostic(&self, package: PackageReference, read: Read) -> Outcomes {
            if matches!(self.mode, Mode::NoEvidence) {
                return Outcomes::Unavailable {
                    package,
                    reason: backend_library::PackageCompilerOutcomesUnavailable::CaptureNotRecorded,
                };
            }
            let profile = SemanticLanguageProfile::new(
                backend_semantic::vocabulary::LanguageProfile::TypeScript(
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript,
                ),
            );
            let reason = if matches!(self.mode, Mode::Cancelled) {
                Reason::Cancelled
            } else {
                Reason::Rejected
            };
            let basis = PackageCompilerOutcomeBasis {
                package_key: package_key(package.as_str()).to_bytes(),
                source_relation_root: if matches!(self.mode, Mode::WrongSource) {
                    [0x99; 32]
                } else {
                    *self.snapshot.root.basis().root.as_bytes()
                },
                source_version: [4; 32],
                capture_relation_root: [5; 32],
            };
            let selector = PackageCompilerOutcomeSelector {
                basis: basis.identity(),
                profile,
            };
            let source_capture = IndexSourceCaptureSummary {
                producer_package: package.clone(),
                request_identity: [6; 32],
                commit_identity: [7; 32],
                workspace_root: [8; 32],
                workspace_sequence: 1,
                profiles: vec![IndexOperationSourceProfile {
                    profile,
                    source_version: [4; 32],
                    input_digest: [9; 32],
                    observation_sequence: 1,
                    source_count: 1,
                    state: State::Unavailable { reason },
                }]
                .into_boxed_slice(),
            };
            if matches!(read, Read::Current) {
                Outcomes::Summary {
                    basis,
                    source_capture,
                    coverage: None,
                    refusals: vec![PackageCompilerProfileRefusal {
                        profile,
                        reason,
                        diagnostic: if matches!(self.mode, Mode::Cancelled) {
                            PackageCompilerDiagnostic::NotRecorded
                        } else {
                            PackageCompilerDiagnostic::Retained {
                                read: Read::Diagnostic(selector),
                            }
                        },
                    }]
                    .into_boxed_slice(),
                }
            } else if matches!(self.mode, Mode::Stale) {
                Outcomes::Stale { package }
            } else {
                Outcomes::Diagnostic {
                    basis,
                    selector,
                    source_capture,
                    refusal: IndexOperationProfileRefusal {
                        profile,
                        reason,
                        compiler_failure: Some(native_failure()),
                    },
                }
            }
        }
    }
    impl Engine for Fake {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            Ok(
                if matches!(self.mode, Mode::ChangedView) && self.surfaces > 0 {
                    view_state_root(&[("changed".to_owned(), "view".to_owned())])
                } else {
                    self.snapshot.root.root()
                },
            )
        }
        fn health(&mut self) -> Result<HealthReport, ClientError> {
            Ok(self.health.clone())
        }
        fn probe(&mut self, probe: crate::Probe<'_>) -> Result<ReplyDto, ClientError> {
            assert!(matches!(probe, crate::Probe::Packages));
            self.package_reads += 1;
            Ok(ReplyDto::new(
                1,
                self.library.execute(Command::Packages).expect("packages"),
            ))
        }
        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            self.surfaces += 1;
            let SurfaceCommand::PackageProfile {
                package,
                compiler_outcome: Some(read),
            } = command
            else {
                panic!("only exact read-only compiler-outcome routes")
            };
            Ok(SurfaceReply::PackageCompilerOutcomes(
                self.diagnostic(package, read),
            ))
        }
    }
    fn refused() -> Vec<Coverage> {
        vec![
            Coverage::Complete,
            Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: backend_library::Reason::Incomplete,
            },
        ]
    }

    #[test]
    fn failed_project_uses_current_native_cause_in_cli_mcp_and_summary_status() {
        let mut engine = Fake::new(Mode::Known, RowState::Failed, 1, refused());
        let answer = crate::answer(&mut engine, &crate::Request::Shelf).expect("shelf");
        let Answer::Shelf(shelf) = &answer else {
            panic!("shelf")
        };
        let fault = shelf.entries()[0]
            .readiness()
            .fault()
            .expect("exact failed cause");
        assert_eq!(fault.compiler_failure(), Some(&native_failure()));
        assert!(crate::text::shelf(shelf, Theme::plain()).contains(CAUSE));
        assert!(crate::markdown::shelf(shelf).contains(CAUSE));
        assert!(!crate::text::shelf(shelf, Theme::plain()).contains("Install Node.js"));
        assert!(!crate::text::shelf(shelf, Theme::plain()).contains("private native source bytes"));
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let encoded =
                crate::encode_answer(&answer, detail, None, crate::DEFAULT_RESPONSE_BUDGET_BYTES)
                    .expect("bounded shelf");
            assert!(
                std::str::from_utf8(&encoded.bytes)
                    .expect("JSON")
                    .contains(CAUSE)
            );
        }
        let answer = crate::answer(&mut engine, &crate::Request::Status).expect("status");
        let Answer::Status(status) = &answer else {
            panic!("status")
        };
        assert_eq!(status.readiness(), "unavailable");
        assert!(status.compiler_refusal().is_some());
        assert!(crate::text::status(status, Theme::plain()).contains(CAUSE));
        assert!(crate::markdown::status(status).contains(CAUSE));
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let bytes =
                crate::encode_answer(&answer, detail, None, crate::DEFAULT_RESPONSE_BUDGET_BYTES)
                    .expect("bounded status")
                    .bytes;
            assert!(std::str::from_utf8(&bytes).expect("JSON").contains(CAUSE));
        }
    }

    #[test]
    fn cancelled_absent_foreign_or_stale_cause_is_never_invented() {
        for mode in [
            Mode::Cancelled,
            Mode::NoEvidence,
            Mode::WrongSource,
            Mode::Stale,
            Mode::ChangedView,
        ] {
            let mut engine = Fake::new(mode, RowState::Failed, 1, refused());
            let snapshot = engine.snapshot.clone();
            assert!(failed_package_causes(&mut engine, &snapshot).is_empty());
            assert!(engine.surfaces <= 2);
        }
    }

    #[test]
    fn ready_partial_or_running_projects_keep_fast_path_and_served_coverage() {
        for (state, coverage) in [
            (RowState::Ready, vec![Coverage::Complete]),
            (
                RowState::Ready,
                vec![
                    Coverage::Complete,
                    Coverage::Gapped {
                        lane: Lane::Semantic,
                    },
                ],
            ),
            (
                RowState::Loading,
                vec![
                    Coverage::Complete,
                    Coverage::Partial {
                        lane: Lane::Semantic,
                        completed: 0,
                        total: 1,
                    },
                ],
            ),
        ] {
            let mut engine = Fake::new(Mode::Known, state, 1, coverage);
            let before = crate::Status::from_report(&engine.health, None);
            let answer = crate::answer(&mut engine, &crate::Request::Status).expect("status");
            let Answer::Status(status) = answer else {
                panic!("status")
            };
            assert_eq!(status.coverage(), before.coverage());
            assert!(status.compiler_refusal().is_none());
            let dto = crate::dto::StatusDto::new(&status);
            assert!(
                serde_json::to_value(dto)
                    .expect("status DTO")
                    .get("compiler_refusal")
                    .is_none()
            );
            for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
                let bytes = crate::encode_answer(
                    &Answer::Status(status.clone()),
                    detail,
                    None,
                    crate::DEFAULT_RESPONSE_BUDGET_BYTES,
                )
                .expect("status without diagnostics")
                .bytes;
                assert!(
                    !std::str::from_utf8(&bytes)
                        .expect("JSON")
                        .contains("compiler_refusal")
                );
            }
            assert_eq!((engine.surfaces, engine.package_reads), (0, 0));
            let _ = crate::answer(&mut engine, &crate::Request::Shelf).expect("shelf");
            assert_eq!(engine.surfaces, 0);
        }
    }

    #[test]
    fn hundreds_of_failed_projects_read_at_most_two_diagnostics() {
        let mut engine = Fake::new(Mode::Known, RowState::Failed, 400, refused());
        let snapshot = engine.snapshot.clone();
        let causes = failed_package_causes(&mut engine, &snapshot);
        assert_eq!(causes.len(), MAX_PROJECTS);
        assert_eq!(engine.surfaces, MAX_PROJECTS * 2);
        assert!(causes.values().all(|fault| {
            serde_json::to_vec(&crate::dto::FaultDto::new(fault))
                .expect("fault JSON")
                .len()
                <= MAX_CAUSE_BYTES
        }));
    }
}
