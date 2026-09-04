//! Fused compile/authority execution for one corpus row.
//!
//! The entry module owns permutation orchestration; this module owns the
//! bounded source render, real authority input selection, compile terminal
//! mapping, and publication/observation handoff for one case.

use super::*;

fn compile_with_authority<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    profile: LanguageProfile,
    source: &'source [u8],
    scope: DeclarationScope<'source>,
    toolchain: ResolvedToolchain<'toolchain>,
    authority: SemanticAuthorityInput<'source>,
    cancelled: &'cancel AtomicBool,
    diagnostic: &'diagnostic mut [u8],
    work: &'work Path,
    fragment_output: &'output mut [u8],
) -> Result<CompiledSemantic<'output>, CompileFailure<'diagnostic>> {
    compile_semantic(
        CompileRequest {
            profile,
            stage: Stage::LowerIr,
            source,
            declaration_scope: scope,
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority,
            control: CompileControl {
                deadline: Instant::now() + DEADLINE,
                cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: work,
        },
        CompileOutput { fragment_output },
    )
}
pub(super) fn run_package(
    package: CorpusPackage,
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
    publisher: &mut PassPublisher,
) -> Result<CaseResult, CorpusAuditError> {
    let key = case_key(package);
    let mut source_output = [0xa5_u8; SOURCE_BYTE_LIMIT];
    let rendered = package.render(&mut source_output)?;
    let source = rendered.source.as_bytes();
    let expected_source = source_identity(source);
    let scope = declaration_scope(package)?;
    let (profile, toolchain) = match native_slot(package.language, resolved) {
        Ok(selection) => selection,
        Err(cause) => {
            return Ok(CaseResult::unavailable(
                CaseObservation::LocallyUnavailable { key, cause: AuthorityUnavailableCause::Native(cause) },
                package,
            ));
        }
    };
    let tool = native_tool(package.language);
    let expected_recipe = expected_recipe(profile, tool, expected_source, toolchain);
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let work = NativeWork::create().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
    let cancelled = AtomicBool::new(false);
    let mut fragment_output = vec![0xa5_u8; FRAGMENT_BYTES];
    let compiled = match package.language {
        CorpusLanguage::Rust => {
            let Some(host) = hosts.rust.host() else {
                return Ok(CaseResult::unavailable(
                    CaseObservation::LocallyUnavailable {
                        key,
                        cause: AuthorityUnavailableCause::Native(hosts.rust.cause),
                    },
                    package,
                ));
            };
            let fixture = match rust_fixture(package, source, host) {
                Ok(fixture) => fixture,
                Err(error) if rust_error_is_unavailable(&error) => {
                    return Ok(CaseResult::unavailable(
                        CaseObservation::LocallyUnavailable {
                            key,
                            cause: AuthorityUnavailableCause::RustAuthority,
                        },
                        package,
                    ));
                }
                Err(error) => {
                    return Err(CorpusAuditError::AuthoritySetup { key, cause: error });
                }
            };
            compile_with_authority(
                profile,
                source,
                scope,
                toolchain,
                SemanticAuthorityInput::Rust {
                    project: &fixture.project,
                    maximum_source_bytes: compiler_languages_rust::SourceByteLimit(
                        u32::try_from(source.len()).unwrap_or(u32::MAX),
                    ),
                    features: fixture.features,
                },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            )
        }
        CorpusLanguage::Go => {
            let fixture = match go_fixture(package, source) {
                Ok(fixture) => fixture,
                Err(AuthorityBuildError::Go(error))
                    if go_error_is_unavailable(&error) => {
                        return Ok(CaseResult::unavailable(
                            CaseObservation::LocallyUnavailable {
                                key,
                                cause: AuthorityUnavailableCause::GoOracle,
                            },
                            package,
                        ));
                    }
                Err(error) => return Err(CorpusAuditError::AuthoritySetup { key, cause: error }),
            };
            compile_with_authority(
                profile,
                source,
                scope,
                toolchain,
                SemanticAuthorityInput::Go { image: &fixture.image },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            )
        }
        CorpusLanguage::Java => {
            let provider = match &authorities.java {
                ProviderSlot::Ready(provider) => provider,
                ProviderSlot::Unavailable(cause) => {
                    return Ok(CaseResult::unavailable(
                        CaseObservation::LocallyUnavailable { key, cause: *cause },
                        package,
                    ));
                }
            };
            let image = provider
                .image(source)
                .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
            compile_with_authority(
                profile,
                source,
                scope,
                toolchain,
                SemanticAuthorityInput::Java { image: &image },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            )
        }
        CorpusLanguage::CSharp => {
            let provider = match &authorities.csharp {
                ProviderSlot::Ready(provider) => provider,
                ProviderSlot::Unavailable(cause) => {
                    return Ok(CaseResult::unavailable(
                        CaseObservation::LocallyUnavailable { key, cause: *cause },
                        package,
                    ));
                }
            };
            let image = provider
                .image(package, source)
                .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
            compile_with_authority(
                profile,
                source,
                scope,
                toolchain,
                SemanticAuthorityInput::CSharp { image: &image },
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            )
        }
        CorpusLanguage::TypeScript | CorpusLanguage::Python | CorpusLanguage::Clang => {
            compile_with_authority(
                profile,
                source,
                scope,
                toolchain,
                SemanticAuthorityInput::None,
                &cancelled,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            )
        }
    };
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let terminal = terminal_kind(&failure);
            let observation = CaseObservation::Terminal {
                key,
                source: expected_source,
                terminal,
            };
            work.assert_empty().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
            return Ok(CaseResult::terminal(observation, package));
        }
    };
    work.assert_empty().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
    // Keep all evolving authority access behind this one observer seam.  The
    // fused result's public contract is the artifact plus owned IR; optional
    // authority planes are reported as typed unavailable until their immutable
    // Ir view is exposed by the representation owner.
    let owned = observe_owned(&compiled.ir, package, &rendered);
    let compact = observe_compact(&compiled.artifact.fragment, package, &rendered);
    let neutral_render = render_neutral(&compiled.ir, owned.primary);
    let expected_fragment = digest_bytes(compiled.artifact.fragment.as_ref());
    let expected_ranges = range_manifest_digest(&compiled.artifact.fragment)?;
    let reopened = publisher.publish(&compiled.artifact)?;
    let dialect_render = RenderVerdict::Unsupported;
    let output = CaseOutputObservation {
        source: compiled.artifact.source,
        recipe: compiled.artifact.recipe,
        owned,
        compact,
        reopened,
        neutral_render,
        dialect_render,
    };
    let mut mismatches = Vec::new();
    expected_mismatches(
        key,
        rendered.expected,
        rendered.expected_symbol.as_bytes(),
        rendered.expected_member.map(str::as_bytes),
        expected_source,
        expected_recipe,
        expected_fragment,
        expected_ranges,
        &output,
        &mut mismatches,
    );
    Ok(CaseResult {
        observation: CaseObservation::Output(output),
        mismatches: mismatches.into_boxed_slice(),
        package,
    })
}

pub(super) struct CaseResult {
    pub(super) observation: CaseObservation,
    pub(super) mismatches: Box<[CorpusMismatch]>,
    package: CorpusPackage,
}

impl CaseResult {
    pub(super) fn unavailable(observation: CaseObservation, package: CorpusPackage) -> Self {
        Self {
            observation,
            mismatches: Box::new([]),
            package,
        }
    }

    pub(super) fn terminal(observation: CaseObservation, package: CorpusPackage) -> Self {
        let key = case_key(package);
        let mismatches = match package.expected_facts().availability {
            CaseAvailability::Output => match observation {
                CaseObservation::Terminal {
                    source, terminal, ..
                } => vec![CorpusMismatch::CompileTerminal {
                    key,
                    source,
                    terminal,
                }]
                .into_boxed_slice(),
                _ => Box::new([]),
            },
            CaseAvailability::AuthorityRequired
            | CaseAvailability::ExplicitUnsupported(_) => Box::new([]),
        };
        Self {
            observation,
            mismatches,
            package,
        }
    }
}

const fn terminal_kind(failure: &CompileFailure<'_>) -> CompileTerminalKind {
    match failure {
        CompileFailure::SourceLength { .. } => CompileTerminalKind::SourceLength,
        CompileFailure::UnsupportedStage { .. } => CompileTerminalKind::UnsupportedStage,
        CompileFailure::ToolchainSelectionMismatch { .. } => {
            CompileTerminalKind::ToolchainSelectionMismatch
        }
        CompileFailure::ToolchainMismatch { .. } => CompileTerminalKind::ToolchainMismatch,
        CompileFailure::NativeWork { .. } => CompileTerminalKind::NativeWork,
        CompileFailure::NativeWorkCleanup { .. } => CompileTerminalKind::NativeWorkCleanup,
        CompileFailure::ToolingUnavailable { .. } => CompileTerminalKind::ToolingUnavailable,
        CompileFailure::ToolStart { .. } => CompileTerminalKind::ToolStart,
        CompileFailure::MissingToolInput { .. } => CompileTerminalKind::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { .. } => {
            CompileTerminalKind::MissingToolInputCleanup
        }
        CompileFailure::MissingToolDiagnostic { .. } => CompileTerminalKind::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { .. } => {
            CompileTerminalKind::MissingToolDiagnosticCleanup
        }
        CompileFailure::ToolInput { .. } => CompileTerminalKind::ToolInput,
        CompileFailure::ToolInputCleanup { .. } => CompileTerminalKind::ToolInputCleanup,
        CompileFailure::ToolTerminate { .. } => CompileTerminalKind::ToolTerminate,
        CompileFailure::ToolWait { .. } => CompileTerminalKind::ToolWait,
        CompileFailure::ToolWaitCleanup { .. } => CompileTerminalKind::ToolWaitCleanup,
        CompileFailure::ToolDiagnosticRead { .. } => CompileTerminalKind::ToolDiagnosticRead,
        CompileFailure::ToolDiagnosticReadCleanup { .. } => {
            CompileTerminalKind::ToolDiagnosticReadCleanup
        }
        CompileFailure::NativeWorkerPanic { .. } => CompileTerminalKind::NativeWorkerPanic,
        CompileFailure::Cancelled { .. } => CompileTerminalKind::Cancelled,
        CompileFailure::DeadlineExceeded { .. } => CompileTerminalKind::DeadlineExceeded,
        CompileFailure::DiagnosticLimit { .. } => CompileTerminalKind::DiagnosticLimit,
        CompileFailure::NativeRejected { .. } => CompileTerminalKind::NativeRejected,
        CompileFailure::Authority { .. } => CompileTerminalKind::Authority,
        CompileFailure::AuthorityInputRequired { .. } => CompileTerminalKind::AuthorityInputRequired,
        CompileFailure::AuthorityInputProfileMismatch { .. } => {
            CompileTerminalKind::AuthorityInputProfileMismatch
        }
        CompileFailure::LoweringUnsupported { .. } => CompileTerminalKind::LoweringUnsupported,
        CompileFailure::ExtensionAtomUnbound { .. } => CompileTerminalKind::ExtensionAtomUnbound,
        CompileFailure::FactRejected { .. } => CompileTerminalKind::FactRejected,
        CompileFailure::CSharpProjection { .. } => CompileTerminalKind::CSharpProjection,
        CompileFailure::ClangProjection { .. } => CompileTerminalKind::ClangProjection,
        CompileFailure::Build { .. } => CompileTerminalKind::Build,
        CompileFailure::Prepare { .. } => CompileTerminalKind::Prepare,
        CompileFailure::Write { .. } => CompileTerminalKind::Write,
        CompileFailure::Validate { .. } => CompileTerminalKind::Validate,
    }
}
