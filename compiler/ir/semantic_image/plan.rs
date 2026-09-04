//! Measured canonical ordering and pre-admitted write facts for core images.

use alloc::vec::Vec;

use crate::{
    AtomId, CoreSemanticEntity, EntityId, Ir, SemanticCoreReader, SemanticImageFacts,
};

use super::model::{
    CoreSemanticImageFault, CoreSemanticImageField, ATOM_ROW_BYTES, DIRECTORY_BYTES,
    DIRECTORY_COUNT, ENTITY_ROW_BYTES, HEADER_BYTES, NONE,
};

/// One atom row after exact owner lookup and canonical sort.
pub(super) struct PlannedAtom<'image> {
    pub(super) bytes: &'image [u8],
    pub(super) start: u32,
    pub(super) length: u32,
}

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
    pub(super) atoms: Vec<PlannedAtom<'image>>,
    pub(super) entities: Vec<PlannedEntity>,
    pub(super) image: SemanticImageFacts,
    pub(super) provenance_scope_atoms: [u32; 3],
    pub(super) wire: CoreImageWireLayout,
}

impl<'image> CoreSemanticImagePlan<'image> {
    /// Plans every allocation, lookup, remap, and checked conversion before
    /// an encoder receives mutable caller bytes.
    pub(super) fn build(ir: &'image Ir) -> Result<Self, CoreSemanticImageFault> {
        let atom_count = ir.storage_columns().atoms.ranges.len();
        let entity_count = ir.entity_count();
        let atom_count_wire = wire_usize(atom_count, CoreSemanticImageField::AtomRange)?;
        let entity_count_wire = wire_usize(entity_count, CoreSemanticImageField::EntityVersion)?;

        let mut source_atoms = Vec::with_capacity(atom_count);
        for raw in 0..atom_count {
            let raw_wire = wire_usize(raw, CoreSemanticImageField::AtomRange)?;
            let atom = AtomId::new(raw_wire);
            let bytes = ir.atom(atom).ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: raw_wire,
                expected: atom_count_wire,
                observed: raw_wire,
            })?;
            source_atoms.push((atom, bytes));
        }
        source_atoms.sort_unstable_by(|(_, left), (_, right)| left.cmp(right));
        for (row, pair) in source_atoms.windows(2).enumerate() {
            let [(_, left), (_, right)] = pair else { continue };
            if left == right {
                return Err(CoreSemanticImageFault::CanonicalOrder {
                    field: CoreSemanticImageField::AtomOrder,
                    previous: wire_usize(row, CoreSemanticImageField::AtomOrder)?,
                    row: wire_usize(
                        row.checked_add(1).ok_or(CoreSemanticImageFault::LengthOverflow {
                            field: CoreSemanticImageField::AtomOrder,
                        })?,
                        CoreSemanticImageField::AtomOrder,
                    )?,
                });
            }
        }
        let mut atom_remap = vec![0_u32; atom_count];
        let mut atom_bytes_len = 0_usize;
        let mut atoms = Vec::with_capacity(atom_count);
        for (canonical, (atom, bytes)) in source_atoms.into_iter().enumerate() {
            let canonical = wire_usize(canonical, CoreSemanticImageField::AtomRange)?;
            atom_remap[atom.index()] = canonical;
            let start = wire_usize(atom_bytes_len, CoreSemanticImageField::AtomRange)?;
            let length = wire_usize(bytes.len(), CoreSemanticImageField::AtomRange)?;
            atom_bytes_len = atom_bytes_len.checked_add(bytes.len()).ok_or(
                CoreSemanticImageFault::LengthOverflow { field: CoreSemanticImageField::AtomRange },
            )?;
            atoms.push(PlannedAtom { bytes, start, length });
        }

        let entity_ids = ir
            .canonical_core_entities()
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        if entity_ids.len() != entity_count {
            return Err(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityVersion,
                row: wire_usize(entity_ids.len(), CoreSemanticImageField::EntityVersion)?,
                expected: entity_count_wire,
                observed: wire_usize(entity_ids.len(), CoreSemanticImageField::EntityVersion)?,
            });
        }
        let mut entity_remap = vec![0_u32; entity_count];
        for (canonical, entity) in entity_ids.iter().copied().enumerate() {
            let raw = entity.index();
            if raw >= entity_remap.len() {
                return Err(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::EntityVersion,
                    row: wire_usize(canonical, CoreSemanticImageField::EntityVersion)?,
                    expected: entity_count_wire,
                    observed: wire_usize(raw, CoreSemanticImageField::EntityVersion)?,
                });
            }
            entity_remap[raw] = wire_usize(canonical, CoreSemanticImageField::EntityVersion)?;
        }
        let map_atom = |atom: AtomId| -> Result<u32, CoreSemanticImageFault> {
            atom_remap.get(atom.index()).copied().ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: 0,
                expected: atom_count_wire,
                observed: wire_usize(atom.index(), CoreSemanticImageField::AtomRange)?,
            })
        };
        let map_entity = |entity: EntityId| -> Result<u32, CoreSemanticImageFault> {
            entity_remap.get(entity.index()).copied().ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityParent,
                row: 0,
                expected: entity_count_wire,
                observed: wire_usize(entity.index(), CoreSemanticImageField::EntityVersion)?,
            })
        };
        let mut entities = Vec::with_capacity(entity_count);
        for (row, id) in entity_ids.into_iter().enumerate() {
            let entity = ir.core_semantic_entity(id).ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityVersion,
                row: wire_usize(row, CoreSemanticImageField::EntityVersion)?,
                expected: entity_count_wire,
                observed: wire_usize(id.index(), CoreSemanticImageField::EntityVersion)?,
            })?;
            let name = map_atom(entity.name)?;
            let parent = match entity.parent {
                Some(parent) => map_entity(parent)?,
                None => NONE,
            };
            let source_file = match entity.source {
                Some(source) => map_atom(source.file())?,
                None => NONE,
            };
            entities.push(PlannedEntity { entity, name, parent, source_file });
        }

        let image = ir.image_facts();
        let provenance_scope_atoms = match image.provenance {
            crate::ImageProvenance::Unavailable => [0, 0, 0],
            crate::ImageProvenance::Captured { scope, .. } => [
                map_atom(scope.ecosystem)?,
                map_atom(scope.package)?,
                map_atom(scope.path)?,
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
        Ok(Self { atoms, entities, image, provenance_scope_atoms, wire })
    }
}

fn wire_usize(
    value: usize,
    field: CoreSemanticImageField,
) -> Result<u32, CoreSemanticImageFault> {
    u32::try_from(value).map_err(|_| CoreSemanticImageFault::LengthOverflow { field })
}
