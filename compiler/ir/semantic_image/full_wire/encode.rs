//! Infallible full semantic-image write phase.

use crate::{ImageProvenance, SemanticImageAuthority};

use super::{
    fault::FullSemanticImageFault,
    plan::{CanonicalVariablePool, FullExtensionPayloads, FullSemanticImagePlan},
    wire::{
        put_u16, put_u32, FullDirectoryKind, ATOM_ROW_BYTES, DIRECTORY_BYTES, HEADER_BYTES,
        HEADER_BYTES_U32, MAGIC, RANGE_ROW_BYTES, SCHEMA, SPARSE_BINDING_ROW_BYTES,
        TYPED_EDGE_ROW_BYTES, TYPED_NODE_ROW_BYTES,
    },
};

/// Measures the complete portable semantic image without touching caller
/// bytes.  It remains crate-private until full validation and a borrowed
/// `SemanticReader` view land in the same public transaction.
pub(super) fn full_semantic_image_len(
    ir: &crate::Ir,
) -> Result<usize, super::full::FullPlanError> {
    Ok(FullSemanticImagePlan::build(ir)?.required)
}

/// Writes a fully prepared portable image. All fallible preparation occurs
/// before `output` is borrowed mutably; a short caller buffer is unchanged and
/// a successful prepared plan writes exactly its measured prefix.
pub(super) fn encode_full_semantic_image(
    ir: &crate::Ir,
    output: &mut [u8],
) -> Result<usize, super::full::FullPlanError> {
    let plan = FullSemanticImagePlan::build(ir)?;
    if output.len() < plan.required {
        return Err(FullSemanticImageFault::OutputTooShort {
            required: plan.required,
            actual: output.len(),
        }.into());
    }
    write_plan(output, &plan);
    Ok(plan.required)
}

fn write_plan(output: &mut [u8], plan: &FullSemanticImagePlan<'_>) {
    output[..plan.required].fill(0);
    output[..4].copy_from_slice(&MAGIC);
    put_u16(output, 4, SCHEMA);
    put_u16(output, 6, FullDirectoryKind::count());
    put_u32(output, 8, plan.required_wire);
    put_u32(output, 12, HEADER_BYTES_U32);
    write_image_facts(output, plan);
    for kind in FullDirectoryKind::ALL {
        let entry = plan.layout.entry(kind);
        let offset = HEADER_BYTES + kind.index() * DIRECTORY_BYTES;
        put_u16(output, offset, kind.code());
        put_u32(output, offset + 4, entry.offset_wire);
        put_u32(output, offset + 8, entry.length_wire);
        put_u32(output, offset + 12, entry.count);
    }
    write_atoms(output, plan);
    write_fixed_rows(output, plan);
    write_typed_rows(output, plan);
    write_variable_pool(output, plan, FullDirectoryKind::EntityLists, FullDirectoryKind::EntityListBytes, &plan.members);
    write_variable_pool(output, plan, FullDirectoryKind::Documentation, FullDirectoryKind::DocumentationBytes, &plan.documentation);
    write_extensions(output, plan, &plan.extensions);
}

fn write_image_facts(output: &mut [u8], plan: &FullSemanticImagePlan<'_>) {
    match plan.image.authority {
        SemanticImageAuthority::Shared => output[16] = 0,
        SemanticImageAuthority::Language(profile) => {
            output[16] = 1;
            output[17..19].copy_from_slice(&<[u8; 2]>::from(profile));
        }
    }
    match plan.image.provenance {
        ImageProvenance::Unavailable => output[20] = 0,
        ImageProvenance::Captured { source, recipe, claim, .. } => {
            output[20] = 1;
            output[24..56].copy_from_slice(source.identity.as_ref());
            put_u32(output, 56, source.byte_len);
            output[60..92].copy_from_slice(recipe.identity.as_ref());
            output[92..94].copy_from_slice(&<[u8; 2]>::from(recipe.profile));
            output[94] = u8::from(recipe.stage);
            output[95] = u8::from(recipe.tool);
            output[96..128].copy_from_slice(recipe.toolchain.as_ref());
            output[128..160].copy_from_slice(claim.identity.as_ref());
            put_u32(output, 160, plan.provenance_scope_atoms[0]);
            put_u32(output, 164, plan.provenance_scope_atoms[1]);
            put_u32(output, 168, plan.provenance_scope_atoms[2]);
        }
    }
}

fn write_atoms(output: &mut [u8], plan: &FullSemanticImagePlan<'_>) {
    let canonical = plan.semantic.typed.canonical();
    let rows = plan.layout.entry(FullDirectoryKind::Atoms);
    let values = plan.layout.entry(FullDirectoryKind::AtomBytes);
    let mut cursor = 0_usize;
    for (row, atom) in canonical.core.atoms.iter().enumerate() {
        let offset = rows.offset + row * ATOM_ROW_BYTES;
        put_u32(output, offset, atom.start);
        put_u32(output, offset + 4, atom.length);
        let end = cursor + atom.bytes.len();
        output[values.offset + cursor..values.offset + end].copy_from_slice(atom.bytes);
        cursor = end;
    }
}

fn write_fixed_rows(output: &mut [u8], plan: &FullSemanticImagePlan<'_>) {
    copy_rows(output, plan.layout.entry(FullDirectoryKind::Entities).offset, &plan.entities);
    copy_rows(output, plan.layout.entry(FullDirectoryKind::Externals).offset, &plan.externals);
    copy_rows(output, plan.layout.entry(FullDirectoryKind::Links).offset, &plan.links);
    copy_rows(output, plan.layout.entry(FullDirectoryKind::Occurrences).offset, &plan.occurrences);
}

fn copy_rows<const N: usize>(output: &mut [u8], offset: usize, rows: &[[u8; N]]) {
    for (row, value) in rows.iter().enumerate() {
        let start = offset + row * N;
        output[start..start + N].copy_from_slice(value);
    }
}

fn write_typed_rows(output: &mut [u8], plan: &FullSemanticImagePlan<'_>) {
    let nodes = plan.layout.entry(FullDirectoryKind::TypedNodes);
    for (row, node) in plan.semantic.typed_wire.nodes.iter().copied().enumerate() {
        let offset = nodes.offset + row * TYPED_NODE_ROW_BYTES;
        output[offset] = node.domain;
        put_u32(output, offset + 4, node.coordinate);
        put_u32(output, offset + 8, node.edge_start);
        put_u32(output, offset + 12, node.edge_count);
    }
    let edges = plan.layout.entry(FullDirectoryKind::TypedEdges);
    for (row, edge) in plan.semantic.typed_wire.edges.iter().copied().enumerate() {
        let offset = edges.offset + row * TYPED_EDGE_ROW_BYTES;
        output[offset] = edge.role;
        output[offset + 1] = edge.target.tag();
        output[offset + 2] = edge.target.domain();
        put_u32(output, offset + 4, edge.role_index);
        put_u32(output, offset + 8, edge.target.low());
        put_u32(output, offset + 12, edge.target.high());
    }
}

fn write_variable_pool(
    output: &mut [u8],
    plan: &FullSemanticImagePlan<'_>,
    range_kind: FullDirectoryKind,
    bytes_kind: FullDirectoryKind,
    pool: &CanonicalVariablePool,
) {
    let ranges = plan.layout.entry(range_kind);
    let bytes = plan.layout.entry(bytes_kind);
    for (row, range) in pool.ranges.iter().copied().enumerate() {
        let offset = ranges.offset + row * RANGE_ROW_BYTES;
        put_u32(output, offset, range.start);
        put_u32(output, offset + 4, range.len);
    }
    output[bytes.offset..bytes.offset + pool.bytes.len()].copy_from_slice(&pool.bytes);
}

fn write_extensions(
    output: &mut [u8],
    plan: &FullSemanticImagePlan<'_>,
    extensions: &FullExtensionPayloads,
) {
    write_extension(
        output, plan, FullDirectoryKind::TypeScriptFacts, FullDirectoryKind::TypeScriptBindings,
        &extensions.typescript, &plan.semantic.extensions.typescript.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::CSharpFacts, FullDirectoryKind::CSharpBindings,
        &extensions.csharp, &plan.semantic.extensions.csharp.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::GoFacts, FullDirectoryKind::GoBindings,
        &extensions.go, &plan.semantic.extensions.go.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::RustFacts, FullDirectoryKind::RustBindings,
        &extensions.rust, &plan.semantic.extensions.rust.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::PythonFacts, FullDirectoryKind::PythonBindings,
        &extensions.python, &plan.semantic.extensions.python.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::JavaFacts, FullDirectoryKind::JavaBindings,
        &extensions.java, &plan.semantic.extensions.java.bindings,
    );
    write_extension(
        output, plan, FullDirectoryKind::ClangFacts, FullDirectoryKind::ClangBindings,
        &extensions.clang, &plan.semantic.extensions.clang.bindings,
    );
}

fn write_extension(
    output: &mut [u8],
    plan: &FullSemanticImagePlan<'_>,
    facts_kind: FullDirectoryKind,
    bindings_kind: FullDirectoryKind,
    facts: &CanonicalVariablePool,
    bindings: &[crate::semantic_image::full::ExtensionBinding],
) {
    let directory = plan.layout.entry(facts_kind);
    for (row, range) in facts.ranges.iter().copied().enumerate() {
        let offset = directory.offset + row * RANGE_ROW_BYTES;
        put_u32(output, offset, range.start);
        put_u32(output, offset + 4, range.len);
    }
    let values = directory.offset + facts.ranges.len() * RANGE_ROW_BYTES;
    output[values..values + facts.bytes.len()].copy_from_slice(&facts.bytes);
    let bindings_directory = plan.layout.entry(bindings_kind);
    for (row, binding) in bindings.iter().copied().enumerate() {
        let offset = bindings_directory.offset + row * SPARSE_BINDING_ROW_BYTES;
        put_u32(output, offset, binding.entity);
        put_u32(output, offset + 4, binding.fact);
    }
}
