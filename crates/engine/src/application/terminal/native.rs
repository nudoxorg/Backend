//! Defines terminal native behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Exhaustive native-driver terminal projection.

mod work;

use crate::driver::{
    CompileFailure, NativeDiagnostic, NativeWorkError, NativeWorkPrimary, ToolchainSelectionFact,
};
use backend_library::interface::{
    CompilerCause, CompilerFragmentFailure, CompilerTerminal, FragmentCause, NativeIoPhase,
    NativePrimaryCause, NativeWorkCause, NativeWorkPhase,
};

use super::common::{
    attempt, authority_diagnostic, compile_from_driver, compiler_diagnostic, io_fact,
    source_authority,
};
use work::{native_primary, native_work_cause, native_work_cleanup};

/// Projects every borrowed driver terminal before its scratch lease expires.
#[rustfmt::skip]
pub(crate) fn compile_terminal(error: CompileFailure<'_>) -> CompilerTerminal {
    match error {
        CompileFailure::SourceLength { actual, .. } => CompilerTerminal::SourceLength { actual },
        CompileFailure::UnsupportedStage { source_identity, language, stage, .. } => unsupported_stage(source_identity, language, stage),
        CompileFailure::ToolchainSelectionMismatch { source_identity, language, stage, selected, provided } => toolchain(source_identity, language, stage, selected, configured_tool(provided)),
        CompileFailure::ToolchainMismatch { source_identity, language, stage, selected, resolved } => toolchain(source_identity, language, stage, selected, Some(resolved)),
        CompileFailure::ToolingUnavailable { source_identity, language, stage, tool } => tooling_unavailable(source_identity, language, stage, tool),
        CompileFailure::Cancelled { source_identity, recipe, diagnostic } => cancelled(source_identity, recipe, diagnostic),
        CompileFailure::NativeWork { source_identity, recipe, phase, cause } => native_work_terminal(source_identity, recipe, phase, cause),
        CompileFailure::NativeWorkCleanup { source_identity, recipe, primary, cleanup } => native_work_cleanup_terminal(source_identity, recipe, primary, cleanup),
        CompileFailure::ToolStart { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Start, &cause),
        CompileFailure::MissingToolInput { source_identity, recipe } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingInput { cleanup: None }),
        CompileFailure::MissingToolInputCleanup { source_identity, recipe, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingInput { cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::MissingToolDiagnostic { source_identity, recipe } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingDiagnostic { cleanup: None }),
        CompileFailure::MissingToolDiagnosticCleanup { source_identity, recipe, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingDiagnostic { cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolInput { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Input, &cause),
        CompileFailure::ToolInputCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::Input { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolTerminate { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Terminate, &cause),
        CompileFailure::ToolWait { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Wait, &cause),
        CompileFailure::ToolWaitCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::Wait { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolDiagnosticRead { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::DiagnosticRead, &cause),
        CompileFailure::ToolDiagnosticReadCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::DiagnosticRead { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::NativeWorkerPanic { source_identity, recipe, cause } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::WorkerPanic(cause)),
        CompileFailure::DeadlineExceeded { source_identity, recipe, diagnostic } => deadline_exceeded(source_identity, recipe, diagnostic),
        CompileFailure::DiagnosticLimit { source_identity, recipe, limit, observed, diagnostic } => diagnostic_limit(source_identity, recipe, limit, observed, diagnostic),
        CompileFailure::NativeRejected { source_identity, recipe, status, diagnostic } => native_rejected(source_identity, recipe, status, diagnostic),
        CompileFailure::Authority { source_identity, recipe, failure } => authority_terminal(source_identity, recipe, &failure),
        CompileFailure::AuthorityInputRequired { source_identity, recipe, .. }
        | CompileFailure::AuthorityInputProfileMismatch { source_identity, recipe, .. } => {
            compile_from_driver(
                source_identity,
                recipe,
                CompilerCause::Authority {
                    phase: backend_semantic::vocabulary::AuthorityPhase::Open,
                    class: backend_semantic::vocabulary::AuthorityDiagnosticClass::Authority,
                    diagnostic: None,
                },
            )
        }
        CompileFailure::LoweringUnsupported { source_identity, recipe, cause } => {
            compile_from_driver(source_identity, recipe, lowering(cause))
        }
        CompileFailure::ExtensionAtomUnbound { source_identity, recipe, row, provisional, atom_count } => compile_from_driver(
            source_identity,
            recipe,
            lowering(backend_semantic::vocabulary::LoweringUnsupported::ExtensionAtomUnbound {
                row: u32::try_from(row).unwrap_or(u32::MAX),
                provisional,
                atom_count: u32::try_from(atom_count).unwrap_or(u32::MAX),
            }),
        ),
        CompileFailure::ExtensionTypeParametersUnbound { source_identity, recipe, row, start, length, element_count } => compile_from_driver(
            source_identity,
            recipe,
            lowering(backend_semantic::vocabulary::LoweringUnsupported::ExtensionTypeParametersUnbound {
                row: u32::try_from(row).unwrap_or(u32::MAX),
                start,
                length,
                element_count: u32::try_from(element_count).unwrap_or(u32::MAX),
            }),
        ),
        CompileFailure::ClangProjection { source_identity, recipe, .. } => compile_from_driver(
            source_identity,
            recipe,
            lowering(backend_semantic::vocabulary::LoweringUnsupported::ClangDeclarationForm),
        ),
        CompileFailure::Build { source_identity, recipe, cause } => fragment_failure_terminal(
            source_identity,
            recipe,
            CompilerFragmentFailure::build(cause),
        ),
        CompileFailure::Prepare { source_identity, recipe, cause } => fragment_failure_terminal(
            source_identity,
            recipe,
            CompilerFragmentFailure::prepare(cause),
        ),
        CompileFailure::Write { source_identity, recipe, cause } => fragment_failure_terminal(
            source_identity,
            recipe,
            CompilerFragmentFailure::write(cause),
        ),
        CompileFailure::Validate { source_identity, recipe, cause } => fragment_failure_terminal(
            source_identity,
            recipe,
            CompilerFragmentFailure::validate(cause),
        ),
    }
}

fn lowering(cause: backend_semantic::vocabulary::LoweringUnsupported) -> CompilerCause {
    CompilerCause::Lowering(backend_library::interface::LoweringCause::new(cause))
}

fn authority_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    failure: &crate::driver::AuthorityFailure<'_>,
) -> CompilerTerminal {
    let projection = failure.projection();
    let diagnostic = authority_diagnostic(projection.diagnostic);
    let diagnostic = match failure {
        crate::driver::AuthorityFailure::TypeScriptTsz { cause, .. } => {
            let kind = crate::application::compiler_typescript_failure::native_failure(cause);
            diagnostic
                .or_else(|| {
                    let detail = kind.detail().as_bytes();
                    backend_library::interface::CompilerDiagnostic::from_native(
                        detail,
                        detail.len(),
                        false,
                    )
                })
                .map(|diagnostic| diagnostic.with_typescript_failure(kind))
        }
        _ => diagnostic,
    };
    let rust_scope = match failure {
        crate::driver::AuthorityFailure::Rust { cause, .. } => rust_source_scope_failure(cause),
        _ => None,
    };
    let diagnostic = match (diagnostic, rust_scope) {
        (Some(diagnostic), Some(scope)) => Some(diagnostic.with_rust_source_scope_failure(scope)),
        (None, Some(scope)) => Some(
            backend_library::interface::CompilerDiagnostic::from_rust_source_scope_failure(scope),
        ),
        (diagnostic, None) => diagnostic,
    };
    compile_from_driver(
        source,
        recipe,
        CompilerCause::Authority {
            phase: projection.phase,
            class: projection.class,
            diagnostic,
        },
    )
}

pub(crate) fn rust_source_scope_failure(
    cause: &backend_frontend_rust::legacy::RustAuthorityError,
) -> Option<backend_library::interface::RustSourceScopeFailureKind> {
    use backend_frontend_rust::legacy::{RustAuthorityError as E, RustCargoTargetResolution as R};
    use backend_library::interface::{
        RustSourceScopeFailureKind as F, RustTargetResolutionKind as T,
    };
    match cause {
        E::DetachedSource {
            active_hir_roots, ..
        } => Some(F::DetachedSource {
            active_hir_root_count: u64::try_from(active_hir_roots.package_crate_count).ok()?,
        }),
        E::AmbiguousSourceOwner {
            definition_count,
            omitted_owner_definitions,
            target_resolution,
            ..
        } => Some(F::AmbiguousSourceOwner {
            definition_count: u64::try_from(*definition_count).ok()?,
            omitted_definition_count: u64::try_from(*omitted_owner_definitions).ok()?,
            target_resolution: match target_resolution {
                R::NotRequested => T::NotRequested,
                R::MetadataUnavailable => T::MetadataUnavailable,
                R::SelectedPackageUnavailable => T::SelectedPackageUnavailable,
                R::ManifestMismatch => T::ManifestMismatch,
                R::NoExactTarget => T::NoExactTarget,
                R::DuplicateExactTarget => T::DuplicateExactTarget,
                R::ExactCargoTarget => T::ExactCargoTarget,
                R::ExplicitUnitTarget => T::ExplicitUnitTarget,
            },
        }),
        _ => None,
    }
}

const fn unsupported_stage(
    source: backend_semantic::ir::SourceIdentity,
    language: backend_semantic::vocabulary::Language,
    stage: backend_semantic::vocabulary::Stage,
) -> CompilerTerminal {
    CompilerTerminal::UnsupportedStage {
        source: source_authority(source),
        cause: backend_semantic::vocabulary::FrontendError::UnsupportedStage { language, stage },
    }
}

const fn toolchain(
    source: backend_semantic::ir::SourceIdentity,
    language: backend_semantic::vocabulary::Language,
    stage: backend_semantic::vocabulary::Stage,
    selected: backend_semantic::vocabulary::NativeTool,
    configured: Option<backend_semantic::vocabulary::NativeTool>,
) -> CompilerTerminal {
    CompilerTerminal::Toolchain {
        source: source_authority(source),
        language,
        stage,
        selected,
        configured,
    }
}

const fn tooling_unavailable(
    source: backend_semantic::ir::SourceIdentity,
    language: backend_semantic::vocabulary::Language,
    stage: backend_semantic::vocabulary::Stage,
    tool: backend_semantic::vocabulary::NativeTool,
) -> CompilerTerminal {
    CompilerTerminal::ToolingUnavailable {
        source: source_authority(source),
        language,
        stage,
        tool,
    }
}

fn cancelled(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    CompilerTerminal::Cancelled {
        attempted: attempt(source_authority(source), recipe),
        diagnostic: compiler_diagnostic(diagnostic),
    }
}

fn native_work_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    phase: NativeWorkPhase,
    cause: NativeWorkError,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(native_work_cause(phase, cause)),
    )
}

fn native_work_cleanup_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    primary: NativeWorkPrimary<'_>,
    cleanup: NativeWorkError,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(NativeWorkCause::PrimaryAndCleanup {
            primary: native_primary(primary),
            cleanup: native_work_cleanup(cleanup),
        }),
    )
}

const fn native_primary_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    primary: NativePrimaryCause,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(NativeWorkCause::Primary(primary)),
    )
}

fn native_io_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    phase: NativeIoPhase,
    cause: &std::io::Error,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeIo {
            phase,
            cause: io_fact(cause),
        },
    )
}

fn deadline_exceeded(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::DeadlineExceeded {
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

fn diagnostic_limit(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    limit: usize,
    observed: usize,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

fn native_rejected(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    status: std::process::ExitStatus,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeRejected {
            code: status.code(),
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

const fn fragment_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    cause: FragmentCause,
) -> CompilerTerminal {
    compile_from_driver(source, recipe, CompilerCause::Fragment(cause))
}

fn fragment_failure_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    failure: CompilerFragmentFailure,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::FragmentFailure(Box::new(failure)),
    )
}

const fn configured_tool(
    fact: ToolchainSelectionFact,
) -> Option<backend_semantic::vocabulary::NativeTool> {
    match fact {
        ToolchainSelectionFact::ResolvedNative { tool } => Some(tool),
        ToolchainSelectionFact::ExplicitlyUnavailable { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::driver::{AuthorityDiagnostic, AuthorityFailure, CompileFailure};
    use backend_frontend_rust::legacy::{
        RustActiveHirRootInventory, RustAuthorityError, RustCargoTargetResolution,
    };
    use backend_frontend_typescript::legacy::{AuthorityError, with_analysis};
    use backend_library::interface::{
        CompilerCause, CompilerFragmentFailure, CompilerFragmentFault, CompilerTerminal,
        RustSourceScopeFailureKind, RustTargetResolutionKind,
    };
    use backend_semantic::ir::{
        BuildError, EntityId, FragmentError, LayoutStep, PrepareError, WriteError,
    };
    use backend_semantic::vocabulary::{
        AuthorityDiagnosticClass, AuthorityPhase, CompileRecipeFact, LanguageProfile, NativeTool,
        PythonVersion, Stage, TypeScriptSource,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    use super::{authority_terminal, compile_terminal};

    #[derive(Debug, thiserror::Error)]
    enum ProjectionError {
        #[error("the terminal did not stay a compile terminal")]
        Terminal,
        #[error("the terminal cause was not the lowering cause")]
        Cause,
        #[error("the extension-atom projection lost its operands")]
        ExtensionOperands,
        #[error("the fact-rejection projection lost its ordinal")]
        FactOrdinal,
    }

    fn lowering_cause(
        terminal: CompilerTerminal,
    ) -> Result<backend_semantic::vocabulary::LoweringUnsupported, ProjectionError> {
        let CompilerTerminal::Compile { cause, .. } = terminal else {
            return Err(ProjectionError::Terminal);
        };
        let CompilerCause::Lowering(cause) = cause else {
            return Err(ProjectionError::Cause);
        };
        Ok(*cause)
    }

    fn source_and_recipe() -> (backend_semantic::ir::SourceIdentity, CompileRecipeFact) {
        let source = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"projection"),
            byte_len: 10,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Python(PythonVersion::Python314),
            Stage::LowerIr,
            NativeTool::Python,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"projection-toolchain"),
        );
        (source, recipe)
    }

    fn fragment_failure(terminal: CompilerTerminal) -> CompilerFragmentFailure {
        let CompilerTerminal::Compile { cause, .. } = terminal else {
            panic!("fragment failures must remain compile terminals")
        };
        let CompilerCause::FragmentFailure(failure) = cause else {
            panic!("fragment failures must not be reclassified as authority failures")
        };
        *failure
    }

    #[test]
    fn compile_terminal_retains_each_concrete_fragment_error_family() {
        let (source, recipe) = source_and_recipe();
        let build = fragment_failure(compile_terminal(CompileFailure::Build {
            source_identity: source,
            recipe,
            cause: BuildError::InvalidOccurrenceSpan {
                owner: EntityId::new(4),
                start: 21,
                end: 29,
            },
        }));
        assert_eq!(
            build.fault(),
            &CompilerFragmentFault::Build(BuildError::InvalidOccurrenceSpan {
                owner: EntityId::new(4),
                start: 21,
                end: 29,
            })
        );

        let prepare = fragment_failure(compile_terminal(CompileFailure::Prepare {
            source_identity: source,
            recipe,
            cause: PrepareError::LayoutOverflow {
                step: LayoutStep::Occurrences,
                entity_count: 31,
                type_node_count: 47,
            },
        }));
        assert!(matches!(
            prepare.fault(),
            CompilerFragmentFault::Prepare(PrepareError::LayoutOverflow {
                step: LayoutStep::Occurrences,
                entity_count: 31,
                type_node_count: 47,
            })
        ));

        let write = fragment_failure(compile_terminal(CompileFailure::Write {
            source_identity: source,
            recipe,
            cause: WriteError::OutputTooSmall {
                required: 8192,
                available: 4096,
            },
        }));
        assert!(matches!(
            write.fault(),
            CompilerFragmentFault::Write(WriteError::OutputTooSmall {
                required: 8192,
                available: 4096,
            })
        ));

        let validate = fragment_failure(compile_terminal(CompileFailure::Validate {
            source_identity: source,
            recipe,
            cause: FragmentError::TruncatedHeader {
                required: 32,
                actual: 7,
            },
        }));
        assert!(matches!(
            validate.fault(),
            CompilerFragmentFault::Validate(FragmentError::TruncatedHeader {
                required: 32,
                actual: 7,
            })
        ));
    }

    #[test]
    fn extension_atom_unbound_projects_with_exact_operands() -> Result<(), ProjectionError> {
        let (source, recipe) = source_and_recipe();
        let failure = crate::driver::CompileFailure::ExtensionAtomUnbound {
            source_identity: source,
            recipe,
            row: 9,
            provisional: 12,
            atom_count: 11,
        };
        let cause = lowering_cause(super::compile_terminal(failure))?;
        let backend_semantic::vocabulary::LoweringUnsupported::ExtensionAtomUnbound {
            row,
            provisional,
            atom_count,
        } = cause
        else {
            return Err(ProjectionError::ExtensionOperands);
        };
        if row != 9 || provisional != 12 || atom_count != 11 {
            return Err(ProjectionError::ExtensionOperands);
        }
        Ok(())
    }

    #[test]
    fn fact_rejection_projects_with_the_exact_ordinal() -> Result<(), ProjectionError> {
        let (source, recipe) = source_and_recipe();
        let failure = crate::driver::CompileFailure::LoweringUnsupported {
            source_identity: source,
            recipe,
            cause: backend_semantic::vocabulary::LoweringUnsupported::FactRejected {
                fact: 17,
                name_len: 6,
                cause: backend_semantic::vocabulary::ProjectionAdmissionFault::Capacity,
            },
        };
        let cause = lowering_cause(super::compile_terminal(failure))?;
        let backend_semantic::vocabulary::LoweringUnsupported::FactRejected {
            fact,
            name_len,
            cause: backend_semantic::vocabulary::ProjectionAdmissionFault::Capacity,
        } = cause
        else {
            return Err(ProjectionError::FactOrdinal);
        };
        if fact != 17 || name_len != 6 {
            return Err(ProjectionError::FactOrdinal);
        }
        Ok(())
    }

    #[test]
    fn rust_source_scope_facts_survive_failure_codec_without_native_paths() {
        let (source, recipe) = source_and_recipe();
        let failure = AuthorityFailure::Rust {
            diagnostic: AuthorityDiagnostic::absent(),
            cause: RustAuthorityError::AmbiguousSourceOwner {
                path: "/private/workspace/src/lib.rs".into(),
                definition_count: 4,
                owner_root_sample: vec!["/private/workspace/src/lib.rs".into()].into_boxed_slice(),
                omitted_owner_definitions: 2,
                target_resolution: RustCargoTargetResolution::NoExactTarget,
            },
        };
        let terminal = authority_terminal(source, recipe, &failure);
        let CompilerTerminal::Compile {
            cause:
                CompilerCause::Authority {
                    class: AuthorityDiagnosticClass::SourceScope,
                    diagnostic: Some(diagnostic),
                    ..
                },
            ..
        } = &terminal
        else {
            panic!("Rust scope refusal remains typed through the terminal boundary");
        };
        assert_eq!(
            diagnostic.rust_source_scope_failure,
            Some(RustSourceScopeFailureKind::AmbiguousSourceOwner {
                definition_count: 4,
                omitted_definition_count: 2,
                target_resolution: RustTargetResolutionKind::NoExactTarget,
            })
        );
        assert_eq!(diagnostic.byte_len, 0, "no native output was supplied");

        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("src/lib.rs", &terminal)
                .expect("bounded refusal projection")
                .expect("Rust authority refusal");
        let encoded = failure
            .encode_bounded_json()
            .expect("canonical bounded refusal");
        let wire = String::from_utf8(encoded.clone()).expect("JSON UTF-8");
        assert!(!wire.contains("/private/workspace"));
        assert!(!wire.contains("native stderr"));
        let reopened = backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("cold reader preserves closed Rust scope facts");
        let backend_library::PackageCompilerFailureCause::Authority {
            diagnostic: Some(facts),
            ..
        } = reopened.cause()
        else {
            panic!("Rust scope diagnostic is retained in the public failure");
        };
        assert_eq!(
            facts.rust_source_scope_failure,
            Some(RustSourceScopeFailureKind::AmbiguousSourceOwner {
                definition_count: 4,
                omitted_definition_count: 2,
                target_resolution: RustTargetResolutionKind::NoExactTarget,
            })
        );
        assert!(reopened.detail().contains("4 active owner definitions"));
        assert!(reopened.detail().contains("no_exact_target"));
        assert!(!reopened.detail().contains("/private/workspace"));

        let detached = RustAuthorityError::DetachedSource {
            path: "/private/workspace/src/detached.rs".into(),
            active_hir_roots: RustActiveHirRootInventory {
                package_crate_count: 3,
                package_relative_roots: vec!["src/lib.rs".into()].into_boxed_slice(),
                omitted_package_crates: 2,
            },
        };
        assert_eq!(
            super::rust_source_scope_failure(&detached),
            Some(RustSourceScopeFailureKind::DetachedSource {
                active_hir_root_count: 3,
            })
        );
    }

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("OXC accepted a syntactically malformed TypeScript fixture")]
        OxcAccepted,
        #[error("OXC returned a non-syntax terminal for malformed TypeScript")]
        OxcFailure,
        #[error("OXC syntax terminal had no retained diagnostics")]
        OxcDiagnostics,
        #[error("bounded authority diagnostic construction failed")]
        Diagnostic,
        #[error("authority terminal did not remain a compile terminal")]
        Terminal,
        #[error("authority terminal changed its source or recipe authority")]
        Attempt,
        #[error("authority terminal lost its OXC parse/syntax classification")]
        Classification,
        #[error("authority terminal lost its bounded primary diagnostic")]
        Projection,
    }

    #[test]
    fn actual_oxc_syntax_failure_projects_to_application_terminal() -> Result<(), TestError> {
        let source_bytes = b"const =;";
        let source_text = "const =;";
        let analyzed = with_analysis(TypeScriptSource::TypeScript, source_text, |_| ());
        let Err(cause) = analyzed else {
            return Err(TestError::OxcAccepted);
        };
        let AuthorityError::Syntax { diagnostics } = cause else {
            return Err(TestError::OxcFailure);
        };
        if diagnostics.is_empty() {
            return Err(TestError::OxcDiagnostics);
        }
        let Ok(diagnostic) = AuthorityDiagnostic::new(b"syntax", 6, false) else {
            return Err(TestError::Diagnostic);
        };
        let source = backend_semantic::ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: 8,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"oxc-authority"),
        );
        let failure = AuthorityFailure::TypeScript {
            diagnostic,
            cause: AuthorityError::Syntax { diagnostics },
        };
        let terminal = authority_terminal(source, recipe, &failure);
        let CompilerTerminal::Compile { attempted, cause } = terminal else {
            return Err(TestError::Terminal);
        };
        if attempted.source.identity != source.identity
            || attempted.source.byte_len != source.byte_len
            || attempted.recipe != recipe.identity
        {
            return Err(TestError::Attempt);
        }
        let CompilerCause::Authority {
            phase,
            class,
            diagnostic,
        } = cause
        else {
            return Err(TestError::Terminal);
        };
        if phase != AuthorityPhase::Parse || class != AuthorityDiagnosticClass::Syntax {
            return Err(TestError::Classification);
        }
        let Some(diagnostic) = diagnostic else {
            return Err(TestError::Projection);
        };
        if diagnostic.byte_len != 6
            || diagnostic.observed != 6
            || diagnostic.truncated
            || diagnostic.bytes.get(..diagnostic.byte_len) != Some(b"syntax")
        {
            return Err(TestError::Projection);
        }
        Ok(())
    }

    #[test]
    fn checked_tsz_query_stops_retain_typed_native_terminal_diagnostics()
    -> Result<(), Box<dyn std::error::Error>> {
        use backend_frontend_typescript::{
            TszAuthorityError, TszCheckerOptions, TszEnvironmentFingerprint,
            TszExecutionCheckpoint, TszFileInput, TszProjectAuthority, TszProjectCheckerExecution,
            TszProjectExecutionStop, TszProjectOptions, TszProjectQuerySessionError,
            TszProjectSemanticOptions,
        };
        use backend_library::interface::{
            TypeScriptAuthorityFailureKind as F, TypeScriptStopPhase, TypeScriptStopSite,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ArmedStop {
            armed: AtomicBool,
            reason: TszProjectExecutionStop,
        }
        impl TszExecutionCheckpoint for ArmedStop {
            fn checkpoint(&self, _: u64) -> Result<(), TszProjectExecutionStop> {
                if self.armed.load(Ordering::Acquire) {
                    Err(self.reason)
                } else {
                    Ok(())
                }
            }
        }
        let text = "export const value: number = 1;";
        let source = backend_semantic::ir::SourceIdentity::from_bytes(text.as_bytes())
            .ok_or("fixture source length")?;
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"native-query-transport"),
        );
        for (reason, expected) in [
            (
                TszProjectExecutionStop::Cancelled,
                F::NativeSessionCancelled,
            ),
            (TszProjectExecutionStop::Deadline, F::NativeSessionDeadline),
            (
                TszProjectExecutionStop::WorkBudgetExhausted,
                F::NativeSessionWorkBudgetExhausted,
            ),
        ] {
            let checkpoint = ArmedStop {
                armed: AtomicBool::new(false),
                reason,
            };
            let mut authority = TszProjectAuthority::new();
            authority.update_with_checker_execution(
                vec![TszFileInput {
                    path: "src/value.ts".into(),
                    source: text.into(),
                }],
                TszProjectOptions {
                    checker: TszCheckerOptions {
                        no_lib: true,
                        ..TszCheckerOptions::default()
                    },
                    semantic_options: TszProjectSemanticOptions::declaration_scoped(),
                    program_diagnostics: Vec::new(),
                    module_resolutions: Vec::new(),
                    file_formats: Vec::new(),
                    environment: TszEnvironmentFingerprint::from_sha256([0x61; 32]),
                },
                &[],
                TszProjectCheckerExecution {
                    checkpoint: &checkpoint,
                    workers: std::num::NonZeroUsize::MIN,
                },
            )?;
            let project = authority.project().ok_or("checked TSZ project missing")?;
            let session = project.checked_query_session(&checkpoint)?;
            let index = session.file_index("src/value.ts")?;
            let entered = AtomicBool::new(false);
            checkpoint.armed.store(true, Ordering::Release);
            let Err(error) = session.with_file_checker_and_types(index, |_, _, _, _| {
                entered.store(true, Ordering::Release);
            }) else {
                return Err("a stopped real query exposed checker authority".into());
            };
            assert_eq!(error, TszProjectQuerySessionError::ExecutionStopped(reason));
            assert!(!entered.load(Ordering::Acquire));
            let terminal = compile_terminal(CompileFailure::Authority {
                source_identity: source,
                recipe,
                failure: AuthorityFailure::TypeScriptTsz {
                    diagnostic: AuthorityDiagnostic::absent(),
                    cause: TszAuthorityError::ProjectCheckerSession(error),
                },
            });
            // Exercise the public refusal codec, not just a borrowed driver value.
            let failure = backend_library::PackageCompilerFailure::from_package_terminal(
                "src/value.ts",
                &terminal,
            )
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .ok_or("native query refusal was not publishable")?;
            let reply = backend_library::ReplyDto::new(
                1,
                backend_library::CommandReply::Failed(
                    backend_library::CommandFailure::CompilerRefused {
                        detail: failure.detail().to_owned(),
                        failure,
                    },
                ),
            );
            let bytes = serde_json::to_vec(&reply)?;
            assert_eq!(backend_library::decode_reply_body(&bytes)?, reply);
            assert!(String::from_utf8_lossy(&bytes).contains(expected.kind_tag()));
            let CompilerTerminal::Compile { attempted, cause } = terminal else {
                return Err("native query refusal lost its compile terminal".into());
            };
            assert_eq!(attempted.source.identity, source.identity);
            assert_eq!(attempted.source.byte_len, source.byte_len);
            assert_eq!(attempted.recipe, recipe.identity);
            let CompilerCause::Authority {
                phase,
                class,
                diagnostic: Some(diagnostic),
            } = cause
            else {
                return Err("native query refusal lost its diagnostic".into());
            };
            assert_eq!(phase, AuthorityPhase::Project);
            assert_eq!(class, AuthorityDiagnosticClass::Projection);
            assert_eq!(diagnostic.typescript_failure, Some(expected));
            assert_eq!(
                diagnostic.bytes.get(..diagnostic.byte_len),
                Some(expected.detail().as_bytes())
            );
            let diagnostic = diagnostic.with_typescript_stop_site(TypeScriptStopSite::new(
                Some(TypeScriptStopPhase::FileQuery),
                Some("src/value.ts"),
                1,
                10,
            ));
            assert!(diagnostic.typescript_stop_site.is_some());
        }
        Ok(())
    }

    #[test]
    fn tsz_nonstop_refusal_keeps_original_bytes_without_inventing_a_stop()
    -> Result<(), Box<dyn std::error::Error>> {
        use backend_frontend_typescript::TszAuthorityError;
        use backend_library::interface::{
            TypeScriptAuthorityFailureKind as F, TypeScriptStopPhase, TypeScriptStopSite,
        };
        let text = "export const value: number = 1;";
        let source = backend_semantic::ir::SourceIdentity::from_bytes(text.as_bytes())
            .ok_or("fixture source length")?;
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"native-query-transport"),
        );
        let bytes = b"retained authority diagnostic";
        let failure = AuthorityFailure::TypeScriptTsz {
            diagnostic: AuthorityDiagnostic::new(bytes, bytes.len(), false)
                .map_err(|_| "fixture diagnostic extent")?,
            cause: TszAuthorityError::SourceMismatch {
                path: "private/source.ts".into(),
            },
        };
        let CompilerTerminal::Compile { cause, .. } = authority_terminal(source, recipe, &failure)
        else {
            return Err("TSZ refusal changed terminal family".into());
        };
        let CompilerCause::Authority {
            diagnostic: Some(diagnostic),
            ..
        } = cause
        else {
            return Err("TSZ refusal lost its diagnostic".into());
        };
        assert_eq!(diagnostic.typescript_failure, Some(F::NativeSourceMismatch));
        assert_eq!(
            diagnostic.bytes.get(..diagnostic.byte_len),
            Some(bytes.as_slice())
        );
        assert_eq!(diagnostic.observed, bytes.len());
        assert!(!diagnostic.truncated);
        let diagnostic = diagnostic.with_typescript_stop_site(TypeScriptStopSite::new(
            Some(TypeScriptStopPhase::FileQuery),
            Some("src/value.ts"),
            1,
            10,
        ));
        assert!(diagnostic.typescript_stop_site.is_none());
        Ok(())
    }
}
