//! Source-bound audit of the frozen real-package inventory.
//!
//! The generated 210-row matrix exercises shape-specific expectations.  This
//! module is the separate package lane: it resolves only the exact committed
//! coordinates, asks the selected authority for each source when a producer
//! exists, and then runs the same fused compile/publication boundary.  A
//! package is never counted as verified merely because a source tree or a
//! native executable happened to be present.

use super::authority::{go_fixture_for_source, rust_fixture_for_source};
use super::comparison::RealAuditField;
use super::execution::{compile_with_authority, terminal_kind};
use super::observation::{
    digest_recipe, digest_semantic, digest_source, digest_typed, digest_u64, observe_owned_semantic,
};
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RealLaneSummary {
    pub(super) attempted: usize,
    pub(super) source_bound: usize,
    pub(super) output: usize,
    pub(super) verified: usize,
    pub(super) unavailable: usize,
    pub(super) terminals: usize,
    pub(super) mismatches: usize,
}

impl RealLaneSummary {
    const ZERO: Self = Self {
        attempted: 0,
        source_bound: 0,
        output: 0,
        verified: 0,
        unavailable: 0,
        terminals: 0,
        mismatches: 0,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RealAuditSummary {
    pub(super) lanes: [RealLaneSummary; CorpusLanguage::ALL.len()],
    pub(super) capacity: inventory::CorpusCapacityVerdict,
    pub(super) mismatches: Box<[CorpusMismatch]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceAuthorityFacts {
    /// The source producer supplied exact name spans.  The common IR reader
    /// compares these after replacing transient file atom IDs with their
    /// canonical path bytes.
    Spans(Digest),
    /// The producer is valid but no common source-fact projection exists yet.
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceExpectation {
    source: SourceIdentity,
    profile: LanguageProfile,
    tool: NativeTool,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    spans: SourceAuthorityFacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RealCaseDisposition {
    Output { mismatches: usize, verified: bool },
    Unavailable(AuthorityUnavailableCause),
    Terminal(CompileTerminalKind),
}

/// Runs all locally source-bound rows in inventory order.  The function is
/// intentionally source-first and returns a summary even when every row is a
/// typed unavailable terminal, allowing the caller to report capacity and
/// producer coverage without silently turning that state green.
pub(super) fn audit_real_inventory(
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
) -> Result<RealAuditSummary, CorpusAuditError> {
    inventory::validate_real_inventory().map_err(|cause| CorpusAuditError::Inventory { cause })?;
    let mut lanes = [RealLaneSummary::ZERO; CorpusLanguage::ALL.len()];
    let mut mismatches = Vec::new();
    let mut publisher = PassPublisher::new(Pass::Original)?;

    for input in inventory::real_package_inputs() {
        let case = match input {
            inventory::RealPackageInput::Source { case, .. }
            | inventory::RealPackageInput::Unavailable { case, .. } => case,
        };
        let lane = &mut lanes[inventory_language_index(case.language)];
        lane.attempted = lane.attempted.saturating_add(1);
        match input {
            inventory::RealPackageInput::Unavailable { cause, .. } => {
                lane.unavailable = lane.unavailable.saturating_add(1);
                mismatches.push(CorpusMismatch::RealUnavailable {
                    case,
                    field: RealAuditField::Source,
                    cause: AuthorityUnavailableCause::Source(cause),
                });
            }
            inventory::RealPackageInput::Source { mut source, .. } => {
                lane.source_bound = lane.source_bound.saturating_add(1);
                match audit_source_case(
                    case,
                    &mut source,
                    hosts,
                    resolved,
                    authorities,
                    &mut publisher,
                    &mut mismatches,
                ) {
                    Ok(RealCaseDisposition::Output {
                        mismatches: count,
                        verified,
                    }) => {
                        lane.output = lane.output.saturating_add(1);
                        lane.mismatches = lane.mismatches.saturating_add(count);
                        lane.verified += usize::from(verified);
                    }
                    Ok(RealCaseDisposition::Unavailable(cause)) => {
                        lane.unavailable = lane.unavailable.saturating_add(1);
                        mismatches.push(CorpusMismatch::RealUnavailable {
                            case,
                            field: RealAuditField::Toolchain,
                            cause,
                        });
                    }
                    Ok(RealCaseDisposition::Terminal(terminal)) => {
                        lane.terminals = lane.terminals.saturating_add(1);
                        lane.mismatches = lane.mismatches.saturating_add(1);
                        mismatches.push(CorpusMismatch::RealTerminal {
                            case,
                            source: source.identity,
                            terminal,
                        });
                    }
                    Err(error) => {
                        let _ = publisher.finish();
                        return Err(error);
                    }
                }
            }
        }
    }
    publisher.finish()?;
    Ok(RealAuditSummary {
        lanes,
        capacity: if lanes.iter().map(|lane| lane.attempted).sum::<usize>()
            >= inventory::MIN_REAL_PACKAGE_COUNT
        {
            inventory::CorpusCapacityVerdict::MeetsMinimum
        } else {
            inventory::CorpusCapacityVerdict::Insufficient {
                observed: lanes.iter().map(|lane| lane.attempted).sum(),
                required: inventory::MIN_REAL_PACKAGE_COUNT,
            }
        },
        mismatches: mismatches.into_boxed_slice(),
    })
}

fn inventory_language_index(language: CorpusLanguage) -> usize {
    match language {
        CorpusLanguage::Rust => 0,
        CorpusLanguage::TypeScript => 1,
        CorpusLanguage::Python => 2,
        CorpusLanguage::Go => 3,
        CorpusLanguage::Java => 4,
        CorpusLanguage::CSharp => 5,
        CorpusLanguage::Clang => 6,
    }
}

fn audit_source_case(
    case: inventory::RealPackageCase,
    source: &mut inventory::ResolvedPackageSource,
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
    publisher: &mut PassPublisher,
    mismatch_sink: &mut Vec<CorpusMismatch>,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let (profile, toolchain) = match native_slot(case.language, resolved) {
        Ok(value) => value,
        Err(cause) => {
            return Ok(RealCaseDisposition::Unavailable(
                AuthorityUnavailableCause::Native(cause),
            ));
        }
    };
    source
        .bind_toolchain(native_tool(case.language), toolchain)
        .map_err(|_| CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::AuthorityBindingMismatch,
        })?;
    let (ecosystem, package) = case.coordinate.lineage();
    let lineage =
        PackageLineage::new(ecosystem, package).map_err(CorpusAuditError::ScopeLineage)?;
    let path = source
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source");
    let scope = DeclarationScope::new(lineage, path).map_err(CorpusAuditError::ScopeKey)?;
    let expected = SourceExpectation {
        source: source.identity,
        profile,
        tool: native_tool(case.language),
        recipe: backend_semantic::vocabulary::CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            native_tool(case.language),
            source.identity.identity,
            toolchain.identity,
        ),
        spans: SourceAuthorityFacts::Unavailable,
    };
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let work = NativeWork::create().map_err(|cause| CorpusAuditError::NativeWork {
        key: CaseKey {
            case_id: case.case_id,
            language: case.language,
            shape: PackageShape::Reference,
        },
        cause,
    })?;
    let cancelled = AtomicBool::new(false);
    let mut fragment_output = vec![0xa5_u8; REAL_FRAGMENT_BYTES];

    match case.language {
        CorpusLanguage::Rust => {
            let Some(host) = hosts.rust.host() else {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::Native(hosts.rust.cause),
                ));
            };
            let fixture = match rust_fixture_for_source(case.case_id.raw(), &source.bytes, host) {
                Ok(fixture) => fixture,
                Err(error) if rust_error_is_unavailable(&error) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::RustAuthority,
                    ));
                }
                Err(error) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause: error,
                    });
                }
            };
            let compiled = compile_with_authority(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Rust {
                    project: &fixture.project,
                    maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit(
                        u32::try_from(source.bytes.len()).unwrap_or(u32::MAX),
                    ),
                    features: fixture.features,
                },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::Go => {
            let fixture = match go_fixture_for_source(case.case_id.raw(), &source.bytes) {
                Ok(fixture) => fixture,
                Err(AuthorityBuildError::Go(error)) if go_error_is_unavailable(&error) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::GoOracle,
                    ));
                }
                Err(error) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause: error,
                    });
                }
            };
            let compiled = compile_with_authority(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Go {
                    image: &fixture.image,
                },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::Java => {
            let ProviderSlot::Ready(provider) = &authorities.java else {
                let ProviderSlot::Unavailable(cause) = &authorities.java else {
                    unreachable!()
                };
                return Ok(RealCaseDisposition::Unavailable(*cause));
            };
            let image = provider.image(&source.bytes).map_err(|cause| {
                CorpusAuditError::AuthoritySetup {
                    key: CaseKey {
                        case_id: case.case_id,
                        language: case.language,
                        shape: PackageShape::Reference,
                    },
                    cause,
                }
            })?;
            let compiled = compile_with_authority(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Java { image: &image },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::CSharp => {
            let ProviderSlot::Ready(provider) = &authorities.csharp else {
                let ProviderSlot::Unavailable(cause) = &authorities.csharp else {
                    unreachable!()
                };
                return Ok(RealCaseDisposition::Unavailable(*cause));
            };
            let image = provider
                .image_for_source(case.case_id.raw(), &source.bytes)
                .map_err(|cause| CorpusAuditError::AuthoritySetup {
                    key: CaseKey {
                        case_id: case.case_id,
                        language: case.language,
                        shape: PackageShape::Reference,
                    },
                    cause,
                })?;
            let compiled = compile_with_authority(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::CSharp { image: &image },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::TypeScript => {
            let checker = backend_frontend_typescript::legacy::Checker::default();
            let LanguageProfile::TypeScript(profile) = profile else {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::TypeScriptChecker,
                ));
            };
            let report = match checker.run_in_package(profile, &source.bytes, &source.source_root) {
                Ok(report) => report,
                Err(_) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::TypeScriptChecker,
                    ));
                }
            };
            let compiled = compile_with_authority(
                LanguageProfile::TypeScript(profile),
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::TypeScript { report: &report },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::Python => {
            let profile = PythonVersion::Python314;
            let facts = match backend_frontend_python::legacy::extract(&source.bytes, profile) {
                Ok(facts) => facts,
                Err(_) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            let checker = backend_frontend_python::legacy::Pyrefly::from_env();
            if !checker.is_available() {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::PythonChecker,
                ));
            }
            let report = match checker.analyze(&source.bytes, profile, &facts) {
                Ok(report) => report,
                Err(_) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            let compiled = compile_with_authority(
                LanguageProfile::Python(profile),
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Python { report: &report },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
        CorpusLanguage::Clang => {
            let compiled = compile_with_authority(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::None,
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(case, expected, compiled, work, publisher, mismatch_sink)
        }
    }
}

const REAL_FRAGMENT_BYTES: usize = 16 * 1024 * 1024;

fn finish_real_compile<'diagnostic, 'output>(
    case: inventory::RealPackageCase,
    expected: SourceExpectation,
    compiled: Result<CompiledSemantic<'output>, CompileFailure<'diagnostic>>,
    work: NativeWork,
    publisher: &mut PassPublisher,
    mismatch_sink: &mut Vec<CorpusMismatch>,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let terminal = terminal_kind(&failure);
            work.assert_empty()
                .map_err(|cause| CorpusAuditError::NativeWork {
                    key: CaseKey {
                        case_id: case.case_id,
                        language: case.language,
                        shape: PackageShape::Reference,
                    },
                    cause,
                })?;
            return Ok(RealCaseDisposition::Terminal(terminal));
        }
    };
    work.assert_empty()
        .map_err(|cause| CorpusAuditError::NativeWork {
            key: CaseKey {
                case_id: case.case_id,
                language: case.language,
                shape: PackageShape::Reference,
            },
            cause,
        })?;
    let owned = observe_owned_semantic(&compiled.ir, None);
    let reopened = publisher.publish(&compiled, None)?;
    let before = mismatch_sink.len();
    check_real_output(
        case,
        expected,
        &compiled,
        owned,
        reopened.semantic,
        mismatch_sink,
    );
    let count = mismatch_sink.len().saturating_sub(before);
    let verified = count == 0;
    Ok(RealCaseDisposition::Output {
        mismatches: count,
        verified,
    })
}

fn check_real_output(
    case: inventory::RealPackageCase,
    expected: SourceExpectation,
    compiled: &CompiledSemantic<'_>,
    owned: observation::SemanticObservation,
    reopened: observation::SemanticObservation,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    if compiled.artifact.source != expected.source {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Source,
            expected: digest_source(expected.source),
            observed: digest_source(compiled.artifact.source),
        });
    }
    if compiled.artifact.recipe.profile != expected.profile {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Profile,
            expected: digest_typed(&expected.profile),
            observed: digest_typed(&compiled.artifact.recipe.profile),
        });
    }
    if compiled.artifact.recipe != expected.recipe {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Recipe,
            expected: digest_recipe(expected.recipe),
            observed: digest_recipe(compiled.artifact.recipe),
        });
    }
    if compiled.artifact.recipe.tool != expected.tool {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Toolchain,
            expected: digest_u64(u64::from(u8::from(expected.tool))),
            observed: digest_u64(u64::from(u8::from(compiled.artifact.recipe.tool))),
        });
    }
    if compiled.artifact.recipe.toolchain != expected.recipe.toolchain {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Toolchain,
            expected: digest_typed(&expected.recipe.toolchain),
            observed: digest_typed(&compiled.artifact.recipe.toolchain),
        });
    }
    if owned != reopened {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Reopened,
            expected: digest_semantic(owned),
            observed: digest_semantic(reopened),
        });
    }
    match expected.spans {
        SourceAuthorityFacts::Spans(expected) => {
            if owned.source_spans != expected {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field: RealAuditField::SourceSpans,
                    expected,
                    observed: owned.source_spans,
                });
            }
            if reopened.source_spans != expected {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field: RealAuditField::Reopened,
                    expected,
                    observed: reopened.source_spans,
                });
            }
        }
        SourceAuthorityFacts::Unavailable => {
            mismatches.push(CorpusMismatch::RealUnavailable {
                case,
                field: RealAuditField::SourceSpans,
                cause: AuthorityUnavailableCause::ObserverUnavailable,
            });
        }
    }
    for field in [
        RealAuditField::Declarations,
        RealAuditField::Types,
        RealAuditField::Relations,
        RealAuditField::Documentation,
        RealAuditField::Extensions,
        RealAuditField::Discovery,
        RealAuditField::CanonicalType,
        RealAuditField::Render,
    ] {
        mismatches.push(CorpusMismatch::RealUnavailable {
            case,
            field,
            cause: AuthorityUnavailableCause::ObserverUnavailable,
        });
    }
    check_image_provenance(
        case,
        expected,
        owned.image,
        RealAuditField::SemanticImage,
        mismatches,
    );
    check_image_provenance(
        case,
        expected,
        reopened.image,
        RealAuditField::Reopened,
        mismatches,
    );
    if owned.identity != reopened.identity {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::SemanticImage,
            expected: digest_typed(&owned.identity),
            observed: digest_typed(&reopened.identity),
        });
    }
}

fn check_image_provenance(
    case: inventory::RealPackageCase,
    expected: SourceExpectation,
    image: Option<backend_semantic::ir::SemanticImageFacts>,
    field: RealAuditField,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let Some(image) = image else {
        mismatches.push(CorpusMismatch::RealUnavailable {
            case,
            field,
            cause: AuthorityUnavailableCause::ObserverUnavailable,
        });
        return;
    };
    match image.provenance {
        ImageProvenance::Captured {
            source: observed_source,
            recipe: observed_recipe,
            ..
        } => {
            if observed_source != expected.source {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field,
                    expected: digest_source(expected.source),
                    observed: digest_source(observed_source),
                });
            }
            if observed_recipe != expected.recipe {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field,
                    expected: digest_recipe(expected.recipe),
                    observed: digest_recipe(observed_recipe),
                });
            }
        }
        ImageProvenance::Unavailable => mismatches.push(CorpusMismatch::RealUnavailable {
            case,
            field,
            cause: AuthorityUnavailableCause::ObserverUnavailable,
        }),
    }
}
