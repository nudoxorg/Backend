//! Packed, entity-aligned structural function-signature carrier roles.

use alloc::vec::Vec;

use super::reader::SignatureCarrierBindings;
use super::semantic::BuildError;
use super::{CapacityError, CapacitySpace, EntityId, SignatureCarrierRole};

/// Complete two-bit role observations for one exact entity row sequence.
pub(crate) struct PackedSignatureCarrierRoles {
    entity_count: usize,
    bytes: Vec<u8>,
}

/// One owner's contiguous role-local target ranges. `None` is a proven
/// unavailable function signature; `Some(start)` with zero lengths is a
/// captured empty signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SignatureCarrierBindingRange {
    pub(crate) owner: EntityId,
    pub(crate) target_start: Option<u32>,
    pub(crate) parameter_count: u32,
    pub(crate) result_count: u32,
}

/// Compact owner-sorted exact bindings. Repeated target IDs are intentional:
/// one declaration may occupy multiple roles, slots, or function owners.
pub(crate) struct PackedSignatureCarrierBindings {
    ranges: Vec<SignatureCarrierBindingRange>,
    targets: Vec<EntityId>,
}

impl PackedSignatureCarrierBindings {
    pub(crate) fn from_parts(
        ranges: Vec<SignatureCarrierBindingRange>,
        targets: Vec<EntityId>,
    ) -> Self {
        Self { ranges, targets }
    }

    pub(crate) fn ranges(&self) -> &[SignatureCarrierBindingRange] {
        &self.ranges
    }

    pub(crate) fn targets(&self) -> &[EntityId] {
        &self.targets
    }

    pub(crate) fn range(&self, owner: EntityId) -> Option<SignatureCarrierBindingRange> {
        let index = self
            .ranges
            .binary_search_by_key(&owner.raw, |range| range.owner.raw)
            .ok()?;
        self.ranges.get(index).copied()
    }

    pub(crate) fn iter(&self, owner: EntityId) -> Option<Option<SignatureCarrierBindings<'_>>> {
        let range = self.range(owner)?;
        let Some(start) = range.target_start else {
            return Some(None);
        };
        let count = usize::try_from(range.parameter_count)
            .ok()?
            .checked_add(usize::try_from(range.result_count).ok()?)?;
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(count)?;
        let targets = self.targets.get(start..end)?;
        Some(Some(SignatureCarrierBindings::owned(
            owner,
            targets,
            range.parameter_count,
            range.result_count,
        )))
    }
}

impl PackedSignatureCarrierRoles {
    pub(crate) fn from_roles(roles: &[SignatureCarrierRole]) -> Result<Self, BuildError> {
        let byte_count = roles.len().div_ceil(4);
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(byte_count).map_err(|_| {
            BuildError::Capacity(CapacityError {
                space: CapacitySpace::Value,
                actual: byte_count,
            })
        })?;
        bytes.resize(byte_count, 0);
        for (index, role) in roles.iter().copied().enumerate() {
            let byte = index / 4;
            let shift = (index % 4) * 2;
            if let Some(slot) = bytes.get_mut(byte) {
                *slot |= role.bits() << shift;
            }
        }
        Ok(Self {
            entity_count: roles.len(),
            bytes,
        })
    }

    pub(crate) fn entity_count(&self) -> usize {
        self.entity_count
    }

    pub(crate) fn role(&self, entity: usize) -> Option<SignatureCarrierRole> {
        if entity >= self.entity_count {
            return None;
        }
        let byte = *self.bytes.get(entity / 4)?;
        let shift = (entity % 4) * 2;
        SignatureCarrierRole::from_bits((byte >> shift) & 0b11)
    }
}
