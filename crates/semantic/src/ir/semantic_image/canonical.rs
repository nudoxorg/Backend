//! Coordinate-free canonical remaps shared by every semantic-image grammar.
//!
//! This layer owns no wire layout. It turns the final owned `Ir` atom and
//! declaration coordinates into canonical image coordinates once, before the
//! complete encoder writes any cross-reference.

use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{
    AtomId, DeclarationIdentity, EntityId, ExternalId, Ir, SemanticCoreReader, SemanticImageFacts,
};

use super::fault::{CoreSemanticImageFault, CoreSemanticImageField};

mod full;

/// One atom borrowed from the owner in canonical byte order.
pub(super) struct CanonicalAtom<'image> {
    pub(super) bytes: &'image [u8],
    pub(super) start: u32,
    pub(super) length: u32,
}

/// Measured canonical coordinates shared by complete-image planes.
pub(super) struct CanonicalImagePlan<'image> {
    pub(super) atoms: Vec<CanonicalAtom<'image>>,
    pub(super) entities: Vec<EntityId>,
    pub(super) image: SemanticImageFacts,
    atom_remap: Vec<u32>,
    entity_remap: Vec<u32>,
}

/// Complete-image extension of the common plan.
pub(super) struct CanonicalFullPlan<'image> {
    pub(super) core: CanonicalImagePlan<'image>,
    pub(super) externals: Vec<ExternalId>,
    external_remap: Vec<u32>,
    /// Fixed coordinate-free terminal keys used by the typed dependency
    /// graph. They are full-plan-only so core encoding pays for no omitted
    /// semantic plane. `external_fingerprints` and `external_keys` remain
    /// indexed by raw `ExternalId` for O(1) source-edge projection; only
    /// `externals` itself is in canonical wire order.
    atom_fingerprints: Vec<[u8; 32]>,
    entity_identities: Vec<DeclarationIdentity>,
    external_keys: Vec<ExternalKey>,
    external_fingerprints: Vec<[u8; 32]>,
}

/// Fully coordinate-free external ordering key. Atom values have already been
/// remapped into canonical atom coordinates; no staging external ordinal is
/// part of this key.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ExternalKey {
    Stable {
        fragment: [u8; 32],
        family: [u8; 16],
        variant: [u8; 16],
    },
    Foreign {
        foreign: [u8; 16],
        variant_tag: u8,
        variant: [u8; 16],
        origin_tag: u8,
        origin_first: u32,
        origin_second: u32,
        path: u32,
        display: u32,
        kind: u16,
    },
    FragmentEntity {
        fragment: [u8; 32],
        ordinal: u32,
        display: u32,
    },
}

impl<'image> CanonicalImagePlan<'image> {
    /// Collects every common coordinate and conversion before a writer borrows
    /// caller output. The two remap vectors are proportional only to existing
    /// atom/entity rows and are dropped with the plan after writing.
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
            let [(_, left), (_, right)] = pair else {
                continue;
            };
            if left == right {
                return Err(CoreSemanticImageFault::CanonicalOrder {
                    field: CoreSemanticImageField::AtomOrder,
                    previous: wire_usize(row, CoreSemanticImageField::AtomOrder)?,
                    row: wire_usize(
                        row.checked_add(1)
                            .ok_or(CoreSemanticImageFault::LengthOverflow {
                                field: CoreSemanticImageField::AtomOrder,
                            })?,
                        CoreSemanticImageField::AtomOrder,
                    )?,
                });
            }
        }
        let mut atom_remap = vec![0_u32; atom_count];
        let mut atom_bytes = 0_usize;
        let mut atoms = Vec::with_capacity(atom_count);
        for (canonical, (atom, bytes)) in source_atoms.into_iter().enumerate() {
            let canonical = wire_usize(canonical, CoreSemanticImageField::AtomRange)?;
            // `source_atoms` was collected from precisely `0..atom_count`.
            atom_remap[atom.index()] = canonical;
            let start = wire_usize(atom_bytes, CoreSemanticImageField::AtomRange)?;
            let length = wire_usize(bytes.len(), CoreSemanticImageField::AtomRange)?;
            atom_bytes = atom_bytes.checked_add(bytes.len()).ok_or(
                CoreSemanticImageFault::LengthOverflow {
                    field: CoreSemanticImageField::AtomRange,
                },
            )?;
            atoms.push(CanonicalAtom {
                bytes,
                start,
                length,
            });
        }

        let entities = ir
            .canonical_core_entities()
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        if entities.len() != entity_count {
            return Err(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityVersion,
                row: wire_usize(entities.len(), CoreSemanticImageField::EntityVersion)?,
                expected: entity_count_wire,
                observed: wire_usize(entities.len(), CoreSemanticImageField::EntityVersion)?,
            });
        }
        let mut entity_remap = vec![0_u32; entity_count];
        for (canonical, entity) in entities.iter().copied().enumerate() {
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
        Ok(Self {
            atoms,
            entities,
            image: ir.image_facts(),
            atom_remap,
            entity_remap,
        })
    }

    pub(super) fn atom(&self, atom: AtomId) -> Result<u32, CoreSemanticImageFault> {
        self.atom_remap
            .get(atom.index())
            .copied()
            .ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: 0,
                expected: wire_usize(self.atom_remap.len(), CoreSemanticImageField::AtomRange)?,
                observed: wire_usize(atom.index(), CoreSemanticImageField::AtomRange)?,
            })
    }

    pub(super) fn entity(&self, entity: EntityId) -> Result<u32, CoreSemanticImageFault> {
        self.entity_remap
            .get(entity.index())
            .copied()
            .ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityParent,
                row: 0,
                expected: wire_usize(
                    self.entity_remap.len(),
                    CoreSemanticImageField::EntityVersion,
                )?,
                observed: wire_usize(entity.index(), CoreSemanticImageField::EntityVersion)?,
            })
    }

    pub(super) fn atom_bytes_len(&self) -> Result<usize, CoreSemanticImageFault> {
        self.atoms.last().map_or(Ok(0), |atom| {
            usize::try_from(atom.start)
                .ok()
                .and_then(|start| start.checked_add(atom.bytes.len()))
                .ok_or(CoreSemanticImageFault::LengthOverflow {
                    field: CoreSemanticImageField::AtomRange,
                })
        })
    }
}

pub(super) fn wire_usize(
    value: usize,
    field: CoreSemanticImageField,
) -> Result<u32, CoreSemanticImageFault> {
    u32::try_from(value).map_err(|_| CoreSemanticImageFault::LengthOverflow { field })
}
