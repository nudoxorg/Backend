use nudox_compile_registry::{AdapterRoute, FullRegistry};
use nudox_compile_vocab::{Language, Stage};
use nudox_id::{ContentId, SourceFactDomain};
use nudox_ir_format::{AtomInput, EntityRecord, FragmentView, PreparedFragment, TypeNode};
use nudox_ir_vocab::{AtomId, TypeId};

use crate::{lower::declaration, native::parse_with_native_tool};

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
    let declaration = declaration(request.language, request.source).map_err(|cause| {
        CompileFailure::LoweringUnsupported {
            source_identity: source,
            recipe,
            cause,
        }
    })?;
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: declaration.kind,
    }];
    let nodes = [TypeNode::Primitive(declaration.semantic_type)];
    let atoms = [AtomInput {
        bytes: declaration.name,
    }];
    let prepared =
        PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms).map_err(|cause| {
            CompileFailure::Prepare {
                source_identity: source,
                recipe,
                cause,
            }
        })?;
    let bytes = prepared
        .write_into(output.fragment_output)
        .map_err(|cause| CompileFailure::Write {
            source_identity: source,
            recipe,
            cause,
        })?;
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
