//! Defines types compile behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{AtomId, TypeId};
use compiler_ir::{
    AtomInput, BuiltinType, ConcreteType, EntityRecord, EntityVersion, FragmentView, IrBuilder,
    ItemKind, PayloadHash, PreparedFragment, StableEntityId, TreeItemInput, TypeNode, Visibility,
};
use compiler_registry::{AdapterRoute, FullRegistry};
use compiler_vocabulary::{Language, LanguageProfile, Stage};
use heart_identity::{ContentId, SourceFactDomain};

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
    let lowered = lower(request, scratch)?;
    let source = lowered.source;
    let recipe = lowered.recipe;
    let declaration = lowered.declaration;
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

/// Compiles directly into the one canonical semantic representation.
pub fn compile_ir<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<super::CompiledIr, CompileFailure<'diagnostic>> {
    let lowered = lower(request, scratch)?;
    let source = lowered.source;
    let recipe = lowered.recipe;
    let declaration = lowered.declaration;
    let mut builder = IrBuilder::new();
    let version = EntityVersion {
        stable: StableEntityId::from_canonical_bytes(source.identity.as_ref()),
        payload: PayloadHash::from_canonical_bytes(request.source),
    };
    let versions = [version];
    let mut tree = builder
        .reserve_tree(&versions)
        .map_err(|cause| CompileFailure::Build {
            source_identity: source,
            recipe,
            cause,
        })?;
    let semantic_type = tree
        .intern_concrete(ConcreteType::Builtin(builtin_type(
            declaration.semantic_type,
        )))
        .map_err(|cause| CompileFailure::Build {
            source_identity: source,
            recipe,
            cause,
        })?;
    let items = [TreeItemInput {
        name: declaration.name,
        kind: item_kind(declaration.kind),
        visibility: Visibility::Public,
        parent: None,
        semantic_type: Some(semantic_type.erase()),
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        typescript: None,
    }];
    tree.commit(&items, &[])
        .map_err(|cause| CompileFailure::Build {
            source_identity: source,
            recipe,
            cause,
        })?;
    let ir = builder.finish().map_err(|cause| CompileFailure::Build {
        source_identity: source,
        recipe,
        cause,
    })?;
    Ok(super::CompiledIr { source, recipe, ir })
}

struct Lowered<'source> {
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    declaration: crate::lower::Declaration<'source>,
}

fn lower<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<Lowered<'source>, CompileFailure<'diagnostic>> {
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
    let native_recipe = NativeRecipe {
        profile: request.profile,
        stage: request.stage,
        source: request.source,
        toolchain: resolved,
    };
    parse_with_native_tool(native_recipe, source, recipe, scratch, request.control)?;
    let declaration = declaration(language, request.source).map_err(|cause| {
        CompileFailure::LoweringUnsupported {
            source_identity: source,
            recipe,
            cause,
        }
    })?;
    Ok(Lowered {
        source,
        recipe,
        declaration,
    })
}

const fn builtin_type(primitive: compiler_ir::PrimitiveType) -> BuiltinType {
    match primitive {
        compiler_ir::PrimitiveType::Bool => BuiltinType::Bool,
        compiler_ir::PrimitiveType::I32 => BuiltinType::I32,
        compiler_ir::PrimitiveType::String => BuiltinType::String,
    }
}

const fn item_kind(kind: compiler_ir::EntityKind) -> ItemKind {
    match kind {
        compiler_ir::EntityKind::Function => ItemKind::Function,
        compiler_ir::EntityKind::Constant => ItemKind::Constant,
        compiler_ir::EntityKind::Record => ItemKind::Record,
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
