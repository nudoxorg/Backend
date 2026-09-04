//! Measured canonical ordering and pre-admitted write facts for core images.

use alloc::vec::Vec;

use crate::{CoreSemanticEntity, Ir, SemanticImageFacts};

use super::canonical::{wire_usize, CanonicalAtom, CanonicalImagePlan};
use super::model::{
    CoreSemanticImageFault, CoreSemanticImageField, ATOM_ROW_BYTES, DIRECTORY_BYTES,
    DIRECTORY_COUNT, ENTITY_ROW_BYTES, HEADER_BYTES, NONE,
};

/// One entity row after every coordinate has been remapped and validated.
#[derive(Clone, Copy)]
pub(super) struct PlannedEntity {
    pub(super) entity: CoreSemanticEntity,
    pub(super) name: u32,
    pub(super) parent: u32,
    pub(super) source_file: u32,
}

/// Fixed offsets and narrow cells proven before caller output is touched.
#[derive(Clone, Copy)]
pub(super) struct CoreImageWireLayout {
    pub(super) required: usize,
    pub(super) required_wire: u32,
    pub(super) directory: usize,
    pub(super) directory_wire: u32,
    pub(super) atoms: usize,
    pub(super) atoms_wire: u32,
    pub(super) atom_rows_len: usize,
    pub(super) atom_rows_len_wire: u32,
    pub(super) atom_bytes: usize,
    pub(super) atom_bytes_wire: u32,
    pub(super) atom_bytes_len: usize,
    pub(super) entities: usize,
    pub(super) entities_wire: u32,
    pub(super) entity_rows_len: usize,
    pub(super) entity_rows_len_wire: u32,
    pub(super) atom_count: u32,
    pub(super) atom_bytes_count: u32,
    pub(super) entity_count: u32,
}

/// Canonical remaps plus all facts required by the infallible write phase.
pub(super) struct CoreSemanticImagePlan<'image> {
    pub(super) atoms: Vec<CanonicalAtom<'image>>,
    pub(super) entities: Vec<PlannedEntity>,
    pub(super) image: SemanticImageFacts,
    pub(super) provenance_scope_atoms: [u32; 3],
    pub(super) wire: CoreImageWireLayout,
}

impl<'image> CoreSemanticImagePlan<'image> {
    /// Plans every allocation, lookup, remap, and checked conversion before
    /// an encoder receives mutable caller bytes.
    pub(super) fn build(ir: &'image Ir) -> Result<Self, CoreSemanticImageFault> {
        let canonical = CanonicalImagePlan::build(ir)?;
        let atom_count = canonical.atoms.len();
        let entity_count = canonical.entities.len();
        let atom_count_wire = wire_usize(atom_count, CoreSemanticImageField::AtomRange)?;
        let entity_count_wire = wire_usize(entity_count, CoreSemanticImageField::EntityVersion)?;
        let mut entities = Vec::with_capacity(entity_count);
        for (row, id) in canonical.entities.iter().copied().enumerate() {
            let entity = ir.core_semantic_entity(id).ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityVersion,
                row: wire_usize(row, CoreSemanticImageField::EntityVersion)?,
                expected: entity_count_wire,
                observed: wire_usize(id.index(), CoreSemanticImageField::EntityVersion)?,
            })?;
            let name = canonical.atom(entity.name)?;
            let parent = match entity.parent {
                Some(parent) => canonical.entity(parent)?,
                None => NONE,
            };
            let source_file = match entity.source {
                Some(source) => canonical.atom(source.file())?,
                None => NONE,
            };
            entities.push(PlannedEntity { entity, name, parent, source_file });
        }

        let image = canonical.image;
        let provenance_scope_atoms = match image.provenance {
            crate::ImageProvenance::Unavailable => [0, 0, 0],
            crate::ImageProvenance::Captured { scope, .. } => [
                canonical.atom(scope.ecosystem)?,
                canonical.atom(scope.package)?,
                canonical.atom(scope.path)?,
            ],
        };
        let atoms_offset = HEADER_BYTES
            .checked_add(DIRECTORY_BYTES.checked_mul(DIRECTORY_COUNT).ok_or(
                CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Directory },
            )?)
            .ok_or(CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Directory })?;
        let atom_rows_len = atom_count.checked_mul(ATOM_ROW_BYTES).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::AtomRange },
        )?;
        let atom_bytes = atoms_offset.checked_add(atom_rows_len).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::AtomRange },
        )?;
        let atom_bytes_len = canonical.atom_bytes_len()?;
        let entities_offset = atom_bytes.checked_add(atom_bytes_len).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::EntityVersion },
        )?;
        let entity_rows_len = entity_count.checked_mul(ENTITY_ROW_BYTES).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::EntityVersion },
        )?;
        let required = entities_offset.checked_add(entity_rows_len).ok_or(
            CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::Header },
        )?;
        let wire = CoreImageWireLayout {
            required,
            required_wire: wire_usize(required, CoreSemanticImageField::Header)?,
            directory: HEADER_BYTES,
            directory_wire: wire_usize(HEADER_BYTES, CoreSemanticImageField::Directory)?,
            atoms: atoms_offset,
            atoms_wire: wire_usize(atoms_offset, CoreSemanticImageField::AtomRange)?,
            atom_rows_len,
            atom_rows_len_wire: wire_usize(atom_rows_len, CoreSemanticImageField::AtomRange)?,
            atom_bytes,
            atom_bytes_wire: wire_usize(atom_bytes, CoreSemanticImageField::AtomRange)?,
            atom_bytes_len,
            entities: entities_offset,
            entities_wire: wire_usize(entities_offset, CoreSemanticImageField::EntityVersion)?,
            entity_rows_len,
            entity_rows_len_wire: wire_usize(entity_rows_len, CoreSemanticImageField::EntityVersion)?,
            atom_count: atom_count_wire,
            atom_bytes_count: wire_usize(atom_bytes_len, CoreSemanticImageField::AtomRange)?,
            entity_count: entity_count_wire,
        };
        Ok(Self { atoms: canonical.atoms, entities, image, provenance_scope_atoms, wire })
    }
}
