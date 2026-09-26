//! Complete-image extension of the shared canonical plan.
//!
//! External rows are ordered by a coordinate-free key after every foreign
//! atom has already been remapped into the common arena.

use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{
    AtomId, DeclarationIdentity, EntityId, ExternalId, ExternalTarget, ForeignTargetOrigin, Ir,
    LinkTarget, SourceSpan, VariantAvailability,
};

use super::{
    CanonicalFullPlan, CanonicalImagePlan, CoreSemanticImageFault, CoreSemanticImageField,
    ExternalKey, wire_usize,
};

impl<'image> CanonicalFullPlan<'image> {
    /// Extends the common plan with external endpoint coordinates. Every atom
    /// in a foreign path/origin/display is already remapped before external
    /// rows are sorted.
    pub(in crate::ir::semantic_image) fn build(
        ir: &'image Ir,
    ) -> Result<Self, CoreSemanticImageFault> {
        let core = CanonicalImagePlan::build(ir)?;
        let mut atom_fingerprints = Vec::with_capacity(core.atoms.len());
        for atom in &core.atoms {
            atom_fingerprints.push(fingerprint_atom(atom.bytes)?);
        }
        let mut entity_identities = Vec::with_capacity(core.entities.len());
        for (row, entity) in core.entities.iter().copied().enumerate() {
            let version = ir
                .version(entity)
                .ok_or(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::EntityVersion,
                    row: wire_usize(row, CoreSemanticImageField::EntityVersion)?,
                    expected: wire_usize(
                        core.entities.len(),
                        CoreSemanticImageField::EntityVersion,
                    )?,
                    observed: wire_usize(entity.index(), CoreSemanticImageField::EntityVersion)?,
                })?;
            entity_identities.push(version.identity());
        }
        let external_count = ir.storage_columns().externals.len();
        let external_count_wire = wire_usize(external_count, CoreSemanticImageField::External)?;
        let mut external_keys = Vec::with_capacity(external_count);
        for raw in 0..external_count {
            let raw_wire = wire_usize(raw, CoreSemanticImageField::External)?;
            let id = ExternalId::new(raw_wire);
            let target = ir.external(id).ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::External,
                row: raw_wire,
                expected: external_count_wire,
                observed: raw_wire,
            })?;
            external_keys.push(external_key(*target, &core)?);
        }
        let mut source_externals = (0..external_count)
            .map(|raw| {
                let raw = wire_usize(raw, CoreSemanticImageField::External)?;
                Ok(ExternalId::new(raw))
            })
            .collect::<Result<Vec<_>, CoreSemanticImageFault>>()?;
        source_externals.sort_unstable_by_key(|id| external_keys[id.index()]);
        for (row, pair) in source_externals.windows(2).enumerate() {
            let [left, right] = pair else { continue };
            if external_keys[left.index()] == external_keys[right.index()] {
                return Err(CoreSemanticImageFault::CanonicalOrder {
                    field: CoreSemanticImageField::External,
                    previous: wire_usize(row, CoreSemanticImageField::External)?,
                    row: wire_usize(
                        row.checked_add(1)
                            .ok_or(CoreSemanticImageFault::LengthOverflow {
                                field: CoreSemanticImageField::External,
                            })?,
                        CoreSemanticImageField::External,
                    )?,
                });
            }
        }
        let mut external_remap = vec![0_u32; external_count];
        let mut externals = Vec::with_capacity(external_count);
        for (canonical, external) in source_externals.into_iter().enumerate() {
            let canonical = wire_usize(canonical, CoreSemanticImageField::External)?;
            external_remap[external.index()] = canonical;
            externals.push(external);
        }
        let mut external_fingerprints = Vec::with_capacity(external_count);
        for key in external_keys.iter().copied() {
            external_fingerprints.push(fingerprint_external(key));
        }
        Ok(Self {
            core,
            externals,
            external_remap,
            atom_fingerprints,
            entity_identities,
            external_keys,
            external_fingerprints,
        })
    }

    pub(in crate::ir::semantic_image) fn external(
        &self,
        external: ExternalId,
    ) -> Result<u32, CoreSemanticImageFault> {
        self.external_remap.get(external.index()).copied().ok_or(
            CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::External,
                row: 0,
                expected: wire_usize(self.external_remap.len(), CoreSemanticImageField::External)?,
                observed: wire_usize(external.index(), CoreSemanticImageField::External)?,
            },
        )
    }

    /// Projects an atom into the common canonical arena. The resulting
    /// coordinate is stable because that arena is ordered by exact bytes.
    pub(in crate::ir::semantic_image) fn atom(
        &self,
        atom: AtomId,
    ) -> Result<u32, CoreSemanticImageFault> {
        self.core.atom(atom)
    }

    /// Projects a local declaration into the common canonical entity arena.
    /// That arena is ordered by exact declaration instance identity.
    pub(in crate::ir::semantic_image) fn entity(
        &self,
        entity: EntityId,
    ) -> Result<u32, CoreSemanticImageFault> {
        self.core.entity(entity)
    }

    pub(in crate::ir::semantic_image) fn atom_fingerprint(
        &self,
        atom: AtomId,
    ) -> Result<[u8; 32], CoreSemanticImageFault> {
        let canonical = usize::try_from(self.core.atom(atom)?).map_err(|_| {
            CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            }
        })?;
        self.atom_fingerprints
            .get(canonical)
            .copied()
            .ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: 0,
                expected: wire_usize(
                    self.atom_fingerprints.len(),
                    CoreSemanticImageField::AtomRange,
                )?,
                observed: wire_usize(canonical, CoreSemanticImageField::AtomRange)?,
            })
    }

    pub(in crate::ir::semantic_image) fn atom_bytes(
        &self,
        atom: AtomId,
    ) -> Result<&'image [u8], CoreSemanticImageFault> {
        let canonical = usize::try_from(self.core.atom(atom)?).map_err(|_| {
            CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::AtomRange,
            }
        })?;
        self.core.atoms.get(canonical).map(|atom| atom.bytes).ok_or(
            CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::AtomRange,
                row: 0,
                expected: wire_usize(self.core.atoms.len(), CoreSemanticImageField::AtomRange)?,
                observed: wire_usize(canonical, CoreSemanticImageField::AtomRange)?,
            },
        )
    }

    pub(in crate::ir::semantic_image) fn entity_identity(
        &self,
        entity: EntityId,
    ) -> Result<DeclarationIdentity, CoreSemanticImageFault> {
        let canonical = usize::try_from(self.core.entity(entity)?).map_err(|_| {
            CoreSemanticImageFault::LengthOverflow {
                field: CoreSemanticImageField::EntityVersion,
            }
        })?;
        self.entity_identities
            .get(canonical)
            .copied()
            .ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::EntityVersion,
                row: 0,
                expected: wire_usize(
                    self.entity_identities.len(),
                    CoreSemanticImageField::EntityVersion,
                )?,
                observed: wire_usize(canonical, CoreSemanticImageField::EntityVersion)?,
            })
    }

    pub(in crate::ir::semantic_image) fn external_fingerprint(
        &self,
        external: ExternalId,
    ) -> Result<[u8; 32], CoreSemanticImageFault> {
        self.external_fingerprints
            .get(external.index())
            .copied()
            .ok_or(CoreSemanticImageFault::Reference {
                field: CoreSemanticImageField::External,
                row: 0,
                expected: wire_usize(
                    self.external_fingerprints.len(),
                    CoreSemanticImageField::External,
                )?,
                observed: wire_usize(external.index(), CoreSemanticImageField::External)?,
            })
    }

    /// Compares two raw external rows by their coordinate-free full key. The
    /// private key representation never leaks into sibling planner modules.
    pub(in crate::ir::semantic_image) fn compare_external(
        &self,
        left: ExternalId,
        right: ExternalId,
    ) -> Result<core::cmp::Ordering, CoreSemanticImageFault> {
        let left =
            self.external_keys
                .get(left.index())
                .ok_or(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::External,
                    row: 0,
                    expected: wire_usize(
                        self.external_keys.len(),
                        CoreSemanticImageField::External,
                    )?,
                    observed: wire_usize(left.index(), CoreSemanticImageField::External)?,
                })?;
        let right =
            self.external_keys
                .get(right.index())
                .ok_or(CoreSemanticImageFault::Reference {
                    field: CoreSemanticImageField::External,
                    row: 0,
                    expected: wire_usize(
                        self.external_keys.len(),
                        CoreSemanticImageField::External,
                    )?,
                    observed: wire_usize(right.index(), CoreSemanticImageField::External)?,
                })?;
        Ok(left.cmp(right))
    }

    /// Compares a graph endpoint without ever consulting raw local/external
    /// coordinates. Local endpoints order by exact declaration identity;
    /// foreign endpoints retain their closed full external key.
    pub(in crate::ir::semantic_image) fn compare_link_target(
        &self,
        left: LinkTarget,
        right: LinkTarget,
    ) -> Result<core::cmp::Ordering, CoreSemanticImageFault> {
        match (left, right) {
            (LinkTarget::Local(left), LinkTarget::Local(right)) => Ok(self
                .entity_identity(left)?
                .cmp(&self.entity_identity(right)?)),
            (LinkTarget::Local(_), LinkTarget::External(_)) => Ok(core::cmp::Ordering::Less),
            (LinkTarget::External(_), LinkTarget::Local(_)) => Ok(core::cmp::Ordering::Greater),
            (LinkTarget::External(left), LinkTarget::External(right)) => {
                self.compare_external(left, right)
            }
        }
    }

    /// Orders optional source evidence by exact file bytes and range instead
    /// of the atom coordinate assigned by an admitting builder.
    pub(in crate::ir::semantic_image) fn compare_source(
        &self,
        left: Option<SourceSpan>,
        right: Option<SourceSpan>,
    ) -> Result<core::cmp::Ordering, CoreSemanticImageFault> {
        match (left, right) {
            (None, None) => Ok(core::cmp::Ordering::Equal),
            (None, Some(_)) => Ok(core::cmp::Ordering::Less),
            (Some(_), None) => Ok(core::cmp::Ordering::Greater),
            (Some(left), Some(right)) => Ok(self
                .atom_bytes(left.file())?
                .cmp(self.atom_bytes(right.file())?)
                .then_with(|| left.start().cmp(&right.start()))
                .then_with(|| left.end().cmp(&right.end()))),
        }
    }
}

fn fingerprint_atom(bytes: &[u8]) -> Result<[u8; 32], CoreSemanticImageFault> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.semantic-image.atom.v1\0");
    let length = wire_usize(bytes.len(), CoreSemanticImageField::AtomRange)?;
    hasher.update(&length.to_le_bytes());
    hasher.update(bytes);
    Ok(*hasher.finalize().as_bytes())
}

fn fingerprint_external(key: ExternalKey) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.semantic-image.external.v1\0");
    match key {
        ExternalKey::Stable {
            fragment,
            family,
            variant,
        } => {
            hasher.update(&[0]);
            hasher.update(&fragment);
            hasher.update(&family);
            hasher.update(&variant);
        }
        ExternalKey::Foreign {
            foreign,
            variant_tag,
            variant,
            origin_tag,
            origin_first,
            origin_second,
            path,
            display,
            kind,
        } => {
            hasher.update(&[1, variant_tag, origin_tag]);
            hasher.update(&foreign);
            hasher.update(&variant);
            hasher.update(&origin_first.to_le_bytes());
            hasher.update(&origin_second.to_le_bytes());
            hasher.update(&path.to_le_bytes());
            hasher.update(&display.to_le_bytes());
            hasher.update(&kind.to_le_bytes());
        }
        ExternalKey::FragmentEntity {
            fragment,
            ordinal,
            display,
        } => {
            hasher.update(&[2]);
            hasher.update(&fragment);
            hasher.update(&ordinal.to_le_bytes());
            hasher.update(&display.to_le_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn external_key(
    target: ExternalTarget,
    core: &CanonicalImagePlan<'_>,
) -> Result<ExternalKey, CoreSemanticImageFault> {
    let atom = |id: AtomId| core.atom(id);
    match target {
        ExternalTarget::Stable { target } => Ok(ExternalKey::Stable {
            fragment: *target.fragment.as_ref(),
            family: *target.declaration.family.as_bytes(),
            variant: *target.declaration.variant.as_bytes(),
        }),
        ExternalTarget::Foreign(value) => {
            let (variant_tag, variant) = match value.identity.variant {
                VariantAvailability::Known(variant) => (1, *variant.as_bytes()),
                VariantAvailability::Unavailable => (0, [0; 16]),
            };
            let (origin_tag, origin_first, origin_second) = match value.origin {
                ForeignTargetOrigin::Package { ecosystem, package } => {
                    (0, atom(ecosystem)?, atom(package)?)
                }
                ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => (1, atom(ecosystem)?, atom(namespace)?),
                ForeignTargetOrigin::Universe { ecosystem } => (2, atom(ecosystem)?, 0),
                ForeignTargetOrigin::Unspecified { ecosystem } => (3, atom(ecosystem)?, 0),
            };
            Ok(ExternalKey::Foreign {
                foreign: *value.identity.foreign.as_bytes(),
                variant_tag,
                variant,
                origin_tag,
                origin_first,
                origin_second,
                path: atom(value.path)?,
                display: atom(value.display)?,
                // Current closed declaration codes fit below `u16::MAX`; the
                // `+1` reserves zero for unknown without a saturating alias.
                kind: match value.kind {
                    Some(kind) => u16::from(kind).checked_add(1).ok_or(
                        CoreSemanticImageFault::LengthOverflow {
                            field: CoreSemanticImageField::External,
                        },
                    )?,
                    None => 0,
                },
            })
        }
        ExternalTarget::FragmentEntity { target, display } => Ok(ExternalKey::FragmentEntity {
            fragment: *target.fragment.as_ref(),
            ordinal: target.ordinal,
            display: atom(display)?,
        }),
    }
}
