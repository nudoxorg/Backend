//! Canonical portable core-image encoding from a pre-admitted plan.

use crate::{FactAvailability, ImageProvenance, ParentageAuthority, SemanticImageAuthority};

use super::{
    model::{
        put_u16, put_u32, CoreSemanticImageFault, DirectoryKind,
        ATOM_ROW_BYTES, DIRECTORY_BYTES, DIRECTORY_COUNT_U16, ENTITY_ROW_BYTES, MAGIC, NONE,
        SCHEMA,
    },
    plan::{CoreSemanticImagePlan, PlannedEntity},
};

/// Measures one canonical portable core image without mutating caller bytes.
pub(crate) fn core_semantic_image_len(
    ir: &crate::Ir,
) -> Result<usize, CoreSemanticImageFault> {
    Ok(CoreSemanticImagePlan::build(ir)?.wire.required)
}

/// Writes a portable core authority image from final owned `Ir` facts.
///
/// All allocation, coordinate remapping, owner lookups, and checked wire
/// conversions complete in `CoreSemanticImagePlan::build` before this method
/// observes its mutable output. A short buffer is therefore byte-for-byte
/// untouched, and a successful plan has an infallible bounded write phase.
pub(crate) fn encode_core_semantic_image(
    ir: &crate::Ir,
    output: &mut [u8],
) -> Result<usize, CoreSemanticImageFault> {
    let plan = CoreSemanticImagePlan::build(ir)?;
    let wire = plan.wire;
    if output.len() < wire.required {
        return Err(CoreSemanticImageFault::OutputTooShort {
            required: wire.required,
            actual: output.len(),
        });
    }
    write_plan(output, &plan);
    Ok(wire.required)
}

fn write_plan(output: &mut [u8], plan: &CoreSemanticImagePlan<'_>) {
    let wire = plan.wire;
    output[..wire.required].fill(0);
    output[..4].copy_from_slice(&MAGIC);
    put_u16(output, 4, SCHEMA);
    put_u16(output, 6, DIRECTORY_COUNT_U16);
    put_u32(output, 8, wire.required_wire);
    put_u32(output, 12, wire.directory_wire);
    write_image_facts(output, plan);
    write_directory(
        output,
        wire.directory,
        DirectoryKind::Atoms,
        wire.atoms_wire,
        wire.atom_rows_len_wire,
        wire.atom_count,
    );
    write_directory(
        output,
        wire.directory + DIRECTORY_BYTES,
        DirectoryKind::AtomBytes,
        wire.atom_bytes_wire,
        wire.atom_bytes_count,
        wire.atom_bytes_count,
    );
    write_directory(
        output,
        wire.directory + DIRECTORY_BYTES * 2,
        DirectoryKind::Entities,
        wire.entities_wire,
        wire.entity_rows_len_wire,
        wire.entity_count,
    );
    let mut atom_cursor = 0_usize;
    for (row, atom) in plan.atoms.iter().enumerate() {
        let row_offset = wire.atoms + row * ATOM_ROW_BYTES;
        put_u32(output, row_offset, atom.start);
        put_u32(output, row_offset + 4, atom.length);
        let end = atom_cursor + atom.bytes.len();
        output[wire.atom_bytes + atom_cursor..wire.atom_bytes + end].copy_from_slice(atom.bytes);
        atom_cursor = end;
    }
    for (row, entity) in plan.entities.iter().copied().enumerate() {
        write_entity(output, wire.entities + row * ENTITY_ROW_BYTES, entity);
    }
}

fn write_directory(
    output: &mut [u8],
    offset: usize,
    kind: DirectoryKind,
    payload_offset: u32,
    payload_len: u32,
    count: u32,
) {
    put_u16(output, offset, kind.code());
    put_u32(output, offset + 4, payload_offset);
    put_u32(output, offset + 8, payload_len);
    put_u32(output, offset + 12, count);
}

fn write_image_facts(output: &mut [u8], plan: &CoreSemanticImagePlan<'_>) {
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
            let scope = plan.provenance_scope_atoms;
            output[20] = 1;
            output[24..56].copy_from_slice(source.identity.as_ref());
            put_u32(output, 56, source.byte_len);
            output[60..92].copy_from_slice(recipe.identity.as_ref());
            output[92..94].copy_from_slice(&<[u8; 2]>::from(recipe.profile));
            output[94] = u8::from(recipe.stage);
            output[95] = u8::from(recipe.tool);
            output[96..128].copy_from_slice(recipe.toolchain.as_ref());
            output[128..160].copy_from_slice(claim.identity.as_ref());
            put_u32(output, 160, scope[0]);
            put_u32(output, 164, scope[1]);
            put_u32(output, 168, scope[2]);
        }
    }
}


fn write_entity(output: &mut [u8], offset: usize, planned: PlannedEntity) {
    let entity = planned.entity;
    put_u32(output, offset, planned.name);
    put_u16(output, offset + 4, u16::from(entity.kind));
    output[offset + 6] = visibility_code(entity.visibility);
    output[offset + 7] = parentage_code(entity.authority.parentage);
    put_u32(output, offset + 8, planned.parent);
    if let Some(source) = entity.source {
        put_u32(output, offset + 12, planned.source_file);
        put_u32(output, offset + 16, source.start());
        put_u32(output, offset + 20, source.end());
    } else {
        put_u32(output, offset + 12, NONE);
    }
    let availability = [
        entity.authority.source,
        entity.authority.source_file,
        entity.authority.members,
        entity.authority.semantic_type,
        entity.authority.documentation,
        entity.authority.visibility,
        entity.authority.attributes,
        entity.authority.language_extension,
    ];
    for (index, availability) in availability.into_iter().enumerate() {
        output[offset + 24 + index] = availability_code(availability);
    }
    output[offset + 36..offset + 52].copy_from_slice(entity.version.family.as_bytes());
    output[offset + 52..offset + 68].copy_from_slice(entity.version.variant.as_bytes());
    output[offset + 68..offset + 84].copy_from_slice(entity.version.core_payload.as_bytes());
    match entity.authority.parentage {
        ParentageAuthority::Bound(parent) => {
            output[offset + 84..offset + 100].copy_from_slice(parent.family.as_bytes());
            output[offset + 100..offset + 116].copy_from_slice(parent.variant.as_bytes());
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            output[offset + 84..offset + 100].copy_from_slice(&owner.as_bytes());
        }
        ParentageAuthority::Root | ParentageAuthority::Unavailable => {}
    }
}

const fn visibility_code(value: crate::Visibility) -> u8 {
    match value {
        crate::Visibility::Unknown => 0,
        crate::Visibility::Private => 1,
        crate::Visibility::Restricted => 2,
        crate::Visibility::Package => 3,
        crate::Visibility::Public => 4,
    }
}

const fn availability_code(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Unavailable => 0,
        FactAvailability::Captured => 1,
    }
}

const fn parentage_code(value: ParentageAuthority) -> u8 {
    match value {
        ParentageAuthority::Root => 0,
        ParentageAuthority::Bound(_) => 1,
        ParentageAuthority::UnrepresentedAuthorityOwner(_) => 2,
        ParentageAuthority::Unavailable => 3,
    }
}
