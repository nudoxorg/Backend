//! Defines types compile behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{FragmentView, PrepareError};
use compiler_registry::{AdapterRoute, FullRegistry};
use compiler_vocabulary::{Language, Stage};
use heart_identity::{ContentId, SourceFactDomain};

use crate::{
    lower::{self, AdmissionFault},
    native::parse_with_native_tool,
};

use super::{
    CompileFailure, CompileOutput, CompileRecipeFact, CompileRequest, CompileScratch,
    CompiledFragment, NativeRecipe, ResolvedToolchain, SourceIdentity, ToolchainSelection,
    ToolchainSelectionFact,
};

pub fn compile<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    let source = source_identity(request.source)?;
    let route = FullRegistry
        .route(request.language, request.stage)
        .map_err(|cause| CompileFailure::UnsupportedStage {
            source_identity: source,
            language: request.language,
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
                    language: request.language,
                    stage: request.stage,
                    selected: tool,
                    provided: request.toolchain.fact(),
                });
            }
            return Err(CompileFailure::ToolingUnavailable {
                source_identity: source,
                language: request.language,
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
                language: request.language,
                stage: request.stage,
                selected,
                provided: request.toolchain.fact(),
            });
        }
    };
    if selected != resolved.tool {
        return Err(CompileFailure::ToolchainMismatch {
            source_identity: source,
            language: request.language,
            stage: request.stage,
            selected,
            resolved: resolved.tool,
        });
    }
    let recipe = recipe_fact(request.language, request.stage, resolved, source);
    let native_recipe = NativeRecipe {
        language: request.language,
        stage: request.stage,
        source: request.source,
        toolchain: resolved,
    };
    parse_with_native_tool(native_recipe, source, recipe, scratch, request.control)?;
    let mut facts = lower::FactSet::new();
    let mut unsupported = lower::UnsupportedLane::new();
    lower::emit(
        request.language,
        request.source,
        &mut facts,
        &mut unsupported,
    )
    .map_err(|cause| CompileFailure::LoweringUnsupported {
        source_identity: source,
        recipe,
        cause,
    })?;
    let bytes =
        lower::admit(&facts, source, recipe, output.fragment_output).map_err(
            |fault| match fault {
                AdmissionFault::Canonical(cause) => CompileFailure::Prepare {
                    source_identity: source,
                    recipe,
                    cause: PrepareError::SemanticData { cause },
                },
                AdmissionFault::Prepare(cause) => CompileFailure::Prepare {
                    source_identity: source,
                    recipe,
                    cause,
                },
                AdmissionFault::Write(cause) => CompileFailure::Write {
                    source_identity: source,
                    recipe,
                    cause,
                },
            },
        )?;
    let fragment = FragmentView::validate(bytes).map_err(|cause| CompileFailure::Validate {
        source_identity: source,
        recipe,
        cause,
    })?;
    Ok(CompiledFragment {
        source,
        recipe,
        fragment,
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
    language: Language,
    stage: Stage,
    toolchain: ResolvedToolchain<'_>,
    source: SourceIdentity,
) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        language,
        stage,
        toolchain.tool,
        source.identity,
        toolchain.identity,
    )
}
