//! Defines types compile behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{FragmentView, PrepareError};
use compiler_registry::{AdapterRoute, FullRegistry};
use compiler_vocabulary::{Language, LanguageProfile, Stage};
use heart_identity::{ContentId, SourceFactDomain};
use std::sync::atomic::Ordering;

use crate::{
    lower::{self, AdmissionFault, typescript::TypeScriptCollectError},
    native::parse_with_native_tool,
};

use super::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, CompileFailure, CompileOutput,
    CompileRecipeFact, CompileRequest, CompileScratch, CompiledFragment, NativeDiagnostic,
    NativeRecipe, ResolvedToolchain, SemanticAuthorityInput, SourceIdentity, ToolchainSelection,
    ToolchainSelectionFact,
};

pub fn compile<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    let mut facts = lower::FactSet::new();
    let prepared = match prepare(request)? {
        PreparedRoute::Direct(prepared) => {
            emit_facts(
                &prepared,
                request.source,
                request.control.cancelled,
                request.authority,
                Some(scratch.diagnostic_output),
                &mut facts,
            )?;
            prepared
        }
        PreparedRoute::Native {
            prepared,
            native_recipe,
        } => {
            parse_with_native_tool(
                native_recipe,
                prepared.source,
                prepared.recipe,
                scratch,
                request.control,
            )?;
            emit_facts(
                &prepared,
                request.source,
                request.control.cancelled,
                request.authority,
                None,
                &mut facts,
            )?;
            prepared
        }
    };
    let bytes = lower::admit(
        &facts,
        prepared.source,
        prepared.recipe,
        request.profile,
        output.fragment_output,
    )
    .map_err(|fault| match fault {
        AdmissionFault::Canonical(cause) => CompileFailure::Prepare {
            source_identity: prepared.source,
            recipe: prepared.recipe,
            cause: PrepareError::SemanticData { cause },
        },
        AdmissionFault::Prepare(cause) => CompileFailure::Prepare {
            source_identity: prepared.source,
            recipe: prepared.recipe,
            cause,
        },
        AdmissionFault::Write(cause) => CompileFailure::Write {
            source_identity: prepared.source,
            recipe: prepared.recipe,
            cause,
        },
        AdmissionFault::ExtensionAtom {
            row,
            provisional,
            atom_count,
        } => CompileFailure::ExtensionAtomUnbound {
            source_identity: prepared.source,
            recipe: prepared.recipe,
            row,
            provisional,
            atom_count,
        },
    })?;
    let fragment = FragmentView::validate(bytes).map_err(|cause| CompileFailure::Validate {
        source_identity: prepared.source,
        recipe: prepared.recipe,
        cause,
    })?;
    Ok(CompiledFragment {
        source: prepared.source,
        recipe: prepared.recipe,
        fragment,
    })
}

/// Materializes a queryable semantic image from the exact admission lane used
/// to write the durable canonical fragment.
pub fn compile_ir<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<super::CompiledIr, CompileFailure<'diagnostic>> {
    let mut facts = lower::FactSet::new();
    let prepared = match prepare(request)? {
        PreparedRoute::Direct(prepared) => {
            emit_facts(
                &prepared,
                request.source,
                request.control.cancelled,
                request.authority,
                Some(scratch.diagnostic_output),
                &mut facts,
            )?;
            prepared
        }
        PreparedRoute::Native {
            prepared,
            native_recipe,
        } => {
            parse_with_native_tool(
                native_recipe,
                prepared.source,
                prepared.recipe,
                scratch,
                request.control,
            )?;
            emit_facts(
                &prepared,
                request.source,
                request.control.cancelled,
                request.authority,
                None,
                &mut facts,
            )?;
            prepared
        }
    };
    let ir = facts
        .build_ir(request.profile, prepared.source)
        .map_err(|cause| CompileFailure::Build {
            source_identity: prepared.source,
            recipe: prepared.recipe,
            cause,
        })?;
    Ok(super::CompiledIr {
        source: prepared.source,
        recipe: prepared.recipe,
        ir,
    })
}

struct PreparedCompile {
    source: SourceIdentity,
    recipe: CompileRecipeFact,
}

enum PreparedRoute<'source, 'toolchain> {
    Direct(PreparedCompile),
    Native {
        prepared: PreparedCompile,
        native_recipe: NativeRecipe<'source, 'toolchain>,
    },
}

fn prepare<'source, 'toolchain, 'cancel, 'diagnostic>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
) -> Result<PreparedRoute<'source, 'toolchain>, CompileFailure<'diagnostic>> {
    let source = source_identity(request.source)?;
    let language = Language::from(request.profile);
    let route = FullRegistry
        .route(language, request.stage)
        .map_err(|cause| CompileFailure::UnsupportedStage {
            source_identity: source,
            language,
            stage: request.stage,
            cause,
        })?;
    let selected = match route {
        AdapterRoute::Native { tool } => tool,
        AdapterRoute::ToolingUnavailable { tool } => {
            if request.toolchain.fact() != (ToolchainSelectionFact::ExplicitlyUnavailable { tool })
            {
                return Err(CompileFailure::ToolchainSelectionMismatch {
                    source_identity: source,
                    language,
                    stage: request.stage,
                    selected: tool,
                    provided: request.toolchain.fact(),
                });
            }
            return Err(CompileFailure::ToolingUnavailable {
                source_identity: source,
                language,
                stage: request.stage,
                tool,
            });
        }
    };
    let resolved = match request.toolchain {
        ToolchainSelection::ResolvedNative(resolved) => resolved,
        ToolchainSelection::ExplicitlyUnavailable { .. } => {
            return Err(CompileFailure::ToolchainSelectionMismatch {
                source_identity: source,
                language,
                stage: request.stage,
                selected,
                provided: request.toolchain.fact(),
            });
        }
    };
    if selected != resolved.tool {
        return Err(CompileFailure::ToolchainMismatch {
            source_identity: source,
            language,
            stage: request.stage,
            selected,
            resolved: resolved.tool,
        });
    }
    let recipe = recipe_fact(request.profile, request.stage, resolved, source);
    let authority_profile_mismatch = match (request.authority, request.profile) {
        (SemanticAuthorityInput::Rust { .. }, profile) => {
            !matches!(profile, LanguageProfile::Rust(_))
        }
        (SemanticAuthorityInput::Go { .. }, profile) => !matches!(profile, LanguageProfile::Go(_)),
        (SemanticAuthorityInput::CSharp { .. }, profile) => {
            !matches!(profile, LanguageProfile::CSharp(_))
        }
        (SemanticAuthorityInput::Java { .. }, profile) => {
            !matches!(profile, LanguageProfile::Java(_))
        }
        (SemanticAuthorityInput::None, _) => false,
    };
    if authority_profile_mismatch {
        return Err(CompileFailure::AuthorityInputProfileMismatch {
            source_identity: source,
            recipe,
            profile: request.profile,
        });
    }
    let requires_project_authority = matches!(
        request.profile,
        LanguageProfile::Go(_) | LanguageProfile::CSharp(_) | LanguageProfile::Java(_)
    );
    if requires_project_authority && matches!(request.authority, SemanticAuthorityInput::None) {
        return Err(CompileFailure::AuthorityInputRequired {
            source_identity: source,
            recipe,
            profile: request.profile,
        });
    }
    if request.control.cancelled.load(Ordering::Acquire) {
        return Err(CompileFailure::Cancelled {
            source_identity: source,
            recipe,
            diagnostic: NativeDiagnostic {
                bytes: &[],
                observed: 0,
                truncated: false,
            },
        });
    }
    if std::time::Instant::now() >= request.control.deadline {
        return Err(CompileFailure::DeadlineExceeded {
            source_identity: source,
            recipe,
            diagnostic: NativeDiagnostic {
                bytes: &[],
                observed: 0,
                truncated: false,
            },
        });
    }
    let native_recipe = NativeRecipe {
        profile: request.profile,
        stage: request.stage,
        source: request.source,
        toolchain: resolved,
    };
    let direct_authority = matches!(
        request.profile,
        LanguageProfile::C(_)
            | LanguageProfile::Cxx(_)
            | LanguageProfile::TypeScript(_)
            | LanguageProfile::Python(_)
    );
    let direct_authority = direct_authority
        || matches!(
            request.authority,
            SemanticAuthorityInput::Rust { .. }
                | SemanticAuthorityInput::Go { .. }
                | SemanticAuthorityInput::CSharp { .. }
                | SemanticAuthorityInput::Java { .. }
        );
    let prepared = PreparedCompile { source, recipe };
    if direct_authority {
        Ok(PreparedRoute::Direct(prepared))
    } else {
        Ok(PreparedRoute::Native {
            prepared,
            native_recipe,
        })
    }
}

fn source_identity<'diagnostic>(
    source_bytes: &[u8],
) -> Result<SourceIdentity, CompileFailure<'diagnostic>> {
    let byte_len =
        u32::try_from(source_bytes.len()).map_err(|source| CompileFailure::SourceLength {
            actual: source_bytes.len(),
            source,
        })?;
    Ok(SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len,
    })
}

fn recipe_fact(
    profile: LanguageProfile,
    stage: Stage,
    toolchain: ResolvedToolchain<'_>,
    source: SourceIdentity,
) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        profile,
        stage,
        toolchain.tool,
        source.identity,
        toolchain.identity,
    )
}

fn emit_facts<'source, 'diagnostic>(
    prepared: &PreparedCompile,
    source: &'source [u8],
    cancelled: &std::sync::atomic::AtomicBool,
    authority: SemanticAuthorityInput<'source>,
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    facts: &mut lower::FactSet<'source>,
) -> Result<(), CompileFailure<'diagnostic>> {
    match prepared.recipe.profile {
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) => {
            lower::clang::collect(prepared.recipe.profile, source, cancelled, facts)
                .map_err(|cause| clang_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::TypeScript(profile) => {
            lower::typescript::collect(profile, source, facts).map_err(|cause| {
                typescript_terminal(
                    diagnostic_output,
                    source,
                    prepared.source,
                    prepared.recipe,
                    cause,
                )
            })?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::Python(profile) => {
            lower::python::collect(profile, source, facts)
                .map_err(|cause| python_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::Rust(profile) => {
            let SemanticAuthorityInput::Rust {
                project,
                maximum_source_bytes,
            } = authority
            else {
                return Err(CompileFailure::AuthorityInputRequired {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    profile: LanguageProfile::Rust(profile),
                });
            };
            lower::rust::collect(project, maximum_source_bytes, cancelled, source, facts)
                .map_err(|cause| rust_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::Go(profile) => {
            let SemanticAuthorityInput::Go { image } = authority else {
                return Err(CompileFailure::AuthorityInputRequired {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    profile: LanguageProfile::Go(profile),
                });
            };
            lower::go::collect(source, image, facts)
                .map_err(|cause| go_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::Java(profile) => {
            let SemanticAuthorityInput::Java { image } = authority else {
                return Err(CompileFailure::AuthorityInputRequired {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    profile: LanguageProfile::Java(profile),
                });
            };
            lower::java::collect(profile, source, image, facts)
                .map_err(|cause| java_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
        LanguageProfile::CSharp(profile) => {
            let SemanticAuthorityInput::CSharp { image } = authority else {
                return Err(CompileFailure::AuthorityInputRequired {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    profile: LanguageProfile::CSharp(profile),
                });
            };
            lower::csharp::collect(source, image, facts)
                .map_err(|cause| csharp_terminal(prepared.source, prepared.recipe, cause))?;
            if facts.len() == 0 {
                return Err(CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause: compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
                });
            }
            Ok(())
        }
    }
}

fn python_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::python::PythonCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::python::PythonCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Python {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::python::PythonCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
        lower::python::PythonCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::PythonSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
    }
}

fn rust_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::rust::RustCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::rust::RustCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Rust {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::rust::RustCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn go_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::go::GoCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::go::GoCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::GoImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::go::GoCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::GoSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::go::GoCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn csharp_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::csharp::CSharpCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::csharp::CSharpCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::csharp::CSharpCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::CSharpSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::csharp::CSharpCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        lower::csharp::CSharpCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn java_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::java::JavaCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::java::JavaCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaBoundImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::java::JavaCollectError::Release {
            requested,
            observed,
        } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaRelease {
                diagnostic: AuthorityDiagnostic::absent(),
                requested,
                observed,
            },
        },
        lower::java::JavaCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::JavaSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::java::JavaCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn clang_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::clang::ClangCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::clang::ClangCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Clang {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::clang::ClangCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn typescript_terminal<'diagnostic>(
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: TypeScriptCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        TypeScriptCollectError::Utf8(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptUtf8 {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        TypeScriptCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScript {
                diagnostic: typescript_diagnostic(diagnostic_output, source, &cause),
                cause,
            },
        },
        TypeScriptCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        TypeScriptCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn typescript_diagnostic<'diagnostic>(
    output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    cause: &compiler_languages_typescript::AuthorityError,
) -> AuthorityDiagnostic<'diagnostic> {
    let Some(span) = cause.primary_span() else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(start) = usize::try_from(span.start) else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(end) = usize::try_from(span.end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(primary) = source.get(start..end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(output) = output else {
        return AuthorityDiagnostic::absent();
    };
    let retained = primary.len().min(output.len());
    let (Some(source), Some(destination)) = (primary.get(..retained), output.get_mut(..retained))
    else {
        return AuthorityDiagnostic::absent();
    };
    destination.copy_from_slice(source);
    match AuthorityDiagnostic::new(destination, primary.len(), retained != primary.len()) {
        Ok(diagnostic) => diagnostic,
        Err(AuthorityDiagnosticFault::PrefixExceedsObserved { .. })
        | Err(AuthorityDiagnosticFault::TruncationMismatch { .. }) => AuthorityDiagnostic::absent(),
    }
}
