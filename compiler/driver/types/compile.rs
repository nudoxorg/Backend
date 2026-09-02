//! Defines types compile behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{FragmentView, PrepareError};
use compiler_registry::{AdapterRoute, FullRegistry};
use compiler_vocabulary::{Language, LanguageProfile, Stage};
use heart_identity::{ContentId, SourceFactDomain};

use crate::{
    lower::{self, AdmissionFault, typescript::TypeScriptCollectError},
    native::parse_with_native_tool,
};

use super::{
    AuthorityDiagnostic, AuthorityFailure, CompileFailure, CompileOutput, CompileRecipeFact,
    CompileRequest, CompileScratch, CompiledFragment, NativeRecipe, ResolvedToolchain,
    SemanticAuthorityInput, SourceIdentity, ToolchainSelection, ToolchainSelectionFact,
};

pub fn compile<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    let prepared = prepare(request, scratch)?;
    let mut facts = lower::FactSet::new();
    let mut unsupported = lower::UnsupportedLane::new();
    emit_facts(
        &prepared,
        request.source,
        request.control.cancelled,
        request.authority,
        &mut facts,
        &mut unsupported,
    )?;
    let bytes = lower::admit(
        &facts,
        prepared.source,
        prepared.recipe,
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
    let prepared = prepare(request, scratch)?;
    let mut facts = lower::FactSet::new();
    let mut unsupported = lower::UnsupportedLane::new();
    emit_facts(
        &prepared,
        request.source,
        request.control.cancelled,
        request.authority,
        &mut facts,
        &mut unsupported,
    )?;
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
    language: Language,
}

fn prepare<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<PreparedCompile, CompileFailure<'diagnostic>> {
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
    if matches!(request.authority, SemanticAuthorityInput::Rust { .. })
        && !matches!(request.profile, LanguageProfile::Rust(_))
    {
        return Err(CompileFailure::AuthorityInputProfileMismatch {
            source_identity: source,
            recipe,
            profile: request.profile,
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
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) | LanguageProfile::Python(_)
    ) || matches!(request.authority, SemanticAuthorityInput::Rust { .. });
    if !direct_authority {
        parse_with_native_tool(native_recipe, source, recipe, scratch, request.control)?;
    }
    Ok(PreparedCompile {
        source,
        recipe,
        language,
    })
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
    facts: &mut lower::FactSet<'source>,
    unsupported: &mut lower::UnsupportedLane<'source>,
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
            lower::typescript::collect(profile, source, facts)
                .map_err(|cause| typescript_terminal(prepared.source, prepared.recipe, cause))?;
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
        LanguageProfile::Go(_) | LanguageProfile::Java(_) | LanguageProfile::CSharp(_) => {
            lower::emit(prepared.language, source, facts, unsupported).map_err(|cause| {
                CompileFailure::LoweringUnsupported {
                    source_identity: prepared.source,
                    recipe: prepared.recipe,
                    cause,
                }
            })
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
                diagnostic: AuthorityDiagnostic::absent(),
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
